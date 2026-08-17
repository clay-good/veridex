//! Temporal checks: per-stream timeline health and the headline cross-stream clock-skew check.

use crate::cdm::{ClockKind, Dataset, Episode, Stream};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};
use std::collections::{BTreeMap, BTreeSet};

const NS_PER_S: f64 = 1_000_000_000.0;

/// The streams of an episode whose timestamps are measured time.
///
/// Every check below that *compares* timestamps — differences them, turns them into a rate, or
/// aligns two streams — reads its streams through here. A positional step index is flawlessly
/// monotonic, perfectly regular, and identical across an episode's streams, so it satisfies all of
/// them trivially; reporting that as a pass would put "these sensors are synchronized" in a report
/// and a signed certificate on the strength of a timeline nobody measured.
///
/// The checks that grade a *declared* field instead — `TEMPORAL.INVALID_RATE` and
/// `TEMPORAL.RATE_INCONSISTENT` read `declared_rate_hz` out of the manifest — deliberately do not
/// filter: a nonsense declared rate is wrong whatever the timestamps are.
fn measured_streams(episode: &Episode) -> impl Iterator<Item = &Stream> {
    episode.streams.iter().filter(|s| s.has_measured_time())
}

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

/// Median of an already-sorted, non-empty slice.
fn median_sorted(sorted: &[i64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    }
}

