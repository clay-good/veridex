//! Reporting: turn a [`Verdict`] (and optional [`TrustScore`]) into output humans read and machines
//! consume. Both renderers derive from the same verdict, so they never disagree.
//!
//! Surface: a human-readable terminal report with per-episode rollups (worst episodes first), a
//! versioned JSON envelope, SARIF 2.1.0 for CI code-scanning, a self-contained HTML report, and the
//! machine-readable check catalog (shared with the CLI and Python bindings).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{json, Value};

use crate::certificate::TrustScore;
use crate::check::{Location, Severity};
use crate::engine::{CheckInfo, Status, Verdict};

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
    /// The per-criterion readiness verdict, when the run named a policy profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<&'a crate::certificate::ReadinessReport>,
}

/// Render the verdict as stable, versioned JSON.
pub fn render_json(verdict: &Verdict, trust_score: Option<TrustScore>) -> String {
    render_json_with_readiness(verdict, trust_score, None)
}

/// As [`render_json`], plus the per-criterion readiness verdict when a profile was named.
///
/// A profile is what a run is judged against, and its criterion results reached only the terminal
/// report — so the consumer most likely to be gating on them, a CI job reading `--json`, could see
/// the profile's tolerances applied and no verdict about them. The block is the same one `certify`
/// signs; here it is simply unsigned. Absent when no profile was named, so an ordinary report's
/// bytes are unchanged.
pub fn render_json_with_readiness(
    verdict: &Verdict,
    trust_score: Option<TrustScore>,
    readiness: Option<&crate::certificate::ReadinessReport>,
) -> String {
    let report = JsonReport {
        schema: REPORT_SCHEMA_VERSION,
        verdict,
        trust_score,
        readiness,
    };
    // Pretty JSON is deterministic here: struct field order is fixed and the verdict's collections
    // are already stably ordered.
    serde_json::to_string_pretty(&report).expect("report serializes")
}

