//! The Canonical Dataset Model (CDM).
//!
//! The CDM is Veridex's single internal representation: every format adapter populates it and
//! every check reads only from it (design D2). It canonicalizes to a stable byte form so that the
//! same dataset bytes and the same Veridex version always produce the same content hash
//! (design D5).
//!
//! MVP subset, per `bootstrap-veridex-mvp/design.md`:
//!
//! - [`Dataset`] `{ id, metadata, provenance, episodes[] }`
//! - [`Episode`]  `{ index, start_ts, end_ts, streams[], task, labels[] }`
//! - [`Stream`]   `{ name, modality, declared_rate, clock_id, frames[] }`
//! - [`Frame`]    `{ ts, value_ref }`
//! - [`Provenance`] `{ scope, elements[] known|asserted|unknown }`

use serde::{Deserialize, Serialize};

/// Nanoseconds since an unspecified but per-clock-consistent epoch.
///
/// Timestamps are integers (not floats) so that canonicalization and content hashing are exact and
/// portable across platforms.
pub type TimestampNs = i64;

/// Metadata key under which an adapter records the source-declared episode count (e.g. LeRobot's
/// `meta/info.json` `total_episodes`), so a check can compare it against the episodes actually
/// ingested. Kept as metadata rather than a typed field to stay adapter-agnostic.
pub const META_DECLARED_EPISODES: &str = "declared_total_episodes";

/// Metadata key under which an adapter records the source-declared frame count (e.g. LeRobot's
/// `meta/info.json` `total_frames`), so a check can compare it against the frames actually ingested.
pub const META_DECLARED_FRAMES: &str = "declared_total_frames";

/// A dataset: the top of the CDM tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dataset {
    /// Stable identifier for the logical dataset (e.g. a Hub repo id or a local path stem).
    pub id: String,
    /// Free-form key/value metadata carried from the source. Order-insensitive: canonicalized by
    /// sorting keys.
    pub metadata: Vec<(String, String)>,
    /// Dataset-level provenance. Episode/stream-scoped provenance may also appear via [`Provenance::scope`].
    pub provenance: Vec<Provenance>,
    /// The episodes. Canonicalized in ascending [`Episode::index`] order.
    pub episodes: Vec<Episode>,
    /// Sensor-rig calibration — the coordinate-frame transform (TF) tree and per-camera intrinsics,
    /// each time-scoped — when the source records one (an autonomy rig log). `None` for a manipulation
    /// dataset, which has no rig calibration; the autonomy spatial checks resolve the transform valid
    /// at each timestamp from this (design A2). Extension for `autonomy-sensor-data` A0.
    #[serde(default)]
    pub calibration: Option<Calibration>,
}

/// One episode (a contiguous recorded trajectory).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    /// The episode's index within the dataset. Unique; used as the canonical sort key.
    pub index: u64,
    /// Episode start timestamp, if known.
    pub start_ts: Option<TimestampNs>,
    /// Episode end timestamp, if known.
    pub end_ts: Option<TimestampNs>,
    /// The streams recorded during this episode. Canonicalized by [`Stream::name`].
    pub streams: Vec<Stream>,
    /// The task/instruction associated with the episode, if any.
    pub task: Option<String>,
    /// Labels/annotations attached to the episode.
    pub labels: Vec<Label>,
    /// The ego-vehicle trajectory over this episode — a sequence of timestamped 6-DoF poses on the
    /// episode's clock — when the source records one (an autonomy rig log). `None` for a manipulation
    /// dataset. The `AUTONOMY.EGO_POSE_CONTINUITY` check (A2) reads this. Extension for
    /// `autonomy-sensor-data` A0.
    #[serde(default)]
    pub ego_poses: Option<Vec<EgoPose>>,
    /// Frame count the **source manifest** declares for this episode (e.g. LeRobot
    /// `meta/episodes.jsonl` `length`), if any. This is an assertion *about* the content, not the
    /// content itself: a structural check compares it against the frames actually ingested to catch
    /// the corrupted-cumulative-length class from lerobot#4143. Deliberately excluded from the CDM
    /// content hash (`canonical.rs`) — a corrupt manifest does not change what frames a dataset holds.
    #[serde(default)]
    pub declared_frame_count: Option<u64>,
}

