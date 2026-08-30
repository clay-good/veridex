//! Behavior tests for configuration parsing and its effect on a run.

use veridex_core::cdm::{ClockKind, Dataset, Episode, Frame, Modality, Stream, ValueRef};
use veridex_core::check::{Category, Severity};
use veridex_core::{content_hash, CheckConfig, FailOn, RunConfig};

fn stream(name: &str, clock: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        dim_names: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        media: None,
        frame_id: None,
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

/// Timestamps at 100 Hz across `span_ns`.
fn dense(span_ns: i64) -> Vec<i64> {
    (0..=(span_ns / 10_000_000))
        .map(|i| i * 10_000_000)
        .collect()
}

fn skewed() -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            // Both sample at 100 Hz. The cadence is load-bearing: a span comparison allows for each
            // stream's own sampling period, so a two-frame stream is a 1 Hz sensor whose span cannot
            // evidence a 500 ms drift (see `temporal::sampling_period_ns`).
            streams: vec![
                stream("cam", "camera", &dense(1_000_000_000)),
                stream("robot", "robot", &dense(1_500_000_000)),
            ],
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
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
    // Only temporal checks ran; provenance findings (a different category) are absent. The one
    // non-temporal finding permitted is the run disclosing its own narrowed scope, which is emitted
    // by the engine rather than by a catalog check precisely so config cannot suppress it.
    assert!(v
        .findings
        .iter()
        .all(|f| f.check_id.starts_with("temporal.")
            || f.check_id == veridex_core::engine::SCOPE_CHECK_ID));
    assert!(v.findings.iter().any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
    assert!(v.findings.iter().any(|f| f.code == "SCOPE.NARROWED"));
}

#[test]
fn tolerances_parse_resolve_and_validate() {
    // Provided values resolve; unset ones fall back to the defaults.
    let cfg = CheckConfig::from_toml(
        "[tolerances]\nclock_skew_ms = 250\nstart_offset_ms = 120\nend_offset_ms = 90\n\
         rate_deviation = 0.2\ngap_factor = 5\njitter_cv = 0.8\nepisode_duration_factor = 6\n\
         saturation_fraction = 0.7\nsaturation_min_samples = 50\noutlier_z = 6\n\
         sequence_drop_fraction = 0.2\nego_max_speed_mps = 40\n",
    )
    .expect("parses");
    let rc = cfg.to_run_config();
    assert_eq!(rc.tolerances.clock_skew_ns, 250_000_000);
    assert_eq!(rc.tolerances.start_offset_ns, 120_000_000);
    assert_eq!(rc.tolerances.end_offset_ns, 90_000_000);
    assert_eq!(rc.tolerances.rate_deviation, 0.2);
    assert_eq!(rc.tolerances.gap_factor, 5.0);
    assert_eq!(rc.tolerances.jitter_cv, 0.8);
    assert_eq!(rc.tolerances.episode_duration_factor, 6.0);
    assert_eq!(rc.tolerances.saturation_fraction, 0.7);
    assert_eq!(rc.tolerances.saturation_min_samples, 50);
    assert_eq!(rc.tolerances.outlier_z, 6.0);
    assert_eq!(rc.tolerances.sequence_drop_fraction, 0.2);
    assert_eq!(rc.tolerances.ego_max_speed_mps, 40.0);

    // Unset time tolerances fall back to the 50 ms default.
    let defaults = CheckConfig::from_toml("[tolerances]\nclock_skew_ms = 250\n")
        .expect("parses")
        .to_run_config();
    assert_eq!(defaults.tolerances.start_offset_ns, 50_000_000);
    assert_eq!(defaults.tolerances.end_offset_ns, 50_000_000);
    // An unset episode-duration factor falls back to the 10x default.
    assert_eq!(defaults.tolerances.episode_duration_factor, 10.0);

    // Invalid values are rejected, not silently ignored.
    assert!(CheckConfig::from_toml("[tolerances]\nclock_skew_ms = -1\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nend_offset_ms = -1\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\ngap_factor = 0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nrate_deviation = -0.5\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\njitter_cv = -0.1\n").is_err());
    // A duration factor of 1.0 or less would flag every episode — rejected.
    assert!(CheckConfig::from_toml("[tolerances]\nepisode_duration_factor = 1.0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nepisode_duration_factor = 0.5\n").is_err());
    // A saturation fraction must be within (0.0, 1.0]: 0 flags everything, >1 is unreachable.
    assert!(CheckConfig::from_toml("[tolerances]\nsaturation_fraction = 0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nsaturation_fraction = 1.5\n").is_err());
    // An unset saturation fraction falls back to the 0.5 default.
    assert_eq!(defaults.tolerances.saturation_fraction, 0.5);
    assert_eq!(defaults.tolerances.saturation_min_samples, 20);
    // Below 1σ the Chebyshev bound says nothing, so every stream would be flagged — rejected.
    assert!(CheckConfig::from_toml("[tolerances]\noutlier_z = 1.0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\noutlier_z = -3\n").is_err());
    // A drop fraction of 1.0 could never be exceeded; a negative one flags everything.
    assert!(CheckConfig::from_toml("[tolerances]\nsequence_drop_fraction = 1.0\n").is_err());
    assert!(CheckConfig::from_toml("[tolerances]\nsequence_drop_fraction = -0.1\n").is_err());
    // A zero or negative speed limit would call every step a teleport.
    assert!(CheckConfig::from_toml("[tolerances]\nego_max_speed_mps = 0\n").is_err());
    assert_eq!(defaults.tolerances.outlier_z, 10.0);
    assert_eq!(defaults.tolerances.sequence_drop_fraction, 0.05);
    assert_eq!(defaults.tolerances.ego_max_speed_mps, 100.0);
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
fn a_loose_end_offset_tolerance_suppresses_the_end_offset_finding() {
    // Two streams on one shared clock, aligned at the start but one ending 500 ms after the other.
    let tail_misaligned = || Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams: vec![
                stream("cam", "wall", &[0, 1_000_000_000]),
                stream("arm", "wall", &[0, 1_500_000_000]),
            ],
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
        }],
    };

    // The default 50 ms tolerance flags the 500 ms tail misalignment.
    let strict = CheckConfig::from_toml("").unwrap();
    let v = run(&tail_misaligned(), &strict.to_run_config());
    assert!(v.findings.iter().any(|f| f.code == "TEMPORAL.END_OFFSET"));

    // Raising end_offset_ms above the misalignment suppresses it — the configured threshold applied.
    let loose = CheckConfig::from_toml("[tolerances]\nend_offset_ms = 800\n").unwrap();
    let v2 = run(&tail_misaligned(), &loose.to_run_config());
    assert!(v2.findings.iter().all(|f| f.code != "TEMPORAL.END_OFFSET"));
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

