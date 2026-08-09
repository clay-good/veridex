//! Reporting: turn a [`Verdict`] (and optional [`TrustScore`]) into output humans read and machines
//! consume. Both renderers derive from the same verdict, so they never disagree.
//!
//! Surface: a human-readable terminal report with per-episode rollups (worst episodes first), a
//! versioned JSON envelope, and SARIF 2.1.0 for CI code-scanning. HTML is a later change.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{json, Value};

use crate::certificate::TrustScore;
use crate::check::{Location, Severity};
use crate::engine::{Status, Verdict};

/// The versioned JSON report schema id. Stable and additive within a major version.
pub const REPORT_SCHEMA_VERSION: &str = "veridex.report/1";

/// The machine-readable JSON report envelope.
#[derive(Debug, Clone, Serialize)]
pub struct JsonReport<'a> {
    /// Schema identifier, e.g. `veridex.report/1`.
    pub schema: &'static str,
    /// The full verdict.
    pub verdict: &'a Verdict,
    /// The trust score, when the report is produced alongside scoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_score: Option<TrustScore>,
}

/// Render the verdict as stable, versioned JSON.
pub fn render_json(verdict: &Verdict, trust_score: Option<TrustScore>) -> String {
    let report = JsonReport {
        schema: REPORT_SCHEMA_VERSION,
        verdict,
        trust_score,
    };
    // Pretty JSON is deterministic here: struct field order is fixed and the verdict's collections
    // are already stably ordered.
    serde_json::to_string_pretty(&report).expect("report serializes")
}

/// Per-episode finding rollup.
struct EpisodeRollup {
    episode: u64,
    errors: u32,
    warnings: u32,
    info: u32,
}

impl EpisodeRollup {
    fn total(&self) -> u32 {
        self.errors + self.warnings + self.info
    }
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pass => "PASS",
        Status::PassWithWarnings => "PASS (warnings)",
        Status::Fail => "FAIL",
    }
}

fn severity_label(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn location_label(loc: &Location) -> String {
    match loc {
        Location::Dataset => "dataset".to_string(),
        Location::Episode { episode } => format!("episode {episode}"),
        Location::Stream { episode, stream } => format!("episode {episode} · stream `{stream}`"),
        Location::FrameRange {
            episode,
            stream,
            start_frame,
            end_frame,
        } => format!("episode {episode} · stream `{stream}` · frames {start_frame}..{end_frame}"),
        Location::TimeRange {
            episode,
            stream,
            start_ts,
            end_ts,
        } => format!("episode {episode} · stream `{stream}` · ts {start_ts}..{end_ts}"),
    }
}

/// Rank episodes worst-first: more errors, then warnings, then info, then lower index for stability.
fn worst_episodes(verdict: &Verdict) -> Vec<EpisodeRollup> {
    let mut by_ep: BTreeMap<u64, EpisodeRollup> = BTreeMap::new();
    for f in &verdict.findings {
        let Some(ep) = f.location.episode() else {
            continue;
        };
        let entry = by_ep.entry(ep).or_insert(EpisodeRollup {
            episode: ep,
            errors: 0,
            warnings: 0,
            info: 0,
        });
        match f.severity {
            Severity::Error => entry.errors += 1,
            Severity::Warning => entry.warnings += 1,
            Severity::Info => entry.info += 1,
        }
    }
    let mut rollups: Vec<EpisodeRollup> = by_ep.into_values().collect();
    rollups.sort_by(|a, b| {
        b.errors
            .cmp(&a.errors)
            .then_with(|| b.warnings.cmp(&a.warnings))
            .then_with(|| b.info.cmp(&a.info))
            .then_with(|| a.episode.cmp(&b.episode))
    });
    rollups
}

/// Render a human-readable terminal report. `max_episodes` bounds the worst-episodes rollup.
pub fn render_terminal(
    verdict: &Verdict,
    trust_score: Option<TrustScore>,
    max_episodes: usize,
) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "Veridex report");
    let _ = writeln!(out, "  CDM hash: {}", verdict.cdm_content_hash);
    let _ = write!(out, "  Status:   {}", status_label(verdict.status));
    if let Some(ts) = trust_score {
        let _ = write!(
            out,
            "   Trust: {} ({})  [data {} · provenance {}%]",
            ts.score,
            ts.grade.letter(),
            ts.data_score,
            ts.provenance_pct
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "  Findings: {} error · {} warning · {} info",
        verdict.counts.error, verdict.counts.warning, verdict.counts.info
    );

    // Worst-episodes rollup.
    let rollups = worst_episodes(verdict);
    if !rollups.is_empty() {
        let _ = writeln!(out, "\nWorst episodes:");
        for r in rollups.iter().take(max_episodes) {
            let _ = writeln!(
                out,
                "  episode {} — {} error, {} warning, {} info ({} total)",
                r.episode,
                r.errors,
                r.warnings,
                r.info,
                r.total()
            );
        }
    }

    // Findings (already stably ordered in the verdict).
    if !verdict.findings.is_empty() {
        let _ = writeln!(out, "\nFindings:");
        for f in &verdict.findings {
            let _ = writeln!(
                out,
                "  [{}] {}  {}",
                severity_label(f.severity),
                f.code,
                location_label(&f.location)
            );
            let _ = writeln!(out, "      {}", f.message);
            if !f.risk.is_empty() {
                let _ = writeln!(out, "      risk:   {}", f.risk);
            }
            if !f.remedy.is_empty() {
                let _ = writeln!(out, "      remedy: {}", f.remedy);
            }
        }
    }

    // Errored checks, listed separately from data findings.
    if !verdict.errored_checks.is_empty() {
        let _ = writeln!(out, "\nErrored checks (failed to run):");
        for e in &verdict.errored_checks {
            let _ = writeln!(out, "  {} (v{}): {}", e.check_id, e.version, e.message);
        }
    }

    out
}

