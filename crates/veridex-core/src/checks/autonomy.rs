//! Autonomy sensor-rig checks (`autonomy-sensor-data`). The first is rig-wide time sync, the
//! N-sensor generalization of the core pairwise [`ClockSkew`](crate::checks::temporal::ClockSkew).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cdm::{Dataset, Episode, Modality, Stream, Transform};
use crate::check::{Category, Check, CheckContext, Finding, Location, Scope, Severity};

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
    let sensors = ep.streams.iter().filter(|s| s.modality.is_rig_sensor());
    let mut modalities = std::collections::BTreeSet::new();
    let mut count = 0usize;
    for s in sensors {
        modalities.insert(s.modality.tag());
        count += 1;
    }
    // Sensor count alone is not a rig. A CAN or MF4 measurement is dozens of `CanSignal` streams off
    // one bus — not several sensors observing the world from different places, which is what the rig
    // checks reason about. Requiring two distinct AV-native modalities keeps a bus-only log out of
    // rig mode (where it would trip rig-wide sync on ordinary raster differences) without excluding
    // any real rig, which always mixes LiDAR/IMU/GNSS/ego-pose.
    count >= RIG_SENSOR_THRESHOLD && modalities.len() >= 2
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
            // Every sensor with a measurable span (name, span, sampling period), sorted by
            // (span, name) so the tightest and widest are deterministic and ties break stably.
            //
            // `is_sensor`, not "every stream in the episode". A rig log carries more than its rig:
            // `/rosout`, `/parameter_events`, `/diagnostics`, a latched transform tree, a
            // `CameraInfo` channel. None of them samples the world, none keeps a sensor's cadence,
            // and all of them are routinely short of the recording's window — so comparing their
            // spans against a LiDAR's reported a synchronized rig as out of sync, at error severity,
            // naming a log topic as the sensor that drifted. Their timing is still covered, by
            // `TEMPORAL.START_OFFSET` / `TEMPORAL.END_OFFSET`, which say what is actually true about
            // them without claiming they are sensors.
            let mut spans: Vec<(&str, i64, i64)> = ep
                .streams
                .iter()
                .filter(|s| s.modality.is_sensor())
                .filter_map(|s| {
                    span_bounds(s).map(|(lo, hi)| {
                        (
                            s.name.as_str(),
                            hi.saturating_sub(lo),
                            crate::checks::temporal::sampling_period_ns(s),
                        )
                    })
                })
                .filter(|(_, span, _)| *span > 0)
                .collect();
            if spans.len() < 2 {
                continue;
            }
            spans.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));
            let (tight_name, tight_span, tight_period) = spans[0];
            let (wide_name, wide_span, wide_period) = spans[spans.len() - 1];
            let spread = wide_span - tight_span;
            // A rig is multi-rate by construction — a 10 Hz LiDAR beside a 100 Hz IMU beside a 5 Hz
            // GNSS — and each sensor's span quantizes to its own period, so a perfectly synchronized
            // rig shows a spread of up to the slower sensor's period with no drift at all. Widen the
            // tolerance by that quantum (see `temporal::sampling_period_ns`); without it the check
            // fires on every honest rig carrying a sensor slower than 20 Hz.
            let allowance = self
                .tolerance_ns
                .saturating_add(tight_period.max(wide_period));
            if spread > allowance {
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

/// **GNSS plausibility (design A2 follow-up).** A satellite fix is the one rig measurement whose
/// validity has an absolute, physical answer: a latitude outside ±90° or a longitude outside ±180°
/// is not a place. When one appears, the receiver, the unit conversion, or the field order is wrong
/// — and every downstream use of the trajectory is wrong with it, silently, because the numbers look
/// like coordinates.
///
/// The second case is a fix at exactly `(0, 0)` for the whole recording. Null Island is a real point
/// in the Gulf of Guinea, so this is judged by **exact equality across every frame**: a receiver that
/// never acquired a fix reports precisely zero, and a vehicle that genuinely drove there would not
/// hold six decimal places of zero for an entire log. That is the same reasoning
/// `STATISTICAL.SATURATED` rests on, and it is what keeps this free of false positives.
///
/// Reads the per-dimension statistics the `NavSatFix` decode produces, so it needs no per-frame
/// join. Silent on a rig whose GNSS was never decoded — `STATISTICAL.UNMEASURED_VALUES` says that,
/// and a check that cannot see the values must not report them plausible.
pub struct GnssPlausibility;

/// Where the `NavSatFix` decode puts each coordinate. Matched by name, not by position, so a future
/// decoder that adds a dimension cannot silently shift which one is checked against which bound.
const GNSS_BOUNDS: &[(&str, f64, f64)] = &[("latitude", -90.0, 90.0), ("longitude", -180.0, 180.0)];

impl Check for GnssPlausibility {
    fn id(&self) -> &'static str {
        "autonomy.gnss-plausibility"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["AUTONOMY.GNSS_IMPLAUSIBLE", "AUTONOMY.GNSS_UNSET"]
    }
    fn title(&self) -> &'static str {
        "GNSS coordinate plausibility"
    }
    fn category(&self) -> Category {
        Category::Autonomy
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
            for stream in ep.streams.iter().filter(|s| s.modality == Modality::Gnss) {
                let (Some(names), Some(dims)) = (&stream.dim_names, &stream.observed_dim_stats)
                else {
                    continue;
                };
                let stat_for = |wanted: &str| {
                    names
                        .iter()
                        .position(|n| n == wanted)
                        .and_then(|i| dims.iter().find(|d| d.dim as usize == i))
                        .map(|d| d.stats)
                };

                for (name, low, high) in GNSS_BOUNDS {
                    let Some(stats) = stat_for(name) else {
                        continue;
                    };
                    // Either bound broken is enough: the extreme is a real recorded value.
                    let (out, value) = if stats.min < *low {
                        (true, stats.min)
                    } else if stats.max > *high {
                        (true, stats.max)
                    } else {
                        (false, 0.0)
                    };
                    if out {
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Autonomy,
                                Severity::Error,
                                Location::Stream {
                                    episode: ep.index,
                                    stream: stream.name.clone(),
                                },
                                "AUTONOMY.GNSS_IMPLAUSIBLE",
                                format!(
                                    "episode {}: stream `{}` records a {name} of {value}, outside \
                                     the possible range [{low}, {high}] — that is not a place",
                                    ep.index, stream.name
                                ),
                            )
                            .with_risk(
                                "A coordinate outside the possible range means the receiver, the \
                                 unit conversion, or the field order is wrong. Every use of the \
                                 trajectory — geo-referencing, map association, cross-drive \
                                 alignment — is wrong with it, and silently, because the numbers \
                                 still look like coordinates.",
                            )
                            .with_remedy(
                                "Check the receiver's output units and the message field order \
                                 (degrees, not radians or scaled integers), then re-record or \
                                 re-convert the affected segment.",
                            ),
                        );
                    }
                }

                // Every fix at exactly (0, 0): a receiver that never acquired one.
                let unset = ["latitude", "longitude"]
                    .iter()
                    .all(|n| stat_for(n).is_some_and(|s| s.min == 0.0 && s.max == 0.0));
                if unset {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Autonomy,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "AUTONOMY.GNSS_UNSET",
                            format!(
                                "episode {}: stream `{}` reports latitude and longitude of exactly \
                                 0 for every frame — a receiver that never acquired a fix, not a \
                                 drive through the Gulf of Guinea",
                                ep.index, stream.name
                            ),
                        )
                        .with_risk(
                            "A trajectory anchored at Null Island places the whole recording \
                             somewhere the vehicle never was. Anything that fuses the drive with a \
                             map, or compares it against another drive, is aligned to the wrong \
                             point on Earth.",
                        )
                        .with_remedy(
                            "Confirm the receiver had a fix for the recording (its `status` field \
                             says so) and drop or re-record the segment that did not.",
                        ),
                    );
                }
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
    /// Above this coefficient of variation the stream has no steady cadence to fall short of, so the
    /// drop estimate is meaningless and the check abstains (see the guard in `run`).
    ///
    /// Lowered from 0.5, which was not consistent with the 5% default drop threshold it guards: on a
    /// 401-frame stream with gaussian jitter and **nothing dropped**, an interval CV of 0.44 measured
    /// "~6% of its frames" and 0.47 measured "~7%" — both inside the zone this constant declared
    /// honest. At high jitter a single interval reaches twice the median by chance often enough that
    /// nothing distinguishes it from a hole, so the estimate is not merely noisy, it is unfounded.
    const MAX_INTERVAL_CV: f64 = 0.40;

    /// Minimum frames for a stable median inter-frame interval; below this the drop estimate is noise.
    const MIN_FRAMES: usize = 8;

    /// How far an inter-frame interval may sit from an exact multiple of the cadence and still be
    /// read as swallowed frames, in units of the cadence. A dropped frame leaves a hole near `k·T`;
    /// an idle burst lands anywhere, so it is not counted.
    ///
    /// Narrowed from 0.25 alongside the CV gate: a window of `[1.75, 2.25]·T` is wide enough that
    /// ordinary jitter walks into it, and each visit was charged as a swallowed frame. Together the
    /// two constants were measured over 40 honest jittery streams (CV 0.1 to 0.45, no drops) with no
    /// false positive, while still catching a real 10% drop rate.
    const MULTIPLE_TOLERANCE: f64 = 0.15;
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
                // A median cadence only means something for a stream that *has* a cadence. An
                // event-driven signal — a change-triggered CAN channel, say — arrives in bursts with
                // long idles, so "frames the median implies over the span" is not a target it ever
                // aimed at, and comparing against it reports a complete log as 88% dropped. Abstain
                // when the intervals are far from uniform; `TEMPORAL.JITTER` is the check for that
                // shape, and a genuinely dropping steady stream stays well inside this bound (5%
                // dropped frames put the CV near 0.2).
                let mean =
                    intervals.iter().map(|d| *d as f64).sum::<f64>() / intervals.len() as f64;
                if mean > 0.0 {
                    let variance = intervals
                        .iter()
                        .map(|d| (*d as f64 - mean).powi(2))
                        .sum::<f64>()
                        / intervals.len() as f64;
                    if variance.sqrt() / mean > Self::MAX_INTERVAL_CV {
                        continue;
                    }
                }
                if median <= 0.0 {
                    continue;
                }
                // Count the frames that are actually missing, rather than dividing the span by the
                // median. A dropped frame leaves a hole that is a *multiple* of the cadence — two
                // periods where one was due — so only an interval close to an integer multiple is
                // counted, and each contributes the `k - 1` frames it swallowed. An interval of no
                // particular relation to the cadence is not a drop: it is a stream that idles, which
                // the span/median estimator charged as missing frames and reported a complete
                // event-driven log as a quarter dropped.
                let missing: f64 = intervals
                    .iter()
                    .map(|d| {
                        let ratio = *d as f64 / median;
                        let k = ratio.round();
                        if k >= 2.0 && (ratio - k).abs() <= Self::MULTIPLE_TOLERANCE {
                            k - 1.0
                        } else {
                            0.0
                        }
                    })
                    .sum();
                if missing <= 0.0 {
                    continue;
                }
                let observed = stream.frames.len() as f64;
                let expected = observed + missing;
                let drop_fraction = missing / expected;
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
                                "episode {}: sensor `{}` dropped ~{:.0}% of its frames — {} arrived, \
                                 and the gaps at multiples of its ~{:.1} ms cadence account for \
                                 ~{:.0} more",
                                ep.index,
                                stream.name,
                                drop_fraction * 100.0,
                                stream.frames.len(),
                                median / 1e6,
                                missing,
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

