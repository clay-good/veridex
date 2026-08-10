//! Temporal checks: per-stream timeline health and the headline cross-stream clock-skew check.

use crate::cdm::{Dataset, Episode, Stream};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};
use std::collections::{BTreeMap, BTreeSet};

const NS_PER_S: f64 = 1_000_000_000.0;

/// `min_ts`, `max_ts` over a stream's frames (frames are not assumed sorted).
fn span_bounds(stream: &Stream) -> Option<(i64, i64)> {
    let mut it = stream.frames.iter().map(|f| f.ts);
    let first = it.next()?;
    let (mut lo, mut hi) = (first, first);
    for ts in it {
        lo = lo.min(ts);
        hi = hi.max(ts);
    }
    Some((lo, hi))
}

/// An episode's overall wall-clock duration in nanoseconds, if measurable. Prefers the adapter's
/// declared `[start_ts, end_ts]`; otherwise falls back to the longest single-stream span — a
/// clock-safe proxy, since one stream's frames share a clock so the subtraction never mixes clocks.
/// `None` when neither is available or positive.
fn episode_duration_ns(ep: &Episode) -> Option<i64> {
    if let (Some(start), Some(end)) = (ep.start_ts, ep.end_ts) {
        if end > start {
            return Some(end - start);
        }
    }
    ep.streams
        .iter()
        .filter_map(|s| span_bounds(s).map(|(lo, hi)| hi.saturating_sub(lo)))
        .filter(|d| *d > 0)
        .max()
}

/// Median of an already-sorted, non-empty slice.
fn median_sorted(sorted: &[i64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    }
}

/// Timestamp monotonicity: within a stream, timestamps must strictly increase. A decrease or repeat
/// means frames are out of order or duplicated.
pub struct Monotonicity;

impl Check for Monotonicity {
    fn id(&self) -> &'static str {
        "temporal.monotonicity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.NON_MONOTONIC"]
    }
    fn title(&self) -> &'static str {
        "Timestamp monotonicity"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                for i in 1..stream.frames.len() {
                    let prev = stream.frames[i - 1].ts;
                    let cur = stream.frames[i].ts;
                    if cur <= prev {
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Temporal,
                                Severity::Error,
                                Location::FrameRange {
                                    episode: ep.index,
                                    stream: stream.name.clone(),
                                    start_frame: (i - 1) as u64,
                                    end_frame: i as u64,
                                },
                                "TEMPORAL.NON_MONOTONIC",
                                format!(
                                    "stream `{}` in episode {}: timestamp does not increase at \
                                     frame {i} ({prev} -> {cur})",
                                    stream.name, ep.index
                                ),
                            )
                            .with_risk(
                                "Out-of-order or duplicated frames corrupt trajectory ordering and \
                                 any windowed learning.",
                            )
                            .with_remedy("Re-sort or de-duplicate the stream by timestamp at the source."),
                        );
                        break; // one finding per stream is enough to locate the fault
                    }
                }
            }
        }
        findings
    }
}

/// Rate conformance: when a stream declares a sample rate, the observed mean rate must match it
/// within a relative tolerance.
pub struct RateConformance {
    /// Allowed relative deviation (0.10 = 10%).
    pub relative_tolerance: f64,
}

impl Default for RateConformance {
    fn default() -> Self {
        RateConformance {
            relative_tolerance: 0.10,
        }
    }
}