/// Minimal HTML-escaping for text embedded in the report.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render the verdict as a self-contained HTML report (inline CSS, no external assets), suitable for
/// sharing or archiving. Derives from the same verdict as every other renderer.
pub fn render_html(verdict: &Verdict, trust_score: Option<TrustScore>) -> String {
    let mut body = String::new();

    let _ = write!(
        body,
        "<h1>Veridex report</h1><p class=\"meta\">CDM hash: <code>{}</code></p>",
        esc(&verdict.cdm_content_hash)
    );

    let status_class = match verdict.status {
        Status::Pass => "pass",
        Status::PassWithWarnings => "warn",
        Status::Fail => "fail",
    };
    let _ = write!(
        body,
        "<p class=\"status {status_class}\">{}</p>",
        status_label(verdict.status)
    );
    if let Some(ts) = &trust_score {
        let _ = write!(
            body,
            "<p class=\"score\">Trust {} ({}) — data {} · provenance {}%</p>",
            ts.score,
            ts.grade.letter(),
            ts.data_score,
            ts.provenance_pct
        );
    }
    let _ = write!(
        body,
        "<p>{} error · {} warning · {} info</p>",
        verdict.counts.error, verdict.counts.warning, verdict.counts.info
    );

    let rollups = worst_episodes(verdict);
    if !rollups.is_empty() {
        body.push_str("<h2>Worst episodes</h2><ul>");
        for r in rollups.iter().take(10) {
            let _ = write!(
                body,
                "<li>episode {} — {} error, {} warning, {} info</li>",
                r.episode, r.errors, r.warnings, r.info
            );
        }
        body.push_str("</ul>");
    }

    if verdict.findings.is_empty() {
        body.push_str("<h2>Findings</h2><p>No findings.</p>");
    } else {
        body.push_str(
            "<h2>Findings</h2><table><thead><tr><th>Severity</th><th>Code</th><th>Location</th>\
             <th>Message</th><th>Risk</th><th>Remedy</th></tr></thead><tbody>",
        );
        for f in &verdict.findings {
            let _ = write!(
                body,
                "<tr class=\"{}\"><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                severity_label(f.severity),
                severity_label(f.severity),
                esc(&f.code),
                esc(&location_label(&f.location)),
                esc(&f.message),
                esc(&f.risk),
                esc(&f.remedy),
            );
        }
        body.push_str("</tbody></table>");
    }

    const STYLE: &str = "body{font-family:system-ui,sans-serif;max-width:60rem;margin:2rem auto;\
        padding:0 1rem;color:#1a1a1a}code{background:#f2f2f2;padding:.1em .3em;border-radius:3px}\
        .status{font-weight:700;display:inline-block;padding:.2em .6em;border-radius:4px;color:#fff}\
        .pass{background:#1a7f37}.warn{background:#9a6700}.fail{background:#cf222e}\
        table{border-collapse:collapse;width:100%}th,td{border:1px solid #ddd;padding:.4em .6em;\
        text-align:left;vertical-align:top;font-size:.9rem}tr.error td:first-child{color:#cf222e;\
        font-weight:700}tr.warning td:first-child{color:#9a6700}th{background:#f6f8fa}";

    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Veridex report</title><style>{STYLE}</style></head><body>{body}</body></html>"
    )
}

/// SARIF severity level for a finding.
fn sarif_level(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

/// Render the verdict as [SARIF 2.1.0](https://sarifweb.azurewebsites.net/) for CI code-scanning
/// (e.g. GitHub code scanning). Rules are the distinct finding codes; results carry the message and
/// a logical location (dataset / episode / stream), since Veridex findings are not file positions.
pub fn render_sarif(verdict: &Verdict) -> Value {
    // Distinct rule ids (finding codes), sorted for determinism.
    let mut rule_ids: Vec<&str> = verdict.findings.iter().map(|f| f.code.as_str()).collect();
    rule_ids.sort_unstable();
    rule_ids.dedup();
    // Enrich each rule with a description (the risk of a representative finding) and a link to the
    // check catalog, so GitHub code scanning shows what each rule means rather than a bare id.
    let rules: Vec<Value> = rule_ids
        .iter()
        .map(|id| {
            let risk = verdict
                .findings
                .iter()
                .find(|f| f.code == *id)
                .map(|f| f.risk.as_str())
                .unwrap_or("");
            json!({
                "id": id,
                "name": id,
                "shortDescription": { "text": id },
                "fullDescription": { "text": risk },
                "helpUri": "https://github.com/clay-good/veridex/blob/main/docs/checks.md"
            })
        })
        .collect();

    let results: Vec<Value> = verdict
        .findings
        .iter()
        .map(|f| {
            json!({
                "ruleId": f.code,
                "level": sarif_level(f.severity),
                "message": { "text": f.message },
                "locations": [{
                    "logicalLocations": [{ "name": location_label(&f.location) }]
                }],
                "properties": {
                    "checkId": f.check_id,
                    "category": category_tag(f),
                    "risk": f.risk,
                    "remedy": f.remedy
                }
            })
        })
        .collect();

    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "Veridex",
                    "informationUri": "https://github.com/clay-good/veridex",
                    "version": crate::VERSION,
                    "rules": rules
                }
            },
            "results": results
        }]
    })
}

/// The finding's category tag (kept local to avoid leaking a formatting helper).
fn category_tag(f: &crate::check::Finding) -> &'static str {
    use crate::check::Category::*;
    match f.category {
        Structural => "structural",
        Temporal => "temporal",
        Statistical => "statistical",
        Semantic => "semantic",
        Video => "video",
        Provenance => "provenance",
    }
}
