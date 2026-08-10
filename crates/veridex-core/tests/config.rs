//! Behavior tests for configuration parsing and its effect on a run.

use veridex_core::cdm::{Dataset, Episode, Frame, Modality, Stream, ValueRef};
use veridex_core::check::{Category, Severity};
use veridex_core::{content_hash, CheckConfig, FailOn, RunConfig};

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

fn skewed() -> Dataset {
    Dataset {
        id: "t".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams: vec![
                stream("cam", "camera", &[0, 1_000_000_000]),
                stream("robot", "robot", &[0, 1_500_000_000]),
            ],
            task: None,
            labels: vec![],
        }],
    }
}

fn run(d: &Dataset, cfg: &RunConfig) -> veridex_core::Verdict {
    // Mirror the pipeline: build the engine with the run's tolerances so configured thresholds apply.
    let engine = veridex_core::checks::default_engine_with(&cfg.tolerances).unwrap();
    engine.run(d, content_hash(d), cfg)
}

#[test]
fn parses_full_config() {
    let toml = r#"
        fail_on = "warning"
        min_score = 80
        categories = ["temporal"]
        disabled_checks = ["temporal.gap"]
        [severity_overrides]
        "temporal.clock-skew" = "warning"
    "#;
    let cfg = CheckConfig::from_toml(toml).expect("parses");
    assert_eq!(cfg.fail_on, FailOn::Warning);
    assert_eq!(cfg.min_score, Some(80));
    assert_eq!(cfg.categories, Some(vec![Category::Temporal]));
    assert_eq!(cfg.disabled_checks, vec!["temporal.gap".to_string()]);
    assert_eq!(
        cfg.severity_overrides.get("temporal.clock-skew"),
        Some(&Severity::Warning)
    );
}

#[test]
fn empty_config_is_the_default() {
    let cfg = CheckConfig::from_toml("").unwrap();
    assert_eq!(cfg.fail_on, FailOn::Error);
    assert!(cfg.categories.is_none());
}

#[test]
fn unknown_keys_are_rejected() {
    assert!(CheckConfig::from_toml("nonsense_key = 1").is_err());
}

#[test]
fn min_score_out_of_range_is_rejected() {
    assert!(CheckConfig::from_toml("min_score = 101").is_err());
    // The valid boundary is accepted.
    assert_eq!(
        CheckConfig::from_toml("min_score = 100").unwrap().min_score,
        Some(100)
    );
}

#[test]
fn unknown_check_id_in_config_is_rejected() {
    let engine = veridex_core::checks::default_engine().unwrap();
    let known = engine.check_ids();

    // A typo in disabled_checks must be rejected, not silently ignored.
    let cfg = CheckConfig::from_toml("disabled_checks = [\"temporal.clock-skwe\"]").unwrap();
    let err = cfg.validate_check_ids(known.clone()).unwrap_err();
    assert!(
        err.to_string().contains("temporal.clock-skwe"),
        "error must name the offending id: {err}"
    );

    // A typo in a severity override key is likewise rejected.
    let cfg2 =
        CheckConfig::from_toml("[severity_overrides]\n\"nope.not-real\" = \"warning\"\n").unwrap();
    assert!(cfg2.validate_check_ids(known.clone()).is_err());

    // A config that only references real checks validates cleanly.
    let cfg3 = CheckConfig::from_toml(
        "disabled_checks = [\"temporal.gap\"]\nonly_checks = [\"temporal.clock-skew\"]\n",
    )
    .unwrap();
    assert!(cfg3.validate_check_ids(known).is_ok());
}

