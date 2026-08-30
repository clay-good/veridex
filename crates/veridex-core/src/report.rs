//! Reporting: turn a [`Verdict`] (and optional [`TrustScore`]) into output humans read and machines
//! consume. Both renderers derive from the same verdict, so they never disagree.
//!
//! Surface: a human-readable terminal report with per-episode rollups (worst episodes first), a
//! versioned JSON envelope, SARIF 2.1.0 for CI code-scanning, a self-contained HTML report, and the
//! machine-readable check catalog (shared with the CLI and Python bindings).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{json, Value};

use crate::certificate::TrustScore;
use crate::check::{Location, Severity};
use crate::engine::{CheckInfo, Status, Verdict};

/// The versioned JSON report schema id. Stable and additive within a major version.
pub const REPORT_SCHEMA_VERSION: &str = "veridex.report/1";

/// Schema tag for `inspect --json`.
pub const INSPECT_SCHEMA_VERSION: &str = "veridex.inspect/1";

/// The machine-readable form of `veridex inspect`: the CDM, plus what the ingest actually covered.
///
/// Shared by both front-ends so they cannot drift — the parity the Python module promises should be
/// by construction, not by two call sites happening to agree.
///
/// The CDM used to be dumped bare, and the caveat the terminal render prints was dropped. That
/// caveat is not decoration: under `--metadata-only` every stream reports `0 frame(s)`, which is
/// indistinguishable from a dataset whose episodes are genuinely empty — the exact defect
/// `STRUCTURAL.EMPTY_STREAM` exists to report. The zeros are the shape of the request, not the data,
/// and a machine reader had no way to tell. A sampled inspect had the same problem: it returned a
/// one-episode CDM with nothing saying the rest were never opened.
pub fn render_inspect_json(ingested: &crate::adapter::Ingested) -> String {
    let coverage = match &ingested.report.coverage {
        crate::adapter::Coverage::Full => json!({ "kind": "full" }),
        crate::adapter::Coverage::Sample {
            sample,
            episodes_ingested,
        } => json!({
            "kind": "sample",
            "request": sample.describe(),
            "episodes_ingested": episodes_ingested,
        }),
        crate::adapter::Coverage::MetadataOnly { episodes_declared } => json!({
            "kind": "metadata_only",
            "episodes_declared": episodes_declared,
        }),
    };
    let doc = json!({
        "schema": INSPECT_SCHEMA_VERSION,
        "format": ingested.report.format_id,
        "cdm_content_hash": crate::content_hash(&ingested.dataset).to_hex(),
        "coverage": coverage,
        // Source the adapter declined to read. Same reasoning as `coverage`: a reader comparing two
        // inspections must be able to see that one of them skipped a shard.
        "unread_sources": ingested
            .report
            .unread_sources
            .iter()
            .map(|u| json!({ "source_path": u.source_path, "note": u.note }))
            .collect::<Vec<_>>(),
        "dataset": ingested.dataset,
    });
    serde_json::to_string_pretty(&doc).expect("inspect summary serializes")
}

#[derive(Debug, Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReportDataset<'a> {
    /// The dataset id the CDM carries — the same one `veridex inspect` prints.
    pub id: &'a str,
}

/// The machine-readable JSON report envelope.
#[derive(Debug, Clone, Serialize)]
pub struct JsonReport<'a> {
    /// Schema identifier, e.g. `veridex.report/1`.
    pub schema: &'static str,
    /// Which dataset the report is about.
    ///
    /// `veridex diff` reads this to refuse a comparison of two *different* datasets — a guard that
    /// is documented on `ReportDiff::dataset_differs` and was dead in practice, because the reports
    /// the CLI writes never carried the id it reads. Absent from a report produced without one, so
    /// an older report still diffs, and the guard treats a missing id as no evidence of a mismatch
    /// rather than as one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<ReportDataset<'a>>,
    /// The full verdict.
    pub verdict: &'a Verdict,
    /// The trust score, when the report is produced alongside scoring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust_score: Option<TrustScore>,
    /// The per-criterion readiness verdict, when the run named a policy profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readiness: Option<&'a crate::certificate::ReadinessReport>,
    /// Findings summarized by category, episode, and stream — the same rollups the terminal report
    /// prints, so a machine consumer does not have to re-derive them from the finding list.
    pub rollups: Rollups,
}