/// **Ego-pose continuity (design A2).** The ego vehicle's trajectory should evolve smoothly: between
/// consecutive poses the implied speed (distance moved / elapsed time) must stay physically plausible.
/// A jump — a GPS glitch, a localization reset, a stitched-together log — teleports the ego frame, so
/// every sensor observation after it is registered against a wrong pose. This flags an episode whose
/// `ego_poses` contain a step whose implied speed exceeds [`EgoPoseContinuity::max_speed_mps`],
/// reporting the worst jump and how many occurred. Translations are metres and timestamps nanoseconds
/// (per the CDM), so the speed is in m/s. Needs at least two poses with a positive time delta.
pub struct EgoPoseContinuity {
    /// Maximum plausible ego speed in metres per second; a step implying more is a discontinuity.
    pub max_speed_mps: f64,
}

impl Default for EgoPoseContinuity {
    fn default() -> Self {
        // ~360 km/h — far above any ground vehicle, so only a true teleport/reset trips it.
        EgoPoseContinuity {
            max_speed_mps: 100.0,
        }
    }
}

impl Check for EgoPoseContinuity {
    fn id(&self) -> &'static str {
        "autonomy.ego-pose-continuity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "AUTONOMY.EGO_POSE_CONTINUITY",
            "AUTONOMY.EGO_POSE_NON_FINITE",
        ]
    }
    fn title(&self) -> &'static str {
        "Ego-pose continuity"
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
        const NS_PER_S: f64 = 1_000_000_000.0;
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            let Some(poses) = &ep.ego_poses else {
                continue;
            };
            if poses.len() < 2 {
                continue;
            }
            let mut breaks = 0u64;
            let mut worst_speed = 0.0f64;
            let mut worst_ts = 0i64;
            let mut non_finite = 0u64;
            for pair in poses.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                let dt = b.ts.saturating_sub(a.ts) as f64 / NS_PER_S;
                if dt <= 0.0 {
                    // Non-increasing pose timestamps are a monotonicity fault, not this check's.
                    continue;
                }
                let dx = b.pose.translation[0] - a.pose.translation[0];
                let dy = b.pose.translation[1] - a.pose.translation[1];
                let dz = b.pose.translation[2] - a.pose.translation[2];
                let dist = (dx * dx + dy * dy + dz * dz).sqrt();
                let speed = dist / dt;
                // A non-finite coordinate makes `dist` NaN, and `NaN > max` is false — so the pair
                // was neither flagged nor mentioned, and it poisons *both* pairs it touches. A
                // trajectory reading (0, NaN, 10000) over two seconds hid a genuine 10 km/s teleport
                // and certified clean. Counted and reported instead: this check cannot measure a
                // trajectory it cannot subtract, and saying nothing is the one answer it must not
                // give.
                if !speed.is_finite() {
                    non_finite += 1;
                    continue;
                }
                if speed > self.max_speed_mps {
                    breaks += 1;
                    if speed > worst_speed {
                        worst_speed = speed;
                        worst_ts = b.ts;
                    }
                }
            }
            if non_finite > 0 {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Autonomy,
                        Severity::Error,
                        Location::Episode { episode: ep.index },
                        "AUTONOMY.EGO_POSE_NON_FINITE",
                        format!(
                            "episode {}: {non_finite} ego-trajectory step(s) have a non-finite position, so the distance travelled over them cannot be computed — the trajectory's continuity is unverifiable across those steps, not verified",
                            ep.index
                        ),
                    )
                    .with_risk(
                        "A NaN or infinite pose breaks every geometric use of the trajectory, and it hides the discontinuities on either side of it: the comparison that would catch a teleport silently evaluates to false against a NaN.",
                    )
                    .with_remedy(
                        "Find where the localization output went non-finite and drop or repair those poses before training on the segment.",
                    ),
                );
            }
            if breaks > 0 {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Autonomy,
                        Severity::Error,
                        Location::Episode { episode: ep.index },
                        "AUTONOMY.EGO_POSE_CONTINUITY",
                        format!(
                            "episode {}: ego trajectory has {breaks} discontinuit{} — the worst \
                             implies {worst_speed:.0} m/s at ts {worst_ts} (max plausible {:.0} m/s)",
                            ep.index,
                            if breaks == 1 { "y" } else { "ies" },
                            self.max_speed_mps,
                        ),
                    )
                    .with_risk(
                        "A jump in the ego pose teleports the vehicle frame: every sensor observation \
                         after it is registered against the wrong world pose, so fused maps and any \
                         world model trained on them are geometrically inconsistent.",
                    )
                    .with_remedy(
                        "Inspect the localization/GNSS source for resets or glitches; drop or re-solve \
                         the affected segment, or split the log where it was stitched.",
                    ),
                );
            }
        }
        findings
    }
}