#[test]
fn example_config_parses_and_references_only_real_checks() {
    // docs/veridex.toml.example is the copy-paste starting point; guard it against drifting from the
    // real config surface and check catalog. Path is relative to this crate's manifest.
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/veridex.toml.example"
    );
    let text = std::fs::read_to_string(path).expect("example config is readable");

    // It parses cleanly: valid TOML, no unknown keys (deny_unknown_fields).
    let cfg = CheckConfig::from_toml(&text).expect("example config parses");
    let engine = veridex_core::checks::default_engine().unwrap();
    cfg.validate_check_ids(engine.check_ids())
        .expect("active ids are real");

    // Every check-id-shaped token anywhere in the file — including the commented-out examples — must
    // name a real check, so the sample can't advertise an id the catalog no longer has.
    let known: std::collections::BTreeSet<&str> = engine.check_ids().into_iter().collect();
    for quoted in text.split('"').skip(1).step_by(2) {
        let looks_like_id = quoted.contains('.')
            && quoted
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '.' || c == '-');
        if looks_like_id {
            assert!(
                known.contains(quoted),
                "example config references `{quoted}`, which is not a registered check id"
            );
        }
    }
}

#[test]
fn category_selection_scopes_the_run() {
    let cfg = CheckConfig::from_toml("categories = [\"temporal\"]").unwrap();
    let v = run(&skewed(), &cfg.to_run_config());
    // Only temporal checks ran; provenance findings (a different category) are absent.
    assert!(v
        .findings
        .iter()
        .all(|f| f.check_id.starts_with("temporal.")));
    assert!(v.findings.iter().any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn tolerances_parse_resolve_and_validate() {
    // Provided values resolve; unset ones fall back to the defaults.
    let cfg = CheckConfig::from_toml(
        "[tolerances]\nclock_skew_ms = 250\nrate_deviation = 0.2\ngap_factor = 5\njitter_cv = 0.8\n",
    )
    .expect("parses");
    let rc = cfg.to_run_config();
    assert_eq!(rc.tolerances.clock_skew_ns, 250_000_000);
    assert_eq!(rc.tolerances.rate_deviation, 0.2);
    assert_eq!(rc.tolerances.gap_factor, 5.0);
    assert_eq!(rc.tolerances.jitter_cv, 0.8);
    // start_offset_ms was unset → default 50 ms.
    assert_eq!(rc.tolerances.start_offset_ns, 50_000_000);

    // Invalid values are rejected, not silently ignored.
    assert!(CheckConfig::from_toml("[tolerances]\nclock_skew_ms = -1\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\ngap_factor = 0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nrate_deviation = -0.5\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\njitter_cv = -0.1\n").is_err());
}

#[test]
fn the_verdict_records_the_tolerances_it_ran_with() {
    // Reproducibility: a configured tolerance is snapshotted into the verdict's effective config.
    let cfg = CheckConfig::from_toml("[tolerances]\nclock_skew_ms = 250\n").unwrap();
    let v = run(&skewed(), &cfg.to_run_config());
    assert_eq!(v.effective_config.tolerances.clock_skew_ns, 250_000_000);
    assert_eq!(v.effective_config.tolerances.gap_factor, 3.0); // default carried through
}

#[test]
fn a_loose_clock_skew_tolerance_suppresses_the_skew_finding() {
    // The skewed dataset drifts 500 ms; the default 50 ms tolerance flags it.
    let strict = CheckConfig::from_toml("").unwrap();
    let v = run(&skewed(), &strict.to_run_config());
    assert!(v.findings.iter().any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));

    // Raising the tolerance above the drift makes it clean — the configured threshold took effect.
    let loose = CheckConfig::from_toml("[tolerances]\nclock_skew_ms = 800\n").unwrap();
    let v2 = run(&skewed(), &loose.to_run_config());
    assert!(v2.findings.iter().all(|f| f.code != "TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn severity_override_downgrades_clock_skew() {
    let cfg =
        CheckConfig::from_toml("[severity_overrides]\n\"temporal.clock-skew\" = \"warning\"\n")
            .unwrap();
    let v = run(&skewed(), &cfg.to_run_config());
    let skew = v
        .findings
        .iter()
        .find(|f| f.code == "TEMPORAL.CLOCK_SKEW")
        .unwrap();
    assert_eq!(skew.severity, Severity::Warning);
    // With the error downgraded and no other errors, the run no longer fails.
    assert_ne!(v.status, veridex_core::Status::Fail);
}
