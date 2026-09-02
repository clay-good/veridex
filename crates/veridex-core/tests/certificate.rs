//! Behavior tests for the signed trust certificate: content binding, signing, offline verify, and
//! tamper/transplant rejection.

use veridex_core::cdm::{ClockKind, Dataset, Episode, Frame, Modality, Stream, ValueRef};
use veridex_core::certificate::{
    score, sign, verify, CertError, Certificate, Issuance, ProvenanceCoverage, SignedCertificate,
};
use veridex_core::{content_hash, ContentHash, RunConfig, SigningKeypair};

fn stream(name: &str, clock: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        dim_names: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        observed_header_stamps: None,
        observed_sequence: None,
        media: None,
        frame_id: None,
        frames: ts
            .iter()
            .map(|t| Frame {
                ts: *t,
                value_ref: ValueRef {
                    uri: "x".into(),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: None,
                },
            })
            .collect(),
    }
}

fn dataset(streams: Vec<Stream>) -> Dataset {
    Dataset {
        id: "acme/demo".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams,
            task: None,
            labels: vec![],
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
    }
}

fn issue_cert(d: &Dataset) -> (Certificate, ContentHash) {
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = content_hash(d);
    let verdict = engine.run(d, hash, &RunConfig::default());
    let ts = score(&verdict, &ProvenanceCoverage::of(d));
    let cert = Certificate::build(
        d.id.clone(),
        &verdict,
        ts,
        ProvenanceCoverage::of(d),
        Issuance {
            key_id: "issuer-1".into(),
            timestamp: "2026-08-07T00:00:00Z".into(),
        },
    );
    (cert, hash)
}

fn keypair() -> SigningKeypair {
    // Fixed seed for reproducible tests.
    SigningKeypair::from_seed([42u8; 32])
}

#[test]
fn certificate_binds_content_and_states_coverage() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000, 2_000_000])]);
    let (cert, hash) = issue_cert(&d);
    assert_eq!(cert.cdm_content_hash, hash.to_hex());
    assert_eq!(cert.dataset_id, "acme/demo");
    // Coverage is reported (known/asserted/unknown); this dataset has no provenance.
    assert_eq!(cert.provenance_coverage.unknown, 6);
    // Checks were run, and the certificate discloses any category the run did not cover. Every
    // category in the catalog now has at least one registered check — `video` was the last one
    // without — so nothing is skipped. This assertion is the guard on that: a category that loses
    // its checks starts appearing here again, which is exactly what a reader needs to know.
    assert!(!cert.checks_run.is_empty());
    assert!(
        cert.categories_skipped.is_empty(),
        "every check category is covered; skipped: {:?}",
        cert.categories_skipped
    );
}

#[test]
fn valid_certificate_verifies_offline() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());

    let verified = verify(&signed, Some(&hash.to_hex()), Some(&keypair().public_hex()))
        .expect("should verify");
    assert_eq!(verified.key_id, keypair().public_hex());
    assert_eq!(verified.timestamp, "2026-08-07T00:00:00Z");
}

#[test]
fn tampering_with_content_is_rejected() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _hash) = issue_cert(&d);
    let mut signed = sign(cert, &keypair());

    // Alter a field after signing — the signature must no longer verify.
    signed.certificate.trust_score.score = 100;
    let err = verify(&signed, None, None).unwrap_err();
    assert_eq!(err, CertError::SignatureMismatch);
}

#[test]
fn an_unsupported_algorithm_is_rejected() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let mut signed = sign(cert, &keypair());

    // A certificate claiming an algorithm this build cannot verify must be rejected explicitly,
    // not silently verified as ed25519.
    signed.algorithm = "rsa-pss".into();
    let err = verify(&signed, Some(&hash.to_hex()), None).unwrap_err();
    assert!(matches!(err, CertError::UnsupportedAlgorithm { .. }));
}

#[test]
fn transplanting_onto_a_different_dataset_is_rejected() {
    let d1 = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _h1) = issue_cert(&d1);
    let signed = sign(cert, &keypair());

    // A different dataset hashes differently; presenting the cert against it must fail on binding.
    let d2 = dataset(vec![stream("s", "c", &[0, 9_000_000])]);
    let h2 = content_hash(&d2);
    let err = verify(&signed, Some(&h2.to_hex()), None).unwrap_err();
    match err {
        CertError::ContentHashMismatch { presented, .. } => {
            assert_eq!(presented, h2.to_hex());
        }
        other => panic!("expected ContentHashMismatch, got {other:?}"),
    }
}

