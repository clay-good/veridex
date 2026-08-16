//! Deterministic canonicalization and content hashing of the CDM (design D5).
//!
//! Canonicalization writes the CDM into a domain-separated, length-prefixed binary form with a
//! fixed field order and stable ordering of order-insensitive collections (episodes by index,
//! streams by name, metadata/labels/provenance by key). The bytes are streamed directly into
//! SHA-256, so hashing a dataset with millions of frames never materializes the encoding in memory.
//!
//! Guarantee: identical [`Dataset`](crate::cdm::Dataset) values — up to the ordering of
//! order-insensitive collections — always produce the same [`ContentHash`], on any platform.

use sha2::{Digest, Sha256};

use crate::cdm::{
    Calibration, CameraIntrinsics, Dataset, DimStats, EgoPose, Episode, Frame, Label, PointField,
    Pose, Provenance, ProvenanceElement, ProvenanceScope, Stream, StreamStats, Transform, ValueRef,
};

/// Version of the canonical encoding. Bumping this deliberately changes every content hash; it is
/// mixed into the domain-separation prefix so hashes from different encoding versions never collide.
///
/// v2 binds every content-bearing `Stream` field into the hash — the stored per-dimension stats
/// (`dim_stats`) and the recomputed `observed_*` fields, which v1's hand-written encoder silently
/// dropped, so two datasets differing only in those fields no longer collide.
///
/// v3 binds the autonomy sensor-rig extensions (`autonomy-sensor-data` A0): the dataset's rig
/// `calibration` (the TF tree + camera intrinsics), each episode's `ego_poses` trajectory, and each
/// stream's declared `point_fields`. A manipulation dataset leaves all three empty, so they add only
/// a fixed "absent" marker to its encoding — but every content-bearing field is still hashed, keeping
/// the "no silently-dropped field" invariant intact.
pub const CANONICAL_VERSION: u32 = 4;

const DOMAIN: &[u8] = b"veridex.cdm.v1\0";

/// A 32-byte SHA-256 content hash of a canonicalized CDM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Lowercase hex encoding, e.g. for display and certificate embedding.
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
            s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
        }
        s
    }
}

impl core::fmt::Display for ContentHash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Compute the deterministic content hash of a dataset.
pub fn content_hash(dataset: &Dataset) -> ContentHash {
    let mut enc = Encoder::new();
    dataset.encode(&mut enc);
    ContentHash(enc.finish())
}

/// Streams canonical bytes directly into a SHA-256 hasher.
struct Encoder {
    hasher: Sha256,
}

impl Encoder {
    fn new() -> Self {
        let mut hasher = Sha256::new();
        hasher.update(DOMAIN);
        hasher.update(CANONICAL_VERSION.to_le_bytes());
        Encoder { hasher }
    }

    fn finish(self) -> [u8; 32] {
        self.hasher.finalize().into()
    }

    fn u8(&mut self, v: u8) {
        self.hasher.update([v]);
    }

    fn u64(&mut self, v: u64) {
        self.hasher.update(v.to_le_bytes());
    }

    fn i64(&mut self, v: i64) {
        self.hasher.update(v.to_le_bytes());
    }

    /// Canonical f64: normalize signed zero and all NaN payloads to single bit patterns so that
    /// `-0.0`/`+0.0` and any NaN never split a hash.
    fn f64(&mut self, v: f64) {
        self.u64(canon_f64_bits(v));
    }

    /// A `StreamStats` quadruple (min/max/mean/std), used for stored and per-dimension stats alike.
    fn stats(&mut self, s: &StreamStats) {
        self.f64(s.min);
        self.f64(s.max);
        self.f64(s.mean);
        self.f64(s.std);
    }

    /// A per-dimension stats sequence (`dim` index + its quadruple).
    fn dim_stats(&mut self, dims: &[DimStats]) {
        self.seq(dims, |e, d| {
            e.u64(d.dim);
            e.stats(&d.stats);
        });
    }

    /// A 6-DoF pose (translation `[x,y,z]` + quaternion `[x,y,z,w]`), each component canonical-f64.
    fn pose(&mut self, p: &Pose) {
        for v in p.translation {
            self.f64(v);
        }
        for v in p.rotation {
            self.f64(v);
        }
    }

