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
    /// The frames, in recorded order. Frame order is data-defined and is preserved (not sorted).
    pub frames: Vec<Frame>,
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
