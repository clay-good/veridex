//! SCRATCH AUDIT TESTS - DELETE
use veridex_core::certificate::{sign, verify, Certificate, Issuance, ProvenanceCoverage, TrustScore};
use veridex_core::engine::{EffectiveConfig, SeverityCounts, Status, Tolerances};
use veridex_core::SigningKeypair;

fn base_cert() -> Certificate {
    Certificate {
        schema: "veridex.certificate/1".into(),
        dataset_id: "acme/demo".into(),
        cdm_content_hash: "aa".repeat(32),
        veridex_version: "0.1.0".into(),
        status: Status::Pass,
        effective_config: EffectiveConfig {
            categories: None,
            only_checks: None,
            disabled_checks: vec![],
            severity_overrides: Default::default(),
            tolerances: Tolerances::default(),
        },
        checks_run: vec![],
        checks_errored: vec![],
        categories_skipped: vec![],
        findings_summary: veridex_core::certificate::FindingsSummary {
            by_severity: SeverityCounts::default(),
            by_category: Default::default(),
        },
        trust_score: TrustScore {
            score: 90,
            grade: veridex_core::certificate::Grade::A,
            rubric_version: "v1".into(),
            data_score: 100,
            provenance_pct: 66,
        },
        provenance_coverage: ProvenanceCoverage {
            known: 4,
            asserted: 0,
            unknown: 2,
        },
        readiness: None,
        issuance: Issuance {
            key_id: "kid".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
        },
    }
}

fn signed_json() -> String {
    let kp = SigningKeypair::from_seed([7u8; 32]);
    let s = sign(base_cert(), &kp);
    serde_json::to_string_pretty(&s).unwrap()
}

/// H1: nested unknown fields survive verification.
#[test]
fn h1_nested_unknown_fields() {
    let json = signed_json();
    // inject an unknown field into trust_score (nested type, no deny_unknown_fields)
    let hostile = json.replace(
        "\"rubric_version\": \"v1\"",
        "\"rubric_version\": \"v1\",\n      \"score_note\": \"CERTIFIED SAFE BY ACME\", \"attacker_grade\": \"A+\"",
    );
    assert_ne!(hostile, json, "injection must have applied");
    let parsed: veridex_core::certificate::SignedCertificate =
        serde_json::from_str(&hostile).expect("hostile cert PARSES");
    let r = verify(&parsed, None, Some(&SigningKeypair::from_seed([7u8; 32]).public_hex()));
    println!("H1 nested-unknown result: {r:?}");
    assert!(r.is_ok(), "H1 CONFIRMED: unknown nested field verified OK");
}

/// H1b: top-level unknown field must be rejected (control).
#[test]
fn h1b_toplevel_unknown_rejected() {
    let json = signed_json();
    let hostile = json.replace("\"algorithm\":", "\"evil\": 1,\n  \"algorithm\":");
    let r: Result<veridex_core::certificate::SignedCertificate, _> = serde_json::from_str(&hostile);
    println!("H1b toplevel-unknown parse: {:?}", r.err().map(|e| e.to_string()));
}

/// H1c: unknown field inside Certificate (deny) - control.
#[test]
fn h1c_cert_unknown_rejected() {
    let json = signed_json();
    let hostile = json.replace("\"dataset_id\":", "\"evil\": 1,\n    \"dataset_id\":");
    let r: Result<veridex_core::certificate::SignedCertificate, _> = serde_json::from_str(&hostile);
    println!("H1c cert-unknown parse: {:?}", r.as_ref().err().map(|e| e.to_string()));
    assert!(r.is_err());
}

/// H1d: unknown field inside checks_run / effective_config / provenance_coverage / by_severity.
#[test]
fn h1d_more_nested() {
    let kp = SigningKeypair::from_seed([7u8; 32]);
    let mut c = base_cert();
    c.checks_run.push(veridex_core::engine::ExecutedCheck {
        check_id: "structural.episode-boundary".into(),
        version: "1".into(),
        category: veridex_core::check::Category::Structural,
    });
    let json = serde_json::to_string_pretty(&sign(c, &kp)).unwrap();
    for (needle, inject) in [
        ("\"version\": \"1\"", "\"version\": \"1\", \"note\": \"attacker text\""),
        ("\"error\": 0", "\"error\": 0, \"critical\": 99"),
        ("\"known\": 4", "\"known\": 4, \"verified_by\": \"nobody\""),
        ("\"disabled_checks\": []", "\"disabled_checks\": [], \"secret\": true"),
    ] {
        let hostile = json.replace(needle, inject);
        assert_ne!(hostile, json, "injection {needle} applied");
        match serde_json::from_str::<veridex_core::certificate::SignedCertificate>(&hostile) {
            Ok(p) => {
                let r = verify(&p, None, Some(&kp.public_hex()));
                println!("H1d [{needle}] parses; verify -> {:?}", r.is_ok());
            }
            Err(e) => println!("H1d [{needle}] REJECTED at parse: {e}"),
        }
    }
}

