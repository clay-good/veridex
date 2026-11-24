//! The append-only issuance log: what it detects, and what it says it cannot.
//!
//! The `security` spec asks for a tamper-evident, append-only record of issuance and verification,
//! and for altering a prior entry to be *detectable*. These tests are the detection, case by case —
//! and the last two are the limits, asserted so they cannot quietly become claims.

use veridex_core::audit::{self, Action, Record};
use veridex_core::certificate::SigningKeypair;

fn record(dataset: &str, n: u8) -> Record {
    Record {
        action: Action::Issued,
        dataset_id: dataset.to_string(),
        cdm_hash: format!("{n:064x}"),
        outcome: "pass".to_string(),
        key_id: String::new(),
        certificate_hash: format!("{:064x}", n as u16 + 1000),
        timestamp: format!("2026-11-2{n}T00:00:00Z"),
    }
}

/// A log of `n` unsigned entries, as the file's text.
fn log_of(n: u8) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    for i in 0..n {
        audit::append(&path, &record("demo", i), None).expect("append");
    }
    std::fs::read_to_string(&path).expect("read back")
}

#[test]
fn a_log_grows_by_one_line_per_entry_and_chains() {
    let text = log_of(3);
    assert_eq!(text.lines().count(), 3);
    let report = audit::verify_log(&text).expect("a log this build wrote is readable");
    assert!(report.intact(), "{:?}", report.broken);
    assert_eq!(report.entries.len(), 3);
    assert_eq!(report.entries[0].prev, audit::GENESIS);
    assert_eq!(report.entries[1].prev, report.entries[0].entry_hash);
    assert_eq!(report.entries[2].prev, report.entries[1].entry_hash);
    assert_eq!(report.head, report.entries[2].entry_hash);
    assert_eq!(report.signed, 0);
}

#[test]
fn altering_a_prior_entry_is_detected_at_that_line() {
    // The requirement in one test: a past entry's content is changed, and the log says which line.
    let text = log_of(3);
    let tampered = text.replacen("\"outcome\":\"pass\"", "\"outcome\":\"fail\"", 1);
    assert_ne!(tampered, text, "the edit must actually land");
    let report = audit::verify_log(&tampered).expect("still parses");
    assert_eq!(
        report.broken,
        Some(audit::Break::EntryAltered { line: 1, seq: 0 }),
        "{:?}",
        report.broken
    );
    assert!(!report.intact());
}

#[test]
fn re_chaining_an_edited_entry_still_breaks_the_next_link() {
    // The sophisticated version of the same attack: the editor fixes the altered entry's own hash,
    // so line 1 hashes correctly. The entry *after* it recorded the old hash, so the chain parts
    // there instead — which is the whole point of a chain over a list.
    let text = log_of(3);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut first: veridex_core::audit::Entry = serde_json::from_str(&lines[0]).expect("an entry");
    first.outcome = "fail".to_string();
    // Recompute the hash the honest way, by asking the module for the entry it would have written.
    let rewritten = audit::next_entry(
        None,
        &Record {
            action: first.action,
            dataset_id: first.dataset_id.clone(),
            cdm_hash: first.cdm_hash.clone(),
            outcome: first.outcome.clone(),
            key_id: first.key_id.clone(),
            certificate_hash: first.certificate_hash.clone(),
            timestamp: first.timestamp.clone(),
        },
        None,
    );
    lines[0] = audit::render_entry(&rewritten);
    let report = audit::verify_log(&lines.join("\n")).expect("still parses");
    match report.broken {
        Some(audit::Break::LinkBroken { line, .. }) => assert_eq!(line, 2),
        other => panic!("a re-hashed edit must break the next link, got {other:?}"),
    }
}

#[test]
fn removing_an_entry_from_the_middle_is_detected() {
    let text = log_of(4);
    let lines: Vec<&str> = text.lines().collect();
    let without_second = format!("{}\n{}\n{}", lines[0], lines[2], lines[3]);
    let report = audit::verify_log(&without_second).expect("still parses");
    // The sequence numbers are the first thing that no longer runs 0, 1, 2 — which is the honest
    // name for what happened, and cheaper to read than a hash mismatch.
    assert_eq!(
        report.broken,
        Some(audit::Break::SequenceJumped {
            line: 2,
            expected: 1,
            found: 2
        }),
        "{:?}",
        report.broken
    );
}

