//! The environment layer of the configuration precedence: `VERIDEX_*` merged onto a parsed file.
//!
//! The environment is the layer a container or CI job sets without writing a file, so the two ways
//! in have to mean exactly the same thing — same keys, same validation, same refusals.

use veridex_core::check::{Category, Severity};
use veridex_core::config::{env, CheckConfig, FailOn};

/// Merge `vars` onto the default config, expecting success.
fn merged(vars: &[(&str, &str)]) -> (CheckConfig, Vec<String>) {
    let (config, keys) = env::merge(CheckConfig::default(), vars.iter().copied())
        .expect("these variables are valid");
    (config, keys.into_iter().collect())
}

/// Merge `vars`, expecting a refusal, and return its message.
fn refused(vars: &[(&str, &str)]) -> String {
    env::merge(CheckConfig::default(), vars.iter().copied())
        .expect_err("these variables must be refused")
        .to_string()
}

#[test]
fn every_config_key_has_an_environment_twin() {
    // A partial mapping is the failure this layer exists to prevent: a variable that looks like it
    // configures something and does not.
    let mut vars: Vec<(&str, &str)> = vec![
        ("VERIDEX_FAIL_ON", "warning"),
        ("VERIDEX_MIN_SCORE", "80"),
        ("VERIDEX_CATEGORIES", "temporal, provenance"),
        ("VERIDEX_ONLY_CHECKS", "temporal.clock-skew"),
        ("VERIDEX_DISABLED_CHECKS", "semantic.task-quality"),
        ("VERIDEX_SEVERITY_OVERRIDES", "temporal.gaps=error"),
    ];
    // A value each key accepts: the two fractions are bounded below 1, the rest take 2.
    let tolerance_vars: Vec<(String, &str)> = env::TOLERANCE_KEYS
        .iter()
        .map(|k| {
            let value = match *k {
                "saturation_fraction" | "sequence_drop_fraction" | "near_duplicate_fraction" => {
                    "0.5"
                }
                _ => "2",
            };
            (format!("VERIDEX_TOLERANCE_{}", k.to_uppercase()), value)
        })
        .collect();
    let tolerance_refs: Vec<(&str, &str)> = tolerance_vars
        .iter()
        .map(|(k, v)| (k.as_str(), *v))
        .collect();
    vars.extend(tolerance_refs.iter().copied());

    let (config, keys) = merged(&vars);
    assert_eq!(config.fail_on, FailOn::Warning);
    assert_eq!(config.min_score, Some(80));
    assert_eq!(
        config.categories,
        Some(vec![Category::Temporal, Category::Provenance])
    );
    assert_eq!(
        config.only_checks.as_deref(),
        Some(["temporal.clock-skew".to_string()].as_slice())
    );
    assert_eq!(config.disabled_checks, vec!["semantic.task-quality"]);
    assert_eq!(
        config.severity_overrides.get("temporal.gaps"),
        Some(&Severity::Error)
    );
    // Every tolerance took its value, and every key is reported as environment-set.
    let run = config.to_run_config().tolerances;
    assert_eq!(run.clock_skew_ns, 2_000_000);
    assert_eq!(run.gap_factor, 2.0);
    assert_eq!(run.saturation_min_samples, 2);
    for key in env::TOLERANCE_KEYS {
        assert!(
            keys.contains(&format!("tolerances.{key}")),
            "`{key}` must be reported as set by the environment: {keys:?}"
        );
    }
    for (_, key) in env::VARIABLES {
        assert!(
            keys.contains(&key.to_string()),
            "`{key}` must be reported as set by the environment: {keys:?}"
        );
    }
}

