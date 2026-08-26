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

    // certify signs the verdict into a content-bound certificate. Its exit code is the *verdict's*
    // — this fixture fails, so `20` — because `certify && publish` would otherwise be a green
    // pipeline over a dataset that failed validation. The certificate is still written: a signed
    // record of a failing dataset is as useful as one of a passing dataset, and withholding it
    // would hide the result rather than report it.
    let (code, stdout, _) = run(&["certify", &dataset, "--key", key_s, "--out", cert_s]);
    assert_eq!(code, 20, "certify reports the verdict it signed");
    assert!(stdout.contains("certified"));
    assert!(
        stdout.contains("fail"),
        "the issuing side must say what it signed, not only the grade: {stdout}"
    );
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
    // Same contract as above: the exit code is the verdict's, and this fixture fails.
    assert_eq!(code, 20, "certify --profile reports the verdict it signed");
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
            // A negative number is a value the user meant, not the next flag swallowed, so it is
            // rejected by the parser that knows what the flag accepts rather than reported as
            // missing.
            vec!["check", "x", "--sample-episodes", "-1"],
            "invalid --sample-episodes `-1`",
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

#[test]
fn a_metadata_only_check_reports_its_coverage_and_cannot_be_certified() {
    let dir = make_lerobot("metadata-only-coverage");
    let path = dir.to_str().unwrap();

    // A manifest-only run says so, in the terminal report and in the JSON.
    let (code, stdout, stderr) = run(&["check", path, "--metadata-only"]);
    assert!(
        code == 0 || code == 10 || code == 20,
        "unexpected exit {code}: {stderr}"
    );
    assert!(
        stdout.contains("Coverage: METADATA-ONLY") && stdout.contains("no stream payload"),
        "the terminal report must state what was not read: {stdout}"
    );

    let (_, stdout, _) = run(&["check", path, "--metadata-only", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON report");
    assert_eq!(report["verdict"]["coverage"]["kind"], "metadata_only");

    // `inspect` honors it too — the same partial CDM, rendered rather than checked.
    let (code, stdout, _) = run(&["inspect", path, "--metadata-only", "--json"]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON CDM");
    // The caveat travels with the CDM. Without it every stream reads `0 frame(s)`, which is
    // indistinguishable from a dataset whose episodes are genuinely empty — the exact defect
    // `STRUCTURAL.EMPTY_STREAM` exists to report. The terminal render says so; the machine one
    // used to dump the bare CDM and drop it.
    assert_eq!(
        doc["coverage"]["kind"], "metadata_only",
        "a machine reader must be told the zeros are the request, not the data: {stdout}"
    );
    let cdm = &doc["dataset"];
    assert!(cdm["episodes"].as_array().is_some_and(|e| !e.is_empty()));
    assert_eq!(
        cdm["episodes"][0]["streams"][0]["frames"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // And it cannot be laundered into a portable certificate.
    let keydir = temp_dir("metadata-only-cert");
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
        "--metadata-only",
    ]);
    assert_eq!(
        code, 2,
        "certifying a metadata-only run must be a tool error"
    );
    assert!(
        stderr.contains("certify does not support --metadata-only"),
        "the refusal must say why: {stderr}"
    );
    assert!(
        !keydir.join("c.json").exists(),
        "no certificate may be written for a metadata-only run"
    );
}

#[test]
fn a_refusal_names_the_thing_that_was_wrong() {
    // Each of these used to be reported as a problem with something the user got right.
    let dataset = fixture_dataset();

    // The file is a perfectly good MCAP; it is the flag value that is not a format.
    let (code, _, stderr) = run(&["check", &dataset, "--format", "nope"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("unknown format `nope`"),
        "must name the value, not blame the source: {stderr}"
    );

    // A remote source is refused because remote ingestion is not built — not because the URL is a
    // mistyped path, which is what "no such file or directory: https://…" reads as.
    let (code, _, stderr) = run(&["check", "https://huggingface.co/datasets/x"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("remote ingestion is not implemented"),
        "unexpected stderr {stderr}"
    );

    // And an ordinary mistyped path still says so.
    let (code, _, stderr) = run(&["check", "/tmp/veridex-no-such-dataset-9e3f"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("no such file or directory"),
        "unexpected stderr {stderr}"
    );
}

#[test]
fn check_reports_the_profile_it_judged_against() {
    // `--help` calls a profile what the run is "judged against", and `check` only borrowed its
    // tolerances — printing no criterion verdicts at all, so the one thing the flag names was the
    // one thing it did not report.
    let dir = temp_dir("check-profile");
    let path = dir.join("av.mcap");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "veridex-core",
            "--example",
            "make_demo_mcap",
            "--",
        ])
        .arg(&path)
        .arg("av")
        .status()
        .expect("run the demo generator");
    assert!(status.success());
    let path = path.to_str().unwrap();

    let (_, stdout, _) = run(&["check", path, "--profile", "world-model-ready"]);
    assert!(
        stdout.contains("world-model-ready profile:"),
        "the criterion verdicts must be reported: {stdout}"
    );
    assert!(
        stdout.contains("autonomy.sensor-frame-resolution"),
        "every criterion, not a subset: {stdout}"
    );
    // Without the flag, nothing about readiness is printed.
    let (_, stdout, _) = run(&["check", path]);
    assert!(!stdout.contains("world-model-ready profile:"), "{stdout}");
}

#[test]
fn a_score_gate_cannot_be_satisfied_by_reading_nothing() {
    // Under `--metadata-only` the data axis is computed from checks that overwhelmingly had nothing
    // to measure, so it lands near 100 whatever the data holds — making `--min-score 90
    // --metadata-only` a one-flag way to satisfy a CI gate on a dataset whose values are garbage.
    let dir = make_lerobot("min-score-metadata-only");
    let path = dir.to_str().unwrap();

    let (code, _, stderr) = run(&["check", path, "--metadata-only", "--min-score", "90"]);
    assert_eq!(code, 2, "the gate must be refused, not silently satisfied");
    assert!(
        stderr.contains("--min-score cannot gate a --metadata-only run"),
        "unexpected stderr {stderr}"
    );

    // A sample was once waved through here as "real data, just less of it". It is less of it
    // exactly where it matters — the skipped episodes are where the defect the gate is meant to
    // catch would be. On this repo's own demo dataset, whose generator puts the flaw in episode 1,
    // a full run fails `--min-score 75` at 69 and `--sample-episodes 1` passes it at 79.
    let (code, _, stderr) = run(&[
        "check",
        path,
        "--sample-episodes",
        "1",
        "--min-score",
        "100",
    ]);
    assert_eq!(code, 2, "a sampled run cannot carry a score gate either");
    assert!(
        stderr.contains("--min-score cannot gate a sampled run"),
        "unexpected stderr {stderr}"
    );

    // Narrowing the catalog is the third way in, and the widest: `categories = []` runs no checks
    // at all, and the data axis starts at 100 and only deducts, so nothing measured scores a
    // perfect 100.
    let cfg = dir.join("none.toml");
    std::fs::write(&cfg, "categories = []\n").unwrap();
    let (code, _, stderr) = run(&[
        "check",
        path,
        "--config",
        cfg.to_str().unwrap(),
        "--min-score",
        "90",
    ]);
    assert_eq!(code, 2, "a run that measured nothing cannot satisfy a gate");
    assert!(
        stderr.contains("--min-score cannot gate a narrowed run"),
        "unexpected stderr {stderr}"
    );
}

/// The flag allow-list is only a guard if it knows every flag.
///
/// `given_flags()` is the single source of truth for `reject_flags_except`, and its doc comment
/// claimed "a test asserts this covers the parser's whole flag set". No such test existed: the array
/// is a fixed `[(&str, bool); N]`, which forces nothing, so a flag added to `parse_args` without a
/// matching entry would be silently accepted by *every* command — precisely the failure the
/// allow-list exists to prevent. Both lists live in one file, so comparing them is a textual fact.
#[test]
fn every_parser_flag_appears_in_the_allow_list() {
    let source = include_str!("../src/main.rs");

    /// The `"--flag"` literals inside a brace-delimited block that starts at `from`.
    fn flags_in_block(source: &str, from: usize) -> Vec<String> {
        let rest = &source[from..];
        let open = rest.find('{').expect("block opens");
        let mut depth = 0usize;
        let mut end = rest.len();
        for (i, c) in rest[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let block = &rest[open..end];
        let mut out: Vec<String> = block
            .match_indices("\"--")
            .filter_map(|(i, _)| {
                let tail = &block[i + 1..];
                tail.find('"').map(|q| tail[..q].to_string())
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    let parser = flags_in_block(
        source,
        source
            .find("match arg.as_str() {")
            .expect("parser dispatch"),
    );
    let allow_list = flags_in_block(
        source,
        source.find("fn given_flags(&self)").expect("given_flags"),
    );

    let missing: Vec<&String> = parser.iter().filter(|f| !allow_list.contains(f)).collect();
    assert!(
        missing.is_empty(),
        "these flags are parsed but absent from given_flags(), so no command rejects them: \
         {missing:?}"
    );

    // The converse: an entry naming a flag the parser does not know is dead weight that reads as
    // coverage.
    let stale: Vec<&String> = allow_list.iter().filter(|f| !parser.contains(f)).collect();
    assert!(
        stale.is_empty(),
        "these entries name flags the parser does not accept: {stale:?}"
    );
}

/// One run writes one report. The dispatch is an if/else chain, so the losing flag used to be
/// dropped without a word — a CI job doing `check --json --sarif > report.json` silently got SARIF,
/// which `veridex diff` then refused as not a Veridex report.
#[test]
fn conflicting_output_formats_are_refused() {
    for pair in [
        ["--json", "--sarif"],
        ["--json", "--html"],
        ["--sarif", "--html"],
    ] {
        let (code, stdout, stderr) = run(&["check", pair[0], pair[1], "."]);
        assert_eq!(
            code, 2,
            "{pair:?} should be a usage error: {stdout}{stderr}"
        );
        assert!(stderr.contains("cannot be combined"), "{pair:?}: {stderr}");
    }
}

/// "Veridex only reads and reports. It never mutates your dataset" is a README promise the adoption
/// guide repeats. The default certificate name is relative, so it landed in the working directory —
/// which *is* the dataset when the user ran `cd my-dataset && veridex certify .`, the most natural
/// way to do it. Nothing is corrupted (the CDM hash is unaffected), but a promise that holds except
/// when it is inconvenient is not one anyone can build a policy on.
#[test]
fn certify_refuses_to_default_its_output_into_the_dataset() {
    let dataset = make_lerobot("certify-into-dataset");
    let keydir = temp_dir("certify-into-dataset-key");
    let key = keydir.join("issuer");
    let (code, _, stderr) = run(&["keygen", key.to_str().unwrap()]);
    assert_eq!(code, 0, "{stderr}");

    let before: Vec<_> = std::fs::read_dir(&dataset)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    let out = Command::new(env!("CARGO_BIN_EXE_veridex"))
        .current_dir(&dataset)
        .args(["certify", ".", "--key", key.to_str().unwrap()])
        .output()
        .expect("spawn veridex");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("never writes into a dataset") && stderr.contains("--out"),
        "the refusal must name the promise and the fix: {stderr}"
    );

    let after: Vec<_> = std::fs::read_dir(&dataset)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before.len(), after.len(), "the dataset gained a file");

    // With `--out` elsewhere it certifies as normal — the refusal is about the destination, not the
    // working directory.
    let cert = keydir.join("cert.json");
    let out = Command::new(env!("CARGO_BIN_EXE_veridex"))
        .current_dir(&dataset)
        .args([
            "certify",
            ".",
            "--key",
            key.to_str().unwrap(),
            "--out",
            cert.to_str().unwrap(),
        ])
        .output()
        .expect("spawn veridex");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(cert.is_file());
}

/// The dataset id is part of the CDM content hash, so every way of naming one directory has to hash
/// alike — otherwise a certificate issued from outside the dataset is rejected from inside it, on
/// identical bytes.
#[test]
fn a_certificate_verifies_from_inside_the_dataset_it_was_issued_for() {
    let dataset = make_lerobot("verify-from-inside");
    let keydir = temp_dir("verify-from-inside-key");
    let key = keydir.join("issuer");
    assert_eq!(run(&["keygen", key.to_str().unwrap()]).0, 0);
    let cert = keydir.join("cert.json");

    let (code, _, stderr) = run(&[
        "certify",
        dataset.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{stderr}");

    let pubkey = format!("{}.pub", key.to_str().unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_veridex"))
        .current_dir(&dataset)
        .args([
            "verify",
            ".",
            "--certificate",
            cert.to_str().unwrap(),
            "--key",
            &pubkey,
        ])
        .output()
        .expect("spawn veridex");
    assert_eq!(
        out.status.code(),
        Some(0),
        "a certificate must verify from inside the dataset it was issued for: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `--profile` is documented as what a run is "judged against", but its criterion verdicts reached
/// only the terminal report — so a CI job, which is exactly who reads `--json`, `--sarif` and
/// `--html`, got the profile's tolerances applied and no verdict about them.
#[test]
fn a_profile_verdict_reaches_the_machine_readable_outputs() {
    let dir = temp_dir("profile-machine-output");
    let path = dir.join("av.mcap");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "veridex-core",
            "--example",
            "make_demo_mcap",
            "--",
        ])
        .arg(&path)
        .arg("av")
        .status()
        .expect("run the demo generator");
    assert!(status.success());
    let path = path.to_str().unwrap();

    let (_, stdout, _) = run(&["check", path, "--json", "--profile", "world-model-ready"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let readiness = &doc["readiness"];
    assert!(
        readiness.is_object(),
        "a profile run must carry its readiness verdict in --json: {stdout}"
    );
    assert_eq!(readiness["profile"], "world-model-ready");
    assert!(readiness["criteria"].as_array().unwrap().len() >= 5);

    let (_, html, _) = run(&["check", path, "--html", "--profile", "world-model-ready"]);
    assert!(
        html.contains("Profile readiness"),
        "the html report omits the readiness block"
    );

    // Without a profile the field is absent, so an ordinary report's bytes are unchanged.
    let (_, plain, _) = run(&["check", path, "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&plain).unwrap();
    assert!(doc.get("readiness").is_none());
}

/// The command names `veridex --help` advertises, read from the `COMMANDS` table in the CLI source.
fn commands_from_source() -> Vec<String> {
    let source = include_str!("../src/main.rs");
    let start = source.find("const COMMANDS").expect("COMMANDS table");
    let block = &source[start..source[start..].find("];").expect("table ends") + start];
    let mut out: Vec<String> = block
        .match_indices('"')
        .filter_map(|(i, _)| {
            let tail = &block[i + 1..];
            tail.find('"').map(|q| tail[..q].to_string())
        })
        // Command names only: the second element of each tuple is a description, which has spaces.
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase()))
        .collect();
    out.sort();
    out.dedup();
    assert!(out.len() >= 8, "the COMMANDS table did not parse: {out:?}");
    out
}

/// `--help` is listed under OPTIONS in the usage block, so `veridex certify --help` reads as
/// supported. Every command rejected it with "unknown option `--help`" and exit 2, because each
/// command's flag allow-list names only the flags it does something *with* — and `--help` is not
/// one of those anywhere. Asking a tool how to use it should not be a tool error.
///
/// The command list is read out of `COMMANDS` in the source rather than retyped here: a hand-kept
/// copy is extended by hand, and every miss is silent — a new command would simply not be covered.
#[test]
fn every_command_accepts_help() {
    for cmd in commands_from_source() {
        let cmd = cmd.as_str();
        let (code, stdout, _) = run(&[cmd, "--help"]);
        assert_eq!(code, 0, "`veridex {cmd} --help` must not be an error");
        assert!(
            stdout.contains("USAGE:"),
            "`veridex {cmd} --help` must print the usage: {stdout}"
        );
        let (code, _, _) = run(&[cmd, "-h"]);
        assert_eq!(code, 0, "`veridex {cmd} -h` must not be an error");
    }

    // ...and a flag that genuinely is not supported still fails, loudly.
    let (code, _, stderr) = run(&["check", "--bogus"]);
    assert_eq!(code, 2, "an unknown flag is still a tool error");
    assert!(stderr.contains("--bogus"), "{stderr}");
}

// ---------------------------------------------------------------------------
// `veridex watch` — validate a dataset while it is being recorded.
// ---------------------------------------------------------------------------

/// Write the five-sensor autonomy-rig demo to `path` with the workspace generator, as the rig tests
/// above do. It stands in for the *second* state of a dataset that changes mid-watch: a different
/// finding set from the manipulation demo's, so the diff a watch prints is a real one.
fn write_av_mcap(path: &std::path::Path) {
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "veridex-core",
            "--example",
            "make_demo_mcap",
            "--",
        ])
        .arg(path)
        .arg("av")
        .status()
        .expect("run the demo generator");
    assert!(status.success(), "the demo generator must succeed");
}

#[test]
fn watch_reports_once_and_stays_quiet_while_nothing_changes() {
    // The first tick has nothing to compare against, so it prints the whole report. Later ticks over
    // an unchanged dataset must print *nothing*: a watch that re-prints its report every two seconds
    // is unreadable, and a watch that re-ingests a growing log every tick is a load generator.
    let (code, stdout, _) = run(&[
        "watch",
        &fixture_dataset(),
        "--iterations",
        "3",
        "--interval",
        "0",
    ]);
    assert_eq!(
        code, 20,
        "the exit code is the last validation's, as `check`"
    );
    assert_eq!(
        stdout.matches("Veridex report").count(),
        1,
        "exactly one full report, on the first tick: {stdout}"
    );
    assert!(
        !stdout.contains("Veridex diff"),
        "an unchanged dataset must produce no diff: {stdout}"
    );
    assert!(stdout.contains("TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn watch_surfaces_the_findings_a_change_introduced() {
    // The spec scenario: a dataset is being recorded, and new findings appear as it grows. Here the
    // "recording" swaps in the autonomy rig, whose finding set differs from the manipulation demo's.
    let dir = temp_dir("watchchange");
    let dataset = dir.join("recording.mcap");
    std::fs::copy(fixture_dataset(), &dataset).expect("seed the recording");
    // Built before the watch starts: the generator shells out to cargo, which is far slower than
    // the poll interval and would otherwise decide when the change lands.
    let next = dir.join("next.mcap");
    write_av_mcap(&next);
    let target = dataset.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        std::fs::copy(&next, &target).expect("the recorder writes");
    });

    let (code, stdout, _) = run(&[
        "watch",
        dataset.to_str().unwrap(),
        "--iterations",
        "8",
        "--interval",
        "0.2",
    ]);
    writer.join().expect("writer thread");

    assert_eq!(code, 20, "the rig fixture fails too, so the exit stays 20");
    assert!(
        stdout.contains("Veridex diff"),
        "a changed dataset must be re-validated and diffed: {stdout}"
    );
    assert!(
        stdout.contains("Introduced:") && stdout.contains("AUTONOMY.RIG_SYNC"),
        "the findings the change introduced must be named: {stdout}"
    );
    assert!(
        stdout.contains("Resolved:") && stdout.contains("TEMPORAL.CLOCK_SKEW"),
        "and the ones it resolved: {stdout}"
    );
}

#[test]
fn watch_survives_a_dataset_that_is_mid_write() {
    // A half-written file is an ordinary moment in a recording, not a reason to exit: aborting would
    // end the watch seconds after a real recording started. The verdict that stands is the last one
    // that completed, so the exit code still speaks for a real validation.
    let dir = temp_dir("watchpartial");
    let dataset = dir.join("recording.mcap");
    std::fs::copy(fixture_dataset(), &dataset).expect("seed the recording");
    let target = dataset.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        // Truncated mid-record: exactly what a reader sees between a recorder's writes.
        let bytes = std::fs::read(&target).expect("read");
        std::fs::write(&target, &bytes[..bytes.len() / 2]).expect("truncate");
    });

    let (code, stdout, stderr) = run(&[
        "watch",
        dataset.to_str().unwrap(),
        "--iterations",
        "8",
        "--interval",
        "0.2",
    ]);
    writer.join().expect("writer thread");

    assert!(
        stderr.contains("still watching"),
        "an unreadable moment must be reported and survived: {stderr}"
    );
    assert_eq!(
        code, 20,
        "the exit code is the last *completed* validation's, not the failed read's"
    );
    assert!(stdout.contains("Veridex report"));
}

#[test]
fn watch_json_is_one_document_per_validation() {
    let (code, stdout, _) = run(&[
        "watch",
        &fixture_dataset(),
        "--iterations",
        "2",
        "--interval",
        "0",
        "--json",
    ]);
    assert_eq!(code, 20);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "one validation happened (nothing changed on tick 2), so one document: {stdout}"
    );
    let doc: serde_json::Value = serde_json::from_str(lines[0]).expect("each line is one JSON doc");
    assert_eq!(doc["schema"], "veridex.watch/1");
    assert_eq!(doc["tick"], 1);
    assert_eq!(doc["report"]["schema"], "veridex.report/1");
    assert!(
        doc["changes"].is_null(),
        "the first validation has nothing to diff against"
    );
}

#[test]
fn a_watch_that_validated_nothing_is_a_tool_error() {
    // Exiting 0 here would tell a CI job the dataset passed when nothing was ever read.
    let dir = temp_dir("watchmissing");
    let (code, _, stderr) = run(&[
        "watch",
        dir.join("no-such-dataset").to_str().unwrap(),
        "--iterations",
        "2",
        "--interval",
        "0",
    ]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("completed no validation"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn watch_rejects_flags_it_would_not_honor_and_values_it_cannot_use() {
    let dataset = fixture_dataset();
    for (extra, expected) in [
        // Flags `watch` would silently ignore. A score gate over a moving dataset and a sample of
        // the episodes recorded *first* are both answers to a question a watch is not asking.
        (
            vec!["--min-score", "90", "--iterations", "1"],
            "watch does not support --min-score",
        ),
        (
            vec!["--sample-episodes", "1", "--iterations", "1"],
            "watch does not support --sample-episodes",
        ),
        // Values it cannot act on. Each would otherwise fall back to a default, which is the
        // failure this whole parser exists to prevent.
        (vec!["--iterations", "0"], "invalid --iterations `0`"),
        (
            vec!["--interval", "soon", "--iterations", "1"],
            "invalid --interval `soon`",
        ),
        (
            vec!["--fail-on", "warn", "--iterations", "1"],
            "invalid --fail-on `warn`",
        ),
    ] {
        let mut argv = vec!["watch", dataset.as_str()];
        argv.extend(extra.iter().copied());
        let (code, _, stderr) = run(&argv);
        assert_eq!(code, 2, "`{extra:?}` must be a tool error");
        assert!(
            stderr.contains(expected),
            "unexpected stderr for {extra:?}: {stderr}"
        );
    }
}

#[test]
fn watch_without_a_path_is_a_tool_error() {
    let (code, _, stderr) = run(&["watch"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("missing dataset path"));
}

// ---------------------------------------------------------------------------
// `veridex check --print-config` — the effective configuration, and where it came from.
// ---------------------------------------------------------------------------

#[test]
fn print_config_names_the_layer_every_value_came_from() {
    // The question a resolved number cannot answer: *why* is this threshold 20 ms when my
    // veridex.toml says 50? Each layer that set a value has to say so.
    let dir = temp_dir("printconfig");
    let config = dir.join("veridex.toml");
    std::fs::write(
        &config,
        "fail_on = 'warning'\nmin_score = 70\ndisabled_checks = ['semantic.task-quality']\n\
         \n[tolerances]\nclock_skew_ms = 50.0\ngap_factor = 2.0\n",
    )
    .expect("write config");

    let (code, stdout, _) = run(&[
        "check",
        "--print-config",
        "--config",
        config.to_str().unwrap(),
        "--profile",
        "world-model-ready",
        "--min-score",
        "90",
    ]);
    assert_eq!(code, 0, "printing a config is not a verdict: {stdout}");
    assert!(stdout.contains("Effective configuration"));
    // The profile tightened the file's value, and says what it tightened.
    assert!(
        stdout.contains("tolerances.clock_skew_ms")
            && stdout.contains("(profile)")
            && stdout.contains("tightened it from 50"),
        "the profile's override must be attributed: {stdout}"
    );
    // The flag beat the file, and says what it beat.
    assert!(
        stdout.contains("--min-score overrides the config file's 70"),
        "the flag's override must be attributed: {stdout}"
    );
    // A value the file set, and one nobody set.
    assert!(stdout.contains("gap_factor") && stdout.contains("(config file)"));
    assert!(stdout.contains("(default)"));
}

#[test]
fn print_config_reads_no_dataset() {
    // The configuration does not depend on a dataset, so requiring one would make the flag useless
    // for the thing it is for: checking a veridex.toml before pointing it at anything.
    let (code, stdout, stderr) = run(&["check", "--print-config"]);
    assert_eq!(code, 0, "unexpected stderr: {stderr}");
    assert!(stdout.contains("Config file: (none"));
    assert!(stdout.contains("tolerances.clock_skew_ms"));
}

#[test]
fn print_config_json_is_the_machine_readable_same_document() {
    let (code, stdout, _) = run(&["check", "--print-config", "--json"]);
    assert_eq!(code, 0);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(doc["schema"], "veridex.config/1");
    assert!(doc["config_file"].is_null());
    let settings = doc["settings"].as_array().expect("settings array");
    let by_key: std::collections::BTreeMap<&str, &serde_json::Value> = settings
        .iter()
        .map(|s| (s["key"].as_str().unwrap(), s))
        .collect();
    assert_eq!(by_key["tolerances.clock_skew_ms"]["value"], "50");
    assert_eq!(by_key["tolerances.clock_skew_ms"]["origin"], "default");
    assert_eq!(by_key["min_score"]["value"], "none");
    // Every key is a `veridex.toml` key, so a printed value can be pasted back into a config.
    for key in [
        "categories",
        "only_checks",
        "disabled_checks",
        "severity_overrides",
        "fail_on",
        "min_score",
    ] {
        assert!(by_key.contains_key(key), "missing setting {key}: {stdout}");
    }
}

#[test]
fn print_config_validates_the_config_it_prints() {
    // A config that would fail a run must fail here too: this is the cheapest way to check a
    // veridex.toml, and one that printed an invalid config as though it were usable would be worse
    // than not having it.
    let dir = temp_dir("printconfig-invalid");
    for (name, body, expected) in [
        (
            "unknown-check.toml",
            "disabled_checks = ['nope.not-a-check']\n",
            "unknown check id",
        ),
        (
            "bad-tolerance.toml",
            "[tolerances]\noutlier_z = 0.5\n",
            "outlier_z must be",
        ),
        ("unknown-key.toml", "frobnicate = true\n", "invalid config"),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write config");
        let (code, _, stderr) = run(&[
            "check",
            "--print-config",
            "--config",
            path.to_str().unwrap(),
        ]);
        assert_eq!(code, 2, "{name} must be a tool error");
        assert!(
            stderr.contains(expected),
            "unexpected stderr for {name}: {stderr}"
        );
    }

    // And an unknown profile is still an unknown profile.
    let (code, _, stderr) = run(&["check", "--print-config", "--profile", "no-such-profile"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("unknown profile"), "{stderr}");
}

// ---------------------------------------------------------------------------
// The environment layer: `VERIDEX_*`, between the config file and the flags.
// ---------------------------------------------------------------------------

/// Run `veridex <args...>` with extra environment variables set.
fn run_with_env(args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_veridex"));
    cmd.args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn veridex");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_environment_configures_a_run_and_says_it_did() {
    let dataset = fixture_dataset();

    // A gate set entirely from the environment behaves exactly as `--min-score` does.
    let (code, _, stderr) = run_with_env(&["check", &dataset], &[("VERIDEX_MIN_SCORE", "90")]);
    assert_eq!(code, 20, "an environment gate must fail the run: {stderr}");
    assert!(
        stderr.contains("trust score 76 is below the required minimum 90"),
        "unexpected stderr: {stderr}"
    );

    // And `--print-config` attributes it to the layer that set it, not to a file nobody wrote.
    let (code, stdout, _) = run_with_env(
        &["check", "--print-config"],
        &[
            ("VERIDEX_MIN_SCORE", "90"),
            ("VERIDEX_TOLERANCE_CLOCK_SKEW_MS", "10"),
        ],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("min_score") && stdout.contains("(environment)"),
        "the environment must be named as the layer: {stdout}"
    );
    let skew = stdout
        .lines()
        .find(|l| l.contains("tolerances.clock_skew_ms"))
        .expect("the tolerance is printed");
    assert!(
        skew.contains("10") && skew.contains("(environment)"),
        "unexpected line: {skew}"
    );
}

#[test]
fn a_flag_beats_the_environment_which_beats_the_file() {
    // The precedence the configuration spec states, exercised through all three layers at once.
    let dir = temp_dir("env-precedence");
    let config = dir.join("veridex.toml");
    std::fs::write(&config, "min_score = 10\nfail_on = 'error'\n").expect("write config");

    let (code, stdout, _) = run_with_env(
        &[
            "check",
            "--print-config",
            "--config",
            config.to_str().unwrap(),
            "--min-score",
            "90",
        ],
        &[("VERIDEX_MIN_SCORE", "50"), ("VERIDEX_FAIL_ON", "warning")],
    );
    assert_eq!(code, 0);
    let line = |key: &str| -> String {
        stdout
            .lines()
            .find(|l| l.trim_start().starts_with(key))
            .unwrap_or_else(|| panic!("{key} is printed: {stdout}"))
            .to_string()
    };
    // The flag wins over both, the environment wins over the file.
    let min_score = line("min_score");
    assert!(
        min_score.contains("90") && min_score.contains("(flag)"),
        "unexpected: {min_score}"
    );
    let fail_on = line("fail_on");
    assert!(
        fail_on.contains("warning") && fail_on.contains("(environment)"),
        "unexpected: {fail_on}"
    );
}

#[test]
fn veridex_config_points_at_a_file_and_the_flag_still_wins() {
    let dir = temp_dir("env-config-path");
    let from_env = dir.join("env.toml");
    let from_flag = dir.join("flag.toml");
    std::fs::write(&from_env, "min_score = 30\n").expect("write config");
    std::fs::write(&from_flag, "min_score = 60\n").expect("write config");

    let (code, stdout, _) = run_with_env(
        &["check", "--print-config"],
        &[("VERIDEX_CONFIG", from_env.to_str().unwrap())],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("env.toml"), "unexpected: {stdout}");
    assert!(stdout.contains("30"), "unexpected: {stdout}");

    let (code, stdout, _) = run_with_env(
        &[
            "check",
            "--print-config",
            "--config",
            from_flag.to_str().unwrap(),
        ],
        &[("VERIDEX_CONFIG", from_env.to_str().unwrap())],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("flag.toml") && !stdout.contains("env.toml"),
        "--config must beat VERIDEX_CONFIG: {stdout}"
    );
}

#[test]
fn a_bad_environment_variable_stops_the_run() {
    // A mistyped tolerance variable moves nothing, so a run that accepted it would quietly use the
    // default threshold the operator meant to change.
    let dataset = fixture_dataset();
    for (var, value, expected) in [
        (
            "VERIDEX_TOLERANCE_CLOCK_SKEW",
            "10",
            "unknown environment variable",
        ),
        ("VERIDEX_FAIL_ON", "warn", "invalid fail_on"),
        ("VERIDEX_TOLERANCE_OUTLIER_Z", "0.5", "outlier_z must be"),
    ] {
        let (code, _, stderr) = run_with_env(&["check", &dataset], &[(var, value)]);
        assert_eq!(code, 2, "`{var}={value}` must be a tool error");
        assert!(
            stderr.contains(expected),
            "unexpected stderr for {var}: {stderr}"
        );
    }
}

#[test]
fn veridex_profile_selects_a_profile_and_the_flag_still_wins() {
    // A CI image can pin the profile every job is judged against; a job can still override it, and
    // an unknown name is refused from either layer.
    let (code, stdout, _) = run_with_env(
        &["check", "--print-config"],
        &[("VERIDEX_PROFILE", "world-model-ready")],
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("Profile:     world-model-ready"),
        "unexpected: {stdout}"
    );
    // The profile tightened cross-sensor sync, exactly as the flag does.
    let skew = stdout
        .lines()
        .find(|l| l.contains("tolerances.clock_skew_ms"))
        .expect("the tolerance is printed");
    assert!(skew.contains("20") && skew.contains("(profile)"), "{skew}");

    let (code, _, stderr) = run_with_env(
        &["check", "--print-config"],
        &[("VERIDEX_PROFILE", "no-such-profile")],
    );
    assert_eq!(
        code, 2,
        "an unknown profile is refused from the environment too"
    );
    assert!(stderr.contains("unknown profile"), "{stderr}");
}

// ---------------------------------------------------------------------------
// `veridex check --redact` — a report that can leave the building.
// ---------------------------------------------------------------------------

#[test]
fn a_redacted_report_keeps_the_findings_and_drops_the_names() {
    let dataset = fixture_dataset();
    let (plain_code, plain, _) = run(&["check", &dataset]);
    let (code, redacted, _) = run(&["check", "--redact", &dataset]);

    assert_eq!(code, plain_code, "redaction is a rendering, not a verdict");
    assert!(
        plain.contains("/camera/image") && plain.contains("/joint_states"),
        "the unredacted report names the streams: {plain}"
    );
    assert!(
        !redacted.contains("/camera/image") && !redacted.contains("/joint_states"),
        "a redacted report must not name them: {redacted}"
    );
    // What the report is *about* survives: the finding, its severity, and its measurement.
    assert!(redacted.contains("TEMPORAL.CLOCK_SKEW"), "{redacted}");
    assert!(redacted.contains("210.0 ms"), "{redacted}");
    assert!(redacted.contains("stream#"), "{redacted}");
    // And it says so, in the report itself.
    assert!(redacted.contains("REPORT.REDACTED"), "{redacted}");
    // The score and the hash are the run's own, so the two documents are comparable.
    let hash = |report: &str| {
        report
            .lines()
            .find(|l| l.contains("CDM hash:"))
            .unwrap_or_default()
            .to_string()
    };
    assert_eq!(hash(&plain), hash(&redacted));
    assert_eq!(
        plain.lines().find(|l| l.contains("Trust:")),
        redacted.lines().find(|l| l.contains("Trust:"))
    );
}

#[test]
fn redaction_reaches_the_machine_readable_reports_too() {
    // A shared report is most often the machine-readable one, and the disclosure has to travel with
    // it — which is why it is a finding rather than a printed banner.
    let dataset = fixture_dataset();
    let (_, stdout, _) = run(&["check", "--redact", "--json", &dataset]);
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(
        !stdout.contains("/camera/image"),
        "the JSON report leaked a stream name: {stdout}"
    );
    let codes: Vec<&str> = report["verdict"]["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["code"].as_str().unwrap_or_default())
        .collect();
    assert!(codes.contains(&"REPORT.REDACTED"), "{codes:?}");
    assert!(codes.contains(&"TEMPORAL.CLOCK_SKEW"), "{codes:?}");

    let (_, sarif, _) = run(&["check", "--redact", "--sarif", &dataset]);
    assert!(!sarif.contains("/camera/image"), "SARIF leaked: {sarif}");
    assert!(sarif.contains("REPORT.REDACTED"), "{sarif}");
}

#[test]
fn a_certificate_cannot_be_redacted() {
    // A certificate attests a dataset by name and hash. Redacting one would produce a signed
    // document that says less than it attests — refused, like every other flag a command would not
    // act on.
    let dir = temp_dir("redact-certify");
    let key = dir.join("issuer");
    let (code, _, _) = run(&["keygen", key.to_str().unwrap()]);
    assert_eq!(code, 0);
    let (code, _, stderr) = run(&[
        "certify",
        &fixture_dataset(),
        "--key",
        key.to_str().unwrap(),
        "--redact",
        "--out",
        dir.join("c.json").to_str().unwrap(),
    ]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("certify does not support --redact"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn print_config_refuses_the_flags_it_would_ignore() {
    // `--print-config` reads no dataset, so every flag describing a run over one would be accepted
    // and do nothing — the exact failure this CLI's allow-list exists to prevent. It shipped
    // accepting all of them, and a dataset path too.
    for (extra, expected) in [
        (
            vec!["--sample-episodes", "3"],
            "--print-config does not support --sample-episodes",
        ),
        (
            vec!["--metadata-only"],
            "--print-config does not support --metadata-only",
        ),
        (
            vec!["--max-frames", "10"],
            "--print-config does not support --max-frames",
        ),
        (vec!["--sarif"], "--print-config does not support --sarif"),
        (vec!["--redact"], "--print-config does not support --redact"),
    ] {
        let mut argv = vec!["check", "--print-config"];
        argv.extend(extra.iter().copied());
        let (code, _, stderr) = run(&argv);
        assert_eq!(code, 2, "`{extra:?}` must be a tool error");
        assert!(stderr.contains(expected), "unexpected stderr: {stderr}");
    }

    // And a dataset path, which is the most natural thing to type here.
    let (code, _, stderr) = run(&["check", "--print-config", &fixture_dataset()]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("--print-config takes no dataset path"),
        "unexpected stderr: {stderr}"
    );

    // What it does support still works.
    let (code, _, _) = run(&["check", "--print-config", "--json"]);
    assert_eq!(code, 0);
}

#[test]
fn a_regression_gate_refuses_a_redacted_report_against_a_plain_one() {
    // The gate exists to compare two runs. One redacted document and one not is a comparison of
    // documents: the same findings appear as introduced *and* resolved, so the gate fires on a
    // dataset that did not change — and in the other direction a real regression hides in the noise.
    let dir = temp_dir("diff-redaction");
    let dataset = fixture_dataset();
    let plain = dir.join("plain.json");
    let redacted = dir.join("redacted.json");
    for (path, args) in [
        (&plain, vec!["check", "--json", &dataset]),
        (&redacted, vec!["check", "--json", "--redact", &dataset]),
    ] {
        let (_, stdout, _) = run(&args);
        std::fs::write(path, stdout).expect("write report");
    }

    let (code, stdout, stderr) = run(&[
        "diff",
        "--fail-on-regression",
        plain.to_str().unwrap(),
        redacted.to_str().unwrap(),
    ]);
    assert_eq!(code, 20, "the mismatch must fail the gate");
    assert!(
        stdout.contains("Redaction: CHANGED"),
        "and lead the report: {stdout}"
    );
    assert!(
        stderr.contains("one of these reports is redacted"),
        "{stderr}"
    );

    // Two redacted reports of the same dataset compare cleanly and pass.
    let (code, _, _) = run(&[
        "diff",
        "--fail-on-regression",
        redacted.to_str().unwrap(),
        redacted.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);
}

#[test]
fn a_threshold_profile_measures_harder_without_narrowing_the_run() {
    // `strict` is not a narrowing: it only tightens, which measures the data harder than the catalog
    // asks and can only *lower* a score. So it must not emit SCOPE.NARROWED, must not print a
    // readiness block it has no criteria for, and must not disqualify a `--min-score` gate.
    let dataset = fixture_dataset();
    let (code, stdout, _) = run(&["check", "--profile", "strict", "--json", &dataset]);
    assert_eq!(code, 20, "the demo still fails, harder");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let codes: Vec<&str> = report["verdict"]["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["code"].as_str().unwrap_or_default())
        .collect();
    assert!(
        !codes.contains(&"SCOPE.NARROWED"),
        "tightening is not narrowing: {codes:?}"
    );
    assert!(
        report.get("readiness").is_none(),
        "a threshold profile makes no readiness claim: {stdout}"
    );

    // The gate a narrowed run would be refused is accepted here.
    let (code, _, stderr) = run(&[
        "check",
        "--profile",
        "strict",
        "--min-score",
        "50",
        &dataset,
    ]);
    assert_eq!(code, 20, "score 76 clears 50, but the findings still fail");
    assert!(
        !stderr.contains("cannot gate"),
        "a tightening profile must not disqualify the gate: {stderr}"
    );

    // And the thresholds it applied are reported, with the layer that set them.
    let (code, stdout, _) = run(&["check", "--print-config", "--profile", "strict"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("`strict` tightened it from 50"),
        "unexpected: {stdout}"
    );
}

#[test]
fn a_loosening_profile_is_refused_by_name_with_its_reason() {
    let (code, _, stderr) = run(&["check", "--profile", "lenient", &fixture_dataset()]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("is not a profile Veridex provides") && stderr.contains("SCOPE.NARROWED"),
        "the refusal must teach, not just reject: {stderr}"
    );

    // A typo still reads as a typo, and names what exists.
    let (code, _, stderr) = run(&["check", "--profile", "strcit", &fixture_dataset()]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("unknown profile `strcit`") && stderr.contains("world-model-ready"),
        "unexpected: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// `veridex label` — the certificate as a dataset card reads it.
// ---------------------------------------------------------------------------

#[test]
fn a_label_says_what_the_certificate_says_and_nothing_more() {
    let dir = temp_dir("label");
    let key = dir.join("issuer");
    let cert = dir.join("c.json");
    assert_eq!(run(&["keygen", key.to_str().unwrap()]).0, 0);
    // `certify` exits with the verdict it signed; the demo fails, and the certificate is written.
    let (code, _, _) = run(&[
        "certify",
        &fixture_dataset(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert.to_str().unwrap(),
    ]);
    assert_eq!(code, 20);

    let (code, label, stderr) = run(&[
        "label",
        "--certificate",
        cert.to_str().unwrap(),
        "--key",
        &format!("{}.pub", key.to_str().unwrap()),
    ]);
    assert_eq!(code, 0, "unexpected stderr: {stderr}");

    // The facts a dataset card needs, from the signed document.
    for expected in [
        "## Veridex trust label",
        "Grade C — 76/100",
        "| Dataset | `demo` |",
        "| Findings | 1 error · 1 warning · 3 info |",
        "| Provenance |",
        "veridex verify",
    ] {
        assert!(label.contains(expected), "missing `{expected}`:\n{label}");
    }
    // And the sentence that keeps it from reading as an endorsement.
    assert!(
        label.contains("statement of fact") && label.contains("not an endorsement"),
        "{label}"
    );
    // With a trusted key, no unverified-issuer caveat.
    assert!(!label.contains("Issuer not verified"), "{label}");
}

#[test]
fn a_label_carries_its_own_caveats() {
    // The caveats have to travel with the pasted text: whoever reads the label is not the person who
    // ran the command, and will never see the terminal it was produced in.
    let dir = temp_dir("label-caveats");
    let key = dir.join("issuer");
    let cert = dir.join("c.json");
    assert_eq!(run(&["keygen", key.to_str().unwrap()]).0, 0);
    let (code, _, _) = run(&[
        "certify",
        &fixture_dataset(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert.to_str().unwrap(),
    ]);
    assert_eq!(code, 20);

    // No trust decision at all is refused, rather than defaulted either way.
    let (code, _, stderr) = run(&["label", "--certificate", cert.to_str().unwrap()]);
    assert_eq!(code, 2);
    assert!(stderr.contains("needs a trusted issuer"), "{stderr}");

    // Opting out puts the caveat in the label itself.
    let (code, label, _) = run(&[
        "label",
        "--certificate",
        cert.to_str().unwrap(),
        "--allow-any-issuer",
    ]);
    assert_eq!(code, 0);
    assert!(label.contains("Issuer not verified"), "{label}");
}

#[test]
fn a_tampered_certificate_gets_no_label() {
    // A label is a paste-ready grade. Rendering one from a document that does not verify would
    // produce precisely the artifact a forger wants.
    let dir = temp_dir("label-tampered");
    let key = dir.join("issuer");
    let cert = dir.join("c.json");
    assert_eq!(run(&["keygen", key.to_str().unwrap()]).0, 0);
    let (code, _, _) = run(&[
        "certify",
        &fixture_dataset(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        cert.to_str().unwrap(),
    ]);
    assert_eq!(code, 20);

    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cert).unwrap()).unwrap();
    doc["certificate"]["trust_score"]["score"] = serde_json::json!(99);
    std::fs::write(&cert, doc.to_string()).unwrap();

    let (code, stdout, stderr) = run(&[
        "label",
        "--certificate",
        cert.to_str().unwrap(),
        "--allow-any-issuer",
    ]);
    assert_eq!(code, 20, "a doctored certificate must not produce a label");
    assert!(
        stderr.contains("refusing to label an unverified certificate"),
        "{stderr}"
    );
    assert!(!stdout.contains("Veridex trust label"), "{stdout}");
}

#[test]
fn the_default_report_is_readable_and_full_restores_everything() {
    let dataset = fixture_dataset();
    let (code, compact, _) = run(&["check", &dataset]);
    assert_eq!(code, 20);
    let (code, full, _) = run(&["check", "--full", &dataset]);
    assert_eq!(code, 20);

    // The error keeps its guidance in both; the info findings lose theirs by default.
    assert!(compact.contains("risk:   Clock drift"), "{compact}");
    assert!(
        !compact.contains("risk:   Unknown clock source"),
        "an info finding's guidance is not printed by default: {compact}"
    );
    assert!(full.contains("risk:   Unknown clock source"), "{full}");
    // Nothing is lost: every code appears in both.
    for code in ["TEMPORAL.CLOCK_SKEW", "PROVENANCE.MISSING_CLOCK"] {
        assert!(compact.contains(code) && full.contains(code), "{code}");
    }
    assert!(
        compact.contains("info finding(s) printed without their risk"),
        "the omission must be disclosed: {compact}"
    );
    assert!(full.len() > compact.len());

    // `--print-config` prints no findings at all, so the flag would do nothing there.
    let (code, _, stderr) = run(&["check", "--print-config", "--full"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("does not support --full"), "{stderr}");
}