/// The number of connected components in the coordinate-frame graph induced by `transforms` (each
/// transform is an undirected edge between its parent and child frame). One component means every
/// frame can be related to every other; more than one means the tree is split and sensors in different
/// components cannot be spatially related.
fn tf_component_count(transforms: &[Transform]) -> usize {
    // Adjacency over frame names.
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut frames: BTreeSet<&str> = BTreeSet::new();
    for t in transforms {
        adj.entry(&t.parent_frame).or_default().push(&t.child_frame);
        adj.entry(&t.child_frame).or_default().push(&t.parent_frame);
        frames.insert(&t.parent_frame);
        frames.insert(&t.child_frame);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut components = 0;
    for &start in &frames {
        if !seen.insert(start) {
            continue;
        }
        components += 1;
        // BFS the component.
        let mut stack = vec![start];
        while let Some(f) = stack.pop() {
            if let Some(neighbors) = adj.get(f) {
                for &n in neighbors {
                    if seen.insert(n) {
                        stack.push(n);
                    }
                }
            }
        }
    }
    components
}

/// **Rig calibration completeness (design A2, the missing-calibration checks).** Spatial fusion — the
/// whole point of a multi-sensor rig — needs the extrinsic transform (TF) tree relating the sensors
/// and camera intrinsics to project into the image. This is the principle-respecting form of the
/// LiDAR-camera reprojection check: Veridex never decodes the bulk point/pixel payload, so it cannot
/// reproject actual points, but it *can* verify the calibration needed to is present and coherent. On
/// a rig with spatial sensors (point-cloud or camera) it flags: no transform tree at all; a transform
/// tree split into disconnected components (sensors that can't be related); or cameras with no
/// intrinsics. Each is a distinct reason the rig cannot be spatially fused.
pub struct CalibrationCompleteness;

impl Check for CalibrationCompleteness {
    fn id(&self) -> &'static str {
        "autonomy.calibration-completeness"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "AUTONOMY.CALIBRATION_INCOMPLETE",
            "AUTONOMY.CALIBRATION_IMPLAUSIBLE",
            "AUTONOMY.CALIBRATION_AMBIGUOUS",
        ]
    }
    fn title(&self) -> &'static str {
        "Rig calibration completeness"
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
        let flag = |episode: u64, msg: String| {
            Finding::new(
                "autonomy.calibration-completeness",
                Category::Autonomy,
                Severity::Warning,
                Location::Episode { episode },
                "AUTONOMY.CALIBRATION_INCOMPLETE",
                msg,
            )
            .with_risk(
                "Without the extrinsic transform tree and camera intrinsics, sensor observations \
                 cannot be projected into a common frame: LiDAR-camera fusion and any world model \
                 built on it are geometrically undefined.",
            )
            .with_remedy(
                "Record the full static TF tree relating every sensor frame, and a CameraInfo \
                 (intrinsics) for each camera, in the log.",
            )
        };
        // The same shape, for calibration that is present and cannot be used. Error, not warning: a
        // focal length of zero is not a judgment call, it is arithmetic with no answer.
        let unusable = |episode: u64, msg: String| {
            Finding::new(
                "autonomy.calibration-completeness",
                Category::Autonomy,
                Severity::Error,
                Location::Episode { episode },
                "AUTONOMY.CALIBRATION_IMPLAUSIBLE",
                msg,
            )
            .with_risk(
                "Calibration that is present but arithmetically impossible is worse than \
                 calibration that is absent: every presence check passes, so the rig certifies as \
                 fusable while every projection built on it is undefined. It is what an \
                 uncalibrated driver publishes by default.",
            )
            .with_remedy(
                "Calibrate the camera and re-record, or re-publish the `CameraInfo` and static \
                 transforms from the calibration file the rig actually uses.",
            )
        };
        // And for calibration that is present, well-formed edge by edge, and not a tree. Error for
        // the same reason: which of two chains places the sensor is not a judgment call, it is a
        // question the log does not answer.
        let ambiguous = |episode: u64, msg: String| {
            Finding::new(
                "autonomy.calibration-completeness",
                Category::Autonomy,
                Severity::Error,
                Location::Episode { episode },
                "AUTONOMY.CALIBRATION_AMBIGUOUS",
                msg,
            )
            .with_risk(
                "Every consumer resolves the sensor's pose through whichever chain it happens to \
                 walk, so two tools fusing the same log place the same sensor differently and \
                 neither is flagged. The tree is connected and every edge is individually valid, \
                 which is why the completeness and per-sensor frame checks pass on it.",
            )
            .with_remedy(
                "Publish exactly one parent per frame: remove the duplicate broadcaster, or \
                 re-parent the sensor under the mount it is actually measured against. For a \
                 cycle, drop the edge that closes the loop so the tree has a single root.",
            )
        };
        for ep in &dataset.episodes {
            if !is_rig_episode(ep) {
                continue;
            }
            let has_cloud = ep
                .streams
                .iter()
                .any(|s| s.modality == Modality::PointCloud);
            let has_camera = ep.streams.iter().any(|s| s.modality == Modality::Video);
            if !has_cloud && !has_camera {
                continue; // no spatial sensors that need extrinsics/intrinsics
            }

            let transforms = dataset
                .calibration
                .as_ref()
                .map(|c| c.transforms.as_slice())
                .unwrap_or(&[]);
            let intrinsics_empty = dataset
                .calibration
                .as_ref()
                .map(|c| c.intrinsics.is_empty())
                .unwrap_or(true);

            if transforms.is_empty() {
                findings.push(flag(
                    ep.index,
                    format!(
                        "episode {}: the rig has spatial sensors but no transform (TF) tree — the \
                         extrinsics relating the sensors are unknown, so they cannot be fused",
                        ep.index
                    ),
                ));
            } else if !break_is_localizable(ep, transforms) {
                // Deferred to `autonomy.sensor-frame-resolution` only when that check can actually
                // name the stranded sensors — which is what a reader acts on. When it cannot (a
                // sensor that declares no frame, or no camera to anchor connectivity against), this
                // episode-level report is the only warning that exists, so it stays.
                let components = tf_component_count(transforms);
                if components > 1 {
                    findings.push(flag(
                        ep.index,
                        format!(
                            "episode {}: the transform tree is disconnected ({components} separate \
                             components) — sensors in different components cannot be spatially related",
                            ep.index
                        ),
                    ));
                }
            }

            let intrinsics_count = dataset
                .calibration
                .as_ref()
                .map(|c| c.intrinsics.len())
                .unwrap_or(0);
            if has_camera && intrinsics_empty {
                findings.push(flag(
                    ep.index,
                    format!(
                        "episode {}: the rig has camera(s) but no camera intrinsics (CameraInfo) — \
                         projecting points into the image is undefined",
                        ep.index
                    ),
                ));
            } else if let Some(cameras) = distinct_cameras(ep) {
                // Present for *one* camera is not present for the rig. The rule above asks only
                // whether the intrinsics list is empty, so a six-camera surround rig that published
                // a single `CameraInfo` — one driver configured, five not — satisfied it, and the
                // `world-model-ready` calibration criterion reported green over five cameras
                // nothing can project into.
                //
                // Counted, not name-matched: a `CameraInfo` names its own topic
                // (`/camera_front/camera_info`), never the image stream it calibrates, so pairing
                // them means guessing at the ROS namespace convention and accusing whichever camera
                // the guess missed. Arithmetic cannot make that mistake — n cameras and fewer than
                // n intrinsics means at least one camera has none, whichever it is.
                if cameras > intrinsics_count {
                    findings.push(flag(
                        ep.index,
                        format!(
                            "episode {}: the rig carries {cameras} camera(s) but only \
                             {intrinsics_count} set(s) of camera intrinsics (CameraInfo) — at least \
                             {} camera cannot be projected into",
                            ep.index,
                            cameras - intrinsics_count
                        ),
                    ));
                }
            }

            // Present is not the same as usable. An uncalibrated ROS camera driver publishes a
            // `CameraInfo` of all zeros, which satisfies every presence test above while making the
            // projection it exists for undefined — and the rig then certifies as world-model-ready
            // on a camera with no focal length. Only impossibilities are judged, never
            // implausibility: a focal length must be positive and finite, a principal point
            // non-negative and finite, a rotation quaternion an actual rotation.
            for reason in unusable_calibration(dataset) {
                findings.push(unusable(
                    ep.index,
                    format!("episode {}: {reason}", ep.index),
                ));
            }

            // Connected is not the same as unique. Both checks above — and the per-sensor frame
            // resolution that succeeds this one — walk the frame graph undirected, so a tree in
            // which some frame has two parents, or which closes into a loop, satisfies every one of
            // them while the transform between two sensors has more than one answer.
            for reason in ambiguous_calibration(dataset) {
                findings.push(ambiguous(
                    ep.index,
                    format!("episode {}: {reason}", ep.index),
                ));
            }
        }
        findings
    }
    /// Abstains entirely under a metadata-only ingest.
    ///
    /// This check concludes from the *absence* of a transform tree and camera intrinsics — and on a
    /// rig log both are decoded from message bodies, which a metadata-only run does not open. So it
    /// read the absence it created itself and reported a fully calibrated bag as having "no
    /// transform (TF) tree", twice, at warning severity. A check that fires on what a run declined
    /// to look at is measuring the request, not the data.
    ///
    /// Unlike the frame-based checks this is not visible from the CDM alone: a metadata-only rig and
    /// a genuinely uncalibrated one carry an identical `None` calibration, which is exactly why the
    /// ingest's own answer has to be the one consulted. The coverage is disclosed by
    /// `COVERAGE.METADATA_ONLY`, so the silence here is not the reader's only signal.
    fn run_in(&self, dataset: &Dataset, context: &CheckContext) -> Vec<Finding> {
        if !context.frames_read {
            return Vec::new();
        }
        self.run(dataset)
    }
}

