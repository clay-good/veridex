//! Temporal checks: per-stream timeline health and the headline cross-stream clock-skew check.

use crate::cdm::{Dataset, Stream};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

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

/// Timestamp monotonicity: within a stream, timestamps must strictly increase. A decrease or repeat
/// means frames are out of order or duplicated.
pub struct Monotonicity;

impl Check for Monotonicity {
    fn id(&self) -> &'static str {
        "temporal.monotonicity"
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