/// Render a check catalog (from [`Engine::catalog`](crate::Engine::catalog)) as stable, pretty JSON.
/// The CLI's `veridex checks --json` and the Python `veridex.catalog()` binding both call this, so
/// the machine-readable catalog is byte-identical across surfaces.
pub fn render_catalog_json(catalog: &[CheckInfo]) -> String {
    serde_json::to_string_pretty(catalog).expect("catalog serializes")
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

/// Escape control characters for terminal output, keeping the text visible.
///
/// Every string a finding carries can originate in the dataset — a stream name copied verbatim from
/// `info.json`, a directory name, a license string — and the terminal report writes them to a TTY
/// that interprets escape sequences. A stream named with a `\x1b[2J\x1b[1;1H` prefix clears the
/// screen and repaints its own text over the real report, so an untrusted dataset could print a
/// forged `Status: PASS` banner over its own failing verdict. A bare newline is milder but still
/// breaks the report's line structure into something that reads as separate findings.
///
/// So control characters are rendered as visible `\xNN` / `\u{..}` escapes rather than executed. The
/// text stays readable and is obviously not doing anything. Printable characters, including every
/// non-ASCII one, pass through untouched — this is not an ASCII filter.
fn tty_safe(s: &str) -> String {
    if !s.contains(|c: char| c.is_control()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            // Tab is the one control character a report line can carry harmlessly.
            '\t' => out.push('\t'),
            c if c.is_control() => {
                let n = c as u32;
                if n <= 0xff {
                    out.push_str(&format!("\\x{n:02x}"));
                } else {
                    out.push_str(&format!("\\u{{{n:x}}}"));
                }
            }
            c => out.push(c),
        }
    }
    out
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

/// Format a tolerance for display without lying about it.
///
/// This line exists to tell a reader the threshold a verdict was produced under, so rounding it is
/// not a cosmetic choice. Integer-dividing nanoseconds by 1e6 printed a `clock_skew_ms = 0.5` run as
/// `clock-skew 0ms`, and `{:.0}%` printed a `rate_deviation = 0.004` run as `rate 0%` — a threshold
/// the operator deliberately tightened, disclosed as zero. Worse in the other direction: 50.9 ms
/// printed as `50ms`, which is exactly the default, so a *loosened* threshold read as untouched and
/// the line would have been better off absent.
///
/// Six decimals then trimmed: enough to render any threshold a human would write, and it absorbs the
/// float noise in `0.004 * 100.0` rather than printing `0.4000000000000001`.
fn trim_num(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// The tolerances that differ from the built-in defaults, as short human labels. Empty when the run
/// used every default.
fn non_default_tolerances(t: &crate::Tolerances) -> Vec<String> {
    let d = crate::Tolerances::default();
    let ms = |ns: i64| trim_num(ns as f64 / 1_000_000.0);
    let mut out = Vec::new();
    if t.clock_skew_ns != d.clock_skew_ns {
        out.push(format!("clock-skew {}ms", ms(t.clock_skew_ns)));
    }
    if t.start_offset_ns != d.start_offset_ns {
        out.push(format!("start-offset {}ms", ms(t.start_offset_ns)));
    }
    if t.end_offset_ns != d.end_offset_ns {
        out.push(format!("end-offset {}ms", ms(t.end_offset_ns)));
    }
    if t.rate_deviation != d.rate_deviation {
        out.push(format!("rate {}%", trim_num(t.rate_deviation * 100.0)));
    }
    if t.gap_factor != d.gap_factor {
        out.push(format!("gap {}x", t.gap_factor));
    }
    if t.jitter_cv != d.jitter_cv {
        out.push(format!("jitter cv {}", t.jitter_cv));
    }
    if t.episode_duration_factor != d.episode_duration_factor {
        out.push(format!("episode-duration {}x", t.episode_duration_factor));
    }
    if t.saturation_fraction != d.saturation_fraction {
        out.push(format!(
            "saturation {}%",
            trim_num(t.saturation_fraction * 100.0)
        ));
    }
    if t.saturation_min_samples != d.saturation_min_samples {
        out.push(format!(
            "saturation min-samples {}",
            t.saturation_min_samples
        ));
    }
    if t.outlier_z != d.outlier_z {
        out.push(format!("outlier {}σ", t.outlier_z));
    }
    if t.sequence_drop_fraction != d.sequence_drop_fraction {
        out.push(format!(
            "sequence drop {}%",
            trim_num(t.sequence_drop_fraction * 100.0)
        ));
    }
    if t.ego_max_speed_mps != d.ego_max_speed_mps {
        out.push(format!("ego max speed {} m/s", t.ego_max_speed_mps));
    }
    out
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

    // A partial run is stated before anything else is read, so "no findings" is never mistaken for
    // "no findings anywhere in the dataset".
    match &verdict.coverage {
        crate::engine::CoverageNote::Full => {}
        crate::engine::CoverageNote::Sample {
            request,
            episodes_ingested,
        } => {
            let _ = writeln!(
                out,
                "  Coverage: SAMPLE — {request}; {episodes_ingested} episode(s) ingested. This \
                 verdict covers only those episodes."
            );
        }
        crate::engine::CoverageNote::MetadataOnly { episodes_declared } => {
            let _ = writeln!(
                out,
                "  Coverage: METADATA-ONLY — {episodes_declared} episode(s) declared; no stream \
                 payload was read. This verdict covers the manifest, the stored statistics, and \
                 the provenance — not the data."
            );
        }
    }

    // Surface any tolerance that was loosened/tightened from its default, so a reader knows a
    // "no findings" result reflects the thresholds actually applied. Silent when all are default.
    let overrides = non_default_tolerances(&verdict.effective_config.tolerances);
    if !overrides.is_empty() {
        let _ = writeln!(out, "  Tolerances (non-default): {}", overrides.join(", "));
    }

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
                tty_safe(&location_label(&f.location))
            );
            let _ = writeln!(out, "      {}", tty_safe(&f.message));
            if !f.risk.is_empty() {
                let _ = writeln!(out, "      risk:   {}", tty_safe(&f.risk));
            }
            if !f.remedy.is_empty() {
                let _ = writeln!(out, "      remedy: {}", tty_safe(&f.remedy));
            }
        }
    }

    // Errored checks, listed separately from data findings.
    if !verdict.errored_checks.is_empty() {
        let _ = writeln!(out, "\nErrored checks (failed to run):");
        for e in &verdict.errored_checks {
            let _ = writeln!(
                out,
                "  {} (v{}): {}",
                e.check_id,
                e.version,
                tty_safe(&e.message)
            );
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
    render_html_with_readiness(verdict, trust_score, None)
}

/// As [`render_html`], plus the per-criterion readiness verdict when a profile was named. The HTML
/// report is the artifact built to travel, so a readiness judgement that reached only the terminal
/// was the one place it was least likely to be read.
pub fn render_html_with_readiness(
    verdict: &Verdict,
    trust_score: Option<TrustScore>,
    readiness: Option<&crate::certificate::ReadinessReport>,
) -> String {
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

    // A shared HTML artifact travels further than the command that produced it, so it has to carry
    // the fact that the run only looked at part of the dataset.
    match &verdict.coverage {
        crate::engine::CoverageNote::Full => {}
        crate::engine::CoverageNote::Sample {
            request,
            episodes_ingested,
        } => {
            let _ = write!(
                body,
                "<p class=\"status warn\">Coverage: SAMPLE — {}; {} episode(s) ingested. \
                 This report covers only those episodes.</p>",
                esc(request),
                episodes_ingested
            );
        }
        crate::engine::CoverageNote::MetadataOnly { episodes_declared } => {
            let _ = write!(
                body,
                "<p class=\"status warn\">Coverage: METADATA-ONLY — {episodes_declared} episode(s) \
                 declared; no stream payload was read. This report covers the manifest, the stored \
                 statistics, and the provenance — not the data.</p>"
            );
        }
    }

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

    // A check that panicked produced no findings, which is not the same as finding nothing. A shared
    // HTML artifact that omitted this read as a clean pass while a check never ran at all.
    if !verdict.errored_checks.is_empty() {
        body.push_str("<h2>Errored checks (failed to run)</h2><ul>");
        for e in &verdict.errored_checks {
            let _ = write!(
                body,
                "<li><code>{}</code> (v{}): {}</li>",
                esc(e.check_id),
                esc(e.version),
                esc(&e.message)
            );
        }
        body.push_str("</ul>");
    }

    // Disclose any loosened threshold, as the terminal report does: "no findings" is only meaningful
    // against the tolerances that produced it.
    let overrides = non_default_tolerances(&verdict.effective_config.tolerances);
    if !overrides.is_empty() {
        let _ = write!(
            body,
            "<p><strong>Tolerances (non-default):</strong> {}</p>",
            esc(&overrides.join(", "))
        );
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

    // The readiness verdict, when a profile was named. `render_readiness` writes plain text, so it
    // goes in a <pre> and is escaped like every other dataset-derived string in this document.
    if let Some(readiness) = readiness {
        let _ = write!(
            body,
            "<h2>Profile readiness</h2><pre>{}</pre>",
            esc(&crate::certificate::render_readiness(readiness, ""))
        );
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

/// The SARIF rule id reported for a check that failed to run.
const CHECK_ERRORED_RULE: &str = "VERIDEX.CHECK_ERRORED";

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
    let mut rules: Vec<Value> = rule_ids
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

    if !verdict.errored_checks.is_empty() {
        rules.push(json!({
            "id": CHECK_ERRORED_RULE,
            "name": CHECK_ERRORED_RULE,
            "shortDescription": { "text": "A check failed to run" },
            "fullDescription": { "text": "The check panicked or errored, so whatever it would have found is absent from this report. A clean result is not evidence that it passed." },
            "helpUri": "https://github.com/clay-good/veridex/blob/main/docs/checks.md"
        }));
    }

    // One result per errored check, so a CI job gating on SARIF cannot read a crashed check as a
    // clean pass.
    let errored_results = verdict.errored_checks.iter().map(|e| {
        json!({
            "ruleId": CHECK_ERRORED_RULE,
            "level": "error",
            "message": { "text": format!("check `{}` (v{}) failed to run: {} — whatever it would have found is absent from this report", e.check_id, e.version, e.message) },
            "locations": [{ "logicalLocations": [{ "name": "dataset" }] }],
            "properties": { "checkId": e.check_id }
        })
    });

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
        .chain(errored_results)
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
        Autonomy => "autonomy",
    }
}
