//! The static registry: publish, look up, supersede, revoke, badge — and the properties that keep a
//! registry from becoming an authority.
//!
//! The `registry` spec's own header calls it the commercial layer and the thing that makes
//! certificates "a standard that compounds". What a core library can honestly build is the part
//! that needs no service: a directory of published records that any static host can serve, with
//! verification that is the same computation wherever it runs. These tests hold the three
//! properties that make such a thing trustworthy — a registry cannot make an invalid certificate
//! look valid, cannot withdraw a valid one, and is never required.

use veridex_core::certificate::{sign, Certificate, Issuance, ProvenanceCoverage, SigningKeypair};
use veridex_core::registry::{self, Status};
use veridex_core::{content_hash, RunConfig};

/// A dataset, a run over it, and a signed certificate — the real pipeline, so the record holds a
/// document a consumer could have been handed any other way.
fn certify(dataset_id: &str, episodes: u64, timestamp: &str) -> veridex_core::SignedCertificate {
    let mut d = veridex_core::cdm::Dataset {
        id: dataset_id.into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: (0..episodes)
            .map(|i| veridex_core::cdm::Episode {
                index: i,
                start_ts: Some(0),
                end_ts: Some(1_000_000_000),
                streams: vec![],
                task: None,
                labels: vec![],
                ego_poses: None,
                ego_frame: None,
                declared_frame_count: None,
            })
            .collect(),
    };
    d.canonicalize_order();
    let engine = veridex_core::checks::default_engine().expect("catalog");
    let hash = content_hash(&d);
    let verdict = engine.run(&d, hash, &RunConfig::default());
    let trust = veridex_core::certificate::score(&verdict, &ProvenanceCoverage::of(&d));
    let keypair = SigningKeypair::from_seed([9u8; 32]);
    let cert = Certificate::build(
        d.id.clone(),
        &verdict,
        trust,
        ProvenanceCoverage::of(&d),
        Issuance {
            key_id: keypair.public_hex(),
            timestamp: timestamp.into(),
        },
    );
    sign(cert, &keypair)
}

#[test]
fn a_published_certificate_is_found_by_the_hash_it_is_bound_to() {
    // The lookup a consumer makes: they hold a dataset, they hash it, and they ask whether anyone
    // has certified those bytes. The record comes back with the document itself, not a summary —
    // so they verify it offline exactly as if it had arrived by email.
    let dir = tempfile::tempdir().expect("tempdir");
    let signed = certify("demo", 2, "2026-03-01T10:00:00Z");
    let published =
        registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    assert_eq!(published.status, Status::Current);

    let found = registry::lookup_by_hash(dir.path(), &signed.certificate.cdm_content_hash)
        .expect("the record is there");
    assert_eq!(
        found.certificate, signed,
        "the document comes back verbatim"
    );
    assert_eq!(found.dataset_id, "demo");
    assert_eq!(found.issuer_key, signed.public_key);

    // And it verifies through the registry exactly as it does offline — the same function, so the
    // two cannot disagree.
    let through = registry::verify_record(&found, Some(&signed.certificate.cdm_content_hash), None)
        .expect("verifies");
    let offline = veridex_core::verify(&signed, Some(&signed.certificate.cdm_content_hash), None)
        .expect("verifies");
    assert_eq!(through, offline);

    // A hash nobody published is "not found", which is a different answer from "not valid".
    assert!(registry::lookup_by_hash(dir.path(), &"0".repeat(64)).is_err());
}

#[test]
fn looking_up_a_dataset_by_name_answers_with_every_certificate_for_it() {
    // A dataset id is not unique over time: re-recording produces different bytes under the same
    // name. So a lookup by name has to answer with all of them and their statuses, rather than one
    // answer that hides the others.
    let dir = tempfile::tempdir().expect("tempdir");
    let first = certify("demo", 2, "2026-03-01T10:00:00Z");
    let second = certify("demo", 3, "2026-04-01T10:00:00Z");
    registry::publish(dir.path(), &first, "2026-03-01T10:05:00Z").expect("publish");
    registry::publish(dir.path(), &second, "2026-04-01T10:05:00Z").expect("publish");

    let records = registry::lookup_by_dataset(dir.path(), "demo").expect("found");
    assert_eq!(records.len(), 2, "both are kept");
    // Newest issuance first.
    assert_eq!(records[0].issued_at, "2026-04-01T10:00:00Z");
    // Publishing the newer one superseded the older rather than overwriting it: a registry whose
    // history can be edited is a registry whose claims cannot be checked.
    assert_eq!(
        records[1].status,
        Status::Superseded {
            by: second.certificate.cdm_content_hash.clone()
        }
    );
    assert_eq!(records[0].status, Status::Current);
    assert!(registry::lookup_by_dataset(dir.path(), "nobody").is_err());
}

