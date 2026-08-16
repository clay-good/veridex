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

/// Write a small LeRobot v3 dataset of `episodes` × 3 frames into `dir`, using the example
/// generator the quickstart documents. Returns the dataset path.
fn make_lerobot(tag: &str) -> std::path::PathBuf {
    let dir = temp_dir(tag).join("lerobot");
    let _ = std::fs::remove_dir_all(&dir);
    let status = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "veridex-core",
            "--example",
            "make_demo_lerobot",
            "--",
        ])
        .arg(&dir)
        .arg("clean")
        .status()
        .expect("generate the demo LeRobot dataset");
    assert!(status.success(), "demo generator failed");
    dir
}

#[test]
fn sampling_flags_are_validated_before_anything_is_read() {
    // A path that does not exist would be a different error; these must fail on the flags alone, so
    // a mistyped sampling request can never be resolved into some other request that runs anyway.
    for (args, expect) in [
        (
            vec![
                "check",
                "x",
                "--sample-episodes",
                "2",
                "--sample-fraction",
                "0.5",
            ],
            "cannot both be given",
        ),
        (
            vec!["check", "x", "--sample-seed", "3"],
            "requires --sample-fraction",
        ),
        (
            vec!["check", "x", "--sample-episodes", "2", "--sample-seed", "3"],
            "--sample-seed applies to --sample-fraction",
        ),
        (
            vec!["check", "x", "--sample-fraction", "banana"],
            "invalid --sample-fraction",
        ),
        (
            vec!["check", "x", "--sample-episodes", "-1"],
            "requires a value",
        ),
    ] {
        let (code, _, stderr) = run(&args);
        assert_eq!(code, 2, "{args:?} must be a tool error");
        assert!(
            stderr.contains(expect),
            "{args:?}: unexpected stderr {stderr}"
        );
    }
}

#[test]
fn commands_that_cannot_honor_a_sample_refuse_the_flags() {
    let dataset = fixture_dataset();
    for cmd in ["verify", "provenance"] {
        let (code, _, stderr) = run(&[cmd, &dataset, "--sample-episodes", "1"]);
        assert_eq!(code, 2, "{cmd} must refuse sampling flags");
        assert!(
            stderr.contains("does not support --sample-episodes"),
            "{cmd}: unexpected stderr {stderr}"
        );
    }
}

#[test]
fn a_single_episode_recording_refuses_a_sample() {
    let (code, _, stderr) = run(&["check", &fixture_dataset(), "--sample-episodes", "1"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("cannot sample"),
        "unexpected stderr {stderr}"
    );
}

#[test]
fn a_sampled_check_reports_its_coverage_and_cannot_be_certified() {
    let dir = make_lerobot("sample-coverage");
    let path = dir.to_str().unwrap();

    // The sampled run says so, in the terminal report and in the JSON.
    let (code, stdout, _) = run(&["check", path, "--sample-episodes", "1"]);
    assert!(
        code == 0 || code == 10 || code == 20,
        "unexpected exit {code}"
    );
    assert!(
        stdout.contains("Coverage: SAMPLE") && stdout.contains("1 episode(s) ingested"),
        "the terminal report must state the run was partial: {stdout}"
    );

    let (_, stdout, _) = run(&["check", path, "--sample-episodes", "1", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON report");
    assert_eq!(report["verdict"]["coverage"]["kind"], "sample");
    assert_eq!(report["verdict"]["coverage"]["episodes_ingested"], 1);

    // A full check carries no coverage banner.
    let (_, stdout, _) = run(&["check", path]);
    assert!(!stdout.contains("Coverage:"), "unexpected banner: {stdout}");

    // And a sample cannot be laundered into a portable certificate.
    let keydir = temp_dir("sample-cert");
    let key = keydir.join("issuer");
    let (code, _, _) = run(&["keygen", key.to_str().unwrap(), "--force"]);
    assert_eq!(code, 0);
    let (code, _, stderr) = run(&[
        "certify",
        path,
        "--key",
        key.to_str().unwrap(),
        "--out",
        keydir.join("c.json").to_str().unwrap(),
        "--sample-episodes",
        "1",
    ]);
    assert_eq!(code, 2, "certifying a sample must be a tool error");
    assert!(
        stderr.contains("certify does not support sampling")
            && stderr.contains("speaks for the whole dataset"),
        "the refusal must say why: {stderr}"
    );
    assert!(
        !keydir.join("c.json").exists(),
        "no certificate may be written for a sampled run"
    );
}

#[test]
fn every_command_refuses_the_flags_it_does_not_act_on() {
    // The shared parser accepts one flag set for every command, so without an allow-list a command
    // silently tolerates flags it has no use for: `check --out r.json` looks like it writes a file,
    // `diff --min-score 90` looks like a gate, and neither is. Each pair below is a flag the command
    // genuinely ignores, and each must be a loud tool error.
    let dataset = fixture_dataset();
    let cases: &[(&str, &[&str])] = &[
        ("check", &["--out", "/dev/null"]),
        ("check", &["--key", "k"]),
        ("check", &["--emit", "croissant"]),
        ("check", &["--timestamp", "1"]),
        ("check", &["--certificate", "c.json"]),
        ("check", &["--force"]),
        ("check", &["--allow-any-issuer"]),
        ("check", &["--fail-on-regression"]),
        ("inspect", &["--sarif"]),
        ("inspect", &["--html"]),
        ("inspect", &["--profile", "world-model-ready"]),
        ("inspect", &["--config", "veridex.toml"]),
        ("checks", &["--min-score", "90"]),
        ("checks", &["--format", "mcap"]),
        ("provenance", &["--json"]),
        ("provenance", &["--profile", "world-model-ready"]),
        ("verify", &["--profile", "world-model-ready"]),
        ("verify", &["--out", "/dev/null"]),
        ("diff", &["--min-score", "90"]),
        ("diff", &["--sarif"]),
        ("diff", &["--max-frames", "10"]),
        ("keygen", &["--json"]),
        ("keygen", &["--profile", "world-model-ready"]),
    ];
    for (cmd, flag) in cases {
        let mut args: Vec<&str> = vec![cmd, &dataset];
        args.extend_from_slice(flag);
        let (code, _, stderr) = run(&args);
        assert_eq!(code, 2, "{cmd} {flag:?} must be a tool error, got {code}");
        assert!(
            stderr.contains("does not support"),
            "{cmd} {flag:?}: unexpected stderr {stderr}"
        );
    }
}

#[test]
fn every_command_still_accepts_the_flags_it_does_act_on() {
    // The guard against over-tightening the allow-lists: rejecting a flag a command genuinely honors
    // would be a worse regression than the silent-ignore it replaced.
    let dataset = fixture_dataset();
    let cases: &[(&str, &[&str])] = &[
        ("check", &["--json"]),
        ("check", &["--sarif"]),
        ("check", &["--html"]),
        ("check", &["--fail-on", "warning"]),
        ("check", &["--min-score", "1"]),
        ("check", &["--profile", "world-model-ready"]),
        ("check", &["--max-frames", "1000000"]),
        ("inspect", &["--json"]),
        ("inspect", &["--max-frames", "1000000"]),
        ("checks", &["--json"]),
        ("provenance", &["--emit", "prov"]),
    ];
    for (cmd, flag) in cases {
        let mut args: Vec<&str> = vec![cmd, &dataset];
        args.extend_from_slice(flag);
        let (_, _, stderr) = run(&args);
        assert!(
            !stderr.contains("does not support"),
            "{cmd} {flag:?} must be honored: {stderr}"
        );
    }
}