/// The effective settings a parsed config resolves to, with no flags, environment or profile.
fn settings_for(file: &veridex_core::CheckConfig) -> Vec<veridex_core::effective::Setting> {
    let empty = std::collections::BTreeSet::new();
    veridex_core::effective::settings(&veridex_core::effective::Inputs {
        config_path: None,
        file,
        from_env: &empty,
        profile: None,
        tolerances: veridex_core::Tolerances::default(),
        fail_on: veridex_core::FailOn::Error,
        fail_on_from_flag: false,
        min_score: None,
        min_score_from_flag: false,
    })
}

#[test]
fn a_setting_written_in_the_file_is_attributed_to_the_file_even_at_its_default() {
    // The effective configuration answers "was this run configured, and by whom" — and it is signed
    // into every certificate. Three settings have no "unset" value to test for (`fail_on` defaults
    // to `error`, `disabled_checks` and `severity_overrides` to empty), so asking "does this differ
    // from the default?" answered a different question, and a file that wrote `fail_on = "error"`
    // had its own setting reported as `(default)`. An auditor could not tell a producer who chose
    // the default from one who never opened the file.
    let text = "fail_on = \"error\"\ndisabled_checks = []\ncategories = [\"structural\"]\n\n\
                [severity_overrides]\n";
    let file = veridex_core::CheckConfig::from_toml(text).expect("parses");
    let settings = settings_for(&file);
    let origin = |key: &str| {
        settings
            .iter()
            .find(|s| s.key == key)
            .map(|s| format!("{:?}", s.origin))
            .unwrap_or_else(|| panic!("no `{key}` setting"))
    };
    for key in [
        "fail_on",
        "disabled_checks",
        "severity_overrides",
        "categories",
    ] {
        assert_eq!(
            origin(key),
            "ConfigFile",
            "`{key}` was written in the file and must be attributed to it"
        );
    }
    // And a key the file did not carry is still a default, so the attribution is a signal.
    assert_eq!(origin("min_score"), "Default");
    assert_eq!(origin("only_checks"), "Default");
}