#[test]
fn the_environment_overrides_the_file_it_is_merged_onto() {
    let file = CheckConfig::from_toml(
        "fail_on = 'error'\nmin_score = 50\n\n[tolerances]\nclock_skew_ms = 50.0\ngap_factor = 4.0\n",
    )
    .expect("valid config");
    let (config, keys) = env::merge(
        file,
        [
            ("VERIDEX_MIN_SCORE", "90"),
            ("VERIDEX_TOLERANCE_CLOCK_SKEW_MS", "5"),
        ],
    )
    .expect("valid environment");

    assert_eq!(config.min_score, Some(90), "the environment wins");
    assert_eq!(config.to_run_config().tolerances.clock_skew_ns, 5_000_000);
    // What it did not set is left exactly as the file had it.
    assert_eq!(config.fail_on, FailOn::Error);
    assert_eq!(config.to_run_config().tolerances.gap_factor, 4.0);
    assert!(!keys.contains("fail_on") && !keys.contains("tolerances.gap_factor"));
}

#[test]
fn a_mistyped_tolerance_variable_is_refused_by_name() {
    // The dangerous case: `VERIDEX_TOLERANCE_CLOCK_SKEW` (no `_MS`) looks set and moves nothing, so
    // the run silently keeps the default threshold the operator meant to change.
    let message = refused(&[("VERIDEX_TOLERANCE_CLOCK_SKEW", "10")]);
    assert!(
        message.contains("VERIDEX_TOLERANCE_CLOCK_SKEW") && message.contains("clock_skew_ms"),
        "the refusal must name the variable and the keys it could have meant: {message}"
    );
}

#[test]
fn a_variable_that_is_not_a_config_key_is_left_alone() {
    // The test harness sets `VERIDEX_BIN`; other tooling sets its own. Refusing every unrecognized
    // `VERIDEX_*` name would make this layer hostile to the environment it runs in.
    let (config, keys) = merged(&[("VERIDEX_BIN", "target/debug/veridex")]);
    assert_eq!(config.min_score, None);
    assert!(keys.is_empty());
}

#[test]
fn an_environment_value_meets_the_same_bar_as_a_file_value() {
    // Each of these is rejected in a `veridex.toml`; arriving through the environment changes
    // nothing about whether the value is usable.
    for (vars, expected) in [
        (
            vec![("VERIDEX_TOLERANCE_OUTLIER_Z", "0.5")],
            "outlier_z must be",
        ),
        (vec![("VERIDEX_MIN_SCORE", "200")], "min_score must be"),
        (
            vec![("VERIDEX_TOLERANCE_GAP_FACTOR", "nan")],
            "gap_factor must be",
        ),
        (vec![("VERIDEX_FAIL_ON", "warn")], "invalid fail_on `warn`"),
        (
            vec![("VERIDEX_MIN_SCORE", "high")],
            "invalid min_score `high`",
        ),
        (
            vec![("VERIDEX_CATEGORIES", "temporal, sideways")],
            "unknown category `sideways`",
        ),
        (
            vec![("VERIDEX_SEVERITY_OVERRIDES", "temporal.gaps")],
            "is not a `check-id=severity` pair",
        ),
        (
            vec![("VERIDEX_SEVERITY_OVERRIDES", "temporal.gaps=loud")],
            "unknown severity `loud`",
        ),
        (
            vec![("VERIDEX_TOLERANCE_SATURATION_MIN_SAMPLES", "1.5")],
            "expected a whole number",
        ),
    ] {
        let message = refused(&vars);
        assert!(
            message.contains(expected),
            "expected `{expected}` in the refusal of {vars:?}, got: {message}"
        );
    }
}

#[test]
fn an_empty_variable_is_a_mistake_not_an_instruction() {
    // `VERIDEX_CATEGORIES=""` in a shell script is almost always an unset variable expanding to
    // nothing — and read as an instruction it would select *no* categories, which runs no checks and
    // scores a perfect data score. Refused rather than obeyed.
    let message = refused(&[("VERIDEX_CATEGORIES", "")]);
    assert!(
        message.contains("VERIDEX_CATEGORIES is empty") && message.contains("unset it"),
        "unexpected refusal: {message}"
    );
}