#[test]
fn wrong_issuer_key_is_rejected() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());

    let other_issuer = SigningKeypair::from_seed([7u8; 32]).public_hex();
    let err = verify(&signed, Some(&hash.to_hex()), Some(&other_issuer)).unwrap_err();
    assert!(matches!(err, CertError::UntrustedIssuer { .. }));
}

#[test]
fn signed_certificate_round_trips_through_json() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());

    let json = serde_json::to_string_pretty(&signed).unwrap();
    let parsed: veridex_core::SignedCertificate = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, signed);
    // The parsed certificate still verifies — signing is over stable canonical bytes.
    verify(&parsed, Some(&hash.to_hex()), None).expect("round-tripped cert verifies");
}

#[test]
fn certification_is_reproducible() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000, 2_000_000])]);
    let (c1, _) = issue_cert(&d);
    let (c2, _) = issue_cert(&d);
    // Same dataset, same version, same rubric => identical certificate content and signature.
    assert_eq!(c1, c2);
    assert_eq!(sign(c1, &keypair()), sign(c2, &keypair()));
}

#[test]
fn a_certificate_does_not_verify_against_a_dataset_with_a_different_manifest_count() {
    // The transplant that used to succeed: two datasets identical but for `declared_frame_count` —
    // which `structural.episode-boundary` reads, so one passes and one fails. They must not share a
    // content hash, or the clean one's certificate attests the failing one.
    let with_count = |declared: Option<u64>| {
        let mut d = dataset(vec![stream("s", "c", &[0, 1_000_000, 2_000_000])]);
        d.episodes[0].declared_frame_count = declared;
        d.canonicalize_order();
        d
    };
    let clean = with_count(Some(3));
    let corrupt = with_count(Some(9999));
    let corrupt_hash = content_hash(&corrupt);

    let (cert, clean_hash) = issue_cert(&clean);
    let signed = sign(cert, &keypair());
    assert_ne!(
        clean_hash, corrupt_hash,
        "the two datasets must not share a hash"
    );
    verify(&signed, Some(&clean_hash.to_hex()), None).expect("its own dataset verifies");
    let err = verify(&signed, Some(&corrupt_hash.to_hex()), None).unwrap_err();
    assert!(
        matches!(err, CertError::ContentHashMismatch { .. }),
        "expected a transplant rejection, got {err:?}"
    );
}

#[test]
fn the_algorithm_and_key_fields_must_be_in_their_canonical_spelling() {
    // Uppercasing the hex fields or the algorithm leaves the same semantic document but a different
    // file. Both must not verify, or two distinct files verify identically and a consumer pinning
    // certificates by file digest can be handed either.
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());
    verify(&signed, Some(&hash.to_hex()), None).expect("the canonical form verifies");

    let mut upper_sig = signed.clone();
    upper_sig.signature = upper_sig.signature.to_uppercase();
    assert!(verify(&upper_sig, Some(&hash.to_hex()), None).is_err());

    let mut upper_key = signed.clone();
    upper_key.public_key = upper_key.public_key.to_uppercase();
    assert!(verify(&upper_key, Some(&hash.to_hex()), None).is_err());

    let mut upper_alg = signed.clone();
    upper_alg.algorithm = upper_alg.algorithm.to_uppercase();
    assert!(matches!(
        verify(&upper_alg, Some(&hash.to_hex()), None),
        Err(CertError::UnsupportedAlgorithm { .. })
    ));
}