impl Dataset {
    /// Reorder the dataset into canonical order in place: episodes ascending by [`Episode::index`],
    /// and each episode's streams ascending by [`Stream::name`] — the same order the content hash
    /// canonicalizes to. This makes the *verdict and reports* order-independent to match the
    /// order-independent [`content_hash`](crate::canonical::content_hash): two datasets that hash
    /// identically but were built with their episodes/streams in a different `Vec` order then also
    /// produce byte-identical findings and `result_content_hash`. Only the top-level `Vec`s are
    /// reordered (cheap struct moves); frames are never touched. Idempotent.
    pub fn canonicalize_order(&mut self) {
        // Every collection the encoder canonicalizes must be sorted here with the *same* key.
        // Anything the hash treats as a set but a check reads as a sequence — or reads by "first
        // match" — otherwise lets two datasets share a content hash and produce different verdicts,
        // which would let a certificate attest a hash that also matches a dataset that fails.
        self.metadata
            .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        for record in &mut self.provenance {
            record.elements.sort_by(|a, b| {
                crate::canonical::element_sort_key(a).cmp(&crate::canonical::element_sort_key(b))
            });
        }
        self.provenance.sort_by(|a, b| {
            crate::canonical::prov_sort_key(a).cmp(&crate::canonical::prov_sort_key(b))
        });
        // Episodes and streams tie-break on full content, because neither `index` nor `name` is
        // guaranteed unique — duplicates of both are faults Veridex reports, so the ordering cannot
        // assume they are absent.
        self.episodes.sort_by(|a, b| {
            a.index.cmp(&b.index).then_with(|| {
                crate::canonical::episode_digest(a).cmp(&crate::canonical::episode_digest(b))
            })
        });
        for ep in &mut self.episodes {
            ep.streams.sort_by(|a, b| {
                a.name.cmp(&b.name).then_with(|| {
                    crate::canonical::stream_digest(a).cmp(&crate::canonical::stream_digest(b))
                })
            });
            ep.labels.sort_by(|a, b| {
                crate::canonical::label_sort_key(a).cmp(&crate::canonical::label_sort_key(b))
            });
            if let Some(poses) = &mut ep.ego_poses {
                poses.sort_by(|a, b| {
                    a.ts.cmp(&b.ts).then_with(|| {
                        crate::canonical::ego_pose_bits(a).cmp(&crate::canonical::ego_pose_bits(b))
                    })
                });
            }
        }
        // Calibration is a set to the encoder, so it must be a sorted sequence here too: a reader
        // that resolves "the transform valid at time t" by first match would otherwise depend on the
        // order the rig happened to record them in.
        if let Some(cal) = &mut self.calibration {
            cal.transforms.sort_by(|a, b| {
                crate::canonical::transform_sort_key(a)
                    .cmp(&crate::canonical::transform_sort_key(b))
            });
            cal.intrinsics.sort_by(|a, b| {
                crate::canonical::intrinsics_sort_key(a)
                    .cmp(&crate::canonical::intrinsics_sort_key(b))
            });
        }
    }
}