impl Check for RateConformance {
    fn id(&self) -> &'static str {
        "temporal.rate-conformance"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.RATE"]
    }
    fn title(&self) -> &'static str {
        "Declared-rate conformance"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let Some(declared) = stream.declared_rate_hz else {
                    continue;
                };
                if declared <= 0.0 || stream.frames.len() < 2 {
                    continue;
                }
                let Some((lo, hi)) = span_bounds(stream) else {
                    continue;
                };
                // saturating: corrupt timestamps spanning the full i64 range must not overflow.
                let seconds = hi.saturating_sub(lo) as f64 / NS_PER_S;
                if seconds <= 0.0 {
                    continue;
                }
                let observed = (stream.frames.len() as f64 - 1.0) / seconds;
                let dev = (observed - declared).abs() / declared;
                if dev > self.relative_tolerance {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Temporal,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "TEMPORAL.RATE",
                            format!(
                                "stream `{}` in episode {}: observed {observed:.3} Hz vs declared \
                                 {declared:.3} Hz ({:.1}% off)",
                                stream.name,
                                ep.index,
                                dev * 100.0
                            ),
                        )
                        .with_risk(
                            "A rate mismatch means the declared timing is wrong; downstream \
                             resampling and sync assumptions break.",
                        )
                        .with_remedy(
                            "Correct the declared rate or investigate dropped/duplicated frames.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// Declared-rate validity. A source that states a sampling rate must state a *usable* one: a
/// positive, finite number. A declared rate of `0`, a negative value, or `NaN`/`inf` is corrupt
/// metadata — and, crucially, [`RateConformance`] and [`Gaps`] both *skip* such a stream (they guard
/// `rate > 0.0`), so without this check a nonsensical declared rate passes silently. This surfaces it
/// as the metadata error it is. Streams that declare no rate are fine and are not flagged.
pub struct RateValidity;

impl Check for RateValidity {
    fn id(&self) -> &'static str {
        "temporal.rate-validity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.INVALID_RATE"]
    }
    fn title(&self) -> &'static str {
        "Declared-rate validity"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let Some(declared) = stream.declared_rate_hz else {
                    continue;
                };
                if declared.is_finite() && declared > 0.0 {
                    continue;
                }
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Temporal,
                        Severity::Error,
                        Location::Stream {
                            episode: ep.index,
                            stream: stream.name.clone(),
                        },
                        "TEMPORAL.INVALID_RATE",
                        format!(
                            "stream `{}` in episode {}: declared rate {declared} Hz is not a \
                             positive, finite number",
                            stream.name, ep.index
                        ),
                    )
                    .with_risk(
                        "A corrupt declared rate is silently ignored by the rate and gap checks, so \
                         the timing metadata downstream tools rely on is wrong and unverified.",
                    )
                    .with_remedy(
                        "Correct the declared sampling rate at the source, or omit it so the \
                         observed rate is used.",
                    ),
                );
            }
        }
        findings
    }
}

/// Gaps: an inter-frame interval far larger than expected indicates dropped frames.
pub struct Gaps {
    /// A gap is an interval greater than `gap_factor` times the expected interval.
    pub gap_factor: f64,
}

impl Default for Gaps {
    fn default() -> Self {
        Gaps { gap_factor: 3.0 }
    }
}

impl Gaps {
    /// Expected interval (ns): from the declared rate if present, else the median positive interval.
    fn expected_interval_ns(stream: &Stream) -> Option<f64> {
        if let Some(rate) = stream.declared_rate_hz {
            if rate > 0.0 {
                return Some(NS_PER_S / rate);
            }
        }
        let mut intervals: Vec<i64> = stream
            .frames
            .windows(2)
            .map(|w| w[1].ts.saturating_sub(w[0].ts))
            .filter(|d| *d > 0)
            .collect();
        if intervals.is_empty() {
            return None;
        }
        intervals.sort_unstable();
        Some(intervals[intervals.len() / 2] as f64)
    }
}

impl Check for Gaps {
    fn id(&self) -> &'static str {
        "temporal.gap"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.GAP"]
    }
    fn title(&self) -> &'static str {
        "Timeline gaps"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                if stream.frames.len() < 2 {
                    continue;
                }
                let Some(expected) = Gaps::expected_interval_ns(stream) else {
                    continue;
                };
                let threshold = expected * self.gap_factor;
                for w in stream.frames.windows(2) {
                    let interval = w[1].ts.saturating_sub(w[0].ts) as f64;
                    if interval > threshold {
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Temporal,
                                Severity::Warning,
                                Location::TimeRange {
                                    episode: ep.index,
                                    stream: stream.name.clone(),
                                    start_ts: w[0].ts,
                                    end_ts: w[1].ts,
                                },
                                "TEMPORAL.GAP",
                                format!(
                                    "stream `{}` in episode {}: {:.1} ms gap (expected ~{:.1} ms)",
                                    stream.name,
                                    ep.index,
                                    interval / 1e6,
                                    expected / 1e6
                                ),
                            )
                            .with_risk("Dropped frames leave holes that bias temporal models.")
                            .with_remedy("Check the recorder for drops; annotate or trim the gap."),
                        );
                    }
                }
            }
        }
        findings
    }
}