/// The set of frame names reachable from `start` in the transform tree, `start` included. Returns an
/// empty set when `start` is not a node of the tree at all.
fn tf_reachable_from<'a>(transforms: &'a [Transform], start: &str) -> HashSet<&'a str> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut known: HashSet<&str> = HashSet::new();
    for t in transforms {
        adj.entry(&t.parent_frame).or_default().push(&t.child_frame);
        adj.entry(&t.child_frame).or_default().push(&t.parent_frame);
        known.insert(&t.parent_frame);
        known.insert(&t.child_frame);
    }
    let Some(&start) = known.get(start) else {
        return HashSet::new();
    };
    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(start);
    let mut stack = vec![start];
    while let Some(f) = stack.pop() {
        if let Some(neighbors) = adj.get(f) {
            for &n in neighbors {
                if seen.insert(n) {
                    stack.push(n);
                }
            }
        }
    }
    seen
}

/// The sensors whose observations are placed in space, and so must resolve through the transform
/// tree: the perception sensors plus the inertial/positioning units that carry a mount pose.
///
/// Deliberately excludes `CanSignal` and `EgoPose`. A bus signal (vehicle speed, steering angle) is a
/// scalar, never projected into an image; and an ego-pose stream's frame (`odom`, `map`) is joined to
/// the body dynamically, not by the static TF tree — demanding a static chain from either would be a
/// threshold meaningful for spatial sensors applied to everything, which is how a check starts
/// flagging honest data.
fn is_spatial_sensor(modality: Modality) -> bool {
    matches!(
        modality,
        Modality::PointCloud | Modality::Video | Modality::Imu | Modality::Gnss
    )
}