    /// Length-prefixed bytes; unambiguous for concatenation.
    fn bytes(&mut self, b: &[u8]) {
        self.u64(b.len() as u64);
        self.hasher.update(b);
    }

    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }

    fn opt<T>(&mut self, v: &Option<T>, f: impl FnOnce(&mut Self, &T)) {
        match v {
            None => self.u8(0),
            Some(inner) => {
                self.u8(1);
                f(self, inner);
            }
        }
    }

    fn seq<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) {
        self.u64(items.len() as u64);
        for item in items {
            f(self, item);
        }
    }
}

impl Dataset {
    fn encode(&self, e: &mut Encoder) {
        e.str(&self.id);

        // metadata: order-insensitive, canonicalized by (key, value)
        let mut metadata: Vec<&(String, String)> = self.metadata.iter().collect();
        metadata.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        e.seq(&metadata, |e, (k, v)| {
            e.str(k);
            e.str(v);
        });

        // provenance: order-insensitive, canonicalized by scope *and* full content — a scope can
        // legitimately carry more than one record, so scope alone is not a total order and would let
        // two set-equal-but-permuted inputs hash differently.
        let mut provenance: Vec<&Provenance> = self.provenance.iter().collect();
        provenance.sort_by(|a, b| prov_sort_key(a).cmp(&prov_sort_key(b)));
        e.seq(&provenance, |e, p| p.encode(e));

        // episodes: order-insensitive, canonicalized by index
        let mut episodes: Vec<&Episode> = self.episodes.iter().collect();
        episodes.sort_by(|a, b| {
            a.index
                .cmp(&b.index)
                .then_with(|| episode_digest(a).cmp(&episode_digest(b)))
        });
        e.seq(&episodes, |e, ep| ep.encode(e));

        // calibration (autonomy rig): absent for manipulation datasets, so this is a single `0` byte
        // there. Present rig calibration is order-insensitive and canonicalized by content.
        e.opt(&self.calibration, |e, c| c.encode(e));
    }
}

impl Calibration {
    fn encode(&self, e: &mut Encoder) {
        // transforms: order-insensitive, canonicalized by full content (frames + validity + pose), so
        // a rig that records the same tree in a different order hashes identically.
        let mut transforms: Vec<&Transform> = self.transforms.iter().collect();
        transforms.sort_by(|a, b| transform_sort_key(a).cmp(&transform_sort_key(b)));
        e.seq(&transforms, |e, t| {
            e.str(&t.parent_frame);
            e.str(&t.child_frame);
            e.pose(&t.pose);
            e.opt(&t.valid_from, |e, v| e.i64(*v));
            e.opt(&t.valid_to, |e, v| e.i64(*v));
        });
        // intrinsics: order-insensitive, canonicalized by full content.
        let mut intrinsics: Vec<&CameraIntrinsics> = self.intrinsics.iter().collect();
        intrinsics.sort_by(|a, b| intrinsics_sort_key(a).cmp(&intrinsics_sort_key(b)));
        e.seq(&intrinsics, |e, c| {
            e.str(&c.stream);
            e.f64(c.fx);
            e.f64(c.fy);
            e.f64(c.cx);
            e.f64(c.cy);
            e.seq(&c.distortion, |e, d| e.f64(*d));
            e.opt(&c.valid_from, |e, v| e.i64(*v));
            e.opt(&c.valid_to, |e, v| e.i64(*v));
        });
    }
}

impl Episode {
    fn encode(&self, e: &mut Encoder) {
        e.u64(self.index);
        e.opt(&self.start_ts, |e, ts| e.i64(*ts));
        e.opt(&self.end_ts, |e, ts| e.i64(*ts));

        // streams: order-insensitive, canonicalized by name then full content (a name is not
        // guaranteed unique, so it is not a total order on its own)
        let mut streams: Vec<&Stream> = self.streams.iter().collect();
        streams.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| stream_digest(a).cmp(&stream_digest(b)))
        });
        e.seq(&streams, |e, s| s.encode(e));

        e.opt(&self.task, |e, t| e.str(t));

        // labels: order-insensitive, canonicalized by (key, value, ts)
        let mut labels: Vec<&Label> = self.labels.iter().collect();
        labels.sort_by(|a, b| label_sort_key(a).cmp(&label_sort_key(b)));
        e.seq(&labels, |e, l| {
            e.str(&l.key);
            e.str(&l.value);
            e.opt(&l.ts, |e, t| e.i64(*t));
        });

        // declared_frame_count: an assertion *about* the content rather than content itself, but
        // `structural.episode-boundary` reads it and fails an episode whose frames disagree with it.
        // A field a check reads has to be in the hash: leaving it out let two datasets — one passing,
        // one failing — share a content hash, so the clean one's certificate verified against the
        // corrupt one.
        e.opt(&self.declared_frame_count, |e, n| e.u64(*n));

        // ego_poses (autonomy trajectory): absent for manipulation episodes. Order-insensitive —
        // canonicalized by (ts, pose) — so the same set of poses hashes identically regardless of the
        // Vec order it was built in.
        e.opt(&self.ego_poses, |e, poses| {
            let mut v: Vec<&EgoPose> = poses.iter().collect();
            v.sort_by(|a, b| {
                a.ts.cmp(&b.ts)
                    .then_with(|| ego_pose_bits(a).cmp(&ego_pose_bits(b)))
            });
            e.seq(&v, |e, p| {
                e.i64(p.ts);
                e.pose(&p.pose);
            });
        });
    }
}

