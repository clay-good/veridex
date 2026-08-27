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
        latched: None,
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

// ---------------------------------------------------------------------------
// Rollups: the summaries triage reads before it reads a finding.
// ---------------------------------------------------------------------------

/// A verdict carrying findings across two categories, two episodes, and two streams.
fn verdict_with_spread() -> veridex_core::Verdict {
    use veridex_core::check::{Category, Finding, Location, Severity};
    let dataset = veridex_core::cdm::Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![],
    };
    let engine = veridex_core::Engine::builder().build();
    let mut verdict = engine.run(
        &dataset,
        veridex_core::content_hash(&dataset),
        &veridex_core::RunConfig::default(),
    );
    let at = |episode: u64, stream: &str| Location::Stream {
        episode,
        stream: stream.into(),
    };
    verdict.findings.push(Finding::new(
        "temporal.clock-skew",
        Category::Temporal,
        Severity::Error,
        at(0, "camera"),
        "TEMPORAL.CLOCK_SKEW",
        "drift",
    ));
    verdict.findings.push(Finding::new(
        "temporal.gaps",
        Category::Temporal,
        Severity::Warning,
        at(1, "camera"),
        "TEMPORAL.GAP",
        "gap",
    ));
    verdict.findings.push(Finding::new(
        "statistical.saturation",
        Category::Statistical,
        Severity::Warning,
        at(1, "arm"),
        "STATISTICAL.SATURATED",
        "pinned",
    ));
    verdict.findings.push(Finding::new(
        "structural.duplicate-episode",
        Category::Structural,
        Severity::Info,
        Location::Dataset,
        "STRUCTURAL.DUPLICATE_EPISODE",
        "dataset-scope",
    ));
    verdict.counts.error += 1;
    verdict.counts.warning += 2;
    verdict.counts.info += 1;
    verdict
}

#[test]
fn findings_roll_up_by_category_episode_and_stream() {
    let summary = veridex_core::rollups(&verdict_with_spread());

    assert_eq!(summary.by_category["temporal"].error, 1);
    assert_eq!(summary.by_category["temporal"].warning, 1);
    assert_eq!(summary.by_category["statistical"].total(), 1);
    assert!(
        !summary.by_category.contains_key("video"),
        "a family with no findings costs no noise"
    );

    // Episodes rank worst-first: episode 0 has the only error.
    assert_eq!(summary.by_episode[0].episode, 0);
    assert_eq!(summary.by_episode[0].counts.error, 1);

    // The stream rollup aggregates a sensor across every episode it appears in — the question a
    // per-episode ranking cannot answer.
    assert_eq!(summary.by_stream[0].stream, "camera");
    assert_eq!(summary.by_stream[0].episodes, 2);
    assert_eq!(summary.by_stream[0].counts.total(), 2);
    assert_eq!(summary.by_stream[1].stream, "arm");
    // A dataset-scope finding names no stream and is not attributed to one.
    assert_eq!(summary.by_stream.len(), 2);
}

#[test]
fn the_machine_readable_report_carries_the_summaries_the_terminal_prints() {
    // A CI job is the only consumer `--json` has, and it had to re-derive every summary the human
    // report was handed.
    let verdict = verdict_with_spread();
    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::render_json(&verdict, None)).expect("valid JSON");
    assert_eq!(json["rollups"]["by_category"]["temporal"]["error"], 1);
    assert_eq!(json["rollups"]["by_episode"][0]["episode"], 0);
    assert_eq!(json["rollups"]["by_stream"][0]["stream"], "camera");
    assert_eq!(json["rollups"]["by_stream"][0]["episodes"], 2);

    // And the terminal report shows the same numbers, from the same function.
    let terminal = veridex_core::render_terminal(&verdict, None, 10);
    assert!(terminal.contains("By category:"), "{terminal}");
    assert!(terminal.contains("Worst streams:"), "{terminal}");
    assert!(terminal.contains("`camera`"), "{terminal}");
}

#[test]
fn a_compact_report_keeps_every_finding_and_drops_only_info_guidance() {
    // A sound dataset's report is mostly `info` — what could not be measured, what provenance is
    // absent — and printing every risk/remedy paragraph buries the two lines that say whether the
    // data is usable. Compact must lose no *finding*, only the guidance on the informational ones.
    use veridex_core::check::{Category, Finding, Location, Severity};
    let dataset = veridex_core::cdm::Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![],
    };
    let engine = veridex_core::Engine::builder().build();
    let mut verdict = engine.run(
        &dataset,
        veridex_core::content_hash(&dataset),
        &veridex_core::RunConfig::default(),
    );
    verdict.findings.push(
        Finding::new(
            "provenance.completeness",
            Category::Provenance,
            Severity::Info,
            Location::Dataset,
            "PROVENANCE.MISSING_CLOCK",
            "provenance is missing `clock`",
        )
        .with_risk("an informational risk nobody needs four times")
        .with_remedy("an informational remedy"),
    );
    verdict.findings.push(
        Finding::new(
            "temporal.clock-skew",
            Category::Temporal,
            Severity::Error,
            Location::Episode { episode: 0 },
            "TEMPORAL.CLOCK_SKEW",
            "streams drift by 210.0 ms",
        )
        .with_risk("the risk that matters")
        .with_remedy("the remedy that matters"),
    );
    verdict.counts.info += 1;
    verdict.counts.error += 1;

    let compact =
        veridex_core::render_terminal_with(&verdict, None, 5, veridex_core::FindingDetail::Compact);
    // Every finding is still named, with its message.
    assert!(compact.contains("PROVENANCE.MISSING_CLOCK"), "{compact}");
    assert!(
        compact.contains("provenance is missing `clock`"),
        "{compact}"
    );
    assert!(compact.contains("TEMPORAL.CLOCK_SKEW"), "{compact}");
    // The error keeps its guidance; the info finding's is dropped, and the report says so.
    assert!(compact.contains("the risk that matters"), "{compact}");
    assert!(
        !compact.contains("an informational risk"),
        "info guidance must be dropped: {compact}"
    );
    assert!(
        compact.contains("1 info finding(s) printed without their risk and remedy"),
        "the omission must be disclosed: {compact}"
    );

    // Full prints everything, and is what `render_terminal` still does.
    let full =
        veridex_core::render_terminal_with(&verdict, None, 5, veridex_core::FindingDetail::Full);
    assert!(full.contains("an informational risk"), "{full}");
    assert!(!full.contains("printed without their risk"), "{full}");
    assert_eq!(full, veridex_core::render_terminal(&verdict, None, 5));
}

