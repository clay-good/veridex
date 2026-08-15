//! Autonomy sensor-rig checks (`autonomy-sensor-data`). The first is rig-wide time sync, the
//! N-sensor generalization of the core pairwise [`ClockSkew`](crate::checks::temporal::ClockSkew).

use crate::cdm::{Dataset, Episode, Stream};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Number of AV-native rig sensors (LiDAR/IMU/GNSS/CAN/ego-pose) an episode must carry to be treated
/// as a sensor rig. At this threshold the dataset is unambiguously an autonomy rig, not a manipulation
/// dataset, so cross-sensor sync is reported rig-wide (`AUTONOMY.RIG_SYNC`) rather than as the pairwise
/// `TEMPORAL.CLOCK_SKEW` — which would emit O(n²) findings for one drifting sensor. A manipulation
/// dataset has zero such sensors, so it never enters rig mode and `TEMPORAL.CLOCK_SKEW` is unchanged.
pub const RIG_SENSOR_THRESHOLD: usize = 3;

/// Whether an episode is an autonomy sensor rig: it carries at least [`RIG_SENSOR_THRESHOLD`] streams
/// with an AV-native rig modality. Shared with [`ClockSkew`](crate::checks::temporal::ClockSkew),
/// which skips rig episodes so the two checks never double-report cross-sensor sync.
pub fn is_rig_episode(ep: &Episode) -> bool {
    ep.streams
        .iter()
        .filter(|s| s.modality.is_rig_sensor())
        .count()
        >= RIG_SENSOR_THRESHOLD
}

/// `min_ts`, `max_ts` over a stream's frames (frames are not assumed sorted). Duplicated from the
/// temporal module's private helper to keep the two check families decoupled.
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

/// **Rig-wide time sync (design A2/A4).** Across an autonomy rig's sensors, the streams recorded over
/// one drive segment should span the same real-time duration. The rig-wide *spread* — the widest
/// sensor span minus the tightest — is the worst cross-sensor drift; when it exceeds the tolerance a
/// sensor came online late, dropped out early, or ran on a drifting clock, silently mis-aligning that
/// sensor from the rest of the rig. This is the N-sensor generalization of the pairwise
/// [`ClockSkew`](crate::checks::temporal::ClockSkew): it emits **one** finding per episode naming the
/// tightest- and widest-spanning sensors and the spread, instead of O(n²) pairwise findings. Like
/// `ClockSkew` it compares *durations*, so it needs no shared epoch across differing clocks.
pub struct RigSync {
    /// Maximum tolerated spread (widest − tightest span) across the rig's sensors, in nanoseconds.
    pub tolerance_ns: i64,
}

impl Default for RigSync {
    fn default() -> Self {
        RigSync {
            tolerance_ns: 50_000_000, // 50 ms, matching ClockSkew
        }
    }
}

impl Check for RigSync {
    fn id(&self) -> &'static str {
        "autonomy.rig-sync"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["AUTONOMY.RIG_SYNC"]
    }
    fn title(&self) -> &'static str {
        "Rig-wide time sync"
    }
    fn category(&self) -> Category {
        Category::Autonomy
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
            if !is_rig_episode(ep) {
                continue;
            }
            // Every sensor with a measurable span (name, span), sorted by (span, name) so the tightest
            // and widest are deterministic and ties break stably.
            let mut spans: Vec<(&str, i64)> = ep
                .streams
                .iter()
                .filter_map(|s| span_bounds(s).map(|(lo, hi)| (s.name.as_str(), hi - lo)))
                .filter(|(_, span)| *span > 0)
                .collect();
            if spans.len() < 2 {
                continue;
            }
            spans.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
            let (tight_name, tight_span) = spans[0];
            let (wide_name, wide_span) = spans[spans.len() - 1];
            let spread = wide_span - tight_span;
            if spread > self.tolerance_ns {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Autonomy,
                        Severity::Error,
                        Location::Episode { episode: ep.index },
                        "AUTONOMY.RIG_SYNC",
                        format!(
                            "episode {}: rig sensors are out of sync — `{tight_name}` spans \
                             {:.1} ms but `{wide_name}` spans {:.1} ms, a {:.1} ms drift across \
                             {} sensors",
                            ep.index,
                            tight_span as f64 / 1e6,
                            wide_span as f64 / 1e6,
                            spread as f64 / 1e6,
                            spans.len(),
                        ),
                    )
                    .with_risk(
                        "A sensor out of sync with the rest of the rig mis-aligns its observations \
                         from every other sensor and from the ego trajectory, so fused perception \
                         and world-model training learn from inconsistent snapshots of the scene.",
                    )
                    .with_remedy(
                        "Re-synchronize the rig against a common time base, or record and apply \
                         per-sensor trigger/latency offsets before fusing.",
                    ),
                );
            }
        }
        findings
    }
}