impl Episode {
    /// The episode's overall wall-clock duration in nanoseconds, if measurable. Prefers the declared
    /// `[start_ts, end_ts]`; otherwise falls back to the longest single-stream frame span — a
    /// clock-safe proxy, since one stream's frames share a clock so the subtraction never mixes
    /// clocks. `None` when neither is available or positive.
    ///
    /// An episode whose streams are all on a step-index clock has **no** measurable duration, and
    /// that includes its declared bounds: those are step indices too. Subtracting them yields a step
    /// count, which the report would print as nanoseconds and the outlier check would compare as a
    /// duration — a 500-step episode among 20-step ones was reported as "lasts 0.0 ms, 26.3x longer
    /// than the dataset median of 0.0 ms".
    pub fn duration_ns(&self) -> Option<TimestampNs> {
        if !self.streams.is_empty() && !self.streams.iter().any(|s| s.has_measured_time()) {
            return None;
        }
        if let (Some(start), Some(end)) = (self.start_ts, self.end_ts) {
            if end > start {
                // Saturating: corrupt boundaries spanning the full i64 range must not overflow
                // (which would panic in debug builds) — Veridex's job is to survive bad data.
                return Some(end.saturating_sub(start));
            }
        }
        self.streams
            .iter()
            // Only a stream whose timestamps are measured time can stand in for a duration. A step
            // index yields a span in "steps" that would be printed as nanoseconds — an episode of
            // 500 steps reported as lasting 0.0 ms, and compared as a duration against its peers.
            .filter(|s| s.has_measured_time())
            .filter_map(|s| {
                let mut it = s.frames.iter().map(|f| f.ts);
                let first = it.next()?;
                let (mut lo, mut hi) = (first, first);
                for ts in it {
                    lo = lo.min(ts);
                    hi = hi.max(ts);
                }
                Some(hi.saturating_sub(lo))
            })
            .filter(|d| *d > 0)
            .max()
    }
}

/// A modality of recorded data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modality {
    /// Camera / image streams.
    Video,
    /// Scalar proprioceptive state (joint positions, gripper, etc.).
    ScalarState,
    /// Commanded actions.
    Action,
    /// Audio.
    Audio,
    /// Tactile or force/torque sensing.
    TactileForceTorque,
    /// LiDAR / radar point-cloud streams (autonomy rig). The per-point field layout is declared in
    /// [`Stream::point_fields`].
    PointCloud,
    /// Inertial measurement unit (linear acceleration + angular velocity).
    Imu,
    /// GNSS global-position fix.
    Gnss,
    /// A decoded CAN-bus signal (a named channel from a DBC).
    CanSignal,
    /// Ego-vehicle pose / odometry.
    EgoPose,
}

impl Modality {
    /// The stable canonical tag for this modality, used in hashing and reporting.
    pub fn tag(self) -> &'static str {
        match self {
            Modality::Video => "video",
            Modality::ScalarState => "scalar-state",
            Modality::Action => "action",
            Modality::Audio => "audio",
            Modality::TactileForceTorque => "tactile-force-torque",
            Modality::PointCloud => "point-cloud",
            Modality::Imu => "imu",
            Modality::Gnss => "gnss",
            Modality::CanSignal => "can-signal",
            Modality::EgoPose => "ego-pose",
        }
    }

    /// Whether this modality is an **AV-native rig sensor** (LiDAR/radar, IMU, GNSS, CAN, ego-pose).
    /// Used to detect an autonomy rig: these modalities never appear in a manipulation dataset, so a
    /// dataset carrying several is a sensor rig, which routes cross-sensor sync to the rig-wide
    /// `AUTONOMY.RIG_SYNC` check instead of the pairwise `TEMPORAL.CLOCK_SKEW`. Camera (`Video`) is a
    /// rig sensor too but also appears in manipulation, so it does not mark a rig on its own.
    pub fn is_rig_sensor(self) -> bool {
        matches!(
            self,
            Modality::PointCloud
                | Modality::Imu
                | Modality::Gnss
                | Modality::CanSignal
                | Modality::EgoPose
        )
    }
}

/// What a stream's timestamps actually are.
///
/// Not every source records time. RLDS/TFDS has no per-step timestamp at all, so its frames are
/// stamped with their step index — a perfectly good *order*, and not a measurement of anything. The
/// distinction has to be in the CDM rather than in a comment, because a check cannot tell the two
/// apart by looking at the numbers: an index is flawlessly monotonic, perfectly regular, and
/// identical across every stream of an episode, so every temporal check *passes* on it. That pass is
/// what reaches the report and the signed certificate, where it reads as "these sensors are
/// synchronized" rather than "there was nothing here to measure".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClockKind {
    /// Timestamps are measured time on some clock. Every temporal check applies.
    #[default]
    Measured,
    /// Timestamps are a positional step index, not time. The checks that compare durations, rates,
    /// gaps, or cross-stream alignment have nothing to measure and abstain, and
    /// `temporal.clock-measurability` reports that they did.
    StepIndex,
}

