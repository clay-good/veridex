//! Autonomy sensor-rig checks (`autonomy-sensor-data`). The first is rig-wide time sync, the
//! N-sensor generalization of the core pairwise [`ClockSkew`](crate::checks::temporal::ClockSkew).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cdm::{Dataset, Episode, Modality, Stream, Transform};
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
            let mut spans: Vec<(&str, i64, i64)> = ep
                .streams
                .iter()
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
    const MAX_INTERVAL_CV: f64 = 0.5;

    /// Minimum frames for a stable median inter-frame interval; below this the drop estimate is noise.
    const MIN_FRAMES: usize = 8;

    /// How far an inter-frame interval may sit from an exact multiple of the cadence and still be
    /// read as swallowed frames, in units of the cadence. A dropped frame leaves a hole near `k·T`;
    /// an idle burst lands anywhere, so it is not counted.
    const MULTIPLE_TOLERANCE: f64 = 0.25;
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
        &["AUTONOMY.EGO_POSE_CONTINUITY"]
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
                if speed > self.max_speed_mps {
                    breaks += 1;
                    if speed > worst_speed {
                        worst_speed = speed;
                        worst_ts = b.ts;
                    }
                }
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
        &["AUTONOMY.CALIBRATION_INCOMPLETE"]
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

            if has_camera && intrinsics_empty {
                findings.push(flag(
                    ep.index,
                    format!(
                        "episode {}: the rig has camera(s) but no camera intrinsics (CameraInfo) — \
                         projecting points into the image is undefined",
                        ep.index
                    ),
                ));
            }
        }
        findings
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
                    continue; // the source declares no frame; nothing was claimed, nothing to check
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
