//! Behavior tests for reporting.

use veridex_core::cdm::{Dataset, Episode, Frame, Modality, Stream, ValueRef};
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

fn episode(index: u64, streams: Vec<Stream>) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams,
        task: None,
        labels: vec![],
    }
}

/// A dataset with a clock-skew error in episode 7 and a clean episode 0.
fn skewed_dataset() -> Dataset {
    Dataset {
        id: "acme/demo".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![
            episode(0, vec![stream("s", "c", &[0, 1_000_000, 2_000_000])]),
            episode(
                7,
                vec![
                    stream("cam", "camera", &[0, 1_000_000_000]),
                    stream("robot", "robot", &[0, 1_500_000_000]),
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
}

#[test]
fn html_escapes_special_characters() {
    // A stream name with angle brackets flows into a finding message; it must be escaped.
    let d = Dataset {
        id: "t".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(
            0,
            vec![
                stream("a<script>b", "camera", &[0, 1_000_000_000]),
                stream("robot", "robot", &[0, 1_500_000_000]),
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