impl ClockKind {
    /// The stable canonical tag for this kind, used in hashing and reporting.
    pub fn tag(self) -> &'static str {
        match self {
            ClockKind::Measured => "measured",
            ClockKind::StepIndex => "step-index",
        }
    }
}

/// One stream within an episode (e.g. a camera, a joint-state channel).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stream {
    /// Stream name, unique within its episode. The canonical sort key.
    pub name: String,
    /// The modality of this stream.
    pub modality: Modality,
    /// Declared/nominal sampling rate in Hz, if the source states one.
    pub declared_rate_hz: Option<f64>,
    /// Identifier of the clock this stream's timestamps are measured against. Streams sharing a
    /// `clock_id` are directly comparable; differing ids require alignment (the `TEMPORAL.CLOCK_SKEW`
    /// check, design D4).
    pub clock_id: String,
    /// Whether [`Frame::ts`] is *measured time* or a positional index. Defaults to
    /// [`ClockKind::Measured`]; a source that records no clock at all (RLDS/TFDS) sets
    /// [`ClockKind::StepIndex`].
    #[serde(default)]
    pub clock_kind: ClockKind,
    /// Declared element data type of each value (e.g. `float32`, `uint8`, `video`), if the source
    /// states one. Checks compare it across a stream's appearances; Veridex never infers it.
    pub dtype: Option<String>,
    /// Declared per-frame value shape (tensor dimensions), if the source states one. A stream that
    /// keeps a different `shape` in different episodes cannot be batched — the
    /// `structural.shape-consistency` check flags that. `None` when the source declares no shape.
    pub shape: Option<Vec<u64>>,
    /// The frames, in recorded order. Frame order is data-defined and is preserved (not sorted).
    pub frames: Vec<Frame>,
    /// Stored per-stream summary statistics, if the source records them (e.g. LeRobot's
    /// `meta/stats.json`). Checks may sanity-check these without decoding frame payloads.
    pub stats: Option<StreamStats>,
    /// Stored statistics **per dimension** of a multi-DoF feature, when the source records per-element
    /// arrays (LeRobot's `meta/stats.json` stores `min`/`max`/… as vectors). `None` for a scalar
    /// feature — its one dimension is already in [`Stream::stats`]. The `statistical.stored-vs-observed`
    /// check pairs these with [`Stream::observed_dim_stats`] so a stale stat in a non-first joint is
    /// caught: robot normalization is per dimension, so an element-0-only comparison gives false
    /// confidence.
    pub dim_stats: Option<Vec<DimStats>>,
    /// Statistics Veridex **recomputed** from the actual feature values, when the adapter reads them
    /// (the LeRobot adapter fingerprints feature cells, so it recomputes these in the same pass). The
    /// `statistical.stored-vs-observed` check compares these against [`Stream::stats`] to catch a
    /// stale or wrong `meta/stats.json`. `None` when the source's values weren't read.
    pub observed_stats: Option<StreamStats>,
    /// How often the stream's recomputed values sit exactly pinned at an extreme, when the adapter
    /// reads them. The `statistical.saturation` check uses this to flag a clamped/saturated actuator.
    /// `None` when the source's values weren't read.
    pub observed_saturation: Option<Saturation>,
    /// Count of **non-finite** scalar feature values (NaN or ±infinity) the adapter encountered when
    /// recomputing from the actual data — scanning *every* dimension of a multi-DoF cell, not just the
    /// first, so a NaN buried in one joint is still counted. These are excluded from [`Stream::observed_stats`]
    /// (a NaN would poison every summary), so this field is the only record that they exist — the
    /// `statistical.non-finite-observed` check flags any stream where it is non-zero. `None` when the
    /// source's values weren't read; `Some(0)` when they were read and all were finite.
    pub observed_non_finite: Option<u64>,
    /// Recomputed statistics **per dimension** of a multi-DoF feature, when the adapter reads values
    /// and the feature has more than one dimension (`None` for scalar streams — their single
    /// dimension is already in [`Stream::observed_stats`] — and where values weren't read). The
    /// `statistical.extreme-outlier` check scans these so a spike buried in one joint of a 7-DoF
    /// `action` is caught, not just element 0.
    pub observed_dim_stats: Option<Vec<DimStats>>,
    /// Declared per-point field layout (e.g. `x`, `y`, `z`, `intensity`, `ring`) for a point-cloud
    /// stream (LiDAR/radar). `None` for every non-cloud stream. Order is significant (it is the
    /// point record's field order), so it is preserved, not sorted. Veridex records the declared
    /// schema, never the point payloads. Extension for `autonomy-sensor-data` A0.
    #[serde(default)]
    pub point_fields: Option<Vec<PointField>>,
    /// The media file backing a video stream, when the source stores its pixels outside the data
    /// table (LeRobot keeps video in `videos/**.mp4` and only the timeline in Parquet). Carries both
    /// what the manifest *declares* about the encoding and what Veridex read out of the container
    /// itself, so the `video.*` checks can compare them. `None` for every stream with no separate
    /// media file — a scalar feature, or a dataset that stores its images inline.
    #[serde(default)]
    pub media: Option<Media>,
    /// The coordinate frame this sensor's data is expressed in (a ROS `header.frame_id`, e.g.
    /// `lidar_top` or `camera_front`), when the source records one. This is the name that has to
    /// appear in [`Calibration::transforms`] for the sensor to be relatable to any other — the
    /// `autonomy.sensor-frame-resolution` check reads it. `None` for a source that declares no frame.
    #[serde(default)]
    pub frame_id: Option<FrameId>,
}

