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