/// The SARIF invariants a consumer actually depends on, rather than the two fields the test above
/// happens to name.
///
/// GitHub code scanning resolves every result's `ruleId` against the rules the driver declares, so a
/// result naming a rule that is not there is a defect that no amount of valid JSON hides — and this
/// tree emits rule ids that belong to no registered check (`REPORT.REDACTED`, `SCOPE.NARROWED`,
/// `VERIDEX.CHECK_ERRORED`), which is precisely where such a dangle would appear.
#[test]
fn every_sarif_result_resolves_to_a_declared_rule() {
    use veridex_core::check::{Category, Finding, Location, Severity};
    let d = skewed_dataset();
    let mut v = verdict_for(&d);
    // A finding from outside the check catalog, as `--redact` and the engine's own disclosures are.
    v.findings.push(Finding::new(
        "report.redaction",
        Category::Structural,
        Severity::Info,
        Location::Dataset,
        "REPORT.REDACTED",
        "this report was redacted for sharing",
    ));
    v.counts.info += 1;

    let sarif = veridex_core::render_sarif(&v);
    let run = &sarif["runs"][0];
    let declared: std::collections::BTreeSet<&str> = run["tool"]["driver"]["rules"]
        .as_array()
        .expect("rules")
        .iter()
        .map(|r| r["id"].as_str().expect("rule id"))
        .collect();

    for result in run["results"].as_array().expect("results") {
        let rule = result["ruleId"]
            .as_str()
            .expect("every result names a rule");
        assert!(
            declared.contains(rule),
            "result names `{rule}`, which the driver does not declare: {declared:?}"
        );
        // The vocabulary SARIF defines; anything else is rejected outright by a consumer.
        let level = result["level"].as_str().expect("every result has a level");
        assert!(
            ["none", "note", "warning", "error"].contains(&level),
            "unexpected level `{level}`"
        );
        assert!(
            !result["message"]["text"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "every result carries a message"
        );
        assert!(
            result["locations"]
                .as_array()
                .is_some_and(|l| !l.is_empty()),
            "every result carries a location"
        );
    }
    assert_eq!(sarif["version"], "2.1.0");
}

/// A hostile identifier cannot script the HTML report.
///
/// The HTML report is the one output built to be *shared* — attached to a ticket, committed to a
/// repo, mailed to a customer — and every string in it comes from a dataset Veridex did not write.
/// A stream named `<script>…` in a report opened in a browser is a stored cross-site scripting
/// payload delivered by the tool that was supposed to be checking the data. Nothing guarded this.
#[test]
fn a_hostile_name_cannot_script_the_shared_html_report() {
    use veridex_core::check::{Category, Finding, Location, Severity};
    let payload = "<script>alert('xss')</script>";
    let dataset = veridex_core::cdm::Dataset {
        id: format!("acme{payload}"),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: vec![],
    };
    let engine = veridex_core::Engine::builder().build();
    let mut verdict = engine.run(
        &dataset,
        veridex_core::content_hash(&dataset),
        &veridex_core::RunConfig::default(),
    );
    // Every string a finding carries, each of them dataset-controlled.
    verdict.findings.push(
        Finding::new(
            "temporal.clock-skew",
            Category::Temporal,
            Severity::Error,
            Location::Stream {
                episode: 0,
                stream: format!("camera{payload}"),
            },
            "TEMPORAL.CLOCK_SKEW",
            format!("message {payload}"),
        )
        .with_risk(format!("risk {payload}"))
        .with_remedy(format!("remedy {payload}")),
    );
    verdict.counts.error += 1;

    let html = veridex_core::render_html(&verdict, None);
    assert!(
        !html.contains("<script>"),
        "the report must not carry an executable script tag from its input"
    );
    // The text is still there — escaped, not dropped, or the report would be lying about the data.
    assert!(
        html.contains("&lt;script&gt;"),
        "the payload must be shown escaped"
    );
    assert!(
        html.contains("camera&lt;script&gt;"),
        "including in the rollups"
    );
}
