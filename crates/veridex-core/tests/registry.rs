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