/// H5: duplicate JSON keys.
#[test]
fn h5_duplicate_keys() {
    let json = signed_json();
    let hostile = json.replace("\"dataset_id\": \"acme/demo\"", "\"dataset_id\": \"acme/demo\", \"dataset_id\": \"evil/data\"");
    match serde_json::from_str::<veridex_core::certificate::SignedCertificate>(&hostile) {
        Ok(p) => {
            let r = verify(&p, None, None);
            println!("H5 dup-key parses as dataset_id={} verify={:?}", p.certificate.dataset_id, r.is_ok());
        }
        Err(e) => println!("H5 dup-key REJECTED: {e}"),
    }
}

/// H3: non-finite floats from JSON -> panic in signing_message?
#[test]
fn h3_nonfinite_float() {
    let json = signed_json();
    let hostile = json.replace("\"gap_factor\": 3.0", "\"gap_factor\": 1e400");
    assert_ne!(hostile, json, "gap_factor injection applied? json={json}");
    match serde_json::from_str::<veridex_core::certificate::SignedCertificate>(&hostile) {
        Ok(p) => {
            println!("H3 parsed gap_factor = {}", p.certificate.effective_config.tolerances.gap_factor);
            let r = verify(&p, None, None);
            println!("H3 verify -> {r:?}");
        }
        Err(e) => println!("H3 REJECTED at parse: {e}"),
    }
}

/// H4: malformed hex fields of odd lengths / huge lengths -> no panic.
#[test]
fn h4_malformed_hex() {
    let kp = SigningKeypair::from_seed([7u8; 32]);
    let good = sign(base_cert(), &kp);
    for sig in ["", "a", &"a".repeat(10_000), "abcdefg", "AABB"] {
        let mut s = good.clone();
        s.signature = sig.to_string();
        println!("H4 sig len {} -> {:?}", sig.len(), verify(&s, None, None).is_ok());
    }
    for pk in ["", "0", &"f".repeat(64), &"0".repeat(64), &"a".repeat(100_000)] {
        let mut s = good.clone();
        s.public_key = pk.to_string();
        println!("H4 pk len {} -> {:?}", pk.len(), verify(&s, None, None).is_ok());
    }
}

/// H2: verify without a presented dataset hash still says "verified".
#[test]
fn h2_no_dataset_binding() {
    let kp = SigningKeypair::from_seed([7u8; 32]);
    let s = sign(base_cert(), &kp);
    let v = verify(&s, None, Some(&kp.public_hex())).unwrap();
    let rendered = veridex_core::render_verified(&s, &v, true);
    println!("H2 rendered:\n{rendered}");
    let j = veridex_core::verified_json(&s, &v, true);
    println!("H2 json:\n{j}");
}

/// H6: whitespace/formatting malleability - two distinct files, same signature.
#[test]
fn h6_file_malleability() {
    let json = signed_json();
    let compact: veridex_core::certificate::SignedCertificate = serde_json::from_str(&json).unwrap();
    let compact_s = serde_json::to_string(&compact).unwrap();
    assert_ne!(compact_s, json);
    let a = verify(&serde_json::from_str(&json).unwrap(), None, None);
    let b = verify(&serde_json::from_str(&compact_s).unwrap(), None, None);
    println!("H6 pretty ok={:?} compact ok={:?} (distinct bytes)", a.is_ok(), b.is_ok());
    // reordered fields
    let reordered = format!(
        "{{\"signature\":{},\"public_key\":{},\"algorithm\":{},\"certificate\":{}}}",
        serde_json::to_string(&compact.signature).unwrap(),
        serde_json::to_string(&compact.public_key).unwrap(),
        serde_json::to_string(&compact.algorithm).unwrap(),
        serde_json::to_string(&compact.certificate).unwrap()
    );
    let r = verify(&serde_json::from_str(&reordered).unwrap(), None, None);
    println!("H6 reordered ok={:?}", r.is_ok());
}

/// H7: unicode escape / non-canonical string encodings in signed strings.
#[test]
fn h7_unicode_escape() {
    let json = signed_json();
    let hostile = json.replace("\"acme/demo\"", "\"\\u0061cme\\/demo\"");
    assert_ne!(hostile, json);
    match serde_json::from_str::<veridex_core::certificate::SignedCertificate>(&hostile) {
        Ok(p) => println!(
            "H7 escaped-form parses as {:?}, verify={:?}",
            p.certificate.dataset_id,
            verify(&p, None, None).is_ok()
        ),
        Err(e) => println!("H7 REJECTED: {e}"),
    }
}