#[test]
fn revocation_is_the_issuers_word_and_not_a_withdrawal_of_the_document() {
    // The honest limit: a certificate that has left cannot be recalled, and it still verifies after
    // revocation. What a registry can offer is the issuer's statement where a consumer will read it.
    let dir = tempfile::tempdir().expect("tempdir");
    let signed = certify("demo", 2, "2026-03-01T10:00:00Z");
    registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    let revoked = registry::revoke(
        dir.path(),
        &signed.certificate.cdm_content_hash,
        "the calibration was wrong",
        "2026-05-01T09:00:00Z",
    )
    .expect("revoke");

    match &revoked.status {
        Status::Revoked { reason, at } => {
            assert_eq!(reason, "the calibration was wrong");
            assert_eq!(at, "2026-05-01T09:00:00Z");
        }
        other => panic!("{other:?}"),
    }
    assert!(!revoked.status.is_current(), "a gate must not accept it");
    // The certificate itself is untouched and still verifies.
    assert!(registry::verify_record(&revoked, None, None).is_ok());
    // And the lookup says so, in the words a reader needs.
    let text = registry::render_record(&revoked);
    assert!(text.contains("revoked"), "{text}");
    assert!(text.contains("the calibration was wrong"), "{text}");
    assert!(
        text.contains("cannot make one valid and cannot withdraw one"),
        "the limit is printed with the answer: {text}"
    );
    // Revoking something nobody published is not found.
    assert!(registry::revoke(dir.path(), &"0".repeat(64), "", "now").is_err());
}

#[test]
fn a_certificate_that_does_not_verify_is_not_published() {
    // The property that keeps a registry from becoming a place where an invalid document looks
    // valid. Checked at publication, with the same function `veridex verify` calls.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut tampered = certify("demo", 2, "2026-03-01T10:00:00Z");
    tampered.certificate.trust_score.score += 10;
    let err = registry::publish(dir.path(), &tampered, "2026-03-01T10:05:00Z")
        .expect_err("a tampered certificate is refused");
    assert!(err.to_string().contains("does not verify"), "{err}");
    // Nothing was written: a refused publication leaves no trace to look up.
    assert!(registry::read_all(dir.path()).expect("readable").is_empty());
}

#[test]
fn the_index_lists_every_record_without_the_documents() {
    // What a static host serves as a listing: enough to browse, not the certificates themselves.
    let dir = tempfile::tempdir().expect("tempdir");
    for (episodes, at) in [(2u64, "2026-03-01T10:00:00Z"), (3, "2026-04-01T10:00:00Z")] {
        let signed = certify("demo", episodes, at);
        registry::publish(dir.path(), &signed, at).expect("publish");
    }
    let index: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(registry::INDEX_FILE)).expect("index written"),
    )
    .expect("json");
    assert_eq!(index["schema"], registry::REGISTRY_SCHEMA_VERSION);
    let entries = index["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["issued_at"], "2026-04-01T10:00:00Z");
    assert_eq!(entries[0]["status"], "current");
    assert_eq!(entries[1]["status"], "superseded");
    assert!(
        entries[0].get("certificate").is_none(),
        "the index is a listing, not a copy of every document"
    );
}

#[test]
fn a_badge_reflects_the_current_status_and_not_the_grade_alone() {
    // A badge is read at a glance, so a revoked certificate must not show a green grade. It is
    // rendered here rather than fetched from a badge service: an image request to somebody else's
    // host tells that host who is reading which dataset's page.
    let dir = tempfile::tempdir().expect("tempdir");
    let signed = certify("demo", 2, "2026-03-01T10:00:00Z");
    let record = registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    let svg = registry::badge(&record);
    assert!(svg.starts_with("<svg"), "{svg}");
    assert!(svg.contains("veridex"), "{svg}");
    assert!(
        svg.contains(record.certificate.certificate.trust_score.grade.letter()),
        "{svg}"
    );
    let revoked = registry::revoke(
        dir.path(),
        &signed.certificate.cdm_content_hash,
        "withdrawn",
        "2026-05-01T09:00:00Z",
    )
    .expect("revoke");
    let svg = registry::badge(&revoked);
    assert!(svg.contains("revoked"), "{svg}");
    assert!(
        svg.contains("#9f9f9f"),
        "a withdrawn certificate is grey, not green: {svg}"
    );
}

