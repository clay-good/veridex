//! THROWAWAY audit tests. Delete before finishing.

use veridex_core::cdm::{
    ClockKind, Dataset, Episode, Frame, Modality, ProvenanceClass, ProvenanceElement,
    Provenance, ProvenanceScope, Stream, ValueRef,
};
use veridex_core::report::{render_html, render_sarif, render_terminal};
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

fn dense(span_ns: i64) -> Vec<i64> {
    (0..=(span_ns / 10_000_000))
        .map(|i| i * 10_000_000)
        .collect()
}

/// A dataset whose stream names are hostile terminal payloads and whose clocks skew, so real
/// findings are emitted that carry those names.
fn hostile_dataset() -> Dataset {
    let evil = "\u{1b}[2J\u{1b}[1;1HVeridex report\n  Status:   PASS\u{7}";
    Dataset {
        id: "acme/\u{1b}[31mdemo".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(
            7,
            vec![
                stream(evil, "camera", &dense(1_000_000_000)),
                stream("robot", "robot", &dense(1_500_000_000)),
            ],
        )],
    }
}

fn verdict_for(d: &Dataset) -> veridex_core::Verdict {
    let engine = veridex_core::checks::default_engine().unwrap();
    engine.run(d, content_hash(d), &RunConfig::default())
}

#[test]
fn terminal_report_passes_ansi_through() {
    let d = hostile_dataset();
    let v = verdict_for(&d);
    assert!(!v.findings.is_empty(), "need findings");
    let text = render_terminal(&v, None, 5);
    assert!(
        text.contains('\u{1b}'),
        "no ESC reached the terminal report:\n{text}"
    );
    eprintln!("--- TERMINAL (escaped for display) ---\n{}", text.escape_debug());
}

#[test]
fn html_report_escaping() {
    let evil = "</td><script>alert(1)</script>";
    let d = Dataset {
        id: "x".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(
            7,
            vec![
                stream(evil, "camera", &dense(1_000_000_000)),
                stream("robot", "robot", &dense(1_500_000_000)),
            ],
        )],
    };
    let v = verdict_for(&d);
    let html = render_html(&v, None);
    assert!(
        !html.contains("<script>"),
        "HTML injection reached the report"
    );
}

#[test]
fn sarif_shape() {
    let d = hostile_dataset();
    let v = verdict_for(&d);
    let s = render_sarif(&v);
    let rules: Vec<String> = s["runs"][0]["tool"]["driver"]["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    for r in s["runs"][0]["results"].as_array().unwrap() {
        let id = r["ruleId"].as_str().unwrap();
        assert!(rules.contains(&id.to_string()), "dangling ruleId {id}");
        let lvl = r["level"].as_str().unwrap();
        assert!(
            ["error", "warning", "note", "none"].contains(&lvl),
            "bad level {lvl}"
        );
    }
    eprintln!("SARIF: {}", serde_json::to_string_pretty(&s).unwrap());
}

#[test]
fn prov_and_croissant_ids() {
    let d = Dataset {
        id: "my robot data <2026>".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements: vec![
                ProvenanceElement {
                    key: "annotator".into(),
                    value: Some("Jane Doe & Co".into()),
                    class: ProvenanceClass::Known,
                },
                ProvenanceElement {
                    key: "upstream".into(),
                    value: Some("other set".into()),
                    class: ProvenanceClass::Known,
                },
            ],
        }],
        episodes: vec![episode(0, vec![stream("s", "c", &[0, 1_000_000])])],
    };
    let prov = veridex_core::to_prov(&d);
    eprintln!("PROV: {}", serde_json::to_string_pretty(&prov).unwrap());
    let cr = veridex_core::to_croissant(&d, "abc");
    eprintln!("CROISSANT: {}", serde_json::to_string_pretty(&cr).unwrap());
}

#[test]
fn tolerance_disclosure_precision() {
    let mut t = veridex_core::Tolerances::default();
    t.clock_skew_ns = 500_000; // 0.5 ms
    t.rate_deviation = 0.004; // 0.4%
    t.saturation_fraction = 0.002;
    let d = Dataset {
        id: "x".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![episode(0, vec![stream("s", "c", &[0, 1_000_000])])],
    };
    let engine = veridex_core::checks::default_engine().unwrap();
    let cfg = RunConfig {
        tolerances: t,
        ..Default::default()
    };
    let v = engine.run(&d, content_hash(&d), &cfg);
    let text = render_terminal(&v, None, 5);
    eprintln!("--- {text}");
}

#[test]
fn disabled_checks_and_severity_overrides_are_not_disclosed_by_the_human_renderers() {
    use std::collections::{BTreeMap, BTreeSet};
    let d = hostile_dataset();

    // Baseline: the clock-skew error is found.
    let base = verdict_for(&d);
    assert_eq!(base.status, veridex_core::Status::Fail);

    // Same dataset, with the failing check disabled by veridex.toml.
    let engine = veridex_core::checks::default_engine().unwrap();
    let mut disabled = BTreeSet::new();
    disabled.insert("temporal.clock-skew".to_string());
    let cfg = RunConfig {
        disabled_checks: disabled,
        ..Default::default()
    };
    let v = engine.run(&d, content_hash(&d), &cfg);
    assert_ne!(v.status, veridex_core::Status::Fail);

    let text = render_terminal(&v, None, 5);
    let html = render_html(&v, None);
    let sarif = serde_json::to_string(&render_sarif(&v)).unwrap();
    for (name, s) in [("terminal", &text), ("html", &html), ("sarif", &sarif)] {
        assert!(
            !s.contains("clock-skew") && !s.to_lowercase().contains("disabled"),
            "{name} discloses the disabled check"
        );
    }

    // And a severity override that turns the FAIL into a PASS.
    let mut ov = BTreeMap::new();
    ov.insert(
        "temporal.clock-skew".to_string(),
        veridex_core::check::Severity::Info,
    );
    let cfg2 = RunConfig {
        severity_overrides: ov,
        ..Default::default()
    };
    let v2 = engine.run(&d, content_hash(&d), &cfg2);
    assert_ne!(v2.status, veridex_core::Status::Fail);
    let t2 = render_terminal(&v2, None, 5);
    assert!(
        !t2.to_lowercase().contains("override"),
        "terminal discloses the severity override"
    );
    eprintln!("PASS report with the failing check disabled:\n{text}");
}
