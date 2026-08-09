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