/// Timeline jitter: even when a stream's mean rate is correct and no single interval is a gap, the
/// inter-frame intervals can be badly irregular. This measures the **coefficient of variation**
/// (std / mean) of the intervals and flags a stream whose CV exceeds a threshold. It is complementary
/// to [`RateConformance`] (which only checks the *mean* rate) and [`Gaps`] (which catches a single
/// large interval): a stream can pass both yet still have a jittery timeline that distorts the
/// fixed-timestep dynamics temporal models assume.
///
/// A short stream gives an unreliable CV, so streams with fewer than [`Jitter::MIN_INTERVALS`]
/// positive intervals are skipped, as are non-monotonic streams (that fault is
/// [`Monotonicity`]'s — a negative interval would corrupt the statistic).
pub struct Jitter {
    /// Maximum tolerated coefficient of variation of the inter-frame intervals.
    pub max_cv: f64,
}

impl Default for Jitter {
    fn default() -> Self {
        Jitter { max_cv: 0.5 }
    }
}

impl Jitter {
    /// Minimum positive intervals (frames − 1) needed for a meaningful CV; below this the statistic
    /// is too noisy to act on.
    const MIN_INTERVALS: usize = 8;
}

impl Check for Jitter {
    fn id(&self) -> &'static str {
        "temporal.jitter"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.JITTER"]
    }
    fn title(&self) -> &'static str {
        "Timeline jitter"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // Consecutive intervals. A non-positive interval means the stream is non-monotonic
                // (Monotonicity's concern) and would make the statistic meaningless — skip the stream.
                let intervals: Vec<f64> = stream
                    .frames
                    .windows(2)
                    .map(|w| w[1].ts.saturating_sub(w[0].ts) as f64)
                    .collect();
                if intervals.len() < Self::MIN_INTERVALS || intervals.iter().any(|d| *d <= 0.0) {
                    continue;
                }
                let n = intervals.len() as f64;
                let mean = intervals.iter().sum::<f64>() / n;
                if mean <= 0.0 {
                    continue;
                }
                let variance = intervals.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / n;
                let cv = variance.sqrt() / mean;
                if cv > self.max_cv {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Temporal,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "TEMPORAL.JITTER",
                            format!(
                                "stream `{}` in episode {}: irregular timeline (interval cv \
                                 {cv:.2} vs allowed {:.2}; mean ~{:.1} ms)",
                                stream.name,
                                ep.index,
                                self.max_cv,
                                mean / 1e6,
                            ),
                        )
                        .with_risk(
                            "Unevenly spaced frames distort the fixed-timestep dynamics temporal \
                             models assume, even when the mean rate looks correct.",
                        )
                        .with_remedy(
                            "Investigate recorder/scheduling jitter, or resample the stream onto a \
                             uniform time base.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// **The headline check (design D4).** Cross-stream clock skew: streams recorded over the same
/// episode should span the same real-time duration. A relative difference in spanned duration is
/// clock drift — one sensor's clock ran fast or slow — and it silently mis-aligns observations from
/// actions. This works across differing `clock_id`s because it compares *durations*, not absolute
/// timestamps, so no shared epoch is required.
pub struct ClockSkew {
    /// Maximum tolerated difference in spanned duration between two streams, in nanoseconds.
    pub tolerance_ns: i64,
}

impl Default for ClockSkew {
    fn default() -> Self {
        ClockSkew {
            tolerance_ns: 50_000_000, // 50 ms
        }
    }
}

impl Check for ClockSkew {
    fn id(&self) -> &'static str {
        "temporal.clock-skew"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.CLOCK_SKEW"]
    }
    fn title(&self) -> &'static str {
        "Cross-stream clock skew"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Episode
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            // (name, clock_id, span) for streams with a measurable span.
            let spans: Vec<(&str, &str, i64)> = ep
                .streams
                .iter()
                .filter_map(|s| {
                    span_bounds(s).map(|(lo, hi)| {
                        (s.name.as_str(), s.clock_id.as_str(), hi.saturating_sub(lo))
                    })
                })
                .filter(|(_, _, span)| *span > 0)
                .collect();

            for i in 0..spans.len() {
                for j in (i + 1)..spans.len() {
                    let (name_a, clock_a, span_a) = spans[i];
                    let (name_b, clock_b, span_b) = spans[j];
                    let drift = (span_a - span_b).abs();
                    if drift > self.tolerance_ns {
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Temporal,
                                Severity::Error,
                                Location::Episode { episode: ep.index },
                                "TEMPORAL.CLOCK_SKEW",
                                format!(
                                    "episode {}: streams `{name_a}` (clock `{clock_a}`, span \
                                     {:.1} ms) and `{name_b}` (clock `{clock_b}`, span {:.1} ms) \
                                     drift by {:.1} ms",
                                    ep.index,
                                    span_a as f64 / 1e6,
                                    span_b as f64 / 1e6,
                                    drift as f64 / 1e6,
                                ),
                            )
                            .with_risk(
                                "Clock drift mis-aligns observations and actions: the policy learns \
                                 to act on stale or future observations.",
                            )
                            .with_remedy(
                                "Re-synchronize the streams against a common time base, or record \
                                 and apply per-stream latency offsets.",
                            ),
                        );
                    }
                }
            }
        }
        findings
    }
}