impl Stream {
    fn encode(&self, e: &mut Encoder) {
        e.str(&self.name);
        e.str(self.modality.tag());
        e.opt(&self.declared_rate_hz, |e, r| e.f64(*r));
        e.str(&self.clock_id);
        e.opt(&self.dtype, |e, d| e.str(d));
        e.opt(&self.shape, |e, sh| e.seq(sh, |e, d| e.u64(*d)));
        // Stored statistics (from the source manifest): the scalar summary and, for a multi-DoF
        // feature, the per-dimension breakdown. Both are source content, so both bind into the hash —
        // omitting `dim_stats` let two datasets with a different corrupted per-joint stat collide.
        e.opt(&self.stats, |e, s| e.stats(s));
        e.opt(&self.dim_stats, |e, dims| e.dim_stats(dims));
        // Recomputed statistics (Veridex's own pass over the values). Bound too, so this hand-written
        // encoder stays in lockstep with the struct — every content field is hashed, none silently
        // dropped. (They are deterministic functions of the hashed frames, so this cannot desync.)
        e.opt(&self.observed_stats, |e, s| e.stats(s));
        e.opt(&self.observed_saturation, |e, s| {
            e.u64(s.sample_count);
            e.u64(s.at_min);
            e.u64(s.at_max);
            e.f64(s.min);
            e.f64(s.max);
            e.u64(s.dim);
        });
        e.opt(&self.observed_non_finite, |e, n| e.u64(*n));
        e.opt(&self.observed_dim_stats, |e, dims| e.dim_stats(dims));
        // point_fields (autonomy point-cloud layout): absent for non-cloud streams. Order is
        // significant (the point record's field order), so it is preserved, not sorted.
        e.opt(&self.point_fields, |e, pfs| {
            e.seq(pfs, |e, pf: &PointField| {
                e.str(&pf.name);
                e.opt(&pf.dtype, |e, d| e.str(d));
            })
        });
        // frames: order is data-defined and preserved (the recorded timeline)
        e.seq(&self.frames, |e, f| f.encode(e));
    }
}

impl Frame {
    fn encode(&self, e: &mut Encoder) {
        e.i64(self.ts);
        self.value_ref.encode(e);
    }
}

impl ValueRef {
    fn encode(&self, e: &mut Encoder) {
        e.str(&self.uri);
        e.opt(&self.byte_offset, |e, o| e.u64(*o));
        e.opt(&self.byte_len, |e, l| e.u64(*l));
        e.opt(&self.content_hash, |e, h| e.bytes(h));
    }
}

impl Provenance {
    fn encode(&self, e: &mut Encoder) {
        encode_scope(&self.scope, e);
        // elements: order-insensitive, canonicalized by full content (key alone is not unique — the
        // same key can appear with a different value/class).
        let mut elements: Vec<&ProvenanceElement> = self.elements.iter().collect();
        elements.sort_by(|a, b| element_sort_key(a).cmp(&element_sort_key(b)));
        e.seq(&elements, |e, el| {
            e.str(&el.key);
            e.opt(&el.value, |e, v| e.str(v));
            e.str(el.class.tag());
        });
    }
}

fn encode_scope(scope: &ProvenanceScope, e: &mut Encoder) {
    match scope {
        ProvenanceScope::Dataset => e.u8(0),
        ProvenanceScope::Episode(idx) => {
            e.u8(1);
            e.u64(*idx);
        }
        ProvenanceScope::Stream { episode, stream } => {
            e.u8(2);
            e.u64(*episode);
            e.str(stream);
        }
    }
}

/// A total-order key for provenance scopes, used only to canonicalize record order.
fn scope_key(scope: &ProvenanceScope) -> (u8, u64, &str) {
    match scope {
        ProvenanceScope::Dataset => (0, 0, ""),
        ProvenanceScope::Episode(idx) => (1, *idx, ""),
        ProvenanceScope::Stream { episode, stream } => (2, *episode, stream.as_str()),
    }
}