#[test]
fn an_entry_signed_by_the_issuer_cannot_be_rewritten_without_the_key() {
    // Why an entry carries a signature at all: the chain alone is only evidence against an attacker
    // who cannot recompute it. A signed entry cannot be rewritten into a consistent history by
    // anyone without the issuer's secret key.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    let key = SigningKeypair::from_seed([7u8; 32]);
    let mut r = record("demo", 0);
    r.key_id = key.public_hex();
    audit::append(&path, &r, Some(&key)).expect("append");
    let text = std::fs::read_to_string(&path).expect("read");
    let report = audit::verify_log(&text).expect("parses");
    assert!(report.intact(), "{:?}", report.broken);
    assert_eq!(report.signed, 1);

    // An attacker with the file edits the outcome and recomputes the entry hash — which they can,
    // it is a public function of public bytes. The signature is what they cannot produce.
    let forged = audit::next_entry(
        None,
        &Record {
            outcome: "pass".to_string(),
            ..{
                let mut r2 = record("demo", 0);
                r2.key_id = key.public_hex();
                r2
            }
        },
        None,
    );
    let mut entry: veridex_core::audit::Entry =
        serde_json::from_str(&audit::render_entry(&forged)).expect("an entry");
    // Keep the real signature from the honest entry; it signs different bytes now.
    entry.signature = report.entries[0].signature.clone();
    entry.outcome = "overwritten".to_string();
    let report = audit::verify_log(&audit::render_entry(&entry)).expect("parses");
    assert!(
        matches!(report.broken, Some(audit::Break::EntryAltered { .. }))
            || matches!(report.broken, Some(audit::Break::SignatureMismatch { .. })),
        "a rewritten signed entry must not read as intact: {:?}",
        report.broken
    );
}

#[test]
fn a_file_that_is_not_a_log_is_an_error_not_a_break() {
    // "This file was edited" and "this file is not a log" are different answers, and conflating
    // them would have `veridex audit` report tampering on a JSON report someone passed by mistake.
    let err = audit::verify_log("{\"status\":\"pass\"}\n").expect_err("not an entry");
    assert!(
        matches!(err, veridex_core::AuditError::NotAnEntry { line: 1, .. }),
        "{err}"
    );
    let unknown =
        "{\"schema\":\"veridex.audit/99\",\"seq\":0,\"prev\":\"genesis\",\"action\":\"issued\",\
         \"dataset_id\":\"d\",\"cdm_hash\":\"\",\"cdm_encoding_version\":1,\"outcome\":\"pass\",\
         \"key_id\":\"\",\"certificate_hash\":\"\",\"timestamp\":\"t\",\"veridex_version\":\"0\",\
         \"entry_hash\":\"\"}"
            .to_string();
    assert!(
        matches!(
            audit::verify_log(&unknown),
            Err(veridex_core::AuditError::UnknownSchema { line: 1, .. })
        ),
        "an entry from a future schema is refused, not guessed at"
    );
}

#[test]
fn a_log_shortened_from_the_end_is_a_valid_chain_and_the_head_is_how_you_notice() {
    // The limit, asserted so it cannot quietly become a claim. Three entries, the last one dropped:
    // what remains is a perfectly valid two-entry chain. Nothing inside the file can say otherwise —
    // which is why `verify_log` reports the head, for a caller to pin or publish.
    let text = log_of(3);
    let full = audit::verify_log(&text).expect("parses");
    let lines: Vec<&str> = text.lines().collect();
    let truncated = audit::verify_log(&lines[..2].join("\n")).expect("parses");
    assert!(
        truncated.intact(),
        "truncation is undetectable from inside the file — that is the limit being documented"
    );
    assert_ne!(
        truncated.head, full.head,
        "but the head moved, which is what a pinned head catches"
    );
    assert_eq!(truncated.head, full.entries[1].entry_hash);
}

#[test]
fn an_empty_or_absent_log_has_the_genesis_head() {
    let report = audit::verify_log("").expect("an empty log is a log");
    assert!(report.intact());
    assert!(report.entries.is_empty());
    assert_eq!(report.head, audit::GENESIS);
}