#[test]
fn the_issuer_secret_key_is_named_rather_than_called_an_untrusted_issuer() {
    // `keygen` writes `issuer` and `issuer.pub` one letter apart, and a secret key is also 64 hex
    // characters — so pointing `--key` at the secret file parses as a public key, and the answer was
    // "untrusted issuer: certificate key ... does not match", which reads as an accusation about the
    // certificate rather than the mistyped path it is.
    let secret = "07".repeat(32);
    let kp = SigningKeypair::from_secret_hex(&secret).expect("key");
    let d = dataset(vec![stream("a", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &kp);

    // The right file verifies.
    verify(&signed, Some(&hash.to_hex()), Some(&kp.public_hex())).expect("the public key verifies");

    // The wrong-but-adjacent one is named for what it is.
    let err = verify(&signed, Some(&hash.to_hex()), Some(&secret))
        .expect_err("the secret key must not verify as an issuer");
    let text = err.to_string();
    assert!(
        text.contains("secret key") && text.contains(".pub"),
        "the message must point at the file, not accuse the certificate: {text}"
    );

    // A genuinely different issuer is still an untrusted issuer.
    let other = SigningKeypair::from_secret_hex(&"09".repeat(32)).expect("key");
    let err = verify(&signed, Some(&hash.to_hex()), Some(&other.public_hex()))
        .expect_err("a different issuer is refused");
    assert!(err.to_string().contains("untrusted issuer"), "{err}");
}

/// The offline reader is the point of this document and cannot re-run Veridex. A check that crashed
/// used to sit under `checks_run` — which records invocation, not success — beside an all-zero
/// severity summary, so the certificate read as a clean, complete verdict over a check that measured
/// nothing at all.
#[test]
fn a_crashed_check_is_named_in_the_certificate_not_listed_as_run() {
    use veridex_core::check::{Category, Check, Finding, Scope, Severity};

    struct Crasher;
    impl Check for Crasher {
        fn id(&self) -> &'static str {
            "test.crasher"
        }
        fn title(&self) -> &'static str {
            "always panics"
        }
        fn category(&self) -> Category {
            Category::Video
        }
        fn default_severity(&self) -> Severity {
            Severity::Error
        }
        fn scope(&self) -> Scope {
            Scope::Dataset
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn finding_codes(&self) -> &'static [&'static str] {
            &["TEST.CRASH"]
        }
        fn run(&self, _dataset: &Dataset) -> Vec<Finding> {
            panic!("boom");
        }
    }

    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let engine = veridex_core::engine::Engine::builder()
        .register(Box::new(Crasher))
        .unwrap()
        .build();
    let hash = content_hash(&d);
    let verdict = engine.run(&d, hash, &RunConfig::default());
    std::panic::set_hook(prev);

    let cert = Certificate::build(
        d.id.clone(),
        &verdict,
        score(&verdict, &ProvenanceCoverage::of(&d)),
        ProvenanceCoverage::of(&d),
        Issuance {
            key_id: "issuer-1".into(),
            timestamp: "2026-08-07T00:00:00Z".into(),
        },
    );

    assert_eq!(
        cert.checks_errored,
        vec!["test.crasher".to_string()],
        "the crash must be on the face of the document"
    );
    assert!(
        cert.checks_run.is_empty(),
        "a check that crashed did not run: {:?}",
        cert.checks_run
    );
    assert!(
        cert.categories_skipped
            .contains(&veridex_core::Category::Video),
        "the category its only check crashed in was not covered: {:?}",
        cert.categories_skipped
    );
    // And it survives the signature round trip, since it is a signed field like any other.
    let signed = sign(cert, &keypair());
    let json = serde_json::to_string(&signed).unwrap();
    let back: veridex_core::certificate::SignedCertificate = serde_json::from_str(&json).unwrap();
    let v = verify(&back, Some(&hash.to_hex()), Some(&keypair().public_hex())).expect("verifies");
    assert_eq!(back.certificate.checks_errored, vec!["test.crasher"]);

    // ...and a reader is actually told. Everything above proves the fact is *on* the document,
    // which is where this test used to stop — and where the defect began: the field was added for
    // the offline reader, who cannot re-run Veridex to find out, and then neither renderer read it.
    // A crashed check sat under `checks_run` beside an all-zero severity count, and because a crash
    // yields `pass (warnings)` — indistinguishable from ordinary warnings — a CI gate keying on
    // `verified && status != "fail"` could not see it either.
    let text = veridex_core::render_verified(&back, &v, true, true);
    assert!(
        text.contains("errored") && text.contains("test.crasher"),
        "the terminal reader must be told a check measured nothing, and which: {text}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&back, &v, true, true)).unwrap();
    assert_eq!(
        json["checks_errored"],
        serde_json::json!(["test.crasher"]),
        "the machine reader needs the same fact as a keyable field"
    );
    // The skipped category is the same gap one axis over: the JSON has carried it since it was
    // added, the terminal render never did.
    assert!(
        text.contains("ran no checks") && text.contains("video"),
        "a category that ran nothing must reach the terminal reader too: {text}"
    );
}

/// Every other version mismatch in `verify` fails closed; the schema did not. A document declaring a
/// future schema whose fields happen to parse under today's struct was verified as though it were
/// today's — the signature makes it unforgeable, not intelligible.
#[test]
fn a_certificate_from_a_future_schema_is_refused() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (mut cert, hash) = issue_cert(&d);
    cert.schema = "veridex.certificate/999-from-the-future".into();
    // Signed *after* the edit, so the signature is genuine and the schema is the only objection.
    let signed = sign(cert, &keypair());

    match verify(&signed, Some(&hash.to_hex()), Some(&keypair().public_hex())) {
        Err(CertError::UnsupportedSchema { found, expected }) => {
            assert_eq!(found, "veridex.certificate/999-from-the-future");
            assert_eq!(expected, "veridex.certificate/1");
        }
        other => panic!("a schema this build cannot read must be refused, got {other:?}"),
    }
}

