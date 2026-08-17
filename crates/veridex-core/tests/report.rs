//! Behavior tests for reporting.

use veridex_core::cdm::{ClockKind, Dataset, Episode, Frame, Modality, Stream, ValueRef};
use veridex_core::certificate::{score, ProvenanceCoverage};
use veridex_core::report::{
    render_html, render_json, render_sarif, render_terminal, REPORT_SCHEMA_VERSION,
};
use veridex_core::{content_hash, RunConfig};

fn stream(name: &str, clock: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
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

fn episode(index: u64, streams: Vec<Stream>) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams,
        task: None,
        labels: vec![],
        ego_poses: None,
        declared_frame_count: None,
    }
}

/// Timestamps at 100 Hz across `span_ns` — a span comparison allows for each stream's own sampling
/// period, so a realistic cadence is what makes the drift below evidence of skew.
fn dense(span_ns: i64) -> Vec<i64> {
    (0..=(span_ns / 10_000_000))
        .map(|i| i * 10_000_000)
        .collect()
}

/// A dataset with a clock-skew error in episode 7 and a clean episode 0.
fn skewed_dataset() -> Dataset {
    Dataset {
        id: "acme/demo".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![
            episode(0, vec![stream("s", "c", &[0, 1_000_000, 2_000_000])]),
            episode(
                7,
                vec![
                    stream("cam", "camera", &dense(1_000_000_000)),
                    stream("robot", "robot", &dense(1_500_000_000)),
                ],
            ),
        ],
    }
}

fn verdict_for(d: &Dataset) -> veridex_core::Verdict {
    let engine = veridex_core::checks::default_engine().unwrap();
    engine.run(d, content_hash(d), &RunConfig::default())
}

#[test]
fn json_report_is_versioned_and_parseable() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let json = render_json(&v, None);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["schema"], REPORT_SCHEMA_VERSION);
    assert!(parsed["verdict"]["findings"].is_array());
    // trust_score omitted when not supplied.
    assert!(parsed.get("trust_score").is_none());
}

#[test]
fn json_report_includes_trust_score_when_supplied() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let ts = score(&v, &ProvenanceCoverage::of(&d));
    let json = render_json(&v, Some(ts));
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["trust_score"]["rubric_version"], "v1");
    assert!(parsed["trust_score"]["score"].is_number());
}

#[test]
fn terminal_report_ranks_worst_episode_first() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let ts = score(&v, &ProvenanceCoverage::of(&d));
    let text = render_terminal(&v, Some(ts), 5);

    assert!(text.contains("Veridex report"));
    assert!(text.contains("FAIL"));
    assert!(text.contains("Trust:"));
    assert!(text.contains("TEMPORAL.CLOCK_SKEW"));
    assert!(text.contains("remedy:"));

    // Episode 7 (the one with the error) must be listed before episode 0 in the rollup.
    let worst_section = text.split("Worst episodes:").nth(1).unwrap();
    let idx7 = worst_section.find("episode 7").unwrap();
    let idx0 = worst_section.find("episode 0");
    if let Some(idx0) = idx0 {
        assert!(
            idx7 < idx0,
            "worst episode (7) must come before clean episode 0"
        );
    }
}

#[test]
fn terminal_report_notes_non_default_tolerances_only_when_set() {
    let d = skewed_dataset();

    // Default tolerances: no tolerance line.
    let default = verdict_for(&d);
    assert!(!render_terminal(&default, None, 5).contains("Tolerances"));

    // Loosened tolerances are surfaced so a reader knows what thresholds applied.
    let cfg = RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 800_000_000,
            episode_duration_factor: 4.0,
            saturation_fraction: 0.7,
            ..veridex_core::Tolerances::default()
        },
        ..RunConfig::default()
    };
    let engine = veridex_core::checks::default_engine_with(&cfg.tolerances).unwrap();
    let v = engine.run(&d, content_hash(&d), &cfg);
    let text = render_terminal(&v, None, 5);
    assert!(text.contains("Tolerances (non-default):"), "got: {text}");
    assert!(text.contains("clock-skew 800ms"));
    assert!(text.contains("episode-duration 4x"), "got: {text}");
    assert!(text.contains("saturation 70%"), "got: {text}");
}