/// **Rig sequence completeness (design A2).** Over an episode, each rig sensor should deliver frames
/// steadily at its own cadence; a sensor that quietly drops a fraction of its frames leaves the rig
/// with incomplete per-tick snapshots — holes a world model trains straight through. This measures the
/// **aggregate** drop rate: the observed frame count against the count the sensor's own median
/// inter-frame interval implies over its active span, flagging a shortfall beyond
/// [`SequenceComplete::max_drop_fraction`].
///
/// It is complementary to the per-stream checks: [`Gaps`](crate::checks::temporal::Gaps) catches a
/// *single* oversized interval, and [`RateConformance`](crate::checks::temporal::RateConformance)
/// needs a *declared* rate (which MCAP rigs don't carry) — a rig sensor can slip past both while
/// still dropping, say, 15% of its frames as many small holes. The median-interval baseline is robust
/// to those holes (most intervals are still nominal), so it needs no declared rate and no shared clock.
/// Only rig episodes are checked, and only streams with enough frames for a stable median.
pub struct SequenceComplete {
    /// Maximum tolerated fraction of expected frames a sensor may be missing before it is flagged.
    pub max_drop_fraction: f64,
}

impl Default for SequenceComplete {
    fn default() -> Self {
        SequenceComplete {
            max_drop_fraction: 0.05, // 5%
        }
    }
}

impl SequenceComplete {
    /// Minimum frames for a stable median inter-frame interval; below this the drop estimate is noise.
    const MIN_FRAMES: usize = 8;
}

impl Check for SequenceComplete {
    fn id(&self) -> &'static str {
        "autonomy.sequence-complete"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["AUTONOMY.SEQUENCE_COMPLETE"]
    }
    fn title(&self) -> &'static str {
        "Rig sequence completeness"
    }
    fn category(&self) -> Category {
        Category::Autonomy
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
            if !is_rig_episode(ep) {
                continue;
            }
            for stream in &ep.streams {
                if stream.frames.len() < Self::MIN_FRAMES {
                    continue;
                }
                // Median positive inter-frame interval — the sensor's own cadence, robust to the very
                // drops we are hunting (a minority of doubled intervals doesn't move the median).
                let mut intervals: Vec<i64> = stream
                    .frames
                    .windows(2)
                    .map(|w| w[1].ts.saturating_sub(w[0].ts))
                    .filter(|d| *d > 0)
                    .collect();
                if intervals.is_empty() {
                    continue;
                }
                intervals.sort_unstable();
                let median = intervals[intervals.len() / 2] as f64;
                let Some((lo, hi)) = span_bounds(stream) else {
                    continue;
                };
                let span = (hi - lo) as f64;
                if median <= 0.0 || span <= 0.0 {
                    continue;
                }
                // Frames the cadence implies over the active span, vs what actually arrived.
                let expected = span / median + 1.0;
                let observed = stream.frames.len() as f64;
                if observed >= expected {
                    continue;
                }
                let drop_fraction = (expected - observed) / expected;
                if drop_fraction > self.max_drop_fraction {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Autonomy,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "AUTONOMY.SEQUENCE_COMPLETE",
                            format!(
                                "episode {}: sensor `{}` dropped ~{:.0}% of its frames — {} arrived \
                                 but its ~{:.1} ms cadence over the episode implies ~{:.0}",
                                ep.index,
                                stream.name,
                                drop_fraction * 100.0,
                                stream.frames.len(),
                                median / 1e6,
                                expected,
                            ),
                        )
                        .with_risk(
                            "A sensor missing a fraction of its frames leaves the rig with incomplete \
                             per-tick snapshots: fusion and world-model training fill or skip the \
                             holes, learning from moments where that modality was absent.",
                        )
                        .with_remedy(
                            "Investigate the recorder/transport for that sensor (bandwidth, buffer \
                             overruns, cabling); re-record or mark the affected segments.",
                        ),
                    );
                }
            }
        }
        findings
    }
}