/// `verify` with no dataset path skips the transplant check entirely, and until now said so nowhere:
/// the output was byte-identical to a run that did compare hashes, and the `bound to:` line read as
/// a confirmation when it was only echoing the certificate's own claim. Hand a consumer dataset D
/// and a certificate issued for D' and every unbound invocation accepts it.
#[test]
fn an_unbound_verification_does_not_read_like_a_bound_one() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());
    let v = verify(&signed, None, Some(&keypair().public_hex())).expect("verifies");

    let unbound = veridex_core::render_verified(&signed, &v, true, false);
    assert!(
        unbound.contains("dataset NOT checked"),
        "an unbound verification must say so: {unbound}"
    );

    let bound = veridex_core::render_verified(&signed, &v, true, true);
    assert!(!bound.contains("dataset NOT checked"));
    assert_ne!(
        unbound, bound,
        "bound and unbound verification must not render identically"
    );

    // A CI script keys on the JSON, so the same fact has to be there too.
    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&signed, &v, true, false)).unwrap();
    assert_eq!(json["verified"], true);
    assert_eq!(json["dataset_checked"], false);
    let json_bound: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&signed, &v, true, true)).unwrap();
    assert_eq!(json_bound["dataset_checked"], true);
}

/// A narrowed run's certificate is genuine, correctly bound, and not a verdict on the dataset.
///
/// The terminal render has always said so ("⚠ narrowed run"). The machine render did not, and
/// `verified && status == "pass"` is the obvious CI gate to write — so a `veridex.toml` in the
/// working directory (auto-discovered, no flag needed) took the demo dataset from a signed
/// `status: fail, grade C` to a signed `status: pass, grade B` on the same content hash, and the
/// gate could not tell. `effective_config` carried the facts, but only for a reader who already
/// suspected. This is why `dataset_checked` and `issuer_verified` are booleans here too.
#[test]
fn a_narrowed_certificate_says_so_to_a_machine_reader() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let hash = content_hash(&d);
    let engine = veridex_core::checks::default_engine().unwrap();

    let issue = |cfg: &RunConfig| {
        let verdict = engine.run(&d, hash, cfg);
        let ts = score(&verdict, &ProvenanceCoverage::of(&d));
        let cert = Certificate::build(
            d.id.clone(),
            &verdict,
            ts,
            ProvenanceCoverage::of(&d),
            Issuance {
                key_id: "issuer-1".into(),
                timestamp: "2026-08-07T00:00:00Z".into(),
            },
        );
        let signed = sign(cert, &keypair());
        let v = verify(&signed, Some(&hash.to_hex()), Some(&keypair().public_hex()))
            .expect("should verify");
        let json: serde_json::Value =
            serde_json::from_str(&veridex_core::verified_json(&signed, &v, true, true)).unwrap();
        (json, veridex_core::render_verified(&signed, &v, true, true))
    };

    // The honest run: the full catalog, default thresholds. Nothing to disclose.
    let (full, _) = issue(&RunConfig::default());
    assert_eq!(full["narrowed"], false);
    assert_eq!(full["narrowing"].as_array().unwrap().len(), 0);

    // Checks deselected.
    let (disabled, disabled_text) = issue(&RunConfig {
        disabled_checks: ["temporal.clock-skew".to_string()].into_iter().collect(),
        ..Default::default()
    });
    assert_eq!(
        disabled["narrowed"], true,
        "a deselected check must be visible to a machine reader, not only in the terminal"
    );
    assert!(disabled["narrowing"][0]
        .as_str()
        .unwrap()
        .contains("temporal.clock-skew"));
    assert!(disabled_text.contains("⚠ narrowed run"));

    // A loosened threshold deselects nothing, so it slipped past the terminal warning too.
    let (moved, moved_text) = issue(&RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 10_000_000_000,
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(
        moved["narrowed"], true,
        "a loosened threshold is a narrowed run: the check ran, measured the defect, and passed it"
    );
    assert!(moved["narrowing"][0]
        .as_str()
        .unwrap()
        .contains("thresholds loosened: clock-skew 10000ms"));
    assert!(
        moved_text.contains("⚠ narrowed run"),
        "the terminal render missed the tolerance axis too: {moved_text}"
    );

    // ...but the *direction* is the whole test. A threshold moved to be stricter measures the data
    // harder than the catalog asks, which can only lower the score — the opposite of what this
    // disclosure warns about. Reporting it as a narrowing told the reader the reverse of what
    // happened, and it was not a corner case: `--profile world-model-ready` tightens cross-sensor
    // sync to 20 ms by construction, so every readiness certificate — the product's flagship
    // artifact — verified carrying a warning that it was less trustworthy than a default run.
    let (tightened, tightened_text) = issue(&RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 20_000_000,
            ..Default::default()
        },
        ..Default::default()
    });
    assert_eq!(
        tightened["narrowed"], false,
        "a tightened threshold narrows nothing: it passes less data, not more"
    );
    assert_eq!(tightened["narrowing"].as_array().unwrap().len(), 0);
    assert!(
        !tightened_text.contains("⚠ narrowed run"),
        "a stricter run must not be warned about as though it were a looser one: {tightened_text}"
    );
}