/// Render the verdict as stable, versioned JSON.
pub fn render_json(verdict: &Verdict, trust_score: Option<TrustScore>) -> String {
    render_json_with_readiness(verdict, trust_score, None, None)
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
    dataset_id: Option<&str>,
) -> String {
    let report = JsonReport {
        schema: REPORT_SCHEMA_VERSION,
        dataset: dataset_id.map(|id| ReportDataset { id }),
        verdict,
        trust_score,
        readiness,
        rollups: rollups(verdict),
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

/// Findings counted by severity, for one slice of a report (a category, a stream, the dataset).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SeverityTally {
    /// Error findings in this slice.
    pub error: u32,
    /// Warning findings.
    pub warning: u32,
    /// Info findings.
    pub info: u32,
}

impl SeverityTally {
    /// Count one finding.
    fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Error => self.error += 1,
            Severity::Warning => self.warning += 1,
            Severity::Info => self.info += 1,
        }
    }

    /// Total findings in this slice.
    pub fn total(&self) -> u32 {
        self.error + self.warning + self.info
    }

    /// Worst-first ordering: errors, then warnings, then info.
    fn rank(&self) -> (u32, u32, u32) {
        (self.error, self.warning, self.info)
    }
}

/// One stream's findings, aggregated across every episode it appears in.
///
/// Keyed by stream *name* rather than by `(episode, stream)` on purpose: the triage question a
/// stream rollup answers is "which sensor is the problem", and a camera that drifts in forty
/// episodes is one answer, not forty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StreamRollup {
    /// The stream name (as it appears in the verdict — placeholder text under `--redact`).
    pub stream: String,
    /// How many distinct episodes contributed a finding about this stream.
    pub episodes: u32,
    /// Findings by severity.
    #[serde(flatten)]
    pub counts: SeverityTally,
}

/// The rollups a report carries: findings sliced by category and by stream, and the ranked worst
/// episodes.
///
/// The terminal and HTML reports have always ranked the worst episodes; the machine-readable ones
/// carried nothing but the flat finding list, so a CI job — the only consumer `--json` has — had to
/// re-derive every summary the human report was given. Categories and streams are the two slices
/// the reporting spec names beside episodes, and neither existed anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rollups {
    /// Findings by check category, in the catalog's category order. Categories with no findings are
    /// omitted, so a clean family costs no noise.
    pub by_category: BTreeMap<String, SeverityTally>,
    /// Episodes, worst first.
    pub by_episode: Vec<EpisodeTally>,
    /// Streams, worst first.
    pub by_stream: Vec<StreamRollup>,
}

/// One episode's findings, in the machine-readable rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EpisodeTally {
    /// Episode index.
    pub episode: u64,
    /// Findings by severity.
    #[serde(flatten)]
    pub counts: SeverityTally,
}

/// Summarize a verdict's findings by category, episode, and stream.
///
/// Derived from the verdict alone, so every renderer that shows a rollup shows the same numbers,
/// and a redacted verdict rolls up to redacted stream names without redaction knowing about
/// rollups.
pub fn rollups(verdict: &Verdict) -> Rollups {
    let mut by_category: BTreeMap<String, SeverityTally> = BTreeMap::new();
    let mut by_stream: BTreeMap<&str, (SeverityTally, BTreeSet<u64>)> = BTreeMap::new();
    for f in &verdict.findings {
        by_category
            .entry(f.category.tag().to_string())
            .or_default()
            .add(f.severity);
        if let Some(stream) = location_stream(&f.location) {
            let entry = by_stream.entry(stream).or_default();
            entry.0.add(f.severity);
            if let Some(episode) = f.location.episode() {
                entry.1.insert(episode);
            }
        }
    }
    let mut streams: Vec<StreamRollup> = by_stream
        .into_iter()
        .map(|(stream, (counts, episodes))| StreamRollup {
            stream: stream.to_string(),
            episodes: episodes.len() as u32,
            counts,
        })
        .collect();
    streams.sort_by(|a, b| {
        b.counts
            .rank()
            .cmp(&a.counts.rank())
            .then_with(|| a.stream.cmp(&b.stream))
    });
    Rollups {
        by_category,
        by_episode: worst_episodes(verdict)
            .into_iter()
            .map(|r| EpisodeTally {
                episode: r.episode,
                counts: SeverityTally {
                    error: r.errors,
                    warning: r.warnings,
                    info: r.info,
                },
            })
            .collect(),
        by_stream: streams,
    }
}