impl Stream {
    /// Whether this stream's timestamps are measured time, and so can be compared, differenced, or
    /// turned into a rate.
    ///
    /// Every temporal check guards on this. A step index passes all of them trivially — it is
    /// perfectly monotonic, perfectly regular, and identical across an episode's streams — and that
    /// pass is indistinguishable in a report from a genuinely well-synchronized rig.
    pub fn has_measured_time(&self) -> bool {
        self.clock_kind == ClockKind::Measured
    }
}

/// Recomputed summary statistics for one dimension of a multi-DoF feature, tagged with its index.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DimStats {
    /// The dimension index within the feature cell (e.g. `6` for the gripper of a 7-DoF `action`).
    pub dim: u64,
    /// The recomputed statistics for that dimension's values.
    pub stats: StreamStats,
}

/// How often a stream's recomputed values sit **exactly** at their extreme — the fingerprint of a
/// saturated/clamped actuator. Values are counted as pinned only on exact equality with the observed
/// min/max (the same false-positive-free philosophy as `STRUCTURAL.STUCK_STREAM`): a real, noisy
/// sensor never lands on the same float many times, so a large pinned fraction is unambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Saturation {
    /// Finite values considered (the denominator for the pinned fractions).
    pub sample_count: u64,
    /// Values exactly equal to `min`.
    pub at_min: u64,
    /// Values exactly equal to `max`.
    pub at_max: u64,
    /// The observed minimum the `at_min` count is pinned to.
    pub min: f64,
    /// The observed maximum the `at_max` count is pinned to.
    pub max: f64,
    /// The dimension index this summary is for, within a multi-DoF feature cell (0 for a scalar
    /// stream, or the saturating joint of a vector — e.g. `6` for the gripper of a 7-DoF `action`).
    /// The adapter reports the worst-saturating dimension; the check names it in its finding.
    pub dim: u64,
}

/// The media file backing a video stream, and what Veridex learned about it.
///
/// A LeRobot video feature is split across two places: the manifest declares the encoding
/// (`meta/info.json` → `features[key].info`) and the timeline lives as one Parquet row per frame,
/// while the pixels live in a separate `.mp4`. Nothing in either half checks the other, which is how
/// a re-encoded or half-uploaded video silently ends up with a different frame count, resolution, or
/// codec than the data it is paired with. This type holds both halves so the `video.*` checks can.
///
/// Veridex reads the container's **headers** only — it never decodes a pixel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Media {
    /// The media file's path relative to the dataset root. For a file the manifest implies but that
    /// is not on disk, this is the path that was looked for.
    pub uri: String,
    /// What the source manifest declares about the encoding.
    pub declared: MediaParams,
    /// Whether the container could be read, and why not when it could not.
    pub status: MediaStatus,
    /// What the container itself says. Every field is `None` unless `status` is
    /// [`MediaStatus::Read`] — and even then a field the container omits stays `None`.
    pub observed: MediaParams,
    /// Frames the container holds (its sample count). Compared against the frames the paired data
    /// stream carries — that disagreement is the video/data desync. `None` when unread.
    pub frame_count: Option<u64>,
}