#[test]
fn nothing_in_the_pipeline_needs_a_registry() {
    // Neutrality, asserted rather than claimed: a run, a certificate and an offline verification
    // happen with no registry anywhere, and the certificate a registry would hold is the same
    // document either way.
    let signed = certify("demo", 2, "2026-03-01T10:00:00Z");
    assert!(
        veridex_core::verify(&signed, Some(&signed.certificate.cdm_content_hash), None).is_ok()
    );
    // Publishing does not alter the document, so a consumer who never looks loses nothing.
    let dir = tempfile::tempdir().expect("tempdir");
    let record = registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    assert_eq!(record.certificate, signed);
}

#[test]
fn a_file_that_is_not_a_record_is_refused_rather_than_skipped() {
    // A registry read that quietly ignored unreadable files would answer "no certificate for this
    // dataset" over a registry that holds one it could not parse.
    let dir = tempfile::tempdir().expect("tempdir");
    let signed = certify("demo", 2, "2026-03-01T10:00:00Z");
    registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    std::fs::write(
        dir.path().join(registry::BY_HASH_DIR).join("junk.json"),
        b"{\"schema\":\"something.else/9\"}",
    )
    .expect("write");
    let err = registry::read_all(dir.path()).expect_err("refused");
    assert!(
        err.to_string().contains("not a Veridex registry record"),
        "{err}"
    );
}

/// A certificate's own fields name a file in the registry, and a hostile one must not choose where.
///
/// `publish` writes `by-hash/<cdm_content_hash>.json`, and the hash comes from the document being
/// published. Verification does not constrain it — it cannot: a registry does not decide whom to
/// trust, so a self-signed certificate is a valid document whoever made it. So a certificate
/// declaring a hash of `../../../tmp/x` is a signed instruction to write outside the registry, and
/// a lookup with the same string reads outside it. A content hash is 64 lowercase hex characters;
/// anything else is not one, and is refused before it reaches a path.
#[test]
fn a_certificate_cannot_choose_where_in_the_filesystem_its_record_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_dir = dir.path().join("registry");
    std::fs::create_dir_all(&registry_dir).expect("mkdir");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");

    let keypair = SigningKeypair::from_seed([4u8; 32]);
    for hostile in [
        "../outside/escaped",
        "../../outside/escaped",
        "/etc/veridex-escaped",
        "not-hex-at-all",
        // The right length, wrong alphabet.
        &"g".repeat(64),
        // Right alphabet, wrong length.
        "abc123",
    ] {
        let mut signed = certify("escape", 1, "2026-03-01T10:00:00Z");
        signed.certificate.cdm_content_hash = hostile.to_string();
        // Re-signed, so the document is internally valid: the refusal has to come from the
        // registry's own rule about what can be a filename, not from a broken signature.
        let signed = sign(signed.certificate, &keypair);
        assert!(
            veridex_core::verify(&signed, None, None).is_ok(),
            "the hostile document is a validly signed one: {hostile}"
        );

        let err = registry::publish(&registry_dir, &signed, "2026-03-01T10:05:00Z")
            .expect_err("a hash that is not a hash is not publishable");
        let text = err.to_string();
        assert!(
            text.contains("content hash"),
            "the refusal says what was wrong: {text}"
        );
        // And nothing was written, anywhere.
        assert!(
            std::fs::read_dir(&outside)
                .expect("the outside directory")
                .next()
                .is_none(),
            "{hostile} wrote outside the registry"
        );

        // The same string on the read side reads nothing rather than reaching out of the registry.
        let err = registry::lookup_by_hash(&registry_dir, hostile)
            .expect_err("a lookup for a non-hash finds nothing");
        assert!(
            err.to_string().contains("content hash"),
            "{}",
            err.to_string()
        );
    }

    // A real publication still works beside all of that, so the rule is not simply "refuse".
    let good = certify("escape", 1, "2026-03-01T11:00:00Z");
    registry::publish(&registry_dir, &good, "2026-03-01T11:05:00Z").expect("publish");
    assert!(registry::lookup_by_hash(&registry_dir, &good.certificate.cdm_content_hash).is_ok());
}