#[test]
fn sarif_is_valid_2_1_0_and_maps_findings() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let sarif = render_sarif(&v);

    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "Veridex");
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    // One SARIF result per finding.
    assert_eq!(results.len(), v.findings.len());
    // The clock-skew error maps to a SARIF `error` result with a logical location.
    let skew = results
        .iter()
        .find(|r| r["ruleId"] == "TEMPORAL.CLOCK_SKEW")
        .unwrap();
    assert_eq!(skew["level"], "error");
    assert!(skew["locations"][0]["logicalLocations"][0]["name"]
        .as_str()
        .unwrap()
        .contains("episode"));
    // info findings map to SARIF `note`.
    assert!(results.iter().any(|r| r["level"] == "note"));

    // Rules are enriched: each carries a description and a help link to the check catalog.
    let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap();
    let skew_rule = rules
        .iter()
        .find(|r| r["id"] == "TEMPORAL.CLOCK_SKEW")
        .unwrap();
    assert_eq!(skew_rule["shortDescription"]["text"], "TEMPORAL.CLOCK_SKEW");
    assert!(!skew_rule["fullDescription"]["text"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(skew_rule["helpUri"]
        .as_str()
        .unwrap()
        .contains("docs/checks.md"));
}

#[test]
fn html_is_self_contained_and_shows_findings() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let ts = score(&v, &ProvenanceCoverage::of(&d));
    let html = render_html(&v, Some(ts));

    assert!(html.starts_with("<!DOCTYPE html>"));
    // Self-contained: styles are inline, no external asset references.
    assert!(html.contains("<style>"));
    assert!(!html.contains("http://") && !html.contains("https://"));
    // Content: the clock-skew finding and the trust score are present.
    assert!(html.contains("TEMPORAL.CLOCK_SKEW"));
    assert!(html.contains("Trust "));
    assert!(html.contains("FAIL"));
    // The training risk is surfaced (the shareable report's whole point), not just the remedy.
    assert!(html.contains("<th>Risk</th>"));
    let skew_risk = v
        .findings
        .iter()
        .find(|f| f.code == "TEMPORAL.CLOCK_SKEW")
        .unwrap()
        .risk
        .clone();
    assert!(!skew_risk.is_empty() && html.contains(&skew_risk));
}

#[test]
fn html_escapes_special_characters() {
    // A stream name with angle brackets flows into a finding message; it must be escaped.
    let d = Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(
            0,
            vec![
                stream("a<script>b", "camera", &dense(1_000_000_000)),
                stream("robot", "robot", &dense(1_500_000_000)),
            ],
        )],
    };
    let v = verdict_for(&d);
    let html = render_html(&v, None);
    // The raw injection must not survive; the escaped form must be present.
    assert!(!html.contains("a<script>b"));
    assert!(html.contains("a&lt;script&gt;b"));
}

#[test]
fn json_and_terminal_agree_on_finding_count() {
    let d = skewed_dataset();
    let v = verdict_for(&d);
    let json = render_json(&v, None);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let json_count = parsed["verdict"]["findings"].as_array().unwrap().len();
    // The terminal report lists one "[severity]" header per finding.
    let text = render_terminal(&v, None, 5);
    let term_count = text.matches("  [error] ").count()
        + text.matches("  [warning] ").count()
        + text.matches("  [info] ").count();
    assert_eq!(json_count, v.findings.len());
    assert_eq!(term_count, v.findings.len());
}

/// A single-episode dataset holding `streams`.
fn simple_dataset(streams: Vec<Stream>) -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(0, streams)],
    }
}

/// A verdict whose only content is a check that failed to run: no findings, nothing wrong found —
/// because the check never ran.
fn verdict_with_an_errored_check() -> veridex_core::Verdict {
    let d = simple_dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let mut v = verdict_for(&d);
    v.findings.clear();
    v.counts = Default::default();
    v.errored_checks.push(veridex_core::engine::ErroredCheck {
        check_id: "temporal.clock-skew",
        version: "1",
        message: "panicked".into(),
    });
    v
}