/// Whether a stream's media file could be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum MediaStatus {
    /// The container parsed; [`Media::observed`] holds what it declared.
    Read,
    /// The manifest implies a media file that is not on disk.
    Missing,
    /// The file exists but its container could not be parsed.
    Unreadable {
        /// What went wrong, in the container parser's words.
        reason: String,
    },
    /// The manifest declares this stream's pixels live in video files, but no file can be attributed
    /// to an episode — a layout that concatenates episodes into shared files rather than writing one
    /// per episode. Nothing about the container is asserted, and the conformance checks abstain.
    ///
    /// Recorded rather than left absent, because `media: None` is indistinguishable from "this is not
    /// a video feature": with nothing attached, the whole video family iterated past these streams
    /// and emitted nothing at all — not even for a file holding no container. A status the checks can
    /// see is what turns that silence into a statement.
    Unattributable {
        /// Why no file could be attributed, in the adapter's words.
        reason: String,
    },
}

/// Video encoding parameters, either as declared by the manifest or as read from the container.
/// Every field is optional: a source states what it states, and Veridex never infers the rest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaParams {
    /// The codec identifier (an ISO-BMFF sample-entry fourcc such as `avc1`/`hvc1`/`av01`, or the
    /// manifest's own name for it). Compared case-insensitively, and across the known aliases for one
    /// codec, so `h264` and `avc1` are not reported as a mismatch.
    pub codec: Option<String>,
    /// Frame width in pixels.
    pub width: Option<u64>,
    /// Frame height in pixels.
    pub height: Option<u64>,
    /// Frames per second.
    pub fps: Option<f64>,
}

/// The canonical name for a codec, so the manifest's spelling and the container's fourcc compare
/// equal when they mean the same encoder — or `None` when the name is one Veridex does not know.
///
/// A manifest's codec field and a container's sample-entry fourcc are drawn from two different, open
/// namespaces: the manifest usually records the *encoder* (`libx264`, `h264_videotoolbox`,
/// `libopenh264`, whatever was passed to ffmpeg), the container records the *format*. New encoders
/// appear constantly. So an unrecognized spelling yields `None` and the comparison abstains, rather
/// than treating "I have not heard of this" as "these differ" — an open namespace cannot be judged
/// by a closed table without flagging honest data.
pub fn canonical_codec(name: &str) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" | "x264" | "libx264" | "libopenh264" | "openh264"
        | "h264_videotoolbox" | "h264_nvenc" | "h264_qsv" | "h264_vaapi" | "h264_amf" => {
            Some("avc1")
        }
        "h265" | "hevc" | "hvc1" | "hev1" | "x265" | "libx265" | "hevc_videotoolbox"
        | "hevc_nvenc" | "hevc_qsv" | "hevc_vaapi" | "hevc_amf" => Some("hvc1"),
        "av1" | "av01" | "libaom-av1" | "libsvtav1" | "librav1e" | "av1_nvenc" | "av1_qsv"
        | "av1_vaapi" => Some("av01"),
        "vp9" | "vp09" | "libvpx-vp9" | "vp9_vaapi" | "vp9_qsv" => Some("vp09"),
        "vp8" | "vp08" | "libvpx" => Some("vp08"),
        "mpeg4" | "mp4v" | "libxvid" | "xvid" => Some("mp4v"),
        "mjpeg" | "jpeg" | "mjpg" => Some("mjpg"),
        "prores" | "apcn" | "prores_ks" | "prores_videotoolbox" => Some("apcn"),
        _ => None,
    }
}