/// A registry directory is somebody else's, so what it can make a reader do is bounded.
///
/// Three shapes, each one a directory entry choosing something the reader did not: a symlink
/// deciding which bytes are read and from where, a record file large enough to be the memory
/// budget, and a file whose name disagrees with the hash the record inside it is bound to. The last
/// is not hostile so much as wrong, and it is the one that makes a lookup by hash and a listing
/// answer differently about the same dataset.
#[test]
fn a_hostile_registry_directory_cannot_choose_what_the_reader_holds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry_dir = dir.path().join("registry");
    let signed = certify("bounded", 1, "2026-03-01T10:00:00Z");
    let record =
        registry::publish(&registry_dir, &signed, "2026-03-01T10:05:00Z").expect("publish");
    let by_hash = registry_dir.join("by-hash");
    let real = by_hash.join(format!("{}.json", record.cdm_content_hash));

    // 1. A symlink standing in for a record.
    let elsewhere = dir.path().join("elsewhere.json");
    std::fs::copy(&real, &elsewhere).expect("copy");
    let linked = by_hash.join(format!("{}.json", "a".repeat(64)));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, &linked).expect("symlink");
    #[cfg(unix)]
    {
        let err = registry::read_all(&registry_dir).expect_err("a symlink is not a record");
        assert!(
            err.to_string().contains("not a regular file"),
            "{}",
            err.to_string()
        );
        std::fs::remove_file(&linked).expect("unlink");
    }

    // 2. A record past the ceiling. The padding goes inside a JSON string, so the file is still a
    // parseable record and the refusal is the size rather than the syntax.
    let big = by_hash.join(format!("{}.json", "b".repeat(64)));
    let text = std::fs::read_to_string(&real).expect("read");
    // Pretty-printed, so the field is spaced as serde writes it.
    let padded = text.replace(
        "\"dataset_id\": \"bounded\"",
        &format!("\"dataset_id\": \"{}\"", "x".repeat(2 * 1024 * 1024)),
    );
    assert_ne!(padded, text, "the padding went in");
    std::fs::write(&big, &padded).expect("write");
    let err = registry::read_all(&registry_dir).expect_err("an oversized record is refused");
    assert!(err.to_string().contains("ceiling"), "{}", err.to_string());
    std::fs::remove_file(&big).expect("remove");

    // 3. A record filed under a hash it is not bound to.
    let misfiled = by_hash.join(format!("{}.json", "c".repeat(64)));
    std::fs::copy(&real, &misfiled).expect("copy");
    let err = registry::read_all(&registry_dir).expect_err("a misfiled record is refused");
    let text = err.to_string();
    assert!(
        text.contains("named for") && text.contains(&record.cdm_content_hash),
        "{text}"
    );
    std::fs::remove_file(&misfiled).expect("remove");

    // And with the hostile entries gone, the registry reads exactly as it did.
    let all = registry::read_all(&registry_dir).expect("reads");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].cdm_content_hash, record.cdm_content_hash);
}

/// Verifying through the registry is the same computation as verifying offline, including failing.
///
/// This is the whole of what a "public verification endpoint" can honestly be. A registry that
/// verified by its own rules would be an authority — a place where a document is valid because the
/// registry says so — and the point of a signed certificate is that the answer does not depend on
/// where it is computed. So the property is an equality, and it has to hold on the failures too: a
/// wrong issuer and a hash from another dataset must be refused through the registry exactly as
/// they are refused by hand.
#[test]
fn verifying_through_the_registry_is_the_same_computation_as_offline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let signed = certify("endpoint", 2, "2026-03-01T10:00:00Z");
    let other = certify("elsewhere", 3, "2026-03-01T10:00:00Z");
    let record = registry::publish(dir.path(), &signed, "2026-03-01T10:05:00Z").expect("publish");
    let found = registry::lookup_by_hash(dir.path(), &signed.certificate.cdm_content_hash)
        .expect("the record is there");
    assert_eq!(
        found.certificate, signed,
        "the document comes back verbatim"
    );

    let hash = signed.certificate.cdm_content_hash.as_str();
    let wrong_hash = other.certificate.cdm_content_hash.as_str();
    let issuer = signed.public_key.as_str();
    let wrong_issuer = "0".repeat(64);
    let cases: [(Option<&str>, Option<&str>); 5] = [
        (None, None),
        (Some(hash), None),
        (Some(hash), Some(issuer)),
        (Some(wrong_hash), None),
        (Some(hash), Some(wrong_issuer.as_str())),
    ];
    for (presented, expected_issuer) in cases {
        let through = registry::verify_record(&record, presented, expected_issuer);
        let offline = veridex_core::verify(&signed, presented, expected_issuer);
        assert_eq!(
            through.is_ok(),
            offline.is_ok(),
            "the two answers differ for ({presented:?}, {expected_issuer:?})"
        );
        match (through, offline) {
            (Ok(a), Ok(b)) => assert_eq!(
                format!("{a:?}"),
                format!("{b:?}"),
                "the same verification has to produce the same statement"
            ),
            (Err(a), Err(b)) => assert_eq!(
                a.to_string(),
                b.to_string(),
                "including the refusal, which is what a reader acts on"
            ),
            _ => unreachable!("the branches agree by the assertion above"),
        }
    }
    // Two of those cases must actually fail, or the equality above is over nothing.
    assert!(registry::verify_record(&record, Some(wrong_hash), None).is_err());
    assert!(registry::verify_record(&record, None, Some(&wrong_issuer)).is_err());
}