/// Whether [`SensorFrameResolution`] can localize a transform-tree break in this episode — the only
/// condition under which [`CalibrationCompleteness`] may stay silent about a disconnected tree.
///
/// The successor speaks about a stream **only if that stream declares a frame**, and its connectivity
/// half needs a camera whose frame the tree knows. So suppression is sound only when both hold for the
/// whole episode: a camera anchors the connectivity question, and every spatial sensor declares a
/// frame. Miss either and the stranded sensor may be one the successor never mentions — leaving the
/// break reported by neither check, which is worse than reporting it twice.
fn break_is_localizable(ep: &Episode, transforms: &[Transform]) -> bool {
    let known: HashSet<&str> = transforms
        .iter()
        .flat_map(|t| [t.parent_frame.as_str(), t.child_frame.as_str()])
        .collect();
    let camera_anchors_the_question = ep
        .streams
        .iter()
        .filter(|s| s.modality == Modality::Video)
        .any(|s| s.frame_id.as_deref().is_some_and(|f| known.contains(f)));
    camera_anchors_the_question
        && ep
            .streams
            .iter()
            .filter(|s| is_spatial_sensor(s.modality))
            .all(|s| s.frame_id.is_some())
}

/// **Per-sensor calibration resolution (design A2, the LiDAR-camera miscalibration class).**
///
/// [`CalibrationCompleteness`] asks whether a rig has a transform tree at all. This asks the question
/// that actually decides whether a fusion pipeline works: for *this* sensor, does a transform chain
/// exist from the frame it stamps its data with to the camera it is meant to be fused against?
///
/// Two ways that fails, and neither is visible from counting the tree's components:
///
/// - **The sensor's frame is not in the tree.** A rig can carry a perfectly connected TF tree that was
///   recorded for `lidar_top` while the LiDAR stamps `lidar_top_v2`. Every geometric operation
///   involving that sensor silently has no transform, and nothing about the tree itself looks wrong.
/// - **The sensor's frame is in the tree but not connected to the camera.** The extrinsics exist for
///   part of the rig and the chain to the image frame is missing, so points cannot be projected.
///
/// Veridex never decodes point coordinates or pixels, so it does not compute a reprojection *error* —
/// it verifies that the reprojection is defined at all. A sensor that declares no frame abstains
/// (nothing was claimed), and a rig with no transform tree abstains too: that is
/// `AUTONOMY.CALIBRATION_INCOMPLETE`, already reported once.
pub struct SensorFrameResolution;

