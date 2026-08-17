//! Verdict diffing: compare two Veridex reports for the same dataset lineage and report what
//! changed — findings introduced, resolved, or unchanged, and how the trust score moved.
//!
//! Operates on the report JSON (`veridex.report/1`) so it works on any two saved reports without
//! re-running validation. A finding is "the same" when its full JSON object is equal.

use serde_json::Value;

/// The result of diffing two reports.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportDiff {
    /// Findings present in the new report but not the old (regressions/new issues).
    pub introduced: Vec<Value>,
    /// Findings present in the old report but not the new (resolved).
    pub resolved: Vec<Value>,
    /// Findings present in both.
    pub unchanged: Vec<Value>,
    /// The old trust score, if present.
    pub old_score: Option<i64>,
    /// The new trust score, if present.
    pub new_score: Option<i64>,
    /// The two reports' coverage kinds (`full`, `sample`, `metadata_only`), when recorded.
    pub old_coverage: Option<String>,
    /// See [`ReportDiff::old_coverage`].
    pub new_coverage: Option<String>,
}

impl ReportDiff {
    /// `new_score - old_score`, when both are present.
    pub fn score_delta(&self) -> Option<i64> {
        match (self.old_score, self.new_score) {
            (Some(o), Some(n)) => Some(n - o),
            _ => None,
        }
    }

    /// Whether the two reports cover different amounts of their dataset.
    ///
    /// A diff assumes the two runs looked at the same thing. When they did not, every comparison it
    /// makes is meaningless in the flattering direction: substituting a metadata-only or sampled
    /// report for a full one silences most of the catalog, and the result reads as findings
    /// *resolved* and a trust score that went up. Callers gating on a diff must treat this as a
    /// regression, not an improvement.
    pub fn coverage_differs(&self) -> bool {
        match (&self.old_coverage, &self.new_coverage) {
            (Some(o), Some(n)) => o != n,
            // One report predating the coverage field is not evidence of a change.
            _ => false,
        }
    }
}