#[test]
fn a_check_that_failed_to_run_is_visible_in_every_renderer() {
    // "No findings" from a check that crashed is not a pass. A shared HTML artifact or a SARIF gate
    // that omitted this read as clean while a check never ran.
    let v = verdict_with_an_errored_check();

    let text = veridex_core::render_terminal(&v, None, 5);
    assert!(text.contains("Errored checks"), "terminal: {text}");

    let html = render_html(&v, None);
    assert!(html.contains("Errored checks"), "html omits errored checks");
    assert!(html.contains("temporal.clock-skew"));

    let sarif = veridex_core::render_sarif(&v);
    let results = sarif["runs"][0]["results"].as_array().expect("results");
    assert_eq!(results.len(), 1, "sarif must report the errored check");
    assert_eq!(results[0]["ruleId"], "VERIDEX.CHECK_ERRORED");
    assert_eq!(results[0]["level"], "error");
    let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .expect("rules");
    assert!(
        rules.iter().any(|r| r["id"] == "VERIDEX.CHECK_ERRORED"),
        "the rule must be declared alongside its result"
    );
}

#[test]
fn a_loosened_tolerance_is_disclosed_in_the_html_report_too() {
    // A shared report has to say what thresholds produced it, not just the terminal one.
    let d = simple_dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let engine = veridex_core::checks::default_engine().unwrap();
    let config = RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 5_000_000_000,
            ..Default::default()
        },
        ..Default::default()
    };
    let v = engine.run(&d, content_hash(&d), &config);
    let html = render_html(&v, None);
    assert!(html.contains("Tolerances (non-default)"), "html: {html}");
    assert!(html.contains("clock-skew 5000ms"));
}

/// A dataset can name its own streams, and those names reach the terminal inside finding messages
/// and location labels. If the terminal renders them raw, a dataset can emit ANSI escapes that clear
/// the screen and repaint a forged verdict over the real one.
#[test]
fn a_dataset_supplied_control_sequence_cannot_repaint_the_terminal_report() {
    let hostile = "\u{1b}[2J\u{1b}[1;1HVeridex report\n  Status:   PASS\u{7}";
    let d = simple_dataset(vec![
        stream(hostile, "c", &[0, 1_000_000]),
        stream("honest", "c", &[0, 5_000_000_000]),
    ]);
    let engine = veridex_core::checks::default_engine().unwrap();
    let v = engine.run(&d, content_hash(&d), &RunConfig::default());

    let text = render_terminal(&v, None, 5);
    assert!(
        !text.contains('\u{1b}'),
        "no escape character may reach the terminal: {text:?}"
    );
    assert!(
        !text.contains('\u{7}'),
        "no bell character may reach the terminal"
    );
    // The name is still shown, just inertly — escaping must not hide what the dataset is called.
    assert!(text.contains("\\x1b"), "the escape is shown, not executed");
    assert!(text.contains("Veridex report"));
}

/// The tolerance line exists to state the threshold a verdict was produced under. Rounding a
/// deliberately tightened threshold to `0`, or to exactly the default, makes it state the wrong one.
#[test]
fn a_sub_millisecond_tolerance_is_disclosed_at_its_real_value() {
    let d = simple_dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let engine = veridex_core::checks::default_engine().unwrap();
    let config = RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 500_000,     // 0.5 ms — tightened, not zero
            rate_deviation: 0.004,      // 0.4% — tightened, not zero
            saturation_fraction: 0.002, // 0.2% — tightened, not zero
            ..Default::default()
        },
        ..Default::default()
    };
    let v = engine.run(&d, content_hash(&d), &config);
    let text = render_terminal(&v, None, 5);

    assert!(text.contains("clock-skew 0.5ms"), "{text}");
    assert!(text.contains("rate 0.4%"), "{text}");
    assert!(text.contains("saturation 0.2%"), "{text}");
    assert!(
        !text.contains("clock-skew 0ms"),
        "a tightened threshold must not read as zero"
    );
}

/// A loosened threshold that rounds to exactly the default is the worst case: the line is there to
/// warn the reader, and it prints the value it was meant to distinguish itself from.
#[test]
fn a_loosened_tolerance_is_not_rounded_back_to_the_default() {
    let d = simple_dataset(vec![stream("s", "c", &[0, 1_000_000])]);
    let engine = veridex_core::checks::default_engine().unwrap();
    let config = RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 50_900_000, // 50.9 ms; the default is 50 ms
            ..Default::default()
        },
        ..Default::default()
    };
    let v = engine.run(&d, content_hash(&d), &config);
    let text = render_terminal(&v, None, 5);
    assert!(text.contains("clock-skew 50.9ms"), "{text}");
}
