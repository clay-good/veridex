//! Reporting: turn a [`Verdict`] (and optional [`TrustScore`]) into output humans read and machines
//! consume. Both renderers derive from the same verdict, so they never disagree.
//!
//! MVP surface: a human-readable terminal report with per-episode rollups (worst episodes first)
//! and a versioned JSON envelope. HTML, SARIF, and diffing are later changes.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

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