/// Cross-stream **start-time offset within a shared clock**. Two streams recorded on the same
/// `clock_id` over one episode should begin at nearly the same absolute time; one that starts
/// materially later than its peers — a sensor that came online late, or a truncated head — mis-aligns
/// the very first observations from actions. Absolute timestamps are only comparable within a single
/// clock, so streams are grouped by `clock_id` and compared only inside a group. This is the same
/// no-shared-epoch discipline as [`ClockSkew`], which instead compares *durations* across clocks;
/// the two are complementary — a late start can leave durations equal yet the alignment wrong.
pub struct StartOffset {
    /// Maximum tolerated difference between the earliest and latest stream start on one clock, in
    /// nanoseconds.
    pub tolerance_ns: i64,
}

impl Default for StartOffset {
    fn default() -> Self {
        StartOffset {
            tolerance_ns: 50_000_000, // 50 ms
        }
    }
}

impl Check for StartOffset {
    fn id(&self) -> &'static str {
        "temporal.start-offset"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.START_OFFSET"]
    }
    fn title(&self) -> &'static str {
        "Cross-stream start offset (shared clock)"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Episode
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            // Group each stream's start (min ts) by clock_id; BTreeMap keeps clocks in a stable
            // order so findings are deterministic.
            let mut by_clock: BTreeMap<&str, Vec<(&str, i64)>> = BTreeMap::new();
            for s in &ep.streams {
                if let Some((lo, _hi)) = span_bounds(s) {
                    by_clock
                        .entry(s.clock_id.as_str())
                        .or_default()
                        .push((s.name.as_str(), lo));
                }
            }
            for (clock, mut starts) in by_clock {
                if starts.len() < 2 {
                    continue;
                }
                // Earliest and latest starting streams on this clock.
                starts.sort_by_key(|(_, start)| *start);
                let (early_name, early_start) = starts[0];
                let (late_name, late_start) = starts[starts.len() - 1];
                let offset = late_start.saturating_sub(early_start);
                if offset > self.tolerance_ns {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Temporal,
                            Severity::Warning,
                            Location::Episode { episode: ep.index },
                            "TEMPORAL.START_OFFSET",
                            format!(
                                "episode {}: on clock `{clock}`, stream `{late_name}` starts \
                                 {:.1} ms after `{early_name}`",
                                ep.index,
                                offset as f64 / 1e6,
                            ),
                        )
                        .with_risk(
                            "A late-starting stream leaves the first observations unpaired with \
                             their actions, so early frames train on missing or stale context.",
                        )
                        .with_remedy(
                            "Confirm all sensors start together, or trim each episode to the \
                             common time window.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// Cross-stream **end-time offset within a shared clock**. The mirror of [`StartOffset`]: two streams
/// recorded on the same `clock_id` over one episode should also *end* at nearly the same absolute
/// time. One that ends materially earlier than its peers — a sensor that dropped out mid-episode, or
/// a truncated tail — leaves the final observations unpaired with their actions. This completes the
/// start / duration / end alignment triple: because `end = start + duration`, a stream can slip past
/// both [`StartOffset`] (|Δstart| ≤ tol) and [`ClockSkew`] (|Δduration| ≤ tol) yet still be misaligned
/// at the tail by up to twice the tolerance, so neither of those checks would catch it. Absolute
/// timestamps are only comparable within a single clock, so streams are grouped by `clock_id` and
/// compared only inside a group.
pub struct EndOffset {
    /// Maximum tolerated difference between the earliest and latest stream end on one clock, in
    /// nanoseconds.
    pub tolerance_ns: i64,
}

impl Default for EndOffset {
    fn default() -> Self {
        EndOffset {
            tolerance_ns: 50_000_000, // 50 ms
        }
    }
}

impl Check for EndOffset {
    fn id(&self) -> &'static str {
        "temporal.end-offset"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.END_OFFSET"]
    }
    fn title(&self) -> &'static str {
        "Cross-stream end offset (shared clock)"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Episode
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            // Group each stream's end (max ts) by clock_id; BTreeMap keeps clocks in a stable
            // order so findings are deterministic.
            let mut by_clock: BTreeMap<&str, Vec<(&str, i64)>> = BTreeMap::new();
            for s in &ep.streams {
                if let Some((_lo, hi)) = span_bounds(s) {
                    by_clock
                        .entry(s.clock_id.as_str())
                        .or_default()
                        .push((s.name.as_str(), hi));
                }
            }
            for (clock, mut ends) in by_clock {
                if ends.len() < 2 {
                    continue;
                }
                // Earliest- and latest-ending streams on this clock.
                ends.sort_by_key(|(_, end)| *end);
                let (early_name, early_end) = ends[0];
                let (late_name, late_end) = ends[ends.len() - 1];
                let offset = late_end.saturating_sub(early_end);
                if offset > self.tolerance_ns {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Temporal,
                            Severity::Warning,
                            Location::Episode { episode: ep.index },
                            "TEMPORAL.END_OFFSET",
                            format!(
                                "episode {}: on clock `{clock}`, stream `{early_name}` ends \
                                 {:.1} ms before `{late_name}`",
                                ep.index,
                                offset as f64 / 1e6,
                            ),
                        )
                        .with_risk(
                            "An early-ending stream leaves the last observations unpaired with \
                             their actions, so late frames train on missing or stale context.",
                        )
                        .with_remedy(
                            "Confirm all sensors stop together, or trim each episode to the \
                             common time window.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// Cross-episode **declared-rate consistency**. The temporal sibling of
/// [`ShapeConsistency`](crate::checks::structural::ShapeConsistency): a stream that declares one
/// sampling rate in some episodes and a materially different rate in others means the dataset pools
/// differently-configured sources — or the rate metadata is wrong. Every per-episode temporal check
/// passes (each episode is internally consistent), yet a global fixed-rate assumption is wrong for
/// part of the data. Streams that declare no rate, or vary only by floating-point noise, are not
/// flagged; the first declared rate seen for a stream name is the baseline the rest are compared to.
pub struct RateConsistency;

impl RateConsistency {
    /// Relative tolerance for treating two declared rates as "the same". Declared rates are metadata
    /// and usually exact, so this only absorbs floating-point noise; a real rate change (30 Hz vs
    /// 10 Hz) is far larger. Not a policy knob, so it is a constant rather than a configurable
    /// tolerance.
    const REL_TOL: f64 = 0.01;
}

impl Check for RateConsistency {
    fn id(&self) -> &'static str {
        "temporal.rate-consistency"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.RATE_INCONSISTENT"]
    }
    fn title(&self) -> &'static str {
        "Cross-episode declared-rate consistency"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // First valid declared rate seen for each stream name, and the episode it came from.
        let mut baseline: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
        // Names already reported, so a stream that drifts across many episodes yields one finding.
        let mut reported: BTreeSet<&str> = BTreeSet::new();
        let mut findings = Vec::new();

        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // Only a positive, finite declared rate is comparable (a corrupt rate is
                // `TEMPORAL.INVALID_RATE`'s concern, not this check's).
                let Some(rate) = stream.declared_rate_hz else {
                    continue;
                };
                if !rate.is_finite() || rate <= 0.0 {
                    continue;
                }
                match baseline.get(stream.name.as_str()) {
                    None => {
                        baseline.insert(&stream.name, (rate, ep.index));
                    }
                    Some(&(base_rate, base_ep)) => {
                        let differs = (rate - base_rate).abs() > Self::REL_TOL * base_rate;
                        if differs && reported.insert(stream.name.as_str()) {
                            findings.push(
                                Finding::new(
                                    self.id(),
                                    Category::Temporal,
                                    Severity::Warning,
                                    Location::Stream {
                                        episode: ep.index,
                                        stream: stream.name.clone(),
                                    },
                                    "TEMPORAL.RATE_INCONSISTENT",
                                    format!(
                                        "stream `{}` declares {base_rate:.3} Hz in episode \
                                         {base_ep} but {rate:.3} Hz in episode {}",
                                        stream.name, ep.index,
                                    ),
                                )
                                .with_risk(
                                    "A stream whose declared sampling rate changes between episodes \
                                     means the dataset pools differently-configured sources (or the \
                                     rate metadata is wrong); a global fixed-rate assumption and any \
                                     resampling will be wrong for part of the data.",
                                )
                                .with_remedy(
                                    "Confirm every episode of this stream was recorded at one rate; \
                                     re-export or split the mismatched episodes, or correct the \
                                     declared rate.",
                                ),
                            );
                        }
                    }
                }
            }
        }
        findings
    }
}