/// The certificate's own version, signed all along, was compared against nothing and reported
/// nowhere — so a certificate from an older Veridex, whose catalog lacked today's checks, read as
/// current. The far weaker `rubric_version` drift already got a warning.
#[test]
fn a_machine_reader_is_told_which_veridex_issued_the_certificate() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());
    let v = verify(&signed, Some(&hash.to_hex()), Some(&keypair().public_hex())).unwrap();

    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&signed, &v, true, true)).unwrap();
    assert_eq!(json["veridex_version"], veridex_core::VERSION);
}

/// `docs/rubric-v1.md`: "The certificate always shows both sub-scores." The terminal render filled
/// the `data` slot with the verdict *status*, so it printed "[data pass · provenance 66%]" — the
/// data sub-score never appeared, and the substitute flattered: ten warnings and no errors is
/// `pass (warnings)` beside a data score of 60.
#[test]
fn verify_prints_the_data_sub_score_not_the_status() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let data_score = cert.trust_score.data_score;
    let signed = sign(cert, &keypair());
    let v = verify(&signed, Some(&hash.to_hex()), Some(&keypair().public_hex())).unwrap();

    let text = veridex_core::render_verified(&signed, &v, true, true);
    assert!(
        text.contains(&format!("[data {data_score} ·")),
        "the data sub-score must appear in its own slot: {text}"
    );
    // The status is still reported — on its own line, where it is not mistaken for a score.
    assert!(text.contains("status:"), "{text}");
}

