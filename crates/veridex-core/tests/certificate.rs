//! Behavior tests for the signed trust certificate: content binding, signing, offline verify, and
//! tamper/transplant rejection.

use veridex_core::cdm::{Dataset, Episode, Frame, Modality, Stream, ValueRef};
use veridex_core::certificate::{
    score, sign, verify, CertError, Certificate, Issuance, ProvenanceCoverage,
};
use veridex_core::{content_hash, ContentHash, RunConfig, SigningKeypair};

fn stream(name: &str, clock: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: clock.into(),
        dtype: None,
        shape: None,
        stats: None,
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
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams,
            task: None,
            labels: vec![],
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
    // Checks were run and some categories skipped (semantic/video have no MVP checks).
    assert!(!cert.checks_run.is_empty());
    assert!(cert.categories_skipped.iter().any(|c| matches!(
        c,
        veridex_core::Category::Semantic | veridex_core::Category::Video
    )));
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