impl Check for SensorFrameResolution {
    fn id(&self) -> &'static str {
        "autonomy.sensor-frame-resolution"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "AUTONOMY.SENSOR_FRAME_UNDECLARED",
            "AUTONOMY.SENSOR_FRAME_UNKNOWN",
            "AUTONOMY.SENSOR_FRAME_UNRELATED",
        ]
    }
    fn title(&self) -> &'static str {
        "Sensor frame resolves through the rig calibration"
    }
    fn category(&self) -> Category {
        Category::Autonomy
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
        let transforms = dataset
            .calibration
            .as_ref()
            .map(|c| c.transforms.as_slice())
            .unwrap_or(&[]);
        if transforms.is_empty() {
            // No tree at all is one defect, reported once by `autonomy.calibration-completeness`.
            return Vec::new();
        }

        let mut findings = Vec::new();
        // The calibration is dataset-level and stream names repeat in every episode, so the same
        // mis-stamped sensor is the same single defect however many episodes it appears in. Claim each
        // (stream, code) once — a 200-episode drive log would otherwise bury the one actionable line
        // under 200 copies of it. Same reason `statistical.rs` dedupes its dataset-level stats checks.
        let mut reported: BTreeSet<(&str, &'static str)> = BTreeSet::new();
        for ep in &dataset.episodes {
            if !is_rig_episode(ep) {
                continue;
            }

            // The camera frames the other sensors have to reach. Only cameras that name a frame the
            // tree knows can serve as a reference; without one there is nothing to measure against and
            // the connectivity half abstains.
            let camera_frames: Vec<&str> = ep
                .streams
                .iter()
                .filter(|s| s.modality == Modality::Video)
                .filter_map(|s| s.frame_id.as_deref())
                .collect();
            let reachable_from_a_camera: HashSet<&str> = camera_frames
                .iter()
                .flat_map(|c| tf_reachable_from(transforms, c))
                .collect();

            for stream in &ep.streams {
                if !is_spatial_sensor(stream.modality) {
                    continue;
                }
                let Some(frame) = stream.frame_id.as_deref() else {
                    // The source declares no frame for a spatial sensor on a rig that *does* carry a
                    // transform tree. Skipping silently was the worst outcome available: the check
                    // then found nothing, the `world-model-ready` criterion it backs read as
                    // satisfied, and a certificate went out attesting "every sensor's own frame
                    // resolves through the tree to a camera" over a rig where not one sensor said
                    // which frame it was in. An unconfigured ROS driver publishing an empty
                    // `header.frame_id` produces exactly this — a well-formed tree beside sensors
                    // nothing in it describes.
                    //
                    // A warning rather than an error: the recording may be perfectly good for
                    // non-geometric use, and Veridex is not claiming the data is wrong. It is saying
                    // it cannot verify this sensor's calibration — which must not be readable as
                    // having verified it. The finding is what blocks readiness.
                    if !reported.insert((stream.name.as_str(), "AUTONOMY.SENSOR_FRAME_UNDECLARED"))
                    {
                        continue;
                    }
                    findings.push(
                        Finding::new(
                            "autonomy.sensor-frame-resolution",
                            Category::Autonomy,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "AUTONOMY.SENSOR_FRAME_UNDECLARED",
                            format!(
                                "episode {}: stream `{}` declares no coordinate frame, so it cannot \
                                 be located in the rig's transform tree — its calibration is \
                                 unverifiable, not verified",
                                ep.index, stream.name
                            ),
                        )
                        .with_risk(
                            "Nothing connects this sensor's data to the rig's geometry, so fusion, \
                             projection, and any world model built on it are unfounded. The tree \
                             being well-formed makes the gap invisible: the calibration checks pass \
                             because there is nothing to contradict, not because anything was \
                             confirmed.",
                        )
                        .with_remedy(
                            "Configure the sensor's driver to stamp its `frame_id` (ROS \
                             `header.frame_id`), then re-record — or re-export the log with the \
                             frame each stream belongs to.",
                        ),
                    );
                    continue;
                };
                let located = tf_reachable_from(transforms, frame);
                if located.is_empty() {
                    if !reported.insert((stream.name.as_str(), "AUTONOMY.SENSOR_FRAME_UNKNOWN")) {
                        continue;
                    }
                    findings.push(
                        Finding::new(
                            "autonomy.sensor-frame-resolution",
                            Category::Autonomy,
                            Severity::Error,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "AUTONOMY.SENSOR_FRAME_UNKNOWN",
                            format!(
                                "episode {}: stream `{}` stamps its data with frame `{frame}`, which \
                                 the rig's transform tree never mentions — this sensor has no \
                                 extrinsics",
                                ep.index, stream.name
                            ),
                        )
                        .with_risk(
                            "Every geometric use of this sensor — fusion, projection, occupancy, any \
                             world model built on the rig — silently has no transform for it. The \
                             calibration looks complete because the tree itself is well-formed; the \
                             sensor simply is not in it.",
                        )
                        .with_remedy(
                            "Reconcile the names: either publish the transform for the frame the \
                             sensor actually stamps, or fix the driver to stamp the frame the \
                             calibration was recorded for.",
                        ),
                    );
                    continue;
                }
                // The sensor is in the tree. Is it connected to a camera?
                if reachable_from_a_camera.is_empty() || camera_frames.contains(&frame) {
                    continue; // no usable camera reference, or this stream is the camera
                }
                if !reachable_from_a_camera.contains(frame) {
                    if !reported.insert((stream.name.as_str(), "AUTONOMY.SENSOR_FRAME_UNRELATED")) {
                        continue;
                    }
                    findings.push(
                        Finding::new(
                            "autonomy.sensor-frame-resolution",
                            Category::Autonomy,
                            Severity::Error,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "AUTONOMY.SENSOR_FRAME_UNRELATED",
                            format!(
                                "episode {}: stream `{}` is in frame `{frame}`, but no chain of \
                                 transforms connects it to any camera frame ({}) — this sensor \
                                 cannot be projected into the image",
                                ep.index,
                                stream.name,
                                camera_frames.join(", ")
                            ),
                        )
                        .with_risk(
                            "LiDAR-camera fusion for this sensor is geometrically undefined: the \
                             extrinsics exist for part of the rig and the chain to the image frame \
                             is missing, so anything that projects its observations is wrong rather \
                             than absent.",
                        )
                        .with_remedy(
                            "Publish the missing link joining this sensor's subtree to the camera's \
                             (typically sensor → base_link → camera), and re-record the calibration.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// Every way a dataset's calibration is present and cannot be used, as sentences naming the element.
///
/// Only impossibilities, never implausibilities. A focal length must be positive and finite — the
/// projection divides by it. A principal point must be finite and non-negative — it is a pixel
/// coordinate. A rotation must be an actual rotation — an all-zero quaternion is the uninitialized
/// value, not a pose. Nothing here judges whether a number is *sensible* for a given camera, which
/// would need the image dimensions the CDM does not carry and would guess where it cannot know.
fn unusable_calibration(dataset: &Dataset) -> Vec<String> {
    let Some(calibration) = &dataset.calibration else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for k in &calibration.intrinsics {
        let positive = |v: f64| v.is_finite() && v > 0.0;
        let pixel = |v: f64| v.is_finite() && v >= 0.0;
        if !positive(k.fx) || !positive(k.fy) {
            out.push(format!(
                "camera `{}` declares a focal length of ({}, {}) — a projection divides by it, so \
                 it must be positive and finite",
                k.stream, k.fx, k.fy
            ));
        } else if !pixel(k.cx) || !pixel(k.cy) {
            out.push(format!(
                "camera `{}` declares a principal point of ({}, {}) — it is a pixel coordinate, so \
                 it must be finite and non-negative",
                k.stream, k.cx, k.cy
            ));
        } else if k.distortion.iter().any(|d| !d.is_finite()) {
            out.push(format!(
                "camera `{}` declares a non-finite distortion coefficient, so undistortion has no \
                 result",
                k.stream
            ));
        }
    }
    for t in &calibration.transforms {
        let q = t.pose.rotation;
        let norm = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
        if t.pose.translation.iter().any(|v| !v.is_finite()) || !norm.is_finite() {
            out.push(format!(
                "the transform `{}` → `{}` holds a non-finite value, so it places nothing",
                t.parent_frame, t.child_frame
            ));
        } else if norm < 1e-6 {
            out.push(format!(
                "the transform `{}` → `{}` carries a zero rotation quaternion, which is not a \
                 rotation — the uninitialized value, not a measured pose",
                t.parent_frame, t.child_frame
            ));
        }
    }
    out
}

/// Every way a dataset's transform tree is present, connected, and still not a tree — as sentences
/// naming the frames.
///
/// [`tf_component_count`] and [`tf_reachable_from`] both walk the frame graph **undirected**, which
/// answers "can these two sensors be related at all" and nothing about whether the relation is
/// *unique*. A transform tree is a tree: every frame has exactly one parent, and there are no
/// cycles. Two shapes break that while leaving the graph connected, so every existing calibration
/// check passes on them:
///
/// - **A frame with two parents.** Two nodes both publish a transform for `lidar_top` — one from
///   `base_link`, one from a `velodyne_base` mount — over overlapping time. tf2 warns
///   (`TF_MULTIPLE_PARENT`) and then resolves the chain through whichever edge it latched, so the
///   LiDAR is placed by one of two different poses and neither the log nor the fused output says
///   which.
/// - **A cycle.** `base_link` → `lidar` → `radar` → `base_link` has no root, so there is no frame
///   the rig is expressed in and the composed transform around the loop is not the identity it must
///   be. A cycle is only reported when its edges are all valid at once; a rig that legitimately
///   reverses a parent/child relation in a *later* time window is not a loop, and reporting it as
///   one would flag honest data.
///
/// Only ambiguity, never disagreement in the numbers: two frames related by two chains whose poses
/// differ by a millimetre is a calibration-quality judgment this does not make. The defect here is
/// that the question has more than one answer at all.
fn ambiguous_calibration(dataset: &Dataset) -> Vec<String> {
    let Some(calibration) = &dataset.calibration else {
        return Vec::new();
    };
    let transforms = calibration.transforms.as_slice();
    if transforms.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();

    // A frame with two parents over overlapping validity. Grouped by child so one mis-parented
    // frame is one sentence however many parents claim it.
    let mut by_child: HashMap<&str, Vec<&Transform>> = HashMap::new();
    for t in transforms {
        by_child.entry(&t.child_frame).or_default().push(t);
    }
    let mut multi_parent: Vec<(&str, Vec<&str>)> = Vec::new();
    for (child, edges) in &by_child {
        let conflicting = parents_claiming_at_once(edges);
        if !conflicting.is_empty() {
            multi_parent.push((child, conflicting));
        }
    }
    multi_parent.sort();
    for (child, parents) in multi_parent {
        // Bounded for the same reason the cycle rendering is: two or three claimants is the real
        // defect and naming them is the remedy, but the number of parents a file may name for one
        // frame is the file's choice, and an unbounded list would reach every renderer and the
        // signed certificate. The count is always exact; only the enumeration is trimmed.
        const MAX_NAMED: usize = 8;
        let named = if parents.len() > MAX_NAMED {
            format!(
                "{}, and {} more",
                parents[..MAX_NAMED].join(", "),
                parents.len() - MAX_NAMED
            )
        } else {
            parents.join(", ")
        };
        out.push(format!(
            "frame `{child}` is given {} different parents at the same time ({named}) — its place \
             on the rig depends on which chain a consumer happens to resolve",
            parents.len(),
        ));
    }

    // A directed cycle, all of whose edges are valid at once.
    if let Some(cycle) = tf_directed_cycle(transforms) {
        // A real rig's loop is three or four frames and naming all of them is the whole remedy. The
        // loop's length is the file's choice, though, so the rendering is bounded: a 100k-frame
        // chain closing on itself would otherwise put a megabyte of frame names into one finding
        // message, which every downstream renderer — terminal, JSON, SARIF, the certificate — then
        // carries. The elision says what it dropped rather than trailing off.
        const MAX_NAMED: usize = 8;
        let rendered = if cycle.len() > MAX_NAMED {
            format!(
                "{} → … → {} ({} frames in the loop)",
                cycle[..MAX_NAMED - 1].join(" → "),
                cycle[cycle.len() - 1],
                // The entry frame is repeated at the end to close the loop, so the number of
                // distinct frames is one fewer than the names rendered.
                cycle.len() - 1
            )
        } else {
            cycle.join(" → ")
        };
        out.push(format!(
            "the transform tree contains a cycle ({rendered}) — it has no root frame, so there is \
             nothing the rig is expressed in"
        ));
    }
    out
}

/// The frames of one directed cycle in the transform tree (repeating the entry frame at the end), or
/// `None` when the tree is acyclic. Only cycles whose edges are **pairwise valid at the same time**
/// are returned; a parent/child relation that reverses between disjoint validity windows is a
/// recalibration, not a loop.
fn tf_directed_cycle(transforms: &[Transform]) -> Option<Vec<String>> {
    let mut edges: HashMap<&str, Vec<&Transform>> = HashMap::new();
    let mut frames: BTreeSet<&str> = BTreeSet::new();
    for t in transforms {
        edges.entry(&t.parent_frame).or_default().push(t);
        frames.insert(&t.parent_frame);
        frames.insert(&t.child_frame);
    }

    // Iterative DFS carrying the edge path, so a found cycle can be checked for simultaneity and
    // named. `state`: absent = unvisited, false = on the current path, true = finished.
    let mut state: HashMap<&str, bool> = HashMap::new();
    for &root in &frames {
        if state.contains_key(root) {
            continue;
        }
        let mut path: Vec<&Transform> = Vec::new();
        // (frame, index of the next outgoing edge to try)
        let mut stack: Vec<(&str, usize)> = vec![(root, 0)];
        state.insert(root, false);
        while let Some((frame, next)) = stack.last_mut() {
            let frame = *frame;
            let outgoing = edges.get(frame).map(|v| v.as_slice()).unwrap_or(&[]);
            if *next >= outgoing.len() {
                state.insert(frame, true);
                stack.pop();
                path.pop();
                continue;
            }
            let edge = outgoing[*next];
            *next += 1;
            let child = edge.child_frame.as_str();
            match state.get(child) {
                Some(false) => {
                    // Back edge: the cycle is the path from `child` onward, plus this edge.
                    let start = path
                        .iter()
                        .position(|e| e.parent_frame == child)
                        .unwrap_or(0);
                    let mut loop_edges: Vec<&Transform> = path[start..].to_vec();
                    loop_edges.push(edge);
                    // Intervals on a line that overlap pairwise share a common point, so "every
                    // edge of this loop is valid at once" is exactly `max(start) <= min(end)` — one
                    // linear pass rather than a comparison of every pair. The distinction matters
                    // because a file chooses the loop's length: a chain of 100k mount frames closing
                    // on itself is a legal input, and the pairwise form would spend 5e9 comparisons
                    // on it.
                    let latest_start = loop_edges
                        .iter()
                        .map(|e| e.valid_from.unwrap_or(i64::MIN))
                        .max()
                        .unwrap_or(i64::MIN);
                    let earliest_end = loop_edges
                        .iter()
                        .map(|e| e.valid_to.unwrap_or(i64::MAX))
                        .min()
                        .unwrap_or(i64::MAX);
                    let simultaneous = latest_start <= earliest_end;
                    if simultaneous {
                        let mut names: Vec<String> =
                            loop_edges.iter().map(|e| e.parent_frame.clone()).collect();
                        names.push(child.to_string());
                        return Some(names);
                    }
                }
                Some(true) => {}
                None => {
                    state.insert(child, false);
                    path.push(edge);
                    stack.push((child, 0));
                }
            }
        }
    }
    None
}

/// How many distinct physical cameras an episode carries, or `None` when that cannot be counted.
///
/// A camera is a device, not a topic. One camera is routinely published more than once in a single
/// bag — `image_raw` beside a `compressed` republication, or a rectified stream beside the raw one —
/// and counting topics would report a rig as short of intrinsics because it published its cameras
/// twice. The coordinate frame is what identifies the device: every encoding of one camera's output
/// carries that camera's `frame_id`.
///
/// Returns `None` when any camera stream declares no frame. The count would then be a guess, and
/// guessing high is what produces the accusation this exists to avoid — the undeclared frame is
/// itself already reported, by `AUTONOMY.SENSOR_FRAME_UNDECLARED`.
fn distinct_cameras(ep: &Episode) -> Option<usize> {
    let mut frames: BTreeSet<&str> = BTreeSet::new();
    for stream in ep.streams.iter().filter(|s| s.modality == Modality::Video) {
        frames.insert(stream.frame_id.as_deref()?);
    }
    Some(frames.len())
}

/// The distinct parent frames that claim one child over a *shared* instant, sorted; empty when no
/// two of `edges` name different parents at the same time.
///
/// A sweep over interval endpoints rather than a comparison of every pair. The pairwise form is the
/// obvious one and is quadratic in the number of edges naming a single child — a number the input
/// file chooses, since nothing caps how many transforms a log may carry and an adapter keys them by
/// `(parent, child)`, so a million distinct parents for one frame all survive ingest. That is a
/// hang, not a finding, and on a check meant to protect against a malformed rig it would be reached
/// by exactly the malformed rigs it exists for. This is `O(k log k)` and answers the same question
/// exactly — no sampling, no cap, nothing skipped.
fn parents_claiming_at_once<'a>(edges: &[&'a Transform]) -> Vec<&'a str> {
    // Fast path: one parent can never conflict with itself, whatever its validity ranges.
    let distinct: BTreeSet<&str> = edges.iter().map(|t| t.parent_frame.as_str()).collect();
    if distinct.len() < 2 {
        return Vec::new();
    }
    // Endpoint events: `+1` at the start of a validity range, `-1` just past its end. An open bound
    // is the whole timeline. Ends are ordered before starts at the same instant only if they do not
    // touch — a range ending at `t` and one starting at `t` *do* overlap (both are valid at `t`), so
    // the close event is placed at `t + 1` and starts sort first at equal keys.
    let mut events: Vec<(i64, i8, &str)> = Vec::with_capacity(edges.len() * 2);
    for t in edges {
        let from = t.valid_from.unwrap_or(i64::MIN);
        let to = t.valid_to.unwrap_or(i64::MAX);
        if from > to {
            continue; // an empty range claims nothing
        }
        events.push((from, 1, t.parent_frame.as_str()));
        events.push((to.saturating_add(1), -1, t.parent_frame.as_str()));
    }
    // Closes sort *before* opens at the same key. The close was already pushed one tick past the
    // range's last valid instant, so a close landing on key `t` means the range ended at `t - 1` and
    // genuinely does not meet a range opening at `t`. Ordering it the other way round reports every
    // honest recalibration — two windows that abut — as a frame with two parents.
    events.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then(a.2.cmp(b.2)));

    let mut active: HashMap<&str, usize> = HashMap::new();
    let mut distinct_active = 0usize;
    for (_, delta, parent) in events {
        if delta > 0 {
            let n = active.entry(parent).or_insert(0);
            *n += 1;
            if *n == 1 {
                distinct_active += 1;
            }
            if distinct_active >= 2 {
                // Report every parent that ever claims this child, not only the two that happened to
                // collide first: a reader reconciling the frame needs the full list of claimants.
                return distinct.into_iter().collect();
            }
        } else if let Some(n) = active.get_mut(parent) {
            *n -= 1;
            if *n == 0 {
                active.remove(parent);
                distinct_active -= 1;
            }
        }
    }
    Vec::new()
}