/// Stored summary statistics for a stream, as recorded by the source (not recomputed by Veridex).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StreamStats {
    /// Minimum value.
    pub min: f64,
    /// Maximum value.
    pub max: f64,
    /// Mean value.
    pub mean: f64,
    /// Standard deviation.
    pub std: f64,
}

// ---- Autonomy / world-model sensor-rig extensions (autonomy-sensor-data A0) ----
//
// These types extend the CDM to represent a multi-sensor autonomy rig without forking the model
// (design A1): a LiDAR is just a [`Stream`] with `Modality::PointCloud`, calibration and the
// transform tree are first-class and time-scoped (A2), and the ego trajectory is a sequence of
// timestamped poses. They are all optional; a manipulation dataset leaves them empty and is
// unaffected.

/// One field of a point-cloud stream's per-point record layout (e.g. `x`, `y`, `z`, `intensity`,
/// `ring`). Veridex records the declared schema, never the point payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PointField {
    /// The field name (e.g. `x`, `intensity`).
    pub name: String,
    /// The field's declared element type (e.g. `float32`, `uint16`), if the source states one.
    pub dtype: Option<String>,
}

/// A 6-DoF pose: a translation in metres and a unit-quaternion rotation `[x, y, z, w]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    /// Translation `[x, y, z]` in metres.
    pub translation: [f64; 3],
    /// Rotation as a unit quaternion `[x, y, z, w]`.
    pub rotation: [f64; 4],
}

/// A rigid-body transform from `parent_frame` to `child_frame`, valid over an optional
/// `[valid_from, valid_to]` time range on the rig clock. Rigs are recalibrated and coordinate frames
/// can move within a log, so a transform is time-scoped; the autonomy spatial checks resolve the
/// transform valid at each timestamp (design A2). A `None` bound is open-ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// The parent coordinate frame (e.g. `base_link`).
    pub parent_frame: FrameId,
    /// The child coordinate frame (e.g. `lidar_top`).
    pub child_frame: FrameId,
    /// The transform itself (parent → child).
    pub pose: Pose,
    /// Start of the validity range on the rig clock, or `None` for open-ended.
    pub valid_from: Option<TimestampNs>,
    /// End of the validity range on the rig clock, or `None` for open-ended.
    pub valid_to: Option<TimestampNs>,
}

/// A coordinate-frame identifier interned as a short string (`base_link`, `lidar_top`, `map`, …).
pub type FrameId = String;

/// Pinhole camera intrinsics for a named camera stream, with an optional validity range. Distortion
/// coefficients are recorded verbatim in source order (their meaning is model-specific and Veridex
/// does not interpret them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraIntrinsics {
    /// The camera [`Stream::name`] these intrinsics calibrate.
    pub stream: String,
    /// Focal length in pixels, x.
    pub fx: f64,
    /// Focal length in pixels, y.
    pub fy: f64,
    /// Principal point x, in pixels.
    pub cx: f64,
    /// Principal point y, in pixels.
    pub cy: f64,
    /// Distortion coefficients in source order (empty when the source records none).
    pub distortion: Vec<f64>,
    /// Start of the validity range on the rig clock, or `None` for open-ended.
    pub valid_from: Option<TimestampNs>,
    /// End of the validity range on the rig clock, or `None` for open-ended.
    pub valid_to: Option<TimestampNs>,
}

/// The rig's calibration: the coordinate-frame transform (TF) tree and per-camera intrinsics, each
/// time-scoped. Both collections are order-insensitive — canonicalized by content — so two logs that
/// record the same calibration in a different order hash identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// The transform tree (edges between coordinate frames), time-scoped.
    pub transforms: Vec<Transform>,
    /// Per-camera intrinsics, time-scoped.
    pub intrinsics: Vec<CameraIntrinsics>,
}

/// One timestamped ego-vehicle pose on the episode's clock. The sequence over an episode forms the
/// ego trajectory the `AUTONOMY.EGO_POSE_CONTINUITY` check reads (design A2).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EgoPose {
    /// The pose timestamp on the episode's clock.
    pub ts: TimestampNs,
    /// The ego-vehicle pose at `ts`.
    pub pose: Pose,
}