/// A stream's own sampling period: the median positive inter-frame interval, in nanoseconds —
/// falling back to the rate the source declares when there are too few intervals to take a median,
/// and 0 when the source declares none either.
///
/// This is the *quantum* of a span comparison. A stream observing a window of length `W` at period
/// `T` spans `floor(W / T) * T`, so its measured span understates `W` by up to one full period —
/// even with a perfect clock. Two synchronized sensors therefore differ in span by up to the larger
/// of their two periods, with no drift whatever: a 10 Hz LiDAR against a 100 Hz IMU carries an
/// intrinsic 100 ms difference, twice the default 50 ms tolerance. Any check comparing spans across
/// streams must widen its tolerance by this, or it reports every honest multi-rate rig as skewed.
pub(crate) fn sampling_period_ns(stream: &Stream) -> i64 {
    let mut intervals: Vec<i64> = stream
        .frames
        .windows(2)
        .map(|w| w[1].ts.saturating_sub(w[0].ts))
        .filter(|d| *d > 0)
        .collect();
    // A single interval is not a cadence, and treating it as one turns this widening against the
    // checks it exists to protect. The allowance is `tolerance + max(period)`, so a stream whose
    // only two frames sit at 0 s and 10 s reports a 10-second "period" — and a rig where that
    // sensor died after two frames while every other stream covered one second gets a ten-second
    // allowance, under which no drift of any size can be reported. One interval is equally
    // consistent with a 0.1 Hz sensor and a sensor that fired twice and stopped; the second is
    // exactly what these checks exist to catch, so the cadence is not *guessed* from it.
    //
    // But the source often *states* the cadence, and a stated rate is not a guess. Returning 0 here
    // meant a slow sensor that lands exactly two samples in a short episode — a 1 Hz LiDAR beside a
    // 100 Hz IMU — got no allowance at all, and its intrinsic one-period span difference was
    // reported as a 990 ms `TEMPORAL.CLOCK_SKEW` **error** on a perfectly synchronized rig. The
    // check flipped between clean and headline error on whether the LiDAR caught 2 samples or 3.
    //
    // So fall back to the declared period, bounded by the one interval actually observed. That
    // bound is what keeps the died-early sensor honest: its declared rate is fast, so it still
    // gets a small allowance, and a corrupt declaration of 0.001 Hz cannot buy a 1000-second one.
    if intervals.len() < 2 {
        let declared = stream
            .declared_rate_hz
            .filter(|hz| hz.is_finite() && *hz > 0.0)
            .map(|hz| (NS_PER_S / hz) as i64)
            .filter(|p| *p > 0);
        return match (declared, intervals.first()) {
            (Some(period), Some(observed)) => period.min(*observed),
            (Some(period), None) => period,
            (None, _) => 0,
        };
    }
    intervals.sort_unstable();
    median_sorted(&intervals).max(0.0) as i64
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
            for stream in measured_streams(ep) {
                // Streams sharing one timeline (a CAN or MF4 group is dozens of channels off a single
                // clock) share the fault too: one repeated timestamp is one defect, not one per
                // channel. Report it against the first stream on that timeline and name the rest, as
                // `Gaps` and `Jitter` already do — at Error severity, eight channels otherwise cost
                // eight deductions for a single stuck clock.
                let (is_representative, shared) = timeline_group(ep, stream);
                if !is_representative {
                    continue;
                }
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
                                     frame {i} ({prev} -> {cur}){}",
                                    stream.name,
                                    ep.index,
                                    shared_suffix(shared)
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
            for stream in measured_streams(ep) {
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

/// A fingerprint of a stream's timeline — the exact timestamp sequence.
///
/// Several streams in one episode routinely share a timeline: an MF4 channel group samples every
/// channel on one raster, and a CAN message decodes into many signals off the same frames. Their
/// timing is one fact, not N, so the per-stream timeline checks report it once and name how many
/// streams carry it, instead of emitting the same finding per stream (one 8-channel event-driven
/// group otherwise produced 32 warnings for 4 root causes, flooring the trust score).
fn timeline_fingerprint(stream: &Stream) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    stream.frames.len().hash(&mut h);
    for f in &stream.frames {
        f.ts.hash(&mut h);
    }
    h.finish()
}

/// How many streams in `ep` share `stream`'s timeline, and whether `stream` is the first of them by
/// name (the representative that reports). Returns `(is_representative, shared_count)`.
fn timeline_group(ep: &crate::cdm::Episode, stream: &Stream) -> (bool, usize) {
    let fp = timeline_fingerprint(stream);
    let mut sharing: Vec<&str> = ep
        .streams
        .iter()
        .filter(|s| timeline_fingerprint(s) == fp)
        .map(|s| s.name.as_str())
        .collect();
    sharing.sort_unstable();
    let first = sharing.first().copied().unwrap_or(stream.name.as_str());
    (first == stream.name.as_str(), sharing.len())
}

/// The suffix naming the other streams a finding also covers, empty when the timeline is unique.
fn shared_suffix(shared: usize) -> String {
    if shared > 1 {
        format!(" (and {} other stream(s) on the same timeline)", shared - 1)
    } else {
        String::new()
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
    /// Expected inter-frame interval (ns) to measure gaps against.
    ///
    /// The declared rate is preferred — a gap is a frame missing relative to the intended cadence —
    /// but only when it is roughly consistent with the stream's own median interval. A grossly
    /// overstated rate (declared 1 kHz on a 10 Hz stream) would otherwise make the expected interval
    /// tiny and flag *every* real interval as a gap, drowning the report in spurious findings for what
    /// is really one wrong-rate value (already reported by `RateConformance`/`RateValidity`). When the
    /// declared and observed intervals disagree by more than `gap_factor`, the declared rate can't be
    /// trusted as the baseline, so the observed median is used instead.
    fn expected_interval_ns(stream: &Stream, gap_factor: f64) -> Option<f64> {
        let observed_median = {
            let mut intervals: Vec<i64> = stream
                .frames
                .windows(2)
                .map(|w| w[1].ts.saturating_sub(w[0].ts))
                .filter(|d| *d > 0)
                .collect();
            if intervals.is_empty() {
                None
            } else {
                intervals.sort_unstable();
                Some(intervals[intervals.len() / 2] as f64)
            }
        };

        if let Some(rate) = stream.declared_rate_hz {
            if rate > 0.0 {
                let declared = NS_PER_S / rate;
                match observed_median {
                    // Declared and observed agree (within the gap factor): trust the declared cadence.
                    Some(obs) if declared <= obs * gap_factor && obs <= declared * gap_factor => {
                        return Some(declared)
                    }
                    // They disagree wildly (corrupt declared rate) — fall back to the observed median
                    // rather than flooding with gaps. With no positive intervals at all, abstain.
                    Some(obs) => return Some(obs),
                    None => return None,
                }
            }
        }
        observed_median
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
            for stream in measured_streams(ep) {
                if stream.frames.len() < 2 {
                    continue;
                }
                // One timeline, one report — see `timeline_group`.
                let (is_representative, shared) = timeline_group(ep, stream);
                if !is_representative {
                    continue;
                }
                let Some(expected) = Gaps::expected_interval_ns(stream, self.gap_factor) else {
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
                                    "stream `{}` in episode {}: {:.1} ms gap (expected ~{:.1} ms){}",
                                    stream.name,
                                    ep.index,
                                    interval / 1e6,
                                    expected / 1e6,
                                    shared_suffix(shared)
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
            for stream in measured_streams(ep) {
                // One timeline, one report — see `timeline_group`.
                let (is_representative, shared) = timeline_group(ep, stream);
                if !is_representative {
                    continue;
                }
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
                                 {cv:.2} vs allowed {:.2}; mean ~{:.1} ms){}",
                                stream.name,
                                ep.index,
                                self.max_cv,
                                mean / 1e6,
                                shared_suffix(shared),
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
            // On an autonomy rig, cross-sensor sync is reported rig-wide by `AUTONOMY.RIG_SYNC` (one
            // finding), so skip the pairwise report here to avoid O(n²) duplicate findings. A
            // manipulation dataset is never a rig, so its behavior is unchanged.
            if crate::checks::autonomy::is_rig_episode(ep) {
                continue;
            }
            // (name, clock_id, span, sampling period) for streams with a measurable span.
            let spans: Vec<(&str, &str, i64, i64)> = ep
                .streams
                .iter()
                .filter(|s| s.has_measured_time())
                .filter_map(|s| {
                    span_bounds(s).map(|(lo, hi)| {
                        (
                            s.name.as_str(),
                            s.clock_id.as_str(),
                            hi.saturating_sub(lo),
                            sampling_period_ns(s),
                        )
                    })
                })
                .filter(|(_, _, span, _)| *span > 0)
                .collect();

            for i in 0..spans.len() {
                for j in (i + 1)..spans.len() {
                    let (name_a, clock_a, span_a, period_a) = spans[i];
                    let (name_b, clock_b, span_b, period_b) = spans[j];
                    let drift = (span_a - span_b).abs();
                    // Two synchronized streams sampling the same window differ in span by up to the
                    // larger of their periods (see `sampling_period_ns`), so that quantum is added to
                    // the tolerance. Without it every multi-rate pairing — a 30 fps camera against a
                    // 10 Hz state stream — reports drift it does not have.
                    let allowance = self.tolerance_ns.saturating_add(period_a.max(period_b));
                    if drift > allowance {
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
            let mut by_clock: BTreeMap<&str, Vec<(&str, i64, i64)>> = BTreeMap::new();
            for s in measured_streams(ep) {
                if let Some((lo, _hi)) = span_bounds(s) {
                    by_clock.entry(s.clock_id.as_str()).or_default().push((
                        s.name.as_str(),
                        lo,
                        sampling_period_ns(s),
                    ));
                }
            }
            for (clock, mut starts) in by_clock {
                if starts.len() < 2 {
                    continue;
                }
                // Earliest and latest starting streams on this clock.
                starts.sort_by_key(|(_, start, _)| *start);
                let (early_name, early_start, early_period) = starts[0];
                let (late_name, late_start, late_period) = starts[starts.len() - 1];
                let offset = late_start.saturating_sub(early_start);
                // The same sampling quantum `ClockSkew` allows for, and for the same reason: a
                // sensor sampling every `T` cannot place its first sample nearer the true start of
                // the recording than `T`, so two perfectly synchronized streams differ in start by
                // up to the slower one's period with no offset at all. Every adapter that puts its
                // streams on one shared clock — MCAP, LeRobot, CAN+DBC — reported this on any rig
                // carrying a sensor slower than 20 Hz, unconditionally, on honest data.
                let allowance = self
                    .tolerance_ns
                    .saturating_add(early_period.max(late_period));
                if offset > allowance {
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
            let mut by_clock: BTreeMap<&str, Vec<(&str, i64, i64)>> = BTreeMap::new();
            for s in measured_streams(ep) {
                if let Some((_lo, hi)) = span_bounds(s) {
                    by_clock.entry(s.clock_id.as_str()).or_default().push((
                        s.name.as_str(),
                        hi,
                        sampling_period_ns(s),
                    ));
                }
            }
            for (clock, mut ends) in by_clock {
                if ends.len() < 2 {
                    continue;
                }
                // Earliest- and latest-ending streams on this clock.
                ends.sort_by_key(|(_, end, _)| *end);
                let (early_name, early_end, early_period) = ends[0];
                let (late_name, late_end, late_period) = ends[ends.len() - 1];
                let offset = late_end.saturating_sub(early_end);
                // Widened by the sampling quantum, exactly as the start-offset and clock-skew checks
                // are: a stream's last sample lands up to its own period before the true end.
                let allowance = self
                    .tolerance_ns
                    .saturating_add(early_period.max(late_period));
                if offset > allowance {
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
            .filter_map(|ep| ep.duration_ns().map(|d| (ep.index, d)))
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

/// Streams whose timestamps are not measured time — and therefore what the rest of this family could
/// not grade.
///
/// This check exists because of what its absence looked like. A source that records no clock (RLDS/
/// TFDS has no per-step timestamp at all) still needs *some* ordering, so its frames carry a step
/// index. Every other temporal check then compares those indices and passes: the timeline is
/// flawlessly monotonic, the intervals are perfectly regular, and every stream in an episode starts,
/// ends, and spans identically. A run over such a dataset reported zero temporal findings and a
/// signed certificate recorded ten temporal checks executed with nothing skipped — which reads as
/// "these sensors are synchronized" when the truth is "there was never anything here to measure".
///
/// So the abstention is reported rather than left as silence, and it is reported as a *finding*,
/// which is the only disclosure that travels: findings reach the terminal report, the JSON, the
/// SARIF, the HTML, and the certificate's own summary. An ingest report's coverage note reaches none
/// of those.
///
/// Informational, not a defect: a dataset is not worse for the format it was published in. What it
/// changes is what a passing verdict is *evidence of*.
pub struct ClockMeasurability;

impl Check for ClockMeasurability {
    fn id(&self) -> &'static str {
        "temporal.clock-measurability"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEMPORAL.UNMEASURED_CLOCK"]
    }
    fn title(&self) -> &'static str {
        "Timestamps are measured time"
    }
    fn category(&self) -> Category {
        Category::Temporal
    }
    fn default_severity(&self) -> Severity {
        Severity::Info
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // Reported once for the dataset, by clock: the clock is a property of the source format, so
        // one finding per episode would be the same fact repeated for every episode in the dataset.
        let mut unmeasured: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                if stream.clock_kind == ClockKind::StepIndex {
                    unmeasured
                        .entry(stream.clock_id.as_str())
                        .or_default()
                        .insert(stream.name.as_str());
                }
            }
        }
        unmeasured
            .into_iter()
            .map(|(clock, streams)| {
                let names: Vec<&str> = streams.into_iter().collect();
                // Naming a few is useful; naming four hundred is not.
                let shown = names
                    .iter()
                    .take(4)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ");
                let rest = names.len().saturating_sub(4);
                let listed = if rest > 0 {
                    format!("{shown} and {rest} more")
                } else {
                    shown
                };
                Finding::new(
                    self.id(),
                    Category::Temporal,
                    Severity::Info,
                    Location::Dataset,
                    "TEMPORAL.UNMEASURED_CLOCK",
                    format!(
                        "clock `{clock}` carries a step index, not measured time, so the rate, \
                         gap, jitter, clock-skew, start/end-offset and episode-duration checks \
                         could not grade {} stream(s) on it ({listed})",
                        names.len(),
                    ),
                )
                .with_risk(
                    "The source records no clock, so nothing in this run can tell you whether the \
                     sensors were synchronized, whether frames were dropped, or how long an episode \
                     actually lasted. A clean temporal result here is the absence of a measurement, \
                     not evidence of good timing.",
                )
                .with_remedy(
                    "Treat the temporal result as unverified for these streams. If timing matters \
                     for your use, check it against the source recording the dataset was converted \
                     from, or re-export in a format that carries per-frame timestamps.",
                )
            })
            .collect()
    }
}
