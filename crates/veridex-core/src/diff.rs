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
    /// Whether the old report was redacted for sharing (carries `REPORT.REDACTED`).
    pub old_redacted: bool,
    /// See [`ReportDiff::old_redacted`].
    pub new_redacted: bool,
    /// The dataset each report is about, by the id the CDM carries.
    pub old_dataset: Option<String>,
    /// See [`ReportDiff::old_dataset`].
    pub new_dataset: Option<String>,
    /// The CDM content hash each report was computed over, when it records one.
    pub old_cdm_hash: Option<String>,
    /// See [`ReportDiff::old_cdm_hash`].
    pub new_cdm_hash: Option<String>,
    /// The Veridex version each report was produced by, when it records one.
    pub old_version: Option<String>,
    /// See [`ReportDiff::old_version`].
    pub new_version: Option<String>,
    /// Ids of checks that crashed instead of producing findings, in the old report.
    pub old_errored: Vec<String>,
    /// See [`ReportDiff::old_errored`].
    pub new_errored: Vec<String>,
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

    /// Whether the two reports are about **different datasets**.
    ///
    /// A diff assumes the two reports describe the same dataset; nothing about the comparison holds
    /// when they do not. A CI gate whose baseline artifact path is wrong, or one pointed at another
    /// project's report, gets a confident "3 resolved, score +12" and exits 0 — a pass that means
    /// nothing, and the one failure mode a regression gate has no other way to notice.
    ///
    /// Identity is the dataset **id**, not the content hash. The hash differs between *every* pair
    /// of reports worth diffing — a dataset that gained an episode since yesterday is the ordinary
    /// case, and the whole point of the comparison — so a guard on the hash fires on the intended
    /// workflow and stays silent on the mistake. The id survives a revision and differs between
    /// datasets, which is exactly the question being asked.
    pub fn dataset_differs(&self) -> bool {
        match (&self.old_dataset, &self.new_dataset) {
            (Some(o), Some(n)) => o != n,
            // One report predating the field, or a bare verdict, is not evidence of a mismatch.
            _ => false,
        }
    }

    /// Whether both reports were computed over byte-identical dataset content.
    ///
    /// Worth saying when true: any finding that moved did so because Veridex or its configuration
    /// changed, not because the data did.
    pub fn same_content(&self) -> bool {
        match (&self.old_cdm_hash, &self.new_cdm_hash) {
            (Some(o), Some(n)) => o == n,
            _ => false,
        }
    }

    /// Whether exactly one of the two reports was redacted.
    ///
    /// A redacted report substitutes every identifier it quotes, so *every* finding that names a
    /// stream, a task, or a path differs textually from its unredacted twin. Diffed against one
    /// another the same findings appear once under `introduced` and once under `resolved`, the
    /// counts are doubled, and a `--fail-on-regression` gate fires on a dataset that did not
    /// change — or, with the redacted report as the *old* one, a real regression hides inside the
    /// noise. Like a coverage change, this is a statement about the two documents, not the data.
    pub fn redaction_differs(&self) -> bool {
        self.old_redacted != self.new_redacted
    }

    /// Whether the two reports were produced by **different Veridex versions**.
    ///
    /// A diff attributes what moved to the data. Across versions it cannot: a release that adds a
    /// check, adds a finding code to one, or reworded a message produces findings under
    /// `introduced` on a dataset that did not change by a byte. `structural.step-alignment` and
    /// `structural.frozen-episode` each did exactly that, and so did one disclosure that fires on
    /// every single-episode recording — so the first `--fail-on-regression` run after an upgrade
    /// reported "3 finding(s) introduced" and sent someone to audit data that was fine.
    ///
    /// Reported like a coverage or redaction mismatch, and for the same reason: it is a statement
    /// about the two documents rather than about the dataset. The gate still fails — silently
    /// passing a comparison that cannot be made is the worse error, and re-baselining after an
    /// upgrade is a deliberate act — but it fails **by name**, so the reader is told the cause is
    /// the tool rather than left to infer it from a finding list.
    pub fn version_differs(&self) -> bool {
        match (&self.old_version, &self.new_version) {
            (Some(o), Some(n)) => o != n,
            // One report predating the field is not evidence of a change.
            _ => false,
        }
    }

    /// Checks that crashed in the new report and did not in the old one.
    ///
    /// A crash is *cheaper* than the finding it suppresses: an errored check costs 10 points and an
    /// error finding costs 15, so a check that panics instead of reporting its error takes the
    /// score **up** by 5. Every other renderer says a check crashed — the terminal report, the HTML
    /// report and SARIF all do — but the diff, which is the one that gates CI, read the vanished
    /// finding as *resolved* and the higher score as an improvement, and exited 0.
    pub fn newly_errored(&self) -> Vec<&str> {
        self.new_errored
            .iter()
            .filter(|id| !self.old_errored.contains(*id))
            .map(String::as_str)
            .collect()
    }
}