#[test]
fn appending_is_deterministic_for_the_same_inputs() {
    // Two logs built from the same records are byte-identical: nothing in an entry comes from a
    // clock, a random source or the filesystem. A log that differed run to run could not be
    // published alongside a certificate.
    assert_eq!(log_of(3), log_of(3));
}

#[test]
fn every_field_of_an_entry_is_inside_its_hash() {
    // The guard that keeps the chain honest as the entry grows: flip each field in turn and the
    // recorded hash must stop matching. A field outside the hash is a field an editor can change
    // for free.
    let text = log_of(1);
    let entry: veridex_core::audit::Entry = serde_json::from_str(text.trim()).expect("an entry");
    let mutations: Vec<(&str, veridex_core::audit::Entry)> = vec![
        ("seq", {
            let mut e = entry.clone();
            e.seq = 9;
            e
        }),
        ("prev", {
            let mut e = entry.clone();
            e.prev = "deadbeef".into();
            e
        }),
        ("action", {
            let mut e = entry.clone();
            e.action = Action::Verified;
            e
        }),
        ("dataset_id", {
            let mut e = entry.clone();
            e.dataset_id = "other".into();
            e
        }),
        ("cdm_hash", {
            let mut e = entry.clone();
            e.cdm_hash = "f".repeat(64);
            e
        }),
        ("cdm_encoding_version", {
            let mut e = entry.clone();
            e.cdm_encoding_version += 1;
            e
        }),
        ("outcome", {
            let mut e = entry.clone();
            e.outcome = "fail".into();
            e
        }),
        ("key_id", {
            let mut e = entry.clone();
            e.key_id = "ab".repeat(32);
            e
        }),
        ("certificate_hash", {
            let mut e = entry.clone();
            e.certificate_hash = "1".repeat(64);
            e
        }),
        ("timestamp", {
            let mut e = entry.clone();
            e.timestamp = "1970-01-01T00:00:00Z".into();
            e
        }),
        ("veridex_version", {
            let mut e = entry.clone();
            e.veridex_version = "99.99.99".into();
            e
        }),
    ];
    for (field, altered) in mutations {
        let report = audit::verify_log(&audit::render_entry(&altered)).expect("parses");
        assert!(
            !report.intact(),
            "`{field}` is outside the entry hash: changing it left the chain reading as intact"
        );
    }
}

#[test]
fn a_log_can_be_asked_whether_it_records_one_certificate_document() {
    // What a consumer holding a certificate and a published log actually wants to know: does the
    // issuer's own record name *this document*? Matched by digest, because that is what an entry
    // holds — so the answer is about bytes, not about claims.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("audit.jsonl");
    let document = br#"{"certificate":{"cdm_content_hash":"abc123"},"signature":"ff"}"#;
    let mut r = record("demo", 0);
    r.cdm_hash = "abc123".to_string();
    r.certificate_hash = audit::document_hash(document);
    audit::append(&path, &r, None).expect("append");
    let report = audit::verify_log(&std::fs::read_to_string(&path).expect("read")).expect("parses");

    assert_eq!(
        audit::certificate_in_log(&report, document),
        audit::CertificateRecord::ThisDocument { seqs: vec![0] }
    );

    // The distinction that makes this worth a type: the same certificate content, re-serialized,
    // is a different document — and a log that records *an* issuance for the dataset is not a log
    // that records this file.
    let reserialized = br#"{ "certificate": { "cdm_content_hash": "abc123" }, "signature": "ff" }"#;
    assert_eq!(
        audit::certificate_in_log(&report, reserialized),
        audit::CertificateRecord::SameDatasetOnly { seqs: vec![0] },
        "re-serialized is not this document, and saying so is the point"
    );

    // A certificate for another dataset entirely: the log says nothing about it.
    let other = br#"{"certificate":{"cdm_content_hash":"ffffff"}}"#;
    assert_eq!(
        audit::certificate_in_log(&report, other),
        audit::CertificateRecord::Absent
    );
    // And something that is not a certificate at all is absent rather than an error: `audit` is
    // answering "is this in the log", and nothing that is not in the log can be.
    assert_eq!(
        audit::certificate_in_log(&report, b"not json"),
        audit::CertificateRecord::Absent
    );
}
