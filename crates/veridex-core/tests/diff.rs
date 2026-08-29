//! Behavior tests for `diff_reports` — the report-comparison logic behind `veridex diff` and its
//! CI regression gate. The CLI drives this over two JSON report files, so the JSON shape handling
//! (enveloped vs. bare, missing scores) is part of the contract.

use serde_json::json;
use veridex_core::{diff_reports, render_diff};

/// An enveloped report: `{ "verdict": { "findings": [...] }, "trust_score": { "score": N } }`.
fn report(findings: serde_json::Value, score: i64) -> serde_json::Value {
    json!({
        "verdict": { "findings": findings },
        "trust_score": { "score": score },
    })
}

fn finding(code: &str) -> serde_json::Value {
    json!({ "code": code, "severity": "error", "message": code })
}

#[test]
fn identical_reports_show_no_change() {
    let r = report(json!([finding("TEMPORAL.CLOCK_SKEW")]), 82);
    let d = diff_reports(&r, &r);
    assert!(d.introduced.is_empty());
    assert!(d.resolved.is_empty());
    assert_eq!(d.unchanged.len(), 1);
    assert_eq!(d.score_delta(), Some(0));
}

#[test]
fn a_new_finding_is_introduced_and_an_old_one_resolved() {
    let old = report(json!([finding("STRUCTURAL.EMPTY_STREAM")]), 90);
    let new = report(json!([finding("TEMPORAL.CLOCK_SKEW")]), 70);
    let d = diff_reports(&old, &new);
    assert_eq!(d.introduced.len(), 1);
    assert_eq!(d.introduced[0]["code"], "TEMPORAL.CLOCK_SKEW");
    assert_eq!(d.resolved.len(), 1);
    assert_eq!(d.resolved[0]["code"], "STRUCTURAL.EMPTY_STREAM");
    // Score dropped 90 -> 70.
    assert_eq!(d.score_delta(), Some(-20));
}

#[test]
fn bare_verdict_form_is_accepted() {
    // A report may be a bare verdict (`{ "findings": [...] }`) with no envelope or score.
    let old = json!({ "findings": [] });
    let new = json!({ "findings": [finding("TEMPORAL.GAP")] });
    let d = diff_reports(&old, &new);
    assert_eq!(d.introduced.len(), 1);
    assert_eq!(d.introduced[0]["code"], "TEMPORAL.GAP");
    // No trust scores present → no delta.
    assert_eq!(d.score_delta(), None);
}