/// Ids of the checks a report records as having crashed.
fn errored_checks(report: &Value) -> Vec<String> {
    report
        .get("verdict")
        .unwrap_or(report)
        .get("errored_checks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    e.get("check_id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
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

/// The dataset id a report is about, if it carries the dataset it was computed over.
fn dataset_id(report: &Value) -> Option<String> {
    report
        .get("dataset")
        .and_then(|d| d.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The CDM content hash a report's verdict was computed over, if it records one.
fn cdm_hash(report: &Value) -> Option<String> {
    report
        .get("verdict")
        .unwrap_or(report)
        .get("cdm_content_hash")
        .and_then(Value::as_str)
        .map(str::to_string)
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
        old_redacted: is_redacted(old),
        new_redacted: is_redacted(new),
        old_coverage: coverage_kind(old),
        new_coverage: coverage_kind(new),
        old_dataset: dataset_id(old),
        new_dataset: dataset_id(new),
        old_cdm_hash: cdm_hash(old),
        new_cdm_hash: cdm_hash(new),
        old_version: veridex_version(old),
        new_version: veridex_version(new),
        old_errored: errored_checks(old),
        new_errored: errored_checks(new),
    }
}

/// The Veridex version a report's verdict records, if it records one.
fn veridex_version(report: &Value) -> Option<String> {
    report
        .get("verdict")
        .and_then(|v| v.get("veridex_version"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Render a diff as a human-readable summary.
/// Whether a report carries the redaction disclosure.
fn is_redacted(report: &Value) -> bool {
    report
        .get("verdict")
        .and_then(|v| v.get("findings"))
        .and_then(Value::as_array)
        .is_some_and(|findings| {
            findings.iter().any(|f| {
                f.get("code").and_then(Value::as_str) == Some(crate::redact::REDACTION_CODE)
            })
        })
}

pub fn render_diff(diff: &ReportDiff) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(out, "Veridex diff");
    // First of all, because it invalidates everything after it more completely than anything else
    // can: two reports about different datasets have nothing to say to each other.
    if diff.dataset_differs() {
        let _ = writeln!(
            out,
            "  Dataset: DIFFERENT — `{}` vs `{}`. These reports are about two different datasets, \
             so every count below is a comparison between unrelated runs.",
            diff.old_dataset.as_deref().unwrap_or("unknown"),
            diff.new_dataset.as_deref().unwrap_or("unknown"),
        );
    } else if diff.same_content() {
        let _ = writeln!(
            out,
            "  Dataset: identical content (same CDM hash), so anything that changed below changed \
             in Veridex or its configuration, not in the data."
        );
    }
    // Stated before anything else, because it invalidates everything after it.
    if diff.redaction_differs() {
        let _ = writeln!(
            out,
            "  Redaction: CHANGED — one of these reports is redacted and the other is not. Every \
             finding that names a stream, a task, or a path differs textually between them, so the \
             counts below are of substitutions, not of changes to the data."
        );
    }
    if diff.coverage_differs() {
        let _ = writeln!(
            out,
            "  Coverage: CHANGED — {} -> {}. The two runs did not look at the same thing, so \
             every comparison below is between unlike reports.",
            diff.old_coverage.as_deref().unwrap_or("unknown"),
            diff.new_coverage.as_deref().unwrap_or("unknown"),
        );
    }
    if diff.version_differs() {
        let _ = writeln!(
            out,
            "  Veridex: CHANGED — {} -> {}. A release that adds a check or a finding code produces \
             findings under `introduced` on a dataset that did not change, so what moved below \
             cannot be attributed to the data. Re-baseline against a report from this version.",
            diff.old_version.as_deref().unwrap_or("unknown"),
            diff.new_version.as_deref().unwrap_or("unknown"),
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
    // A check that crashed in the new run is why some of those "resolved" findings are missing, and
    // it *raised* the score — an errored check costs 10 points, the error finding it suppressed
    // costs 15. Said right under the counts it explains.
    let newly_errored = diff.newly_errored();
    if !newly_errored.is_empty() {
        let _ = writeln!(
            out,
            "  Checks newly crashed: {} — a check that did not run cannot have resolved anything, \
             and costs the score less than the findings it suppressed",
            newly_errored.join(", ")
        );
    }

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
/// `{coverage, introduced, resolved, unchanged_count, score_delta}`. Shared by the CLI's
/// `veridex diff --json` and the Python `veridex.diff` binding, so both emit byte-identical output.
///
/// Coverage leads the document because it qualifies everything after it. Substituting a
/// metadata-only report for a full one silences most of the catalog, so the full run's findings
/// appear under `resolved` and the score goes up -- the terminal render says
/// `Coverage: CHANGED` before anything else, and `--fail-on-regression` treats it as the top
/// regression signal, but this document carried no coverage field at all. A machine consumer, which
/// is the only consumer this document has, read a benign diff across a run that stopped looking.
pub fn render_diff_json(old: &Value, new: &Value) -> String {
    let diff = diff_reports(old, new);
    let doc = serde_json::json!({
        // Leads for the same reason coverage does, one step further out: a diff between reports
        // about two different datasets is not a weaker comparison, it is not a comparison.
        "dataset": {
            "old": diff.old_dataset,
            "new": diff.new_dataset,
            "changed": diff.dataset_differs(),
            "same_content": diff.same_content(),
        },
        "coverage": {
            "old": diff.old_coverage,
            "new": diff.new_coverage,
            "changed": diff.coverage_differs(),
        },
        // Same reason coverage leads: one redacted report and one not makes every identifier-bearing
        // finding look introduced *and* resolved, which is a fact about the documents, not the data.
        "redaction": {
            "old": diff.old_redacted,
            "new": diff.new_redacted,
            "changed": diff.redaction_differs(),
        },
        // And the same reason again, for the cause a reader is least likely to guess: a release
        // that adds a check produces `introduced` findings on a dataset that did not change.
        "veridex_version": {
            "old": diff.old_version,
            "new": diff.new_version,
            "changed": diff.version_differs(),
        },
        "introduced": diff.introduced,
        "resolved": diff.resolved,
        "unchanged_count": diff.unchanged.len(),
        "score_delta": diff.score_delta(),
        // Why findings vanished and the score rose, for the consumer that cannot see the terminal.
        "newly_errored_checks": diff.newly_errored(),
    });
    serde_json::to_string_pretty(&doc).expect("diff serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Substituting a metadata-only report for a full one silences most of the catalog, so the full
    /// run's findings read as *resolved* and the score goes up. The terminal render leads with
    /// `Coverage: CHANGED` and `--fail-on-regression` gates on it, but the JSON document — the only
    /// one a machine consumer reads — carried no coverage field at all.
    #[test]
    fn the_json_diff_carries_the_coverage_the_terminal_render_leads_with() {
        let full = json!({
            "schema": "veridex.report/1",
            "verdict": { "coverage": { "kind": "full" }, "findings": [] },
            "trust_score": { "score": 90 }
        });
        let partial = json!({
            "schema": "veridex.report/1",
            "verdict": { "coverage": { "kind": "metadata_only" }, "findings": [] },
            "trust_score": { "score": 99 }
        });

        let doc: Value = serde_json::from_str(&render_diff_json(&full, &partial)).unwrap();
        assert_eq!(doc["coverage"]["old"], "full");
        assert_eq!(doc["coverage"]["new"], "metadata_only");
        assert_eq!(doc["coverage"]["changed"], true);
    }

    /// Two full runs report no coverage change, so the field cannot be read as noise.
    #[test]
    fn an_unchanged_coverage_is_reported_as_unchanged() {
        let full = json!({
            "schema": "veridex.report/1",
            "verdict": { "coverage": { "kind": "full" }, "findings": [] },
            "trust_score": { "score": 90 }
        });
        let doc: Value = serde_json::from_str(&render_diff_json(&full, &full)).unwrap();
        assert_eq!(doc["coverage"]["changed"], false);
    }

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