#[test]
fn an_explicitly_empty_selection_reads_as_none_not_as_a_blank() {
    // `only_checks = []` runs *no* checks — a very different run from "all" — and it rendered as an
    // empty cell, which says neither.
    let file = veridex_core::CheckConfig::from_toml("only_checks = []\ncategories = []\n")
        .expect("parses");
    let settings = settings_for(&file);
    for key in ["only_checks", "categories"] {
        let value = settings
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.value.clone())
            .unwrap_or_default();
        assert_eq!(value, "none", "`{key}` reads as `{value}`");
    }
}

#[test]
fn every_configurable_key_is_documented_in_the_example() {
    // `docs/veridex.toml.example` is where a reader learns a key exists — the README points at it
    // for the environment twins too, and `--print-config` names keys without saying they can be set.
    // A tolerance added to the parser and not to the example is a knob nobody can find, and nothing
    // held the two together. The source is walked rather than reflected over, the way this repo's
    // other source-walking guards do, because Rust has no field reflection.
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/config.rs"))
        .expect("config.rs is readable");
    let example = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/veridex.toml.example"
    ))
    .expect("the example is readable");

    // The `pub <name>:` fields of one struct.
    let fields_of = |struct_name: &str| -> Vec<String> {
        let start = source
            .find(&format!("pub struct {struct_name} {{"))
            .unwrap_or_else(|| panic!("`{struct_name}` is declared in config.rs"));
        let body = &source[start..];
        let end = body.find("\n}").expect("the struct closes");
        body[..end]
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub "))
            .filter_map(|l| l.split_once(": "))
            .map(|(name, _)| name.to_string())
            .collect()
    };

    let tolerances = fields_of("TolerancesConfig");
    assert!(tolerances.len() >= 13, "found {tolerances:?}");
    for key in &tolerances {
        assert!(
            example.contains(key.as_str()),
            "`tolerances.{key}` is settable and docs/veridex.toml.example never names it"
        );
        // And its environment twin, which is the key upper-cased.
        let twin = format!("VERIDEX_TOLERANCE_{}", key.to_uppercase());
        assert!(
            example.contains("VERIDEX_TOLERANCE_"),
            "the example must document the tolerance environment pattern ({twin})"
        );
    }

    // The top-level keys, minus the one that is not a setting.
    let top: Vec<String> = fields_of("CheckConfig")
        .into_iter()
        .filter(|k| k != "keys_present")
        .collect();
    assert!(top.len() >= 6, "found {top:?}");
    for key in &top {
        assert!(
            example.contains(key.as_str()),
            "`{key}` is settable and docs/veridex.toml.example never names it"
        );
    }

    // Every environment variable the merge layer honors.
    for line in source.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("(\"VERIDEX_") else {
            continue;
        };
        let Some(name) = rest.split('"').next() else {
            continue;
        };
        let var = format!("VERIDEX_{name}");
        assert!(
            example.contains(&var),
            "`{var}` changes a run and docs/veridex.toml.example never names it"
        );
    }
}