/// A total ordering key for a provenance element: its full (key, value, class) content, so two
/// elements sharing a `key` never tie.
pub(crate) type ElementKey<'a> = (&'a str, Option<&'a str>, &'a str);

pub(crate) fn element_sort_key(el: &ProvenanceElement) -> ElementKey<'_> {
    (el.key.as_str(), el.value.as_deref(), el.class.tag())
}

/// A total ordering key for a provenance record: its scope plus every element's content (sorted), so
/// two records in the same scope never tie and the encoding is permutation-independent.
pub(crate) type ProvKey<'a> = ((u8, u64, &'a str), Vec<ElementKey<'a>>);

pub(crate) fn prov_sort_key(p: &Provenance) -> ProvKey<'_> {
    let mut elements: Vec<_> = p.elements.iter().map(element_sort_key).collect();
    elements.sort();
    (scope_key(&p.scope), elements)
}

/// The digest of an episode's own canonical encoding, used only to break a tie between two episodes
/// carrying the same `index`.
///
/// `index` alone is not a total order, and duplicate indices are not hypothetical — flagging them is
/// `structural.episode-boundary`'s job — so without a tie-break the hash depended on the `Vec` order
/// of exactly the datasets Veridex exists to catch. `Ordering::then_with` is lazy, so this is
/// computed only for episodes that actually tie.
pub(crate) fn episode_digest(ep: &Episode) -> [u8; 32] {
    let mut e = Encoder::new();
    ep.encode(&mut e);
    e.finish()
}

/// The digest of a stream's own canonical encoding, breaking a tie between two streams with the same
/// `name`. Uniqueness of stream names within an episode is a CDM invariant nothing enforces, and
/// `semantic.stream-key-clarity` exists to report violations of it — so the ordering must not assume it.
pub(crate) fn stream_digest(s: &Stream) -> [u8; 32] {
    let mut e = Encoder::new();
    s.encode(&mut e);
    e.finish()
}

/// A total ordering key for a label: its content, all of it.
pub(crate) fn label_sort_key(l: &Label) -> (&str, &str, Option<i64>) {
    (l.key.as_str(), l.value.as_str(), l.ts)
}

/// Canonical f64 bit pattern: normalize signed zero and all NaN payloads so `-0.0`/`+0.0` and any NaN
/// map to one value. Shared by the encoder (so the hash is stable) and the content sort keys below (so
/// the order canonicalization stays in lockstep with what is hashed).
pub(crate) fn canon_f64_bits(v: f64) -> u64 {
    if v.is_nan() {
        0x7ff8_0000_0000_0000
    } else if v == 0.0 {
        0 // collapses -0.0 to +0.0
    } else {
        v.to_bits()
    }
}

/// A total ordering key for a [`Transform`]: its full content, so two transforms sharing frames and a
/// validity range never tie and the tree encoding is permutation-independent.
pub(crate) type TransformKey<'a> = (
    &'a str,
    &'a str,
    Option<i64>,
    Option<i64>,
    [u64; 3],
    [u64; 4],
);

pub(crate) fn transform_sort_key(t: &Transform) -> TransformKey<'_> {
    (
        t.parent_frame.as_str(),
        t.child_frame.as_str(),
        t.valid_from,
        t.valid_to,
        t.pose.translation.map(canon_f64_bits),
        t.pose.rotation.map(canon_f64_bits),
    )
}

/// A total ordering key for [`CameraIntrinsics`]: its full content.
pub(crate) type IntrinsicsKey<'a> = (&'a str, Option<i64>, Option<i64>, [u64; 4], Vec<u64>);

pub(crate) fn intrinsics_sort_key(c: &CameraIntrinsics) -> IntrinsicsKey<'_> {
    (
        c.stream.as_str(),
        c.valid_from,
        c.valid_to,
        [
            canon_f64_bits(c.fx),
            canon_f64_bits(c.fy),
            canon_f64_bits(c.cx),
            canon_f64_bits(c.cy),
        ],
        c.distortion.iter().copied().map(canon_f64_bits).collect(),
    )
}

/// Content tie-break key for an [`EgoPose`] (used only to order poses that share a timestamp).
pub(crate) fn ego_pose_bits(p: &EgoPose) -> ([u64; 3], [u64; 4]) {
    (
        p.pose.translation.map(canon_f64_bits),
        p.pose.rotation.map(canon_f64_bits),
    )
}
