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
    Dataset, Episode, Frame, Label, Provenance, ProvenanceElement, ProvenanceScope, Stream,
    ValueRef,
};

/// Version of the canonical encoding. Bumping this deliberately changes every content hash; it is
/// mixed into the domain-separation prefix so hashes from different encoding versions never collide.
pub const CANONICAL_VERSION: u32 = 1;

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

        // provenance: order-insensitive, canonicalized by scope
        let mut provenance: Vec<&Provenance> = self.provenance.iter().collect();
        provenance.sort_by(|a, b| scope_key(&a.scope).cmp(&scope_key(&b.scope)));
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

        // labels: order-insensitive, canonicalized by (key, value)
        let mut labels: Vec<&Label> = self.labels.iter().collect();
        labels.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.value.cmp(&b.value)));
        e.seq(&labels, |e, l| {
            e.str(&l.key);
            e.str(&l.value);
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
        e.opt(&self.stats, |e, s| {
            e.f64(s.min);
            e.f64(s.max);
            e.f64(s.mean);
            e.f64(s.std);
        });
        e.opt(&self.observed_stats, |e, s| {
            e.f64(s.min);
            e.f64(s.max);
            e.f64(s.mean);
            e.f64(s.std);
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
        // elements: order-insensitive, canonicalized by key
        let mut elements: Vec<&ProvenanceElement> = self.elements.iter().collect();
        elements.sort_by(|a, b| a.key.cmp(&b.key));
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