/// The stream a location names, if it names one.
fn location_stream(location: &Location) -> Option<&str> {
    match location {
        Location::Dataset | Location::Episode { .. } => None,
        Location::Stream { stream, .. }
        | Location::FrameRange { stream, .. }
        | Location::TimeRange { stream, .. } => Some(stream.as_str()),
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

/// A tolerance that departs from the built-in defaults: its short human label, and whether the
/// departure **loosened** the threshold.
///
/// The direction matters because the two consumers want opposite things. The reports print every
/// departure, because an operator should see any threshold that is not the catalog's. Scope
/// disclosure wants only the loosenings, because its whole premise is that narrowing a run *raises*
/// the score — and a threshold moved to be stricter lowers it, which needs no warning.
///
/// Every one of the twelve is an upper bound on tolerated deviation, so the larger value is always
/// the looser one. That includes `saturation_min_samples`, the sample count below which the check
/// abstains: a larger one abstains on *more* streams, measuring less.
pub(crate) struct ToleranceDeparture {
    pub(crate) label: String,
    pub(crate) loosened: bool,
}

/// Every tolerance that differs from the built-in default, tagged with its direction. Empty when the
/// run used every default.
///
/// Shared with [`crate::engine`] and the certificate renderer: a threshold the operator moved is a
/// way the run departed from the declared catalog, and the twelve comparisons below are the one
/// place that knows which ones moved and which way.
pub(crate) fn tolerance_departures(t: &crate::Tolerances) -> Vec<ToleranceDeparture> {
    let d = crate::Tolerances::default();
    let ms = |ns: i64| trim_num(ns as f64 / 1_000_000.0);
    let mut out = Vec::new();
    macro_rules! departure {
        ($field:ident, $label:expr) => {
            if t.$field != d.$field {
                out.push(ToleranceDeparture {
                    label: $label,
                    loosened: t.$field > d.$field,
                });
            }
        };
    }
    departure!(
        clock_skew_ns,
        format!("clock-skew {}ms", ms(t.clock_skew_ns))
    );
    departure!(
        start_offset_ns,
        format!("start-offset {}ms", ms(t.start_offset_ns))
    );
    departure!(
        end_offset_ns,
        format!("end-offset {}ms", ms(t.end_offset_ns))
    );
    departure!(
        rate_deviation,
        format!("rate {}%", trim_num(t.rate_deviation * 100.0))
    );
    departure!(gap_factor, format!("gap {}x", t.gap_factor));
    departure!(jitter_cv, format!("jitter cv {}", t.jitter_cv));
    departure!(
        episode_duration_factor,
        format!("episode-duration {}x", t.episode_duration_factor)
    );
    departure!(
        saturation_fraction,
        format!("saturation {}%", trim_num(t.saturation_fraction * 100.0))
    );
    departure!(
        saturation_min_samples,
        format!("saturation min-samples {}", t.saturation_min_samples)
    );
    departure!(outlier_z, format!("outlier {}\u{3c3}", t.outlier_z));
    departure!(
        sequence_drop_fraction,
        format!(
            "sequence drop {}%",
            trim_num(t.sequence_drop_fraction * 100.0)
        )
    );
    departure!(
        ego_max_speed_mps,
        format!("ego max speed {} m/s", t.ego_max_speed_mps)
    );
    departure!(
        near_duplicate_fraction,
        format!(
            "near-duplicate {}%",
            trim_num(t.near_duplicate_fraction * 100.0)
        )
    );
    out
}

/// The tolerances that differ from the built-in defaults, as short human labels, in either
/// direction. What the terminal and HTML reports print.
pub(crate) fn non_default_tolerances(t: &crate::Tolerances) -> Vec<String> {
    tolerance_departures(t)
        .into_iter()
        .map(|dep| dep.label)
        .collect()
}

/// The tolerances the run **loosened** — the subset that narrows what the catalog measured.
///
/// A profile may only tighten ([`crate::profile::Profile::apply_tolerances`]), so this is empty for
/// a profile run that moved nothing else: `--profile world-model-ready` cannot, by construction,
/// narrow a run, and must not be disclosed as though it had.
pub(crate) fn loosened_tolerances(t: &crate::Tolerances) -> Vec<String> {
    tolerance_departures(t)
        .into_iter()
        .filter(|dep| dep.loosened)
        .map(|dep| dep.label)
        .collect()
}

/// How much of each finding the terminal report prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingDetail {
    /// Every finding with its risk and remedy — everything the verdict holds.
    Full,
    /// `error` and `warning` findings in full; `info` findings as their code, location and message,
    /// without the risk and remedy paragraphs.
    ///
    /// A sound dataset's report is mostly `info`: what could not be measured, what provenance is
    /// absent, what a partial run did not cover. Each of those carries a risk and a remedy worth
    /// reading *once*, and printing all of them by default buries the two lines that say whether the
    /// data is usable under forty that say what was not looked at. Nothing is dropped — the codes
    /// and messages are all still there, `--full` prints the rest, and every machine-readable output
    /// is unchanged.
    Compact,
}

/// Render a human-readable terminal report. `max_episodes` bounds the worst-episodes rollup.
///
/// Prints every finding in full; [`render_terminal_with`] takes a [`FindingDetail`].
pub fn render_terminal(
    verdict: &Verdict,
    trust_score: Option<TrustScore>,
    max_episodes: usize,
) -> String {
    render_terminal_with(verdict, trust_score, max_episodes, FindingDetail::Full)
}

/// [`render_terminal`], choosing how much of each finding to print.
pub fn render_terminal_with(
    verdict: &Verdict,
    trust_score: Option<TrustScore>,
    max_episodes: usize,
    detail: FindingDetail,
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

    // Rollups: which families the findings are in, then the worst episodes and streams. Triage
    // reads these before it reads a single finding.
    let summary = rollups(verdict);
    if !summary.by_category.is_empty() {
        let by_category: Vec<String> = summary
            .by_category
            .iter()
            .map(|(category, counts)| format!("{category} {}", counts.total()))
            .collect();
        let _ = writeln!(out, "  By category: {}", by_category.join(" · "));
    }

    // Worst-episodes rollup.
    let episode_rollups = worst_episodes(verdict);
    if !episode_rollups.is_empty() {
        let _ = writeln!(out, "\nWorst episodes:");
        for r in episode_rollups.iter().take(max_episodes) {
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

    // Worst-streams rollup: the question "which sensor is the problem" that a per-episode ranking
    // cannot answer, since one bad camera spreads its findings across every episode it appears in.
    if !summary.by_stream.is_empty() {
        let _ = writeln!(out, "\nWorst streams:");
        for r in summary.by_stream.iter().take(max_episodes) {
            let _ = writeln!(
                out,
                "  `{}` — {} error, {} warning, {} info ({} total, across {} episode(s))",
                r.stream,
                r.counts.error,
                r.counts.warning,
                r.counts.info,
                r.counts.total(),
                r.episodes
            );
        }
    }

    // Findings (already stably ordered in the verdict).
    if !verdict.findings.is_empty() {
        let _ = writeln!(out, "\nFindings:");
        let mut compacted = 0usize;
        for f in &verdict.findings {
            let _ = writeln!(
                out,
                "  [{}] {}  {}",
                severity_label(f.severity),
                f.code,
                tty_safe(&location_label(&f.location))
            );
            let _ = writeln!(out, "      {}", tty_safe(&f.message));
            // An `info` finding says what was not measured; a warning or an error says what is
            // wrong. Under `Compact` the second keeps its risk and remedy and the first does not.
            if detail == FindingDetail::Compact && f.severity == Severity::Info {
                compacted += 1;
                continue;
            }
            if !f.risk.is_empty() {
                let _ = writeln!(out, "      risk:   {}", tty_safe(&f.risk));
            }
            if !f.remedy.is_empty() {
                let _ = writeln!(out, "      remedy: {}", tty_safe(&f.remedy));
            }
        }
        if compacted > 0 {
            let _ = writeln!(
                out,
                "\n  {compacted} info finding(s) printed without their risk and remedy — \
                 `--full` prints those, and every machine-readable output already carries them."
            );
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

    // The same rollups the terminal report prints, from the same function, so a shared HTML
    // artifact and the terminal it came from cannot summarize the run differently.
    let summary = rollups(verdict);
    if !summary.by_category.is_empty() {
        let by_category: Vec<String> = summary
            .by_category
            .iter()
            .map(|(category, counts)| format!("{} {}", esc(category), counts.total()))
            .collect();
        let _ = write!(body, "<p>By category: {}</p>", by_category.join(" · "));
    }

    let episode_rollups = worst_episodes(verdict);
    if !episode_rollups.is_empty() {
        body.push_str("<h2>Worst episodes</h2><ul>");
        for r in episode_rollups.iter().take(10) {
            let _ = write!(
                body,
                "<li>episode {} — {} error, {} warning, {} info</li>",
                r.episode, r.errors, r.warnings, r.info
            );
        }
        body.push_str("</ul>");
    }

    if !summary.by_stream.is_empty() {
        body.push_str("<h2>Worst streams</h2><ul>");
        for r in summary.by_stream.iter().take(10) {
            let _ = write!(
                body,
                "<li><code>{}</code> — {} error, {} warning, {} info, across {} episode(s)</li>",
                esc(&r.stream),
                r.counts.error,
                r.counts.warning,
                r.counts.info,
                r.episodes
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

/// The SARIF rule id under which a profile's readiness verdict is reported.
const PROFILE_RULE: &str = "VERIDEX.PROFILE_NOT_READY";

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
    render_sarif_with_readiness(verdict, None)
}

/// Like [`render_sarif`], but also reports a profile's readiness verdict.
///
/// Readiness is rendered for *every* output shape by design — a profile is what the run is judged
/// against, and a CI consumer is precisely who reads SARIF. The `--sarif` branch was the one that
/// called the readiness-free renderer, so terminal, JSON, and HTML all reported the profile verdict
/// and a code-scanning gate on the SARIF alone could not see that the profile did not apply, that a
/// criterion abstained, or that the rig was not ready.
///
/// Synthesized as a result the way a crashed check is, for the same reason: findings and results
/// are the only channel a code-scanning system reads.
pub fn render_sarif_with_readiness(
    verdict: &Verdict,
    readiness: Option<&crate::certificate::ReadinessReport>,
) -> Value {
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

    if readiness.is_some() {
        rules.push(json!({
            "id": PROFILE_RULE,
            "name": PROFILE_RULE,
            "shortDescription": { "text": "A policy profile's readiness verdict" },
            "fullDescription": { "text": "The dataset was judged against a named policy profile. A profile that does not apply, or a criterion whose check did not run, is not a pass — it is an absence of judgement." },
            "helpUri": "https://github.com/clay-good/veridex/blob/main/docs/profiles.md"
        }));
    }

    // The readiness verdict, as one result. `ready` is reported at `error` level and anything else
    // at `note`, so a code-scanning gate can act on it.
    let readiness_results = readiness.into_iter().map(|r| {
        let (level, text) = match (r.applicable, r.ready) {
            (false, _) => (
                "note",
                format!(
                    "`{}` profile: N/A (profile does not apply to this dataset, or the run was \
                     partial or narrowed) — this is not a pass",
                    r.profile
                ),
            ),
            (true, true) => (
                "note",
                format!(
                    "`{}` profile: READY ({} criteria)",
                    r.profile,
                    r.criteria.len()
                ),
            ),
            (true, false) => {
                let failed: Vec<&str> = r
                    .criteria
                    .iter()
                    .filter(|c| !c.passed)
                    .map(|c| c.check_id.as_str())
                    .collect();
                (
                    "error",
                    format!(
                        "`{}` profile: NOT READY — {} of {} criteria unsatisfied ({})",
                        r.profile,
                        failed.len(),
                        r.criteria.len(),
                        failed.join(", ")
                    ),
                )
            }
        };
        json!({
            "ruleId": PROFILE_RULE,
            "level": level,
            "message": { "text": text },
            "locations": [{ "logicalLocations": [{ "name": "dataset" }] }],
            "properties": { "profile": r.profile, "applicable": r.applicable, "ready": r.ready }
        })
    });

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
        .chain(readiness_results)
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