/// The coverage kind a report records (`full`, `sample`, `metadata_only`), if it carries one.
fn coverage_kind(report: &Value) -> Option<String> {
    report
        .get("verdict")
        .unwrap_or(report)
        .get("coverage")
        .and_then(|c| c.get("kind"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Whether a JSON value is shaped like a Veridex report at all: it must carry a findings array,
/// either in a full envelope (`{"verdict": {"findings": [...]}}`) or as a bare verdict
/// (`{"findings": [...]}`).
///
/// Absence is not emptiness. Without this, a truncated or wrong-shaped artifact — an empty `{}`, or a
/// SARIF file handed over by mistake — read as "no findings", so a CI step gating on
/// `--fail-on-regression` saw every prior finding as resolved and passed.
pub fn is_report_shaped(report: &Value) -> bool {
    report
        .get("verdict")
        .and_then(|v| v.get("findings"))
        .or_else(|| report.get("findings"))
        .is_some_and(Value::is_array)
}

/// Findings array from a report, tolerant of either a full report envelope
/// (`{"verdict": {"findings": [...]}}`) or a bare verdict (`{"findings": [...]}`).
fn findings(report: &Value) -> Vec<Value> {
    let arr = report
        .get("verdict")
        .and_then(|v| v.get("findings"))
        .or_else(|| report.get("findings"))
        .and_then(Value::as_array);
    arr.cloned().unwrap_or_default()
}

/// Trust score from a report envelope, if present.
fn trust_score(report: &Value) -> Option<i64> {
    report
        .get("trust_score")
        .and_then(|t| t.get("score"))
        .and_then(Value::as_i64)
}

/// Diff two reports.
pub fn diff_reports(old: &Value, new: &Value) -> ReportDiff {
    let old_findings = findings(old);
    let new_findings = findings(new);

    let introduced: Vec<Value> = new_findings
        .iter()
        .filter(|f| !old_findings.contains(f))
        .cloned()
        .collect();
    let resolved: Vec<Value> = old_findings
        .iter()
        .filter(|f| !new_findings.contains(f))
        .cloned()
        .collect();
    let unchanged: Vec<Value> = new_findings
        .iter()
        .filter(|f| old_findings.contains(f))
        .cloned()
        .collect();

    ReportDiff {
        introduced,
        resolved,
        unchanged,
        old_score: trust_score(old),
        new_score: trust_score(new),
        old_coverage: coverage_kind(old),
        new_coverage: coverage_kind(new),
    }
}

/// Render a diff as a human-readable summary.
pub fn render_diff(diff: &ReportDiff) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(out, "Veridex diff");
    // Stated before anything else, because it invalidates everything after it.
    if diff.coverage_differs() {
        let _ = writeln!(
            out,
            "  Coverage: CHANGED — {} -> {}. The two runs did not look at the same thing, so \
             every comparison below is between unlike reports.",
            diff.old_coverage.as_deref().unwrap_or("unknown"),
            diff.new_coverage.as_deref().unwrap_or("unknown"),
        );
    }
    if let (Some(o), Some(n)) = (diff.old_score, diff.new_score) {
        let delta = n - o;
        let sign = if delta > 0 { "+" } else { "" };
        let _ = writeln!(out, "  Trust score: {o} -> {n} ({sign}{delta})");
    }
    let _ = writeln!(
        out,
        "  Findings: {} introduced · {} resolved · {} unchanged",
        diff.introduced.len(),
        diff.resolved.len(),
        diff.unchanged.len()
    );

    let mut section = |title: &str, items: &[Value]| {
        if items.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n{title}:");
        for f in items {
            let code = f.get("code").and_then(Value::as_str).unwrap_or("?");
            let sev = f.get("severity").and_then(Value::as_str).unwrap_or("?");
            let msg = f.get("message").and_then(Value::as_str).unwrap_or("");
            let _ = writeln!(out, "  [{sev}] {code}  {msg}");
        }
    };
    section("Introduced", &diff.introduced);
    section("Resolved", &diff.resolved);

    out
}

/// Diff two report JSON values and render the machine-readable summary as a pretty JSON string:
/// `{introduced, resolved, unchanged_count, score_delta}`. Shared by the CLI's `veridex diff --json`
/// and the Python `veridex.diff` binding, so both emit byte-identical output.
pub fn render_diff_json(old: &Value, new: &Value) -> String {
    let diff = diff_reports(old, new);
    let doc = serde_json::json!({
        "introduced": diff.introduced,
        "resolved": diff.resolved,
        "unchanged_count": diff.unchanged.len(),
        "score_delta": diff.score_delta(),
    });
    serde_json::to_string_pretty(&doc).expect("diff serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(findings: Value, score: i64) -> Value {
        json!({
            "schema": "veridex.report/1",
            "verdict": { "findings": findings },
            "trust_score": { "score": score }
        })
    }

    fn finding(code: &str, sev: &str) -> Value {
        json!({ "check_id": "c", "code": code, "severity": sev, "message": "m", "location": {"kind":"dataset"} })
    }

    #[test]
    fn classifies_introduced_resolved_unchanged() {
        let old = report(json!([finding("A", "error"), finding("B", "warning")]), 60);
        let new = report(json!([finding("B", "warning"), finding("C", "info")]), 75);

        let d = diff_reports(&old, &new);
        assert_eq!(d.introduced, vec![finding("C", "info")]);
        assert_eq!(d.resolved, vec![finding("A", "error")]);
        assert_eq!(d.unchanged, vec![finding("B", "warning")]);
        assert_eq!(d.score_delta(), Some(15));
    }

    #[test]
    fn render_diff_json_carries_the_summary_fields() {
        let old = report(json!([finding("A", "error")]), 60);
        let new = report(json!([finding("A", "error"), finding("C", "info")]), 55);
        let doc: Value = serde_json::from_str(&render_diff_json(&old, &new)).unwrap();
        assert_eq!(doc["introduced"], json!([finding("C", "info")]));
        assert_eq!(doc["resolved"], json!([]));
        assert_eq!(doc["unchanged_count"], 1);
        assert_eq!(doc["score_delta"], -5);
    }

    #[test]
    fn a_changed_severity_shows_as_resolved_plus_introduced() {
        let old = report(json!([finding("A", "warning")]), 80);
        let new = report(json!([finding("A", "error")]), 65);
        let d = diff_reports(&old, &new);
        assert_eq!(d.resolved, vec![finding("A", "warning")]);
        assert_eq!(d.introduced, vec![finding("A", "error")]);
        assert_eq!(d.score_delta(), Some(-15));
    }

    #[test]
    fn identical_reports_have_no_changes() {
        let r = report(json!([finding("A", "error")]), 50);
        let d = diff_reports(&r, &r);
        assert!(d.introduced.is_empty());
        assert!(d.resolved.is_empty());
        assert_eq!(d.unchanged.len(), 1);
        assert_eq!(d.score_delta(), Some(0));
    }

    #[test]
    fn tolerates_bare_verdict_without_envelope() {
        let old = json!({ "findings": [finding("A", "error")] });
        let new = json!({ "findings": [] });
        let d = diff_reports(&old, &new);
        assert_eq!(d.resolved.len(), 1);
        assert_eq!(d.score_delta(), None);
    }
}
