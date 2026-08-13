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
}

impl Episode {
    /// The episode's overall wall-clock duration in nanoseconds, if measurable. Prefers the declared
    /// `[start_ts, end_ts]`; otherwise falls back to the longest single-stream frame span — a
    /// clock-safe proxy, since one stream's frames share a clock so the subtraction never mixes
    /// clocks. `None` when neither is available or positive.
    pub fn duration_ns(&self) -> Option<TimestampNs> {
        if let (Some(start), Some(end)) = (self.start_ts, self.end_ts) {
            if end > start {
                // Saturating: corrupt boundaries spanning the full i64 range must not overflow
                // (which would panic in debug builds) — Veridex's job is to survive bad data.
                return Some(end.saturating_sub(start));
            }
        }
        self.streams
            .iter()
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