#[test]
fn render_diff_summarizes_scores_and_counts() {
    let old = report(json!([]), 100);
    let new = report(json!([finding("TEMPORAL.CLOCK_SKEW")]), 85);
    let text = render_diff(&diff_reports(&old, &new));
    assert!(text.contains("100 -> 85"), "unexpected: {text}");
    assert!(text.contains("1 introduced"), "unexpected: {text}");
    assert!(text.contains("TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn a_partial_report_swapped_for_a_full_one_is_a_regression_not_a_fix() {
    // A diff assumes the two runs looked at the same thing. When they did not, every comparison it
    // makes is wrong in the flattering direction: substituting a metadata-only report for a full one
    // silences most of the catalog, so the findings the full run reported read as *resolved* and the
    // trust score goes up. A `--fail-on-regression` gate then passes precisely because the new run
    // stopped looking.
    let full = serde_json::json!({
        "verdict": {
            "coverage": { "kind": "full" },
            "findings": [{ "code": "STATISTICAL.RANGE_INVERTED", "severity": "error" }],
        },
        "trust_score": { "score": 70 },
    });
    let partial = serde_json::json!({
        "verdict": { "coverage": { "kind": "metadata_only" }, "findings": [] },
        "trust_score": { "score": 80 },
    });

    let diff = veridex_core::diff_reports(&full, &partial);
    assert_eq!(diff.resolved.len(), 1, "the finding does read as resolved");
    assert_eq!(
        diff.score_delta(),
        Some(10),
        "and the score does read as up"
    );
    assert!(
        diff.coverage_differs(),
        "so the coverage change is what has to carry the truth"
    );
    let rendered = veridex_core::render_diff(&diff);
    assert!(rendered.contains("Coverage: CHANGED"), "{rendered}");

    // Two runs of the same coverage compare normally.
    let same = veridex_core::diff_reports(&full, &full);
    assert!(!same.coverage_differs());
    // And a report predating the coverage field is not evidence of a change.
    let old_style = serde_json::json!({ "verdict": { "findings": [] } });
    assert!(!veridex_core::diff_reports(&old_style, &full).coverage_differs());
}

/// A redacted report substitutes every identifier it quotes, so diffed against its unredacted twin
/// the *same* findings appear once as introduced and once as resolved. Read as a comparison of runs
/// that is nonsense in both directions: a gate fires on a dataset that did not change, and — with
/// the redacted report as the old one — a real regression hides in the noise.
#[test]
fn one_redacted_report_and_one_not_is_a_comparison_of_documents() {
    let plain = serde_json::json!({
        "schema": "veridex.report/1",
        "verdict": { "coverage": { "kind": "full" }, "findings": [
            {"code": "TEMPORAL.CLOCK_SKEW", "severity": "error",
             "message": "streams `/camera/image` and `/joint_states` drift by 210.0 ms"}
        ]},
        "trust_score": { "score": 76 }
    });
    let redacted = serde_json::json!({
        "schema": "veridex.report/1",
        "verdict": { "coverage": { "kind": "full" }, "findings": [
            {"code": "REPORT.REDACTED", "severity": "info", "message": "this report was redacted"},
            {"code": "TEMPORAL.CLOCK_SKEW", "severity": "error",
             "message": "streams `stream#1` and `stream#2` drift by 210.0 ms"}
        ]},
        "trust_score": { "score": 76 }
    });

    let diff = veridex_core::diff_reports(&plain, &redacted);
    assert!(!diff.old_redacted && diff.new_redacted);
    assert!(diff.redaction_differs(), "the mismatch must be detected");
    // The same finding, counted twice — which is exactly why the mismatch has to be said out loud.
    assert_eq!(diff.introduced.len(), 2);
    assert_eq!(diff.resolved.len(), 1);

    let rendered = veridex_core::render_diff(&diff);
    assert!(
        rendered.starts_with("Veridex diff\n  Redaction: CHANGED"),
        "the mismatch leads the report, because it invalidates everything after it: {rendered}"
    );

    let json: serde_json::Value =
        serde_json::from_str(&veridex_core::render_diff_json(&plain, &redacted)).expect("JSON");
    assert_eq!(json["redaction"]["changed"], true);
    assert_eq!(json["redaction"]["new"], true);

    // Two reports redacted the same way compare normally.
    let both = veridex_core::diff_reports(&redacted, &redacted);
    assert!(!both.redaction_differs());
    assert!(both.introduced.is_empty() && both.resolved.is_empty());
}

/// A report as the pipeline writes one: the dataset it was computed over, and the verdict's hash of
/// that dataset's content.
fn report_for(
    dataset: &str,
    cdm_hash: &str,
    findings: serde_json::Value,
    score: i64,
) -> serde_json::Value {
    json!({
        "dataset": { "id": dataset },
        "verdict": { "findings": findings, "cdm_content_hash": cdm_hash },
        "trust_score": { "score": score },
    })
}

/// A diff assumes the two reports are about the same dataset, and nothing enforced it. A CI gate
/// whose baseline artifact path is wrong compares one project's report against another's and gets a
/// confident "resolved, score up" — the one failure mode a regression gate has no other way to
/// notice.
#[test]
fn two_reports_about_different_datasets_are_not_a_comparison() {
    let old = report_for(
        "warehouse_rig",
        "aaaa",
        json!([
            finding("TEMPORAL.CLOCK_SKEW"),
            finding("STRUCTURAL.EMPTY_STREAM")
        ]),
        61,
    );
    let new = report_for("some_other_dataset", "bbbb", json!([]), 98);
    let d = diff_reports(&old, &new);

    assert!(d.dataset_differs());
    assert_eq!(d.old_dataset.as_deref(), Some("warehouse_rig"));
    assert_eq!(d.new_dataset.as_deref(), Some("some_other_dataset"));

    let text = render_diff(&d);
    assert!(
        text.contains("Dataset: DIFFERENT")
            && text.contains("warehouse_rig")
            && text.contains("some_other_dataset"),
        "the mismatch has to lead, because it invalidates every count under it: {text}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&veridex_core::render_diff_json(&old, &new)).expect("json");
    assert_eq!(doc["dataset"]["changed"], json!(true));
    assert_eq!(doc["dataset"]["old"], json!("warehouse_rig"));
}

/// The ordinary case, and the reason identity cannot be the content hash: a dataset that gained an
/// episode since yesterday hashes differently, and diffing those two reports is the whole point.
#[test]
fn the_same_dataset_with_changed_content_is_the_ordinary_diff() {
    let old = report_for(
        "warehouse_rig",
        "aaaa",
        json!([finding("TEMPORAL.GAP")]),
        74,
    );
    let new = report_for("warehouse_rig", "bbbb", json!([]), 88);
    let d = diff_reports(&old, &new);

    assert!(!d.dataset_differs(), "same dataset, later revision");
    assert!(!d.same_content());
    let text = render_diff(&d);
    assert!(
        !text.contains("Dataset:"),
        "nothing to say about identity when it matches: {text}"
    );
    assert_eq!(d.resolved.len(), 1);
}

/// Byte-identical content with a different verdict says something specific and useful: whatever
/// moved, moved in Veridex or its configuration.
#[test]
fn identical_content_says_the_change_was_not_in_the_data() {
    let old = report_for("warehouse_rig", "aaaa", json!([]), 90);
    let new = report_for(
        "warehouse_rig",
        "aaaa",
        json!([finding("STATISTICAL.SATURATED_DIMENSION")]),
        82,
    );
    let d = diff_reports(&old, &new);
    assert!(d.same_content() && !d.dataset_differs());
    let text = render_diff(&d);
    assert!(
        text.contains("identical content") && text.contains("not in the data"),
        "{text}"
    );
}

/// A report that carries no dataset id — a bare verdict, or one written before the field existed —
/// is not evidence of a mismatch, and must not fail a gate.
#[test]
fn a_report_without_a_dataset_id_is_not_a_mismatch() {
    let old = report(json!([finding("TEMPORAL.GAP")]), 74);
    let new = report_for("warehouse_rig", "bbbb", json!([]), 88);
    assert!(!diff_reports(&old, &new).dataset_differs());
    assert!(!diff_reports(&new, &old).dataset_differs());
}

#[test]
fn two_veridex_versions_are_a_comparison_of_catalogs_not_of_data() {
    // A diff attributes what moved to the data. Across versions it cannot: a release that adds a
    // check, adds a finding code, or rewords a message puts findings under `introduced` on a dataset
    // that did not change by a byte — which is exactly what shipping `structural.step-alignment`,
    // `structural.frozen-episode` and `STRUCTURAL.UNCOMPARED_EPISODES` did. The first
    // `--fail-on-regression` run after an upgrade reported "3 finding(s) introduced" and sent
    // someone to audit data that was fine.
    let old = json!({
        "verdict": {
            "veridex_version": "0.1.0",
            "status": "pass",
            "findings": [],
            "coverage": {"kind": "full"},
        },
        "dataset": {"id": "d", "content_hash": "abc"},
        "trust_score": {"score": 90},
    });
    let mut new = old.clone();
    new["verdict"]["veridex_version"] = json!("0.2.0");
    new["verdict"]["findings"] = json!([{
        "code": "STRUCTURAL.UNCOMPARED_EPISODES",
        "severity": "info",
        "message": "this run covers 1 episode(s)",
    }]);

    let diff = diff_reports(&old, &new);
    assert!(diff.version_differs());
    assert_eq!(diff.old_version.as_deref(), Some("0.1.0"));
    assert_eq!(diff.new_version.as_deref(), Some("0.2.0"));

    // The human render names the tool as the cause, ahead of the finding counts.
    let text = veridex_core::render_diff(&diff);
    assert!(
        text.contains("Veridex: CHANGED — 0.1.0 -> 0.2.0"),
        "the cause must be named, not inferred from the finding list:\n{text}"
    );
    // And the machine document carries it, since that is the only consumer a CI gate has.
    let doc: serde_json::Value =
        serde_json::from_str(&veridex_core::render_diff_json(&old, &new)).expect("valid json");
    assert_eq!(doc["veridex_version"]["changed"], json!(true));
    assert_eq!(doc["veridex_version"]["old"], json!("0.1.0"));

    // Same version on both sides is the ordinary case and says nothing.
    let same = diff_reports(&old, &old);
    assert!(!same.version_differs());
    assert!(!veridex_core::render_diff(&same).contains("Veridex: CHANGED"));

    // A report predating the field is not evidence of a change.
    let mut legacy = old.clone();
    legacy["verdict"]
        .as_object_mut()
        .unwrap()
        .remove("veridex_version");
    assert!(!diff_reports(&legacy, &new).version_differs());
}
