//! Integration tests that run the real `veridex` binary and assert its command dispatch, argument
//! validation, and exit codes — the CI contract users depend on. Cargo exposes the built binary as
//! `CARGO_BIN_EXE_veridex`, so no dataset fixture is needed for these paths.

use std::process::Command;

/// Run `veridex <args...>` and return (exit code, stdout, stderr).
fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_veridex"))
        .args(args)
        .output()
        .expect("spawn veridex");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn version_prints_and_exits_zero() {
    let (code, stdout, _) = run(&["--version"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("veridex"), "unexpected: {stdout}");
}

#[test]
fn checks_lists_the_catalog_and_exits_zero() {
    let (code, stdout, _) = run(&["checks"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("built-in checks"));
    // A representative finding code is surfaced under its check.
    assert!(stdout.contains("TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn checks_json_is_valid_and_carries_finding_codes() {
    let (code, stdout, _) = run(&["checks", "--json"]);
    assert_eq!(code, 0);
    let catalog: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let arr = catalog.as_array().expect("array");
    assert!(!arr.is_empty());
    // Every entry declares its id, category, and finding codes.
    for entry in arr {
        assert!(entry["id"].is_string());
        assert!(entry["category"].is_string());
        assert!(
            entry["finding_codes"]
                .as_array()
                .is_some_and(|c| !c.is_empty()),
            "each check must declare finding codes: {entry}"
        );
    }
}

#[test]
fn unknown_command_is_a_tool_error() {
    let (code, _, stderr) = run(&["frobnicate"]);
    assert_eq!(
        code, 2,
        "unknown command must exit with the tool-error code"
    );
    assert!(stderr.contains("unknown command"));
}

#[test]
fn check_without_a_path_is_a_tool_error() {
    let (code, _, stderr) = run(&["check"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("missing dataset path"));
}

#[test]
fn check_on_a_nonexistent_path_is_a_tool_error() {
    let (code, _, stderr) = run(&["check", "/no/such/dataset-xyz"]);
    assert_eq!(code, 2);
    assert!(stderr.starts_with("veridex:"), "unexpected: {stderr}");
}

#[test]
fn invalid_fail_on_value_is_rejected_not_silently_ignored() {
    // A `--fail-on warn` typo must be a hard error, never a silent fallback that disables strictness.
    let (code, _, stderr) = run(&["check", "--fail-on", "warn", "/tmp/whatever"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("invalid --fail-on"));
}

#[test]
fn invalid_min_score_is_rejected() {
    let (code, _, stderr) = run(&["check", "--min-score", "150", "/tmp/whatever"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("invalid --min-score"));
}

#[test]
fn an_unknown_flag_is_rejected_not_silently_ignored() {
    // A mistyped `--min-scor` must fail loudly — silently ignoring it would drop the score gate and
    // let low-scoring data pass CI.
    let (code, _, stderr) = run(&["check", "--min-scor", "90", "/tmp/whatever"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("unknown option `--min-scor`"),
        "unexpected: {stderr}"
    );
}

#[test]
fn a_value_flag_will_not_swallow_the_next_flag() {
    // `--key --format` must not consume `--format` as the key value; the missing value is an error.
    let (code, _, stderr) = run(&["certify", "--key", "--format", "mcap", "/tmp/whatever"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("--key requires a value"),
        "unexpected: {stderr}"
    );
}

/// Write `content` to a uniquely-named temp file and return its path.
fn temp_report(tag: &str, content: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "veridex-cli-test-{tag}-{}.json",
        std::process::id()
    ));
    std::fs::write(&p, content).expect("write temp report");
    p
}

#[test]
fn diff_regression_fails_only_with_the_flag() {
    // new has a finding old didn't and a lower score → a regression.
    let old = temp_report(
        "old",
        r#"{"verdict":{"findings":[]},"trust_score":{"score":90}}"#,
    );
    let new = temp_report(
        "new",
        r#"{"verdict":{"findings":[{"code":"TEMPORAL.CLOCK_SKEW","severity":"error","message":"x"}]},"trust_score":{"score":70}}"#,
    );
    let (o, n) = (old.to_str().unwrap(), new.to_str().unwrap());

    // Without the flag, diff is purely informational → exit 0.
    let (code, _, _) = run(&["diff", o, n]);
    assert_eq!(code, 0);

    // With --fail-on-regression, the regression fails the run → exit 20.
    let (code, _, stderr) = run(&["diff", "--fail-on-regression", o, n]);
    assert_eq!(code, 20);
    assert!(stderr.contains("regression"));

    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(&new);
}

#[test]
fn diff_requires_two_report_files() {
    let (code, _, stderr) = run(&["diff", "only-one.json"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("two report files"));
}

/// The committed MCAP fixture standing in for a real dataset file on disk.
fn fixture_dataset() -> String {
    format!("{}/tests/fixtures/demo.mcap", env!("CARGO_MANIFEST_DIR"))
}

/// A unique, per-test temp directory (created), so parallel test runs don't collide.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("veridex-cli-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

#[test]
fn check_on_a_real_dataset_reports_and_exits_on_findings() {
    let dataset = fixture_dataset();

    // Terminal report: the demo carries a clock-skew error, so the run fails with exit 20.
    let (code, stdout, _) = run(&["check", &dataset]);
    assert_eq!(code, 20, "a dataset with an error finding must exit 20");
    assert!(stdout.contains("Veridex report"), "unexpected: {stdout}");
    assert!(stdout.contains("Trust:"), "report must carry a trust score");
    assert!(
        stdout.contains("TEMPORAL.CLOCK_SKEW"),
        "unexpected: {stdout}"
    );

    // JSON report: same run, machine-readable, with the versioned schema and a bound content hash.
    let (code, stdout, _) = run(&["check", &dataset, "--json"]);
    assert_eq!(code, 20);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON report");
    assert_eq!(report["schema"], "veridex.report/1");
    assert!(report["verdict"]["cdm_content_hash"].is_string());
    assert_eq!(report["trust_score"]["rubric_version"], "v1");
}

#[test]
fn full_keygen_certify_verify_flow() {
    // The primary trust flow, exercised through the real binary end-to-end: mint a key, certify a
    // dataset, then verify the certificate offline against that same dataset and the public key.
    let dataset = fixture_dataset();
    let dir = temp_dir("certflow");
    let key = dir.join("issuer");
    let key_s = key.to_str().unwrap();
    let cert = dir.join("cert.json");
    let cert_s = cert.to_str().unwrap();

    // keygen writes the secret key and its `.pub` companion.
    let (code, stdout, _) = run(&["keygen", key_s]);
    assert_eq!(code, 0, "keygen must succeed");
    assert!(stdout.contains("issuer key id"));
    assert!(key.exists() && dir.join("issuer.pub").exists());

    // On Unix the secret key must be owner-only (0600): another local user on a shared host or CI
    // runner must not be able to read the private signing key and forge certificates.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "secret key must be 0600, got {mode:o}");
    }

    // certify signs the verdict into a content-bound certificate.
    let (code, stdout, _) = run(&["certify", &dataset, "--key", key_s, "--out", cert_s]);
    assert_eq!(code, 0, "certify must succeed");
    assert!(stdout.contains("certified"));
    assert!(cert.exists());

    // verify accepts the certificate against the same dataset and the trusted public key.
    let pubkey = dir.join("issuer.pub");
    let (code, stdout, _) = run(&[
        "verify",
        &dataset,
        "--certificate",
        cert_s,
        "--key",
        pubkey.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "verify must accept a valid certificate");
    assert!(stdout.contains("verified"), "unexpected: {stdout}");

    // A different issuer key must be rejected — the certificate is not from that issuer.
    let other = dir.join("other");
    run(&["keygen", other.to_str().unwrap()]);
    let (code, _, _) = run(&[
        "verify",
        &dataset,
        "--certificate",
        cert_s,
        "--key",
        other.with_extension("pub").to_str().unwrap(),
    ]);
    assert_ne!(code, 0, "an untrusted issuer key must fail verification");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_readiness_certificate_is_issued_and_read_back_by_verify() {
    // A readiness certificate must be *usable* offline: `verify` has to report what it attests —
    // the trust score and each readiness criterion — not just that the signature checks out.
    let dataset = fixture_dataset();
    let dir = temp_dir("readiness");
    let key = dir.join("issuer");
    let key_s = key.to_str().unwrap();
    let cert = dir.join("cert.json");
    let cert_s = cert.to_str().unwrap();
    run(&["keygen", key_s]);

    let (code, stdout, _) = run(&[
        "certify",
        &dataset,
        "--key",
        key_s,
        "--out",
        cert_s,
        "--profile",
        "world-model-ready",
    ]);
    assert_eq!(code, 0, "certify --profile must succeed");
    assert!(stdout.contains("world-model-ready profile"), "{stdout}");

    // Terminal verification reports the profile verdict and every criterion.
    let pubkey = dir.join("issuer.pub");
    let pub_s = pubkey.to_str().unwrap().to_string();
    let (code, stdout, _) = run(&["verify", &dataset, "--certificate", cert_s, "--key", &pub_s]);
    assert_eq!(code, 0, "verify must accept the certificate");
    assert!(stdout.contains("trust:"), "{stdout}");
    assert!(stdout.contains("bound to:"), "{stdout}");
    // The demo is a manipulation dataset, so the autonomy profile is honestly N/A, never a pass.
    assert!(stdout.contains("N/A (profile does not apply)"), "{stdout}");

    // `--json` carries the same signed facts for a machine.
    let (code, stdout, _) = run(&[
        "verify",
        &dataset,
        "--certificate",
        cert_s,
        "--key",
        &pub_s,
        "--json",
    ]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(doc["verified"], true);
    assert_eq!(doc["issuer_verified"], true);
    assert_eq!(doc["readiness"]["profile"], "world-model-ready");
    assert_eq!(doc["readiness"]["ready"], false);
    assert!(doc["trust_score"]["score"].is_number());

    // An unknown profile is a tool error, never a silently unprofiled certificate.
    let (code, _, stderr) = run(&[
        "certify",
        &dataset,
        "--key",
        key_s,
        "--out",
        cert_s,
        "--profile",
        "nope",
    ]);
    assert_eq!(code, 2, "unknown profile must be a tool error");
    assert!(stderr.contains("unknown profile"), "{stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verify_demands_a_trust_decision_about_the_issuer() {
    // A valid signature only proves a certificate is self-consistent. Anyone can mint one about a
    // dataset they hold, and it will verify — so `verify` must not imply endorsement by default.
    let dataset = fixture_dataset();
    let dir = temp_dir("issuer");
    let key = dir.join("issuer");
    let cert = dir.join("cert.json");
    let cert_s = cert.to_str().unwrap();
    run(&["keygen", key.to_str().unwrap()]);
    run(&[
        "certify",
        &dataset,
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert_s,
    ]);

    // No trust decision at all is a usage error, not a silent pass.
    let (code, _, stderr) = run(&["verify", &dataset, "--certificate", cert_s]);
    assert_eq!(code, 2, "verify without a trust decision must be an error");
    assert!(stderr.contains("trusted issuer"), "{stderr}");

    // Opting out explicitly works, but says plainly that the issuer was not checked.
    let (code, stdout, _) = run(&[
        "verify",
        &dataset,
        "--certificate",
        cert_s,
        "--allow-any-issuer",
    ]);
    assert_eq!(code, 0);
    assert!(stdout.contains("issuer NOT verified"), "{stdout}");

    let (code, stdout, _) = run(&[
        "verify",
        &dataset,
        "--certificate",
        cert_s,
        "--allow-any-issuer",
        "--json",
    ]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(doc["verified"], true);
    assert_eq!(
        doc["issuer_verified"], false,
        "a machine consumer must see that the issuer was not checked"
    );
}

#[test]
fn a_certificate_carrying_unknown_fields_is_rejected() {
    // The signature covers the struct, so a field Veridex does not know about would ride along
    // inside a document it just called authentic.
    let dataset = fixture_dataset();
    let dir = temp_dir("unknownfields");
    let key = dir.join("issuer");
    let cert = dir.join("cert.json");
    run(&["keygen", key.to_str().unwrap()]);
    run(&[
        "certify",
        &dataset,
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert.to_str().unwrap(),
    ]);

    let text = std::fs::read_to_string(&cert).expect("read cert");
    let injected = text.replacen(
        "\"schema\":",
        "\"trust_score_override\": {\"score\": 100}, \"schema\":",
        1,
    );
    let tampered = dir.join("tampered.json");
    std::fs::write(&tampered, injected).expect("write");

    let (code, _, stderr) = run(&[
        "verify",
        &dataset,
        "--certificate",
        tampered.to_str().unwrap(),
        "--key",
        dir.join("issuer.pub").to_str().unwrap(),
    ]);
    assert_ne!(code, 0, "an unknown field must not verify");
    assert!(
        stderr.contains("not a valid certificate") || stderr.contains("unknown field"),
        "{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn diff_rejects_an_unknown_flag_instead_of_ignoring_it() {
    // `diff` used to scan argv for the flags it knew and drop the rest, so one dropped letter turned
    // a CI gate off and still exited 0.
    let old = temp_report(
        "old_typo",
        r#"{"verdict":{"findings":[]},"trust_score":{"score":90}}"#,
    );
    let new = temp_report(
        "new_typo",
        r#"{"verdict":{"findings":[{"code":"X","severity":"error","message":"x"}]},"trust_score":{"score":70}}"#,
    );
    let (o, n) = (old.to_str().unwrap(), new.to_str().unwrap());

    let (code, _, stderr) = run(&["diff", "--fail-on-regresion", o, n]);
    assert_eq!(code, 2, "a typo'd gate flag must be a tool error");
    assert!(stderr.contains("unknown option"), "unexpected: {stderr}");

    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(&new);
}

#[test]
fn diff_refuses_a_file_that_is_not_a_report() {
    // An empty or wrong-shaped artifact is not "a report with no findings": read that way, a
    // truncated CI artifact looked like every finding had been resolved and passed the gate.
    let old = temp_report(
        "old_shape",
        r#"{"verdict":{"findings":[{"code":"X","severity":"error","message":"x"}]},"trust_score":{"score":70}}"#,
    );
    let empty = temp_report("new_shape", "{}");
    let (o, e) = (old.to_str().unwrap(), empty.to_str().unwrap());

    let (code, _, stderr) = run(&["diff", "--fail-on-regression", o, e]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("not a Veridex report"),
        "unexpected: {stderr}"
    );

    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_file(&empty);
}

#[test]
fn check_honors_the_profile_it_accepts() {
    // `check --profile` was parsed and thrown away, so the run silently used the looser defaults
    // while the user believed the profile's thresholds applied.
    let dataset = fixture_dataset();
    let (code, stdout, _) = run(&[
        "check",
        "--profile",
        "world-model-ready",
        "--json",
        &dataset,
    ]);
    assert!(
        code == 0 || code == 10 || code == 20,
        "unexpected exit {code}"
    );
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(
        report["verdict"]["effective_config"]["tolerances"]["clock_skew_ns"], 20_000_000,
        "the profile's tightened tolerance must reach the run"
    );

    // And an unknown profile is refused rather than silently ignored.
    let (code, _, stderr) = run(&["check", "--profile", "no-such-profile", &dataset]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown profile"), "unexpected: {stderr}");
}

#[test]
fn a_command_refuses_a_gate_flag_it_cannot_honor() {
    // The shared parser accepts one flag set for every command, so `inspect --min-score 90` used to
    // look like a gate and silently be none. Naming it is better than ignoring it.
    let dataset = fixture_dataset();
    let (code, _, stderr) = run(&["inspect", "--min-score", "90", &dataset]);
    assert_eq!(code, 2);
    assert!(stderr.contains("does not support --min-score"), "{stderr}");

    // The commands that do gate still accept it.
    let (code, _, _) = run(&["check", "--min-score", "0", &dataset]);
    assert!(
        code == 0 || code == 10 || code == 20,
        "unexpected exit {code}"
    );
}