/// What the signature actually pins: the parsed document, not the file.
///
/// SECURITY.md claimed "a signed certificate has exactly one byte form … cannot be presented as two
/// different files that both verify", and the test named
/// `a_signed_certificate_has_exactly_one_byte_form` checked only the three *outer* fields' casing —
/// so it did not test its own name. `signing_message` re-serializes the deserialized certificate, so
/// every encoding that parses to the same struct verifies. Pinned as a fact, since a reader relying
/// on byte-uniqueness would be relying on something that is not true.
#[test]
fn a_reencoded_certificate_still_verifies_because_content_is_what_is_signed() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, hash) = issue_cert(&d);
    let signed = sign(cert, &keypair());

    let pretty = serde_json::to_string_pretty(&signed).unwrap();
    let minified: serde_json::Value = serde_json::from_str(&pretty).unwrap();
    let compact = serde_json::to_string(&minified).unwrap();
    assert_ne!(
        pretty.len(),
        compact.len(),
        "the two encodings differ in bytes"
    );

    let reparsed: veridex_core::certificate::SignedCertificate =
        serde_json::from_str(&compact).unwrap();
    verify(
        &reparsed,
        Some(&hash.to_hex()),
        Some(&keypair().public_hex()),
    )
    .expect("a re-encoded certificate verifies: the signature covers content, not bytes");

    // And content is genuinely pinned: change one signed field and it fails.
    let mut tampered = reparsed;
    tampered.certificate.trust_score.score = 100;
    assert!(
        verify(
            &tampered,
            Some(&hash.to_hex()),
            Some(&keypair().public_hex())
        )
        .is_err(),
        "altering a signed field must break the signature"
    );
}

/// A crashed check must cost the trust score, not only the verdict status.
///
/// The status is guarded (`a_run_in_which_every_check_crashed_is_not_a_pass`) and SARIF is guarded,
/// but the *score* — the number `--min-score` gates on, and the number printed on the certificate —
/// was not. A mutation audit zeroed the errored-check penalty and all 692 tests passed, so a check
/// that panicked was free.
///
/// It must not be free, and by more than pedantry: a crash suppresses whatever the check would have
/// found. If crashing cost nothing while the finding it hid costs 15, a check that panics instead
/// of reporting would *raise* the score — the shape `diff --fail-on-regression` already treats as a
/// regression for exactly this reason.
#[test]
fn a_crashed_check_costs_the_trust_score() {
    use veridex_core::check::{Category, Check, Finding, Scope, Severity};

    struct Quiet(bool);
    impl Check for Quiet {
        fn id(&self) -> &'static str {
            "test.quiet"
        }
        fn title(&self) -> &'static str {
            "finds nothing, or dies trying"
        }
        fn category(&self) -> Category {
            Category::Video
        }
        fn default_severity(&self) -> Severity {
            Severity::Error
        }
        fn scope(&self) -> Scope {
            Scope::Dataset
        }
        fn version(&self) -> &'static str {
            "1"
        }
        fn finding_codes(&self) -> &'static [&'static str] {
            &["TEST.QUIET"]
        }
        fn run(&self, _dataset: &Dataset) -> Vec<Finding> {
            if self.0 {
                panic!("boom");
            }
            Vec::new()
        }
    }

    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let hash = content_hash(&d);
    let coverage = ProvenanceCoverage::of(&d);
    let run = |crashes: bool| {
        veridex_core::engine::Engine::builder()
            .register(Box::new(Quiet(crashes)))
            .unwrap()
            .build()
            .run(&d, hash, &RunConfig::default())
    };

    let clean = run(false);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let crashed = run(true);
    std::panic::set_hook(prev);

    assert!(
        !crashed.errored_checks.is_empty(),
        "the fixture must actually crash a check"
    );
    assert_eq!(
        crashed.findings.len(),
        clean.findings.len(),
        "both runs produce no findings, so only the crash itself differs"
    );
    let (a, b) = (
        score(&crashed, &coverage).data_score,
        score(&clean, &coverage).data_score,
    );
    assert!(
        a < b,
        "a check that measured nothing must not be free: {a} vs {b}"
    );
}

// ---- the CDM encoding version a certificate is bound under ----

#[test]
fn a_certificate_records_the_encoding_its_hash_was_computed_under() {
    let d = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _) = issue_cert(&d);
    assert_eq!(
        cert.cdm_encoding_version,
        Some(veridex_core::canonical::CANONICAL_VERSION)
    );
}

