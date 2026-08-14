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
    Dataset, DimStats, Episode, Frame, Label, Provenance, ProvenanceElement, ProvenanceScope,
    Stream, StreamStats, ValueRef,
};

/// Version of the canonical encoding. Bumping this deliberately changes every content hash; it is
/// mixed into the domain-separation prefix so hashes from different encoding versions never collide.
///
/// v2 binds every content-bearing `Stream` field into the hash — the stored per-dimension stats
/// (`dim_stats`) and the recomputed `observed_*` fields, which v1's hand-written encoder silently
/// dropped, so two datasets differing only in those fields no longer collide.
pub const CANONICAL_VERSION: u32 = 2;

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
        let bits = if v.is_nan() {
            0x7ff8_0000_0000_0000u64
        } else if v == 0.0 {
            0u64 // collapses -0.0 to +0.0
        } else {
            v.to_bits()
        };
        self.u64(bits);
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
        episodes.sort_by_key(|ep| ep.index);
        e.seq(&episodes, |e, ep| ep.encode(e));
    }
}

impl Episode {
    fn encode(&self, e: &mut Encoder) {
        e.u64(self.index);
        e.opt(&self.start_ts, |e, ts| e.i64(*ts));
        e.opt(&self.end_ts, |e, ts| e.i64(*ts));

        // streams: order-insensitive, canonicalized by name
        let mut streams: Vec<&Stream> = self.streams.iter().collect();
        streams.sort_by(|a, b| a.name.cmp(&b.name));
        e.seq(&streams, |e, s| s.encode(e));

        e.opt(&self.task, |e, t| e.str(t));

        // labels: order-insensitive, canonicalized by (key, value, ts)
        let mut labels: Vec<&Label> = self.labels.iter().collect();
        labels.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.value.cmp(&b.value))
                .then_with(|| a.ts.cmp(&b.ts))
        });
        e.seq(&labels, |e, l| {
            e.str(&l.key);
            e.str(&l.value);
            e.opt(&l.ts, |e, t| e.i64(*t));
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
type ElementKey<'a> = (&'a str, Option<&'a str>, &'a str);

fn element_sort_key(el: &ProvenanceElement) -> ElementKey<'_> {
    (el.key.as_str(), el.value.as_deref(), el.class.tag())
}

/// A total ordering key for a provenance record: its scope plus every element's content (sorted), so
/// two records in the same scope never tie and the encoding is permutation-independent.
type ProvKey<'a> = ((u8, u64, &'a str), Vec<ElementKey<'a>>);

fn prov_sort_key(p: &Provenance) -> ProvKey<'_> {
    let mut elements: Vec<_> = p.elements.iter().map(element_sort_key).collect();
    elements.sort();
    (scope_key(&p.scope), elements)
}