/// Cross-episode **duration outlier**. Robot episodes legitimately vary in length, but an episode
/// whose total duration is a large multiple away from the dataset's *typical* duration is almost
/// always a recording fault — a capture cut short, or a recorder left running — not natural task
/// variation. Such an episode trains a policy on a fragment (or a frozen scene) while still counting
/// as a full labeled trajectory. The baseline is the **median** episode duration, robust to the very
/// outliers it is looking for; an episode is flagged when its duration is below `median / factor` or
/// above `median * factor`. Needs at least [`EpisodeDuration::MIN_EPISODES`] episodes with a
/// measurable duration, or there is no stable "typical" to compare against.
pub struct EpisodeDuration {
    /// An episode is an outlier when its duration is more than this multiple away (in either
    /// direction) from the dataset's median episode duration.
    pub factor: f64,
}

impl EpisodeDuration {
    /// Minimum number of measurable-duration episodes before the check runs. Below this a median is
    /// not a meaningful "typical", so the check abstains rather than flag on thin evidence.
    const MIN_EPISODES: usize = 4;
}

impl Default for EpisodeDuration {
    fn default() -> Self {
        EpisodeDuration { factor: 10.0 }
    }
}

impl Check for EpisodeDuration {
    fn id(&self) -> &'static str {
        "temporal.episode-duration"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.EPISODE_DURATION_OUTLIER"]
    }
    fn title(&self) -> &'static str {
        "Cross-episode duration outlier"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // (episode index, duration ns) for every episode with a measurable duration, in ascending
        // index order (episodes are canonicalized that way), so findings come out deterministic.
        let durations: Vec<(u64, i64)> = dataset
            .episodes
            .iter()
            .filter_map(|ep| episode_duration_ns(ep).map(|d| (ep.index, d)))
            .collect();
        // A guard factor of <= 1.0 would flag every episode; treat it as "disabled" defensively (the
        // config layer already rejects it) and skip on too few episodes to form a baseline.
        if durations.len() < Self::MIN_EPISODES || self.factor <= 1.0 {
            return Vec::new();
        }

        let mut sorted: Vec<i64> = durations.iter().map(|(_, d)| *d).collect();
        sorted.sort_unstable();
        let median = median_sorted(&sorted);
        if median <= 0.0 {
            return Vec::new();
        }
        let low = median / self.factor;
        let high = median * self.factor;

        let mut findings = Vec::new();
        for (index, dur) in durations {
            let d = dur as f64;
            if d < low || d > high {
                let (ratio, direction) = if d < median {
                    (median / d, "shorter")
                } else {
                    (d / median, "longer")
                };
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Temporal,
                        Severity::Warning,
                        Location::Episode { episode: index },
                        "TEMPORAL.EPISODE_DURATION_OUTLIER",
                        format!(
                            "episode {index} lasts {:.1} ms — {ratio:.1}x {direction} than the \
                             dataset median of {:.1} ms",
                            d / 1e6,
                            median / 1e6,
                        ),
                    )
                    .with_risk(
                        "An episode far shorter or longer than the rest is usually a truncated \
                         capture or a stuck recorder, not a real trajectory; training on it teaches \
                         the policy from a fragment or a frozen scene while it still counts as a \
                         full labeled episode.",
                    )
                    .with_remedy(
                        "Inspect the outlier episode: drop it if it is a broken recording, or split \
                         it if two trajectories were merged.",
                    ),
                );
            }
        }
        findings
    }
}
