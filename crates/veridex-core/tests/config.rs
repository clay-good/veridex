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
    let engine = veridex_core::checks::default_engine().unwrap();
    engine.run(d, content_hash(d), cfg)
}

#[test]
fn parses_full_config() {
    let toml = r#"
        fail_on = "warning"
        categories = ["temporal"]
        disabled_checks = ["temporal.gap"]
        [severity_overrides]
        "temporal.clock-skew" = "warning"
    "#;
    let cfg = CheckConfig::from_toml(toml).expect("parses");
    assert_eq!(cfg.fail_on, FailOn::Warning);
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