#[test]
fn a_hash_mismatch_under_a_different_encoding_is_not_reported_as_tampering() {
    // The situation every encoding bump creates: the data is untouched, and this build hashes it
    // differently than the build that issued the certificate. Saying "content-hash mismatch" there
    // accuses the holder of altering data they did not touch, and sends them looking for a problem
    // that is in Veridex, not in their dataset.
    let d1 = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (mut cert, _) = issue_cert(&d1);
    // Issued by a build one encoding behind. Re-signed, so the signature is sound and *only* the
    // encoding differs — which is the real case.
    cert.cdm_encoding_version = Some(veridex_core::canonical::CANONICAL_VERSION - 1);
    let signed = sign(cert, &keypair());

    let d2 = dataset(vec![stream("s", "c", &[0, 9_000_000])]);
    let h2 = content_hash(&d2);
    match verify(&signed, Some(&h2.to_hex()), None) {
        Err(CertError::EncodingVersionMismatch {
            issued_under,
            verifying_with,
        }) => {
            assert_eq!(issued_under, veridex_core::canonical::CANONICAL_VERSION - 1);
            assert_eq!(verifying_with, veridex_core::canonical::CANONICAL_VERSION);
            let msg = CertError::EncodingVersionMismatch {
                issued_under,
                verifying_with,
            }
            .to_string();
            assert!(
                msg.contains("says nothing about whether the data changed"),
                "the message must not read as an accusation: {msg}"
            );
            assert!(!msg.starts_with("content-hash mismatch"), "{msg}");
        }
        other => panic!("expected an encoding-version refusal, got {other:?}"),
    }
}

#[test]
fn a_forged_encoding_version_does_not_reach_that_message() {
    // The declared encoding is inside the signed payload, and the signature is checked first — so
    // editing it to claim an older encoding produces a signature failure, not the softer message.
    let d1 = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _) = issue_cert(&d1);
    let signed = sign(cert, &keypair());
    let mut v: serde_json::Value = serde_json::to_value(&signed).unwrap();
    v["certificate"]["cdm_encoding_version"] =
        serde_json::json!(veridex_core::canonical::CANONICAL_VERSION - 1);
    let forged: SignedCertificate = serde_json::from_value(v).unwrap();

    let d2 = dataset(vec![stream("s", "c", &[0, 9_000_000])]);
    match verify(&forged, Some(&content_hash(&d2).to_hex()), None) {
        Err(CertError::SignatureMismatch) => {}
        other => panic!("expected a signature failure, got {other:?}"),
    }
}

#[test]
fn a_genuine_transplant_is_still_reported_as_one() {
    // Same encoding on both sides, so the hashes *are* comparable and their difference is about the
    // data. The new variant must not swallow this, and there is nothing to caveat.
    let d1 = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (cert, _) = issue_cert(&d1);
    let signed = sign(cert, &keypair());
    let d2 = dataset(vec![stream("s", "c", &[0, 9_000_000])]);
    match verify(&signed, Some(&content_hash(&d2).to_hex()), None) {
        Err(CertError::ContentHashMismatch { version_note, .. }) => {
            assert!(version_note.is_empty(), "{version_note}");
        }
        other => panic!("expected a transplant refusal, got {other:?}"),
    }
}

#[test]
fn a_certificate_issued_before_this_field_existed_still_verifies() {
    // The field is skipped when absent, so an older certificate's bytes — and therefore its
    // signature over them — are unchanged. Getting this wrong would invalidate every certificate
    // already issued, which is the one thing a portable trust document must never do.
    let d1 = dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let (mut cert, h1) = issue_cert(&d1);
    cert.cdm_encoding_version = None;
    let signed = sign(cert, &keypair());

    // Re-serializing must not put the field back: if it did, an old certificate's signature would
    // fail against its own bytes.
    let round: serde_json::Value = serde_json::to_value(&signed).unwrap();
    assert!(
        round["certificate"].get("cdm_encoding_version").is_none(),
        "an absent encoding version must stay absent: {round}"
    );
    verify(&signed, Some(&h1.to_hex()), None).expect("an older certificate still verifies");

    // And when it does not match, the message says the comparability is unknown rather than
    // accusing anyone.
    let d2 = dataset(vec![stream("s", "c", &[0, 9_000_000])]);
    match verify(&signed, Some(&content_hash(&d2).to_hex()), None) {
        Err(CertError::ContentHashMismatch { version_note, .. }) => {
            assert!(
                version_note.contains("records no CDM encoding version"),
                "{version_note}"
            );
        }
        other => panic!("expected a hash mismatch, got {other:?}"),
    }
}