/// One frame: a timestamped pointer to a value in streamed storage.
///
/// Values are not necessarily held in memory; [`Frame::value_ref`] locates the bytes so checks can
/// stream them on demand (design D2/D5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The frame timestamp on its stream's clock.
    pub ts: TimestampNs,
    /// Where the frame's value lives.
    pub value_ref: ValueRef,
}

/// A reference into streamed storage locating a frame's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueRef {
    /// The backing store for the value (a shard path, an MCAP channel, etc.).
    pub uri: String,
    /// Byte offset of the value within `uri`, if applicable.
    pub byte_offset: Option<u64>,
    /// Byte length of the value, if known.
    pub byte_len: Option<u64>,
    /// SHA-256 of the value bytes, if the source records or Veridex has computed it.
    pub content_hash: Option<[u8; 32]>,
}

/// A label / annotation attached to an episode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Label {
    /// The label kind/namespace (e.g. `language`, `success`).
    pub key: String,
    /// The label value.
    pub value: String,
    /// When the annotation applies, on the episode's clock, for a **timestamped** annotation such as a
    /// language event (an instruction that takes effect part-way through an episode). `None` for a
    /// persistent episode-level label. The `semantic.annotation-integrity` check verifies that a
    /// timestamped annotation falls within its episode's time span, never editing it.
    pub ts: Option<TimestampNs>,
}

/// How well a provenance element is known.
///
/// Veridex never infers provenance: an element is either extracted from the source (`Known`),
/// attested by the producer (`Asserted`), or absent (`Unknown`) — design D9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceClass {
    /// Extracted directly from the source bytes.
    Known,
    /// Attested by the producer (e.g. a signed assertion).
    Asserted,
    /// Not present.
    Unknown,
}

impl ProvenanceClass {
    /// The stable canonical tag for this class.
    pub fn tag(self) -> &'static str {
        match self {
            ProvenanceClass::Known => "known",
            ProvenanceClass::Asserted => "asserted",
            ProvenanceClass::Unknown => "unknown",
        }
    }
}

/// The scope a [`Provenance`] record applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceScope {
    /// Applies to the whole dataset.
    Dataset,
    /// Applies to a single episode (by index).
    Episode(u64),
    /// Applies to a single stream within an episode.
    Stream { episode: u64, stream: String },
}

/// A provenance record: a set of classified elements for a given scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// What this record describes.
    pub scope: ProvenanceScope,
    /// The provenance elements. Canonicalized by [`ProvenanceElement::key`].
    pub elements: Vec<ProvenanceElement>,
}

/// One provenance fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceElement {
    /// The element name (e.g. `sensor`, `license`, `annotator`, `upstream_dataset`).
    pub key: String,
    /// The element value. `None` is valid and expected when `class` is [`ProvenanceClass::Unknown`].
    pub value: Option<String>,
    /// How well this element is known.
    pub class: ProvenanceClass,
}

impl ProvenanceElement {
    /// True when the element carries a value that isn't a low-information placeholder. Placeholder
    /// values (`unknown`, `n/a`, `none`, …) are present in form but empty in substance, so they must
    /// not count as real provenance — for either the completeness check or the coverage score.
    pub fn has_real_value(&self) -> bool {
        self.value
            .as_deref()
            .is_some_and(|v| !is_placeholder_value(v))
    }
}

/// Low-information provenance values that are present in form but empty in substance. Compared
/// case-insensitively after trimming.
const PLACEHOLDER_VALUES: &[&str] = &[
    "",
    "unknown",
    "n/a",
    "na",
    "none",
    "null",
    "nil",
    "todo",
    "tbd",
    "unspecified",
    "placeholder",
    "-",
    "--",
    "?",
];

/// Whether a provenance value is an effectively-empty placeholder.
pub fn is_placeholder_value(value: &str) -> bool {
    let norm = value.trim().to_ascii_lowercase();
    PLACEHOLDER_VALUES.contains(&norm.as_str())
}
