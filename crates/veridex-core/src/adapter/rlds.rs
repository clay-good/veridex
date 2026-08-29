//! RLDS / TFDS adapter: the format Open X-Embodiment and most TFDS-published robot datasets ship
//! in, read into the CDM.
//!
//! A TFDS dataset directory holds two manifests and a set of record shards:
//!
//! - `dataset_info.json` — name, version, file format, per-split shard lengths, license, citation.
//! - `features.json` — the feature tree. RLDS nests every per-step feature under a `steps`
//!   sequence (`observation/*`, `action`, `reward`, `is_first`, …), and each leaf declares its
//!   dtype and per-step shape.
//! - `*.tfrecord-XXXXX-of-YYYYY` — one TFRecord per **episode**, each a `tf.train.Example` whose
//!   keys are the feature tree flattened with `/` and whose values are every step's values
//!   concatenated into one list.
//!
//! So an episode's step count is not stored anywhere: it is *derived*, per feature, by dividing the
//! serialized list length by the element size `features.json` declares. This adapter does that
//! division for every step feature and requires the answers to agree — a record whose features
//! disagree contradicts the schema it was serialized against, and is rejected as a parse error
//! rather than mapped into a CDM that would quietly under-report an episode (design D2: ingestion
//! never silently drops information that could affect a verdict).
//!
//! **The timeline is the step index.** RLDS records no wall clock — there is no per-step timestamp
//! in the format — so frames are stamped with their step index on a clock named
//! `rlds-step-index`, and the ingest report states the omission. Nothing here fabricates a rate:
//! `declared_rate_hz` stays `None`, so the rate and gap checks abstain rather than grade a dataset
//! against a period Veridex made up.
//!
//! Integrity is checked as the file is read: every TFRecord frames its payload with masked CRC-32C
//! checksums over both the length prefix and the payload, and both are verified. A shard that fails
//! is a corrupt shard, and is reported as such instead of being parsed past.
//!
//! Step values are fingerprinted into `frame.value_ref.content_hash` (never stored), so the CDM
//! content hash is sensitive to actual recorded content, exactly as in the other adapters.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, Episode, Frame, Label, Modality, Provenance, ProvenanceClass,
    ProvenanceElement, ProvenanceScope, Stream, ValueRef,
};

use super::{
    Adapter, Coverage, Detection, FrameBudget, IngestError, IngestOptions, IngestReport, Ingested,
    Sample, Source, UnmappedField,
};

const FORMAT_ID: &str = "rlds";

/// The clock RLDS frames are stamped on. Named for what it is — a step index, not a wall clock — so
/// a reader of the CDM, a report, or a certificate can never mistake it for measured time.
const CLOCK_ID: &str = "rlds-step-index";

/// TFDS record containers this adapter reads. `array_record`, TFDS's other backend, is a different
/// container and is rejected by name rather than mis-parsed.
const SUPPORTED_FILE_FORMATS: &[&str] = &["tfrecord"];

/// The flattened-key prefix TFDS gives every per-step feature of an RLDS episode.
const STEPS_PREFIX: &str = "steps/";

/// The flattened key holding the raw file an episode was converted from — the one lineage fact the
/// RLDS conversion scripts carry through.
const EPISODE_SOURCE_FILE_KEY: &str = "episode_metadata/file_path";

/// The ceiling on how large an episode set this adapter will materialize to resolve a sample.
///
/// Resolving a draw means building the set of candidate episode indices, and that set's size comes
/// from `dataset_info.json` — a few hundred bytes of attacker-controllable JSON, read before any
/// ingest budget exists. A manifest declaring 16 million episodes measured 75 seconds and about a
/// gigabyte; `u64::MAX` is a hang. Chosen far above any real dataset: the largest Open X-Embodiment
/// datasets are in the hundreds of thousands of episodes.
const MAX_EPISODE_SET_FOR_SAMPLING: u64 = 1_000_000;

/// Leaf names RLDS uses for the natural-language instruction. Matched on the final path segment, so
/// both the top-level (`steps/language_instruction`) and observation-nested
/// (`steps/observation/natural_language_instruction`) conventions resolve.
const INSTRUCTION_LEAVES: &[&str] = &["language_instruction", "natural_language_instruction"];

/// Adapter for an RLDS dataset published in the TFDS on-disk layout.
pub struct RldsAdapter;

impl RldsAdapter {
    fn info_path(dir: &Path) -> PathBuf {
        dir.join("dataset_info.json")
    }
    fn features_path(dir: &Path) -> PathBuf {
        dir.join("features.json")
    }
}

// ---- CRC-32C (Castagnoli), as TFRecord frames it ----

/// The reflected CRC-32C table, built at compile time.
const CRC32C_TABLE: [u32; 256] = build_crc32c_table();

const fn build_crc32c_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

fn crc32c(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc = CRC32C_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

/// TFRecord stores a *masked* CRC so a checksum can never be confused with the data it covers.
fn mask_crc(crc: u32) -> u32 {
    crc.rotate_right(15).wrapping_add(0xA282_EAD8)
}

// ---- TFRecord framing ----

/// Walks the records of one shard, verifying both checksums, without holding the shard in memory.
///
/// Real Open X-Embodiment shards run from 60 MB to nearly a gigabyte (`kuka`'s first shard is
/// 850 MB), and a dataset ships hundreds of them. Reading one whole shard to look at one episode —
/// which is what `--sample-episodes 1` did — is an allocation the size of the file for a few
/// kilobytes of interest. This reads a record at a time into a buffer it reuses, so peak memory is
/// the largest single episode rather than the largest shard, and an unselected record is stepped
/// over by seeking rather than by reading its payload at all.
struct ShardReader {
    file: BufReader<File>,
    /// The shard's size on disk. Every declared record length is checked against it before a byte is
    /// allocated, so a corrupt length cannot ask for an arbitrary allocation.
    file_len: u64,
    pos: u64,
    ordinal: u64,
    /// Reused across records: one episode's payload at a time.
    buf: Vec<u8>,
}

/// One record's framing, read before deciding whether to read its payload.
struct RecordHeader {
    payload_len: u64,
    ordinal: u64,
    /// Byte offset of the record's own header, for error messages.
    at: u64,
}

impl ShardReader {
    fn open(path: &Path) -> Result<Self, IngestError> {
        let file = File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
        let file_len = file
            .metadata()
            .map_err(|e| IngestError::Io(e.to_string()))?
            .len();
        Ok(ShardReader {
            file: BufReader::new(file),
            file_len,
            pos: 0,
            ordinal: 0,
            buf: Vec::new(),
        })
    }

    /// The next record's framing, `None` at a clean end of file, or `Err` with what is wrong.
    ///
    /// A record is: u64 length (LE), masked CRC-32C of those 8 bytes, payload, masked CRC-32C of the
    /// payload. The length prefix is checksummed here, before its value is used for anything.
    fn next_header(&mut self) -> Option<Result<RecordHeader, String>> {
        if self.pos >= self.file_len {
            return None;
        }
        let at = self.pos;
        let mut header = [0u8; 12];
        if let Err(e) = self.file.read_exact(&mut header) {
            // A trailing fragment shorter than a header is truncation, not a clean end.
            return Some(Err(format!(
                "truncated record header at byte {at} ({} bytes remain, 12 needed): {e}",
                self.file_len.saturating_sub(at),
            )));
        }
        self.pos += 12;
        let payload_len = u64::from_le_bytes(header[0..8].try_into().expect("8 bytes"));
        let length_crc = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes"));
        if mask_crc(crc32c(&header[0..8])) != length_crc {
            return Some(Err(format!(
                "record {} at byte {at}: length-prefix checksum mismatch (the shard is corrupt)",
                self.ordinal
            )));
        }
        // Checked against the file's own size before anything is allocated: `payload_len` comes
        // straight from the file, so an absurd value must fail cleanly rather than wrap or ask for
        // a gigabyte.
        let ends_at = payload_len
            .checked_add(4)
            .and_then(|n| self.pos.checked_add(n));
        match ends_at {
            Some(end) if end <= self.file_len => {}
            _ => {
                return Some(Err(format!(
                    "record {} at byte {at}: declares {payload_len} payload bytes, past the end of \
                     a {}-byte shard",
                    self.ordinal, self.file_len
                )))
            }
        }
        let ordinal = self.ordinal;
        self.ordinal += 1;
        Some(Ok(RecordHeader {
            payload_len,
            ordinal,
            at,
        }))
    }

    /// Read and verify the record's payload. Borrowed from a buffer this reader reuses, so it is
    /// valid only until the next call.
    fn read_payload(&mut self, header: &RecordHeader) -> Result<&[u8], String> {
        // Bounded by `next_header` against the shard's real size, so this resize cannot be asked for
        // more than the file holds.
        let len = usize::try_from(header.payload_len).map_err(|_| {
            format!(
                "record {} at byte {}: {} payload bytes do not fit in this platform's address space",
                header.ordinal, header.at, header.payload_len
            )
        })?;
        self.buf.resize(len, 0);
        let mut trailer = [0u8; 4];
        self.file
            .read_exact(&mut self.buf)
            .and_then(|()| self.file.read_exact(&mut trailer))
            .map_err(|e| {
                format!(
                    "record {} at byte {}: shard ended mid-record: {e}",
                    header.ordinal, header.at
                )
            })?;
        self.pos += header.payload_len + 4;
        if mask_crc(crc32c(&self.buf)) != u32::from_le_bytes(trailer) {
            return Err(format!(
                "record {} at byte {}: payload checksum mismatch (the shard is corrupt)",
                header.ordinal, header.at
            ));
        }
        Ok(&self.buf)
    }

    /// Step over a record whose episode this run did not select, without reading its payload.
    ///
    /// This is what makes sampling cheap: the record is framed and its length prefix verified, but
    /// its bytes are never read, so drawing 10 episodes from a 900 MB shard costs 10 episodes of
    /// I/O. The consequence, stated because it is a real limit: an unselected record's *payload*
    /// checksum is not verified, so a sampled run does not attest the integrity of what it skipped.
    fn skip_payload(&mut self, header: &RecordHeader) -> Result<(), String> {
        let skip = header.payload_len + 4;
        self.file
            .seek_relative(skip as i64)
            .map_err(|e| format!("record {} at byte {}: {e}", header.ordinal, header.at))?;
        self.pos += skip;
        Ok(())
    }
}

// ---- Minimal protobuf reader, enough for `tf.train.Example` ----

struct Buf<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Buf<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Buf { bytes, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = *self.bytes.get(self.pos)?;
            self.pos += 1;
            // Ten groups of 7 bits is the most a u64 can hold; more is a malformed varint.
            if shift >= 64 {
                return None;
            }
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
            shift += 7;
        }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }

    fn length_delimited(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.varint()?).ok()?;
        self.take(len)
    }

    /// The next `(field_number, wire_type)`.
    fn tag(&mut self) -> Option<(u64, u8)> {
        let key = self.varint()?;
        Some((key >> 3, (key & 0x7) as u8))
    }

    /// Skip a field whose contents this parser does not need.
    fn skip(&mut self, wire: u8) -> Option<()> {
        match wire {
            0 => self.varint().map(|_| ()),
            1 => self.take(8).map(|_| ()),
            2 => self.length_delimited().map(|_| ()),
            5 => self.take(4).map(|_| ()),
            _ => None,
        }
    }
}

/// One serialized `tf.train.Feature`: a list of one of three element types.
#[derive(Debug)]
enum FeatureValues<'a> {
    Bytes(Vec<&'a [u8]>),
    /// Float values kept as their **raw wire bits**, never as `f32`. A fingerprint must depend only
    /// on the bytes the file holds: loading a signaling NaN into a float register and storing it
    /// back can quiet it on some targets, which would make the same dataset hash differently there.
    Floats(Vec<u32>),
    Ints(Vec<i64>),
}

impl FeatureValues<'_> {
    fn len(&self) -> u64 {
        match self {
            FeatureValues::Bytes(v) => v.len() as u64,
            FeatureValues::Floats(v) => v.len() as u64,
            FeatureValues::Ints(v) => v.len() as u64,
        }
    }

    /// The element type's name, for a fidelity note.
    fn kind(&self) -> &'static str {
        match self {
            FeatureValues::Bytes(_) => "bytes_list",
            FeatureValues::Floats(_) => "float_list",
            FeatureValues::Ints(_) => "int64_list",
        }
    }

    /// SHA-256 over the elements in `range` — one step's values. Byte entries are length-prefixed so
    /// that two different splits of the same bytes cannot fingerprint alike.
    fn hash_range(&self, start: usize, end: usize) -> [u8; 32] {
        let mut hasher = Sha256::new();
        match self {
            FeatureValues::Bytes(v) => {
                for entry in &v[start..end] {
                    hasher.update((entry.len() as u64).to_le_bytes());
                    hasher.update(entry);
                }
            }
            FeatureValues::Floats(v) => {
                for value in &v[start..end] {
                    hasher.update(value.to_le_bytes());
                }
            }
            FeatureValues::Ints(v) => {
                for value in &v[start..end] {
                    hasher.update(value.to_le_bytes());
                }
            }
        }
        hasher.finalize().into()
    }

    /// The byte entry at `index`, when this is a `bytes_list` — how an instruction string is read.
    fn bytes_at(&self, index: usize) -> Option<&[u8]> {
        match self {
            FeatureValues::Bytes(v) => v.get(index).copied(),
            _ => None,
        }
    }
}

/// Parse a `tf.train.Example` into its feature map. `None` for anything malformed — a record that
/// does not decode is never partially believed.
fn parse_example(record: &[u8]) -> Option<BTreeMap<String, FeatureValues<'_>>> {
    let mut out: BTreeMap<String, FeatureValues<'_>> = BTreeMap::new();
    let mut example = Buf::new(record);
    while !example.done() {
        let (field, wire) = example.tag()?;
        match (field, wire) {
            // Example.features
            (1, 2) => {
                let features = example.length_delimited()?;
                let mut features = Buf::new(features);
                while !features.done() {
                    let (field, wire) = features.tag()?;
                    match (field, wire) {
                        // Features.feature: map<string, Feature>, one message per entry.
                        (1, 2) => {
                            let entry = features.length_delimited()?;
                            let (key, values) = parse_map_entry(entry)?;
                            // A duplicate key makes the record's meaning ambiguous; refuse it rather
                            // than pick a winner.
                            if out.insert(key, values).is_some() {
                                return None;
                            }
                        }
                        _ => features.skip(wire)?,
                    }
                }
            }
            _ => example.skip(wire)?,
        }
    }
    Some(out)
}

/// Parse one `map<string, Feature>` entry.
///
/// Both fields are refused on a repeat rather than letting the last one win. That is the guard
/// [`set_once`] applies to `Feature`'s oneof and the one the caller applies to duplicate map keys,
/// and it was missing exactly one level down: an entry carrying the `value` submessage *twice* had
/// its first instance silently dropped. Protobuf's own rule for a repeated embedded message is to
/// *merge* them, so TensorFlow concatenates the two `float_list`s and reads three steps where
/// Veridex read two — the same shard bytes yielding a different episode length depending on who
/// reads them, and Veridex certifying its own answer. Refusing means the two can never disagree.
fn parse_map_entry(entry: &[u8]) -> Option<(String, FeatureValues<'_>)> {
    let mut buf = Buf::new(entry);
    let mut key: Option<String> = None;
    let mut values: Option<FeatureValues<'_>> = None;
    while !buf.done() {
        let (field, wire) = buf.tag()?;
        match (field, wire) {
            (1, 2) => {
                if key.is_some() {
                    return None;
                }
                key = Some(String::from_utf8(buf.length_delimited()?.to_vec()).ok()?);
            }
            (2, 2) => set_once(&mut values, parse_feature(buf.length_delimited()?)?)?,
            _ => buf.skip(wire)?,
        }
    }
    Some((key?, values?))
}

/// Fill `slot`, or fail if it already held a list.
///
/// `tf.train.Feature` is a `oneof`: exactly one of `bytes_list` / `float_list` / `int64_list`. A
/// record carrying two of them has no single meaning, and letting the last one win would hand an
/// attacker a way to make the same bytes yield a different step count depending on which list a
/// reader honors. Refused for the same reason a duplicate map key is.
fn set_once<'a>(slot: &mut Option<FeatureValues<'a>>, values: FeatureValues<'a>) -> Option<()> {
    match slot {
        Some(_) => None,
        None => {
            *slot = Some(values);
            Some(())
        }
    }
}

fn parse_feature(feature: &[u8]) -> Option<FeatureValues<'_>> {
    let mut buf = Buf::new(feature);
    let mut out: Option<FeatureValues<'_>> = None;
    while !buf.done() {
        let (field, wire) = buf.tag()?;
        match (field, wire) {
            // Feature.bytes_list
            (1, 2) => {
                let list = buf.length_delimited()?;
                let mut entries = Vec::new();
                let mut list = Buf::new(list);
                while !list.done() {
                    let (field, wire) = list.tag()?;
                    match (field, wire) {
                        (1, 2) => entries.push(list.length_delimited()?),
                        _ => list.skip(wire)?,
                    }
                }
                set_once(&mut out, FeatureValues::Bytes(entries))?;
            }
            // Feature.float_list
            (2, 2) => {
                let list = buf.length_delimited()?;
                let mut entries = Vec::new();
                let mut list = Buf::new(list);
                while !list.done() {
                    let (field, wire) = list.tag()?;
                    match (field, wire) {
                        // Packed (the normal encoding) …
                        (1, 2) => {
                            let packed = list.length_delimited()?;
                            if packed.len() % 4 != 0 {
                                return None;
                            }
                            for chunk in packed.chunks_exact(4) {
                                entries
                                    .push(u32::from_le_bytes(chunk.try_into().expect("4 bytes")));
                            }
                        }
                        // … and unpacked, which the wire format still permits.
                        (1, 5) => {
                            let raw = list.take(4)?;
                            entries.push(u32::from_le_bytes(raw.try_into().expect("4 bytes")));
                        }
                        _ => list.skip(wire)?,
                    }
                }
                set_once(&mut out, FeatureValues::Floats(entries))?;
            }
            // Feature.int64_list
            (3, 2) => {
                let list = buf.length_delimited()?;
                let mut entries = Vec::new();
                let mut list = Buf::new(list);
                while !list.done() {
                    let (field, wire) = list.tag()?;
                    match (field, wire) {
                        (1, 2) => {
                            let packed = list.length_delimited()?;
                            let mut packed = Buf::new(packed);
                            while !packed.done() {
                                entries.push(packed.varint()? as i64);
                            }
                        }
                        (1, 0) => entries.push(list.varint()? as i64),
                        _ => list.skip(wire)?,
                    }
                }
                set_once(&mut out, FeatureValues::Ints(entries))?;
            }
            _ => buf.skip(wire)?,
        }
    }
    out
}

// ---- The TFDS manifests ----

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DatasetInfoJson {
    name: Option<String>,
    version: Option<String>,
    citation: Option<String>,
    file_format: Option<String>,
    module_name: Option<String>,
    splits: Option<Vec<SplitJson>>,
    redistribution_info: Option<RedistributionJson>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SplitJson {
    /// The split's name (`train`, `test`, …), used to say which split a manifest gap belongs to.
    name: Option<String>,
    /// Episodes per shard. Proto3 JSON writes int64 as a string, so these arrive as strings.
    shard_lengths: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct RedistributionJson {
    license: Option<String>,
}

/// A JSON number that proto3 may have written as a string.
fn json_u64(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// One mappable leaf of the TFDS feature tree.
#[derive(Debug, Clone)]
struct Leaf {
    /// The flattened key, exactly as the serialized `Example` spells it.
    path: String,
    /// Whether the leaf lives inside the `steps` sequence (one value per step) or is episode-level.
    per_step: bool,
    dtype: Option<String>,
    /// The per-step element shape, when every dimension is declared.
    shape: Option<Vec<u64>>,
    /// Values per step: the product of `shape` for a tensor, 1 for an encoded image or a string.
    /// `None` when the declared shape has an unknown dimension, so no division is possible.
    elem_len: Option<u64>,
    modality: Modality,
}

impl Leaf {
    /// The final path segment — the feature's own name.
    fn leaf_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    fn is_instruction(&self) -> bool {
        INSTRUCTION_LEAVES.contains(&self.leaf_name())
    }
}

/// A declared shape, or `None` when any dimension is unknown (TFDS writes `-1`).
fn shape_dims(shape: Option<&serde_json::Value>) -> Option<Vec<u64>> {
    let Some(shape) = shape else {
        // No `shape` object at all is a scalar, which has a known (empty) shape.
        return Some(Vec::new());
    };
    // A `shape` that is present but is not an object does not declare a scalar — it declares
    // something this reader cannot interpret, and reading it as a scalar sets the element width to
    // 1. A `"shape": ["7"]` beside 14 float32s then yields 14 frames where 2 was right: a 7x
    // inflated timeline, `shape: None`, and nothing in `unmapped_fields` or `omitted_fields` to say
    // so. `shape_dims` failed closed for `-1` and for a non-array `dimensions`, and failed open
    // here. TFDS itself always writes the object form, so this is a malformed-manifest guard.
    if !shape.is_object() {
        return None;
    }
    let Some(dims) = shape.get("dimensions") else {
        return Some(Vec::new());
    };
    let dims = dims.as_array()?;
    let mut out = Vec::with_capacity(dims.len());
    for dim in dims {
        // `-1` is TFDS for "this dimension varies"; a shape holding one cannot size an element.
        out.push(json_u64(dim)?);
    }
    Some(out)
}

/// TFDS spells dtypes both as numpy names (`uint8`) and as TF enum names (`DT_UINT8`).
fn normalize_dtype(dtype: Option<&serde_json::Value>) -> Option<String> {
    let raw = dtype?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    Some(lower.strip_prefix("dt_").unwrap_or(&lower).to_string())
}

fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Walk the TFDS feature tree, flattening it exactly the way TFDS serializes it: nested dicts join
/// with `/`, and a sequence contributes no key of its own — its leaves keep the sequence's prefix
/// and hold every step's values concatenated.
fn walk_features(
    node: &serde_json::Value,
    path: &str,
    in_sequence: bool,
    leaves: &mut Vec<Leaf>,
    unmapped: &mut Vec<UnmappedField>,
) {
    if let Some(dict) = node.get("featuresDict") {
        match dict.get("features") {
            Some(serde_json::Value::Object(map)) => {
                // `serde_json`'s default map is ordered, so the walk is deterministic.
                for (name, child) in map {
                    walk_features(child, &join_path(path, name), in_sequence, leaves, unmapped);
                }
            }
            // Proto3 JSON omits an empty map, so `{"featuresDict": {}}` is a real (and legal)
            // shape — an empty group, not a malformed one.
            None => {}
            Some(_) => unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note: "`featuresDict.features` is not an object, so this group's leaves cannot be \
                       walked; no stream is built from it"
                    .into(),
            }),
        }
        return;
    }
    if let Some(sequence) = node.get("sequence") {
        if in_sequence {
            // A sequence inside a sequence is ragged: TFDS serializes it with `ragged_flat_values`
            // and `ragged_row_lengths_N` side tables this parser does not read, so nothing under it
            // is claimed. (TFDS emits those only at sequence rank >= 2, which is exactly here.)
            unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note: "a nested (ragged) sequence is serialized with row-length side tables that \
                       this adapter does not read; no stream is built from it"
                    .into(),
            });
            return;
        }
        match sequence.get("feature") {
            Some(inner) => walk_features(inner, path, true, leaves, unmapped),
            None => unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note: "a sequence with no inner `feature` declares nothing to read; no stream is \
                       built from it"
                    .into(),
            }),
        }
        return;
    }
    if let Some(image) = node.get("image") {
        // An Image is serialized as one encoded blob (JPEG/PNG) per step, whatever its pixel shape.
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            dtype: normalize_dtype(image.get("dtype")),
            shape: shape_dims(image.get("shape")).filter(|s| !s.is_empty()),
            elem_len: Some(1),
            modality: Modality::Video,
        });
        return;
    }
    if node.get("text").is_some() {
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            dtype: Some("string".into()),
            shape: None,
            elem_len: Some(1),
            modality: modality_for(path, Modality::ScalarState),
        });
        return;
    }
    if let Some(tensor) = node.get("tensor") {
        let shape = shape_dims(tensor.get("shape"));
        // An *encoded* tensor is not serialized as its declared shape. When `encoding` is anything
        // but `none`, TFDS replaces the serialized spec with a single opaque blob per step
        // (`TensorInfo(shape=(), dtype=object)`), whatever the declared dimensions say — this is how
        // `kuka`, one of the largest Open X-Embodiment datasets, stores several of its observations.
        // Dividing by the declared shape's product there would refuse a sound dataset and blame the
        // data for the adapter's mistake.
        let encoded = tensor
            .get("encoding")
            .and_then(|e| e.as_str())
            .map(|e| !matches!(e.trim().to_ascii_lowercase().as_str(), "" | "none"))
            .unwrap_or(false);
        let elem_len = if encoded {
            Some(1)
        } else {
            match shape.as_ref().map(|dims| checked_product(dims)) {
                Some(Some(n)) if n > 0 => Some(n),
                _ => None,
            }
        };
        if elem_len.is_none() {
            unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note:
                    "the declared shape has an unknown, zero, or unrepresentably large dimension, \
                       so the values per step cannot be derived; no stream is built from it"
                        .into(),
            });
            return;
        }
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            dtype: normalize_dtype(tensor.get("dtype")),
            shape: shape.filter(|s| !s.is_empty()),
            elem_len,
            modality: modality_for(path, Modality::ScalarState),
        });
        return;
    }
    if node.get("classLabel").is_some() {
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            // `ClassLabel` carries only `num_classes`; TFDS serializes it as an int64.
            dtype: Some("int64".into()),
            shape: None,
            elem_len: Some(1),
            modality: Modality::ScalarState,
        });
        return;
    }
    unmapped.push(UnmappedField {
        source_path: path.to_string(),
        note: "feature class is not one the CDM represents (not a tensor, image, text, or class \
               label); no stream is built from it"
            .into(),
    });
}

/// The product of a declared shape's dimensions, or `None` when it overflows.
///
/// Both factors come straight from `features.json`, so `["4294967296", "4294967296"]` — 200 bytes of
/// manifest — otherwise panics in a checked build and, worse, *wraps silently* in a release one:
/// a wrapped element size divides the serialized list into a step count that was never in the file,
/// and that fabricated timeline is what gets signed into the CDM content hash.
fn checked_product(dims: &[u64]) -> Option<u64> {
    dims.iter().try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
}

/// The modality a leaf's own name implies. Only `action` is inferred — every other leaf keeps the
/// caller's default, because guessing a modality from a substring is how honest data gets
/// mis-modelled.
fn modality_for(path: &str, default: Modality) -> Modality {
    match path.rsplit('/').next() {
        Some("action") => Modality::Action,
        _ => default,
    }
}

/// Whether a filename is a TFRecord data shard, as TFDS's default template names one:
/// `{dataset}-{split}.tfrecord-XXXXX-of-YYYYY`.
///
/// Matched on the real shape rather than on "contains `.tfrecord`", because TFDS also writes
/// `<shard>_index.json` siblings next to the shards it indexes. Those contain `.tfrecord` in their
/// names, and feeding one to the record reader would report a perfectly healthy dataset as a
/// corrupt shard.
fn is_shard_name(name: &str) -> bool {
    let Some((_, tail)) = name.split_once(".tfrecord") else {
        return false;
    };
    // What follows is `-XXXXX-of-YYYYY` and nothing else.
    let Some(rest) = tail.strip_prefix('-') else {
        return false;
    };
    match rest.split_once("-of-") {
        Some((index, total)) => {
            !index.is_empty()
                && !total.is_empty()
                && index.bytes().all(|b| b.is_ascii_digit())
                && total.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// The split a shard belongs to, from TFDS's default filename template
/// (`{dataset}-{split}.tfrecord-…`). `None` when the name does not follow it.
fn shard_split(name: &str) -> Option<String> {
    let stem = name.split_once(".tfrecord")?.0;
    let (_, split) = stem.rsplit_once('-')?;
    (!split.is_empty()).then(|| split.to_string())
}

/// The shard files of a TFDS directory, in filename order (which is shard order).
fn find_shards(dir: &Path) -> Vec<PathBuf> {
    let mut shards: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(is_shard_name)
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    // Numeric order: a shard set written `-0-of-12` … `-10-of-12` rather than TFDS's zero-padded
    // form would otherwise be read with `-10` third, which renumbers every episode after it. See
    // `super::natural_key`.
    shards.sort_by_key(|p| super::natural_key(&p.to_string_lossy()));
    shards
}

/// A path relative to the dataset root, with forward slashes on every platform — it is bound into
/// the content hash, so it must not vary with the host.
///
/// `None` for a path that is not valid UTF-8. A lossy conversion would map every undecodable byte to
/// the same replacement character, so two shards whose names differ only in such a byte would
/// produce byte-identical value references and therefore an identical content hash.
fn relative_uri(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut parts = Vec::new();
    for component in rel.components() {
        parts.push(component.as_os_str().to_str()?.to_string());
    }
    Some(parts.join("/"))
}

fn parse_error(message: String) -> IngestError {
    IngestError::Parse {
        format_id: FORMAT_ID,
        message,
    }
}

/// One ingested episode, before it becomes a CDM [`Episode`].
struct EpisodeBuild {
    index: u64,
    streams: Vec<Stream>,
    task: Option<String>,
    labels: Vec<Label>,
    steps: u64,
    /// The `episode_metadata/file_path` this episode was converted from, when the dataset records
    /// one — the single lineage fact RLDS carries.
    source_file: Option<String>,
    /// The TFDS split this episode's shard belongs to (`train`, `test`, …), when the shard's name
    /// follows TFDS's default template. A TFDS directory holds every split side by side.
    split: Option<String>,
}

impl Adapter for RldsAdapter {
    fn format_id(&self) -> &'static str {
        FORMAT_ID
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        SUPPORTED_FILE_FORMATS
    }

    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(dir)
                if RldsAdapter::info_path(dir).is_file()
                    && RldsAdapter::features_path(dir).is_file() =>
            {
                Detection::Yes { version: None }
            }
            _ => Detection::No,
        }
    }

    /// The manifest — `dataset_info.json` and `features.json` — is a few kilobytes beside a
    /// directory that, for an Open X-Embodiment dataset, runs to hundreds of gigabytes, and it is
    /// what declares the episode count, the per-step features, and the licence.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        let dir = match source {
            Source::Local(p) => p,
            Source::Remote(_) => {
                return Err(parse_error(
                    "remote RLDS/TFDS ingestion is not supported in v0.1".into(),
                ))
            }
        };

        let info_bytes = std::fs::read(RldsAdapter::info_path(dir))
            .map_err(|e| IngestError::Io(e.to_string()))?;
        let info: DatasetInfoJson = serde_json::from_slice(&info_bytes)
            .map_err(|e| parse_error(format!("dataset_info.json: {e}")))?;

        // A TFDS directory can be backed by `array_record` instead of TFRecord. That is a different
        // container, so it is refused by name rather than read as if it were one.
        let file_format = info
            .file_format
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .unwrap_or("tfrecord");
        if !SUPPORTED_FILE_FORMATS.contains(&file_format) {
            return Err(IngestError::UnsupportedVersion {
                format_id: FORMAT_ID,
                version: Some(file_format.to_string()),
                supported: SUPPORTED_FILE_FORMATS,
            });
        }

        let features_bytes = std::fs::read(RldsAdapter::features_path(dir))
            .map_err(|e| IngestError::Io(e.to_string()))?;
        let features_json: serde_json::Value = serde_json::from_slice(&features_bytes)
            .map_err(|e| parse_error(format!("features.json: {e}")))?;

        let mut leaves = Vec::new();
        let mut unmapped_fields = Vec::new();
        walk_features(&features_json, "", false, &mut leaves, &mut unmapped_fields);

        // Only the leaves inside the `steps` sequence become streams. Episode-level leaves
        // (`episode_metadata/*`) describe the episode, not a timeline.
        let step_leaves: Vec<&Leaf> = leaves
            .iter()
            .filter(|l| l.per_step && l.path.starts_with(STEPS_PREFIX))
            .collect();
        if step_leaves.is_empty() {
            return Err(parse_error(
                "features.json declares no per-step features under a `steps` sequence, so there is \
                 no RLDS timeline to read"
                    .into(),
            ));
        }
        for leaf in leaves.iter().filter(|l| l.per_step) {
            if !leaf.path.starts_with(STEPS_PREFIX) {
                unmapped_fields.push(UnmappedField {
                    source_path: leaf.path.clone(),
                    note: "a sequence outside `steps` has no RLDS timeline meaning; no stream is \
                           built from it"
                        .into(),
                });
            }
        }

        // The declared episode total, from the per-split shard lengths — but only when *every*
        // split declares them. A total that silently omits a split would be compared against the
        // episodes of all of them, reporting a complete dataset as truncated. Which splits fell
        // short is recorded so the report can say why the count check is not running.
        let mut splits_without_lengths: Vec<String> = Vec::new();
        let declared_episodes: Option<u64> = info.splits.as_ref().and_then(|splits| {
            let mut total = 0u64;
            let mut saw_any = false;
            for (n, split) in splits.iter().enumerate() {
                match split.shard_lengths.as_ref() {
                    Some(lengths) => {
                        for length in lengths {
                            // An entry that will not parse as a number is *not* the same as a split
                            // that declares no lengths. `json_u64` returning None used to abandon
                            // the whole closure, disabling the count check and reporting "declares
                            // none" — which sends a reader looking for a missing key that is right
                            // there, holding a value nothing could read. Named as its own case.
                            let Some(parsed) = json_u64(length) else {
                                splits_without_lengths.push(format!(
                                    "{} (unreadable shard length)",
                                    split.name.clone().unwrap_or_else(|| format!("split #{n}"))
                                ));
                                break;
                            };
                            total = total.saturating_add(parsed);
                            saw_any = true;
                        }
                    }
                    None => splits_without_lengths
                        .push(split.name.clone().unwrap_or_else(|| format!("split #{n}"))),
                }
            }
            (saw_any && splits_without_lengths.is_empty()).then_some(total)
        });

        // With the manifest read and the features resolved, a metadata-only run has everything it
        // is going to get: no shard is opened past this point.
        if options.metadata_only {
            return ingest_metadata_only(
                dir,
                &info,
                file_format,
                &step_leaves,
                declared_episodes,
                &splits_without_lengths,
                unmapped_fields,
                options,
            );
        }

        // Resolve the sampling request into concrete episode ordinals before any shard is read.
        // `FirstEpisodes` needs no manifest — the first n ordinals are the first n records — while a
        // random draw has to rank the whole episode set, which only the declared shard lengths give.
        let selected: Option<BTreeSet<u64>> = if options.sample.is_partial() {
            match &options.sample {
                // Clamped to the episodes the manifest says exist. Unclamped, `--sample-episodes 10`
                // over a 3-episode dataset would record a *declared* total of 10 — a claim the
                // manifest never made — and the declared-count check would then fail a sound
                // dataset for the size of the user's own flag.
                Sample::FirstEpisodes(n) => {
                    let want = declared_episodes.map_or(*n, |total| (*n).min(total));
                    // Bounded before it is materialized: `n` is an operator's number, and
                    // `--sample-episodes 18446744073709551615` must not become an allocation.
                    if want > MAX_EPISODE_SET_FOR_SAMPLING {
                        return Err(IngestError::SamplingUnsupported {
                            format_id: FORMAT_ID,
                            reason: format!(
                                "a sample of {want} episodes is over the \
                                 {MAX_EPISODE_SET_FOR_SAMPLING} ceiling for resolving an episode \
                                 set; ask for fewer, or run the full dataset"
                            ),
                        });
                    }
                    Some((0..want).collect())
                }
                _ => {
                    let Some(total) = declared_episodes.filter(|t| *t > 0) else {
                        return Err(IngestError::SamplingUnsupported {
                            format_id: FORMAT_ID,
                            reason: if splits_without_lengths.is_empty() {
                                "dataset_info.json declares no split shard lengths, so which \
                                 episodes exist is not known without reading every shard"
                                    .into()
                            } else {
                                format!(
                                    "dataset_info.json omits shard lengths for split(s) {}, so \
                                     which episodes exist is not known without reading every shard",
                                    splits_without_lengths.join(", ")
                                )
                            },
                        });
                    };
                    // The random draw ranks the whole set, so it is the arm that must materialize
                    // it — and `total` is a number in a few-hundred-byte manifest that nothing else
                    // bounds. `"shardLengths": ["18446744073709551615"]` is a 92-byte file, and
                    // without this ceiling it is a permanent hang and a certain OOM.
                    if total > MAX_EPISODE_SET_FOR_SAMPLING {
                        return Err(IngestError::SamplingUnsupported {
                            format_id: FORMAT_ID,
                            reason: format!(
                                "dataset_info.json declares {total} episodes, over the \
                                 {MAX_EPISODE_SET_FOR_SAMPLING} ceiling for ranking an episode set \
                                 to draw from — use --sample-episodes, or run the full dataset"
                            ),
                        });
                    }
                    Some(options.sample.select(&(0..total).collect()))
                }
            }
        } else {
            None
        };
        // Past the last selected episode there is nothing left to find, so reading stops there.
        let last_selected = selected.as_ref().and_then(|s| s.iter().max().copied());

        let shards = find_shards(dir);
        if shards.is_empty() {
            return Err(parse_error(format!(
                "no `*.tfrecord-*` shards in {}",
                dir.display()
            )));
        }

        let mut budget = FrameBudget::new(options);
        let mut builds: Vec<EpisodeBuild> = Vec::new();
        // Feature keys the records carried that features.json never declared, and vice versa —
        // reconciled in both directions so neither is dropped silently.
        let mut seen_keys: BTreeSet<String> = BTreeSet::new();
        let mut absent_leaves: BTreeSet<String> = BTreeSet::new();
        // Instructions whose bytes are not decodable text, by episode. Dropping one silently would
        // leave an episode with no task and nothing saying why.
        let mut undecodable_instructions: BTreeSet<String> = BTreeSet::new();
        // Which splits the ingested episodes came from. A TFDS directory holds every split side by
        // side, and this adapter reads all of them into one episode-index space — so which split an
        // episode came from is recorded per episode rather than lost.
        let mut splits_seen: BTreeSet<String> = BTreeSet::new();
        let mut episode_ordinal: u64 = 0;

        'shards: for shard in &shards {
            let shard_uri = relative_uri(dir, shard).ok_or_else(|| {
                parse_error(format!(
                    "shard name {} is not valid UTF-8; it is bound into the content hash, so it \
                     cannot be read losslessly",
                    shard.display()
                ))
            })?;
            let split = shard
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(shard_split);
            let mut reader = ShardReader::open(shard)?;
            while let Some(header) = reader.next_header() {
                let header = header.map_err(|e| parse_error(format!("{shard_uri}: {e}")))?;
                let index = episode_ordinal;
                episode_ordinal += 1;
                if selected.as_ref().is_some_and(|sel| !sel.contains(&index)) {
                    // Not selected: stepped over without reading its payload, which is what keeps a
                    // sample cheap on a 900 MB shard.
                    reader
                        .skip_payload(&header)
                        .map_err(|e| parse_error(format!("{shard_uri}: {e}")))?;
                    if last_selected.is_some_and(|last| index > last) {
                        break 'shards;
                    }
                    continue;
                }
                let ordinal = header.ordinal;
                let payload = reader
                    .read_payload(&header)
                    .map_err(|e| parse_error(format!("{shard_uri}: {e}")))?;
                let build = build_episode(
                    index,
                    payload,
                    ordinal,
                    &shard_uri,
                    split.clone(),
                    &step_leaves,
                    &mut seen_keys,
                    &mut absent_leaves,
                    &mut undecodable_instructions,
                    &mut budget,
                )?;
                if let Some(split) = &build.split {
                    splits_seen.insert(split.clone());
                }
                builds.push(build);
                if last_selected.is_some_and(|last| index >= last) {
                    break 'shards;
                }
            }
        }

        // Episode-scoped lineage: the source file each episode was converted from.
        let mut provenance: Vec<Provenance> = Vec::new();
        let episodes: Vec<Episode> = builds
            .iter()
            .map(|b| {
                // Episode-scoped facts: the raw file it was converted from, and the split its shard
                // belongs to. The split matters because a TFDS directory holds every split side by
                // side and this adapter reads them into one episode-index space — without this,
                // "the first 2 episodes" of an OXE dataset can silently be eval data.
                let mut elements = Vec::new();
                if let Some(file) = &b.source_file {
                    elements.push(ProvenanceElement {
                        key: "upstream".into(),
                        value: Some(file.clone()),
                        class: ProvenanceClass::Known,
                    });
                }
                if let Some(split) = &b.split {
                    elements.push(ProvenanceElement {
                        key: "tfds_split".into(),
                        value: Some(split.clone()),
                        class: ProvenanceClass::Known,
                    });
                }
                if !elements.is_empty() {
                    provenance.push(Provenance {
                        scope: ProvenanceScope::Episode(b.index),
                        elements,
                    });
                }
                Episode {
                    index: b.index,
                    // The step index is the timeline: an episode spans step 0 to step n-1. An
                    // episode with no steps spans nothing, and says so.
                    start_ts: (b.steps > 0).then_some(0),
                    end_ts: (b.steps > 0).then(|| b.steps as i64 - 1),
                    streams: b.streams.clone(),
                    task: b.task.clone(),
                    labels: b.labels.clone(),
                    // RLDS is a manipulation format: no ego trajectory.
                    ego_poses: None,
                    // RLDS declares no per-episode step count — it is derived, not asserted.
                    declared_frame_count: None,
                }
            })
            .collect();

        // Dataset provenance: what the manifests actually record, never inferred.
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some(FORMAT_ID.into()),
            class: ProvenanceClass::Known,
        }];
        if let Some(license) = info
            .redistribution_info
            .as_ref()
            .and_then(|r| r.license.as_ref())
        {
            elements.push(ProvenanceElement {
                key: "license".into(),
                value: Some(license.clone()),
                class: ProvenanceClass::Known,
            });
        }
        provenance.push(Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        });

        let mut metadata: Vec<(String, String)> = vec![
            ("source_format".into(), FORMAT_ID.into()),
            ("tfds_file_format".into(), file_format.into()),
        ];
        if let Some(name) = &info.name {
            metadata.push(("tfds_name".into(), name.clone()));
        }
        if let Some(version) = &info.version {
            metadata.push(("tfds_version".into(), version.clone()));
        }
        if let Some(module) = &info.module_name {
            metadata.push(("tfds_module".into(), module.clone()));
        }
        if let Some(citation) = &info.citation {
            metadata.push(("citation".into(), citation.clone()));
        }
        if !splits_seen.is_empty() {
            metadata.push((
                "tfds_splits_ingested".into(),
                splits_seen.iter().cloned().collect::<Vec<_>>().join(","),
            ));
        }
        // The declared episode total is an assertion about the whole dataset, so it is only
        // comparable against a full ingest. Under a sample, what is comparable is the count the
        // sample itself selected.
        //
        // But only when the manifest declared a total at all. `FirstEpisodes` is clamped against
        // `declared_episodes`, and when that is `None` there is nothing to clamp against, so `want`
        // stayed at the operator's `n`: `--sample-episodes 10` over a 3-episode dataset whose
        // `dataset_info.json` omits `splits` (or has one split without `shardLengths`) recorded a
        // *declared* total of 10 and then failed the dataset with
        // `STRUCTURAL.EPISODE_COUNT_MISMATCH`, an Error, for the size of the user's own flag. The
        // manifest never said 10. With no declaration to compare against, the declared-count check
        // has nothing to do — exactly as it already has nothing to do on a full ingest of the same
        // dataset.
        match &selected {
            Some(sel) => {
                if declared_episodes.is_some() {
                    metadata.push((
                        crate::cdm::META_DECLARED_EPISODES.to_string(),
                        sel.len().to_string(),
                    ));
                }
            }
            None => {
                if let Some(total) = declared_episodes {
                    metadata.push((
                        crate::cdm::META_DECLARED_EPISODES.to_string(),
                        total.to_string(),
                    ));
                }
            }
        }

        let dataset = Dataset {
            id: info
                .name
                .clone()
                .unwrap_or_else(|| crate::adapter::dataset_id_from_path(dir, FORMAT_ID)),
            metadata,
            provenance,
            episodes,
            // RLDS records no sensor-rig calibration.
            calibration: None,
        };

        let mut mapped_fields = vec![
            "features.json steps/* -> streams".into(),
            "step index -> frame.ts (clock `rlds-step-index`)".into(),
            "tfrecord record -> episode".into(),
            "leaf dtype -> stream.dtype".into(),
            "leaf shape -> stream.shape".into(),
            "step values -> frame.value_ref.content_hash (SHA-256)".into(),
            // Under a sample, only the *length prefix* of an unselected record is checked; its
            // payload is stepped over and its payload CRC is never computed, so a corrupt payload in
            // a skipped record passes the run. Claiming "verified on every record" beside an honest
            // `Coverage::Sample` was a false attestation about the integrity of data never read.
            if selected.is_some() {
                "tfrecord masked CRC-32C -> verified on every record read (a sample checks the \
                 length prefix of the records it skips, not their payload)"
                    .into()
            } else {
                "tfrecord masked CRC-32C -> verified on every record".into()
            },
        ];
        let mut omitted_fields = vec![
            "per-step timestamps (RLDS records no wall clock; the timeline is the step index, so \
             the rate, gap, jitter and skew checks have no measured time to grade)"
                .into(),
            "image/point payload decoding (frames are fingerprints, not pixels)".into(),
        ];
        if builds.iter().any(|b| b.task.is_some()) {
            mapped_fields
                .push("steps/*language_instruction -> episode.task + language labels".into());
        }
        if builds.iter().any(|b| b.source_file.is_some()) {
            mapped_fields.push("episode_metadata/file_path -> provenance.upstream".into());
        }
        if info
            .redistribution_info
            .as_ref()
            .and_then(|r| r.license.as_ref())
            .is_some()
        {
            mapped_fields.push("redistributionInfo.license -> provenance.license".into());
        } else {
            omitted_fields.push("license (dataset_info.json records no redistributionInfo)".into());
        }
        match (&selected, declared_episodes) {
            (Some(_), _) => omitted_fields.push(
                "split shard lengths (a dataset-level total is not comparable against a sampled \
                 ingest)"
                    .into(),
            ),
            (None, Some(_)) => {
                mapped_fields.push("split shardLengths -> declared episode-count check".into())
            }
            // Say which of the two it is. Reporting "declares none" when it declares some for one
            // split and not another sends a reader looking for the wrong thing.
            (None, None) if !splits_without_lengths.is_empty() => omitted_fields.push(format!(
                "shard lengths for split(s) {} (so the declared episode-count check cannot run: a \
                 partial total would report a complete dataset as truncated)",
                splits_without_lengths.join(", ")
            )),
            (None, None) => {
                omitted_fields.push("split shard lengths (dataset_info.json declares none)".into())
            }
        }
        if !splits_seen.is_empty() {
            mapped_fields.push(
                "shard filename split -> provenance.tfds_split (per episode; every split in the \
                 directory is read into one episode-index space)"
                    .into(),
            );
        }
        unmapped_fields.push(UnmappedField {
            source_path: "tf.train.Example feature values".into(),
            note: "step values are fingerprinted (hashed) into content_hash, never decoded or \
                   interpreted"
                .into(),
        });
        // Reconcile declared features against the keys the records actually carried.
        let declared: BTreeSet<&str> = step_leaves.iter().map(|l| l.path.as_str()).collect();
        let mut unread_sources: Vec<UnmappedField> = Vec::new();
        // Every key the records carried, not just the `steps/` ones — but the two mean different
        // things. A `steps/*` key is a *per-step value*: it is in the records, no stream represents
        // it, and its values went unread, which is a hole in the run's coverage and has to reach the
        // verdict. An `episode_metadata/*` key is a fact about the episode that the CDM has no shape
        // for, which costs the reader no coverage — and undeclared episode metadata is ordinary in
        // the Open X-Embodiment corpus, so calling it unread would fire on sound datasets.
        for key in &seen_keys {
            if declared.contains(key.as_str()) || key.as_str() == EPISODE_SOURCE_FILE_KEY {
                continue;
            }
            if key.starts_with(STEPS_PREFIX) {
                unread_sources.push(UnmappedField {
                    source_path: format!("tf.train.Example key `{key}`"),
                    note: "a per-step value present in the records but not declared in \
                           features.json, so no stream is built from it and its values went unread"
                        .into(),
                });
            } else {
                unmapped_fields.push(UnmappedField {
                    source_path: format!("tf.train.Example key `{key}`"),
                    note: "episode-level information present in the records but not declared in \
                           features.json; the CDM has no shape for it"
                        .into(),
                });
            }
        }
        for path in &absent_leaves {
            // "At least one" rather than "the records": this is a per-episode observation, and a
            // feature missing from one episode of a thousand must not read as missing everywhere.
            // Each such episode carries the feature as an empty stream, which is what the verdict
            // grades.
            omitted_fields.push(format!(
                "feature `{path}` is declared in features.json but absent from at least one record; \
                 those episodes carry it as an empty stream"
            ));
        }
        for path in &undecodable_instructions {
            omitted_fields.push(format!(
                "instruction text in `{path}` for at least one step (the bytes are not decodable \
                 UTF-8, so no task or language label was taken from them)"
            ));
        }

        let report = IngestReport {
            unread_sources,
            format_id: FORMAT_ID,
            source_version: Some(file_format.to_string()),
            coverage: match &selected {
                None => Coverage::Full,
                Some(_) => Coverage::Sample {
                    sample: options.sample.clone(),
                    episodes_ingested: dataset.episodes.len() as u64,
                },
            },
            mapped_fields,
            unmapped_fields,
            omitted_fields,
        };

        Ok(Ingested { dataset, report })
    }
}

/// Ingest a TFDS/RLDS dataset from `dataset_info.json` and `features.json` alone, opening no shard.
///
/// The two manifest files are a few kilobytes beside a directory that, for an Open X-Embodiment
/// dataset, is routinely hundreds of gigabytes — and between them they answer the questions worth
/// asking before a download: how many episodes the dataset declares, what per-step features it
/// declares and with what dtypes and shapes, what file format the shards are in, and what licence
/// and citation it ships under.
///
/// What it does **not** cover is everything a record would answer: no steps, no values, no content
/// hashes, no CRC verification, no language instructions, and no per-episode length — RLDS records
/// the episode *count* per shard, never the step count of an episode. Every episode therefore
/// carries zero frames and no declared frame count *by request*, which is what
/// [`Coverage::MetadataOnly`] tells the checks: abstain rather than read that absence as a defect.
#[allow(clippy::too_many_arguments)]
fn ingest_metadata_only(
    dir: &Path,
    info: &DatasetInfoJson,
    file_format: &str,
    step_leaves: &[&Leaf],
    declared_episodes: Option<u64>,
    splits_without_lengths: &[String],
    mut unmapped_fields: Vec<UnmappedField>,
    options: &IngestOptions,
) -> Result<Ingested, IngestError> {
    // The episode set can only come from the declared shard lengths — reading a shard is exactly
    // what this mode does not do. Without them there is no episode set at all, and returning an
    // empty dataset (which reads as a clean pass) is refused instead.
    let Some(total) = declared_episodes else {
        return Err(parse_error(if splits_without_lengths.is_empty() {
            "dataset_info.json declares no split shard lengths, so a metadata-only check has no \
             episode set to check — run a full check, which reads the episodes from the shards"
                .into()
        } else {
            format!(
                "dataset_info.json omits shard lengths for split(s) {}, so a metadata-only check \
                 cannot know the episode set: a partial total would report a complete dataset as \
                 truncated. Run a full check instead",
                splits_without_lengths.join(", ")
            )
        }));
    };

    // No frame is read, so the frame budget — charged per frame — never fires, and nothing else
    // bounds what this manifest can make Veridex allocate. It builds one `Stream` per
    // (episode x feature), and both factors come from a few hundred bytes of JSON: a
    // `"shardLengths": ["100000000"]` against 60 declared features is a 100-byte file asking for
    // six billion streams. Charge the product before a single one is built, against the same
    // ceiling the frame count uses, so `--max-frames` raises both together.
    let declared_streams = total.saturating_mul(step_leaves.len() as u64);
    let mut budget = FrameBudget::new(options);
    budget.take(FORMAT_ID, declared_streams)?;

    let episodes: Vec<Episode> = (0..total)
        .map(|index| Episode {
            index,
            // No record was read, so there is no measured window, and RLDS has no wall clock to
            // derive one from even if there were.
            start_ts: None,
            end_ts: None,
            streams: step_leaves.iter().map(|l| empty_stream(l)).collect(),
            // The language instruction is a per-step value inside a record. Left absent rather than
            // guessed; the task checks abstain on `None`.
            task: None,
            labels: Vec::new(),
            ego_poses: None,
            // RLDS declares how many episodes each shard holds, never how many steps an episode
            // holds. Left `None` rather than derived, so `STRUCTURAL.EPISODE_LENGTH_MISMATCH` has
            // nothing to compare rather than a number Veridex made up.
            declared_frame_count: None,
        })
        .collect();

    let mut elements = vec![ProvenanceElement {
        key: "source_format".into(),
        value: Some(FORMAT_ID.into()),
        class: ProvenanceClass::Known,
    }];
    if let Some(license) = info
        .redistribution_info
        .as_ref()
        .and_then(|r| r.license.as_ref())
    {
        elements.push(ProvenanceElement {
            key: "license".into(),
            value: Some(license.clone()),
            class: ProvenanceClass::Known,
        });
    }

    let mut metadata: Vec<(String, String)> = vec![
        ("source_format".into(), FORMAT_ID.into()),
        ("tfds_file_format".into(), file_format.into()),
    ];
    if let Some(name) = &info.name {
        metadata.push(("tfds_name".into(), name.clone()));
    }
    if let Some(version) = &info.version {
        metadata.push(("tfds_version".into(), version.clone()));
    }
    if let Some(module) = &info.module_name {
        metadata.push(("tfds_module".into(), module.clone()));
    }
    if let Some(citation) = &info.citation {
        metadata.push(("citation".into(), citation.clone()));
    }
    // Deliberately no `META_DECLARED_EPISODES`: the episode set was derived from that very number,
    // so the declared-count check would be comparing `n` against `n` — a check that cannot fail,
    // whose pass would then be reported as though something had been verified.

    let dataset = Dataset {
        id: info
            .name
            .clone()
            .unwrap_or_else(|| crate::adapter::dataset_id_from_path(dir, FORMAT_ID)),
        metadata,
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        }],
        episodes,
        calibration: None,
    };

    let mut mapped_fields = vec![
        "features.json steps/* -> streams".into(),
        "leaf dtype -> stream.dtype".into(),
        "leaf shape -> stream.shape".into(),
        "split shardLengths -> episode set (0..total)".into(),
    ];
    if info
        .redistribution_info
        .as_ref()
        .and_then(|r| r.license.as_ref())
        .is_some()
    {
        mapped_fields.push("redistributionInfo.license -> provenance.license".into());
    }

    let mut omitted_fields = vec![
        "step values, step counts, and content hashes (no shard was opened)".into(),
        "tfrecord masked CRC-32C verification (it is computed over record bytes, and no record was \
         read)"
            .into(),
        "language instructions and episode tasks (they are per-step values inside a record)".into(),
        "episode_metadata/file_path -> provenance.upstream (it is stored inside each record)".into(),
        "per-episode step counts (RLDS declares episodes per shard, never steps per episode, so \
         this is not a file that went unread — the manifest does not carry it)"
            .into(),
        "split shard lengths -> declared episode-count check (the episode set was derived from \
         that same total, so the comparison could not fail)"
            .into(),
    ];
    if info
        .redistribution_info
        .as_ref()
        .and_then(|r| r.license.as_ref())
        .is_none()
    {
        omitted_fields.push("license (dataset_info.json records no redistributionInfo)".into());
    }
    unmapped_fields.push(UnmappedField {
        source_path: "tf.train.Example feature values".into(),
        note: "no record was opened, so no key the records carry could be reconciled against \
               features.json in either direction"
            .into(),
    });

    Ok(Ingested {
        report: IngestReport {
            unread_sources: Vec::new(),
            format_id: FORMAT_ID,
            source_version: info.version.clone(),
            coverage: Coverage::MetadataOnly {
                episodes_declared: dataset.episodes.len() as u64,
            },
            mapped_fields,
            unmapped_fields,
            omitted_fields,
        },
        dataset,
    })
}

/// The stream a declared `steps/*` leaf implies, with no frames in it.
///
/// Two callers, for two different reasons that must not be conflated in the report: a feature the
/// records did not carry (an `STRUCTURAL.EMPTY_STREAM` defect), and a metadata-only run that opened
/// no record at all (an abstention the coverage note states). The shape of the stream is the same;
/// what differs is what the run says about why it is empty.
fn empty_stream(leaf: &Leaf) -> Stream {
    Stream {
        name: leaf
            .path
            .strip_prefix(STEPS_PREFIX)
            .unwrap_or(&leaf.path)
            .to_string(),
        modality: leaf.modality,
        // RLDS declares no sampling rate, and none is invented from the step count.
        declared_rate_hz: None,
        clock_id: CLOCK_ID.into(),
        // RLDS has no per-step timestamp: the timeline is the step index, and the CDM says so, so
        // the temporal checks abstain instead of passing on a timeline nobody measured.
        clock_kind: ClockKind::StepIndex,
        dtype: leaf.dtype.clone(),
        shape: leaf.shape.clone(),
        frames: Vec::new(),
        // TFDS stores no summary statistics, and this adapter does not decode values, so there is
        // nothing to recompute from.
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        media: None,
        frame_id: None,
    }
}

/// Turn one record into an episode: derive the step count from every step feature, require the
/// answers to agree, and build one stream per feature.
#[allow(clippy::too_many_arguments)]
fn build_episode(
    index: u64,
    payload: &[u8],
    ordinal: u64,
    shard_uri: &str,
    split: Option<String>,
    step_leaves: &[&Leaf],
    seen_keys: &mut BTreeSet<String>,
    absent_leaves: &mut BTreeSet<String>,
    undecodable_instructions: &mut BTreeSet<String>,
    budget: &mut FrameBudget,
) -> Result<EpisodeBuild, IngestError> {
    let values = parse_example(payload).ok_or_else(|| {
        parse_error(format!(
            "{shard_uri} record {ordinal}: not a decodable tf.train.Example"
        ))
    })?;
    for key in values.keys() {
        seen_keys.insert(key.clone());
    }

    // Derive each step feature's step count. RLDS serializes every step of a feature into one list,
    // so the count is the list length divided by the values one step holds.
    let mut counts: Vec<(&Leaf, u64)> = Vec::new();
    // Features the manifest declares but this record does not carry. They still become streams —
    // empty ones — because a feature that vanished from the data is a defect, and a stream that is
    // simply absent from the CDM is invisible to both the content hash and every check: a dataset
    // that lost a whole camera during export would otherwise hash and certify exactly like one that
    // never had it. As an empty stream it is bound into the hash and reported as
    // `STRUCTURAL.EMPTY_STREAM`.
    let mut absent: Vec<&Leaf> = Vec::new();
    for leaf in step_leaves {
        let Some(feature) = values.get(&leaf.path) else {
            absent_leaves.insert(leaf.path.clone());
            absent.push(leaf);
            continue;
        };
        let elem = leaf
            .elem_len
            .expect("leaves without an element size are unmapped");
        let len = feature.len();
        if len % elem != 0 {
            return Err(parse_error(format!(
                "{shard_uri} record {ordinal}: feature `{}` holds {len} {} values, not a whole \
                 multiple of the {elem} values per step that features.json declares — the record \
                 contradicts its own schema",
                leaf.path,
                feature.kind(),
            )));
        }
        counts.push((leaf, len / elem));
    }

    // Every step feature comes from the same `steps` sequence, so they must agree on its length.
    // A disagreement means the record cannot be mapped faithfully, and under-reporting an episode's
    // length silently is exactly what ingestion must not do.
    let steps = counts.first().map(|(_, n)| *n).unwrap_or(0);
    if let Some((leaf, n)) = counts.iter().find(|(_, n)| *n != steps) {
        let (first_leaf, _) = counts[0];
        return Err(parse_error(format!(
            "{shard_uri} record {ordinal}: feature `{}` holds {n} steps but `{}` holds {steps}; \
             the record's step features disagree on the length of the episode",
            leaf.path, first_leaf.path,
        )));
    }

    // Charge the budget on the frames this record is about to materialize, before allocating them.
    budget.take(FORMAT_ID, (counts.len() as u64).saturating_mul(steps))?;

    let mut streams = Vec::with_capacity(counts.len() + absent.len());
    let mut task = None;
    let mut labels = Vec::new();
    for (leaf, _) in &counts {
        let feature = &values[&leaf.path];
        let elem = leaf.elem_len.expect("checked above") as usize;
        // One `uri` per stream, shared by its frames: it is identical for every step, and a separate
        // heap string per frame turned a 2 MB shard into hundreds of megabytes.
        let uri = format!("{shard_uri}#{ordinal}/{}", leaf.path);
        let frames = (0..steps as usize)
            .map(|step| Frame {
                ts: step as i64,
                value_ref: ValueRef {
                    uri: uri.clone(),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: Some(feature.hash_range(step * elem, (step + 1) * elem)),
                },
            })
            .collect();
        // The natural-language instruction is both a stream (it has a per-step value) and the
        // episode's task. Only text that actually decodes becomes a task — Veridex never invents an
        // instruction out of bytes it cannot read, and records when it could not read them.
        if leaf.is_instruction() {
            let mut previous: Option<String> = None;
            for step in 0..steps as usize {
                let Some(text) = feature
                    .bytes_at(step)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                else {
                    undecodable_instructions.insert(leaf.path.clone());
                    continue;
                };
                if task.is_none() {
                    task = Some(text.clone());
                }
                // A mid-episode instruction change becomes a timestamped `language` annotation, the
                // same way the LeRobot adapter surfaces a task transition.
                if previous.as_ref().is_some_and(|p| p != &text) {
                    labels.push(Label {
                        key: "language".into(),
                        value: text.clone(),
                        ts: Some(step as i64),
                    });
                }
                previous = Some(text);
            }
        }
        streams.push(Stream {
            name: leaf
                .path
                .strip_prefix(STEPS_PREFIX)
                .unwrap_or(&leaf.path)
                .to_string(),
            modality: leaf.modality,
            // RLDS declares no sampling rate, and none is invented from the step count.
            declared_rate_hz: None,
            clock_id: CLOCK_ID.into(),
            // RLDS has no per-step timestamp: these are step indices, and the CDM says so, so the
            // temporal checks abstain instead of passing on a timeline nobody measured.
            clock_kind: ClockKind::StepIndex,
            dtype: leaf.dtype.clone(),
            shape: leaf.shape.clone(),
            frames,
            // TFDS stores no summary statistics, and this adapter does not decode values, so there
            // is nothing to recompute from.
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            latched: None,
            declared_range: None,
            point_fields: None,
            media: None,
            frame_id: None,
        });
    }
    // A declared feature this record does not carry, as an empty stream — present in the CDM, bound
    // into the content hash, and reported as `STRUCTURAL.EMPTY_STREAM` rather than silently absent.
    for leaf in &absent {
        streams.push(empty_stream(leaf));
    }

    // The one lineage fact RLDS carries: the raw file each episode was converted from.
    let source_file = values
        .get(EPISODE_SOURCE_FILE_KEY)
        .and_then(|v| v.bytes_at(0))
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    Ok(EpisodeBuild {
        index,
        streams,
        task,
        labels,
        steps,
        source_file,
        split,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_matches_the_reference_vectors() {
        // The standard CRC-32C check values.
        assert_eq!(crc32c(b""), 0x0000_0000);
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b"a"), 0xC1D0_4330);
    }

    #[test]
    fn a_masked_crc_is_recoverable_by_remasking() {
        // TFRecord stores mask(crc); verification remasks rather than unmasking, so this is the
        // property that matters: the same bytes always mask to the same stored word.
        let crc = crc32c(b"veridex");
        assert_eq!(mask_crc(crc), mask_crc(crc32c(b"veridex")));
        assert_ne!(mask_crc(crc), crc, "the mask must actually move the value");
    }

    #[test]
    fn a_varint_longer_than_a_u64_is_refused() {
        // Eleven continuation bytes cannot describe a u64; a parser that shifted past 64 would
        // silently wrap and accept a forged length.
        let bytes = [0xFFu8; 12];
        assert_eq!(Buf::new(&bytes).varint(), None);
    }

    #[test]
    fn an_unknown_dimension_yields_no_element_size() {
        let shape: serde_json::Value = serde_json::json!({"dimensions": ["7", "-1"]});
        assert_eq!(shape_dims(Some(&shape)), None);
    }

    #[test]
    fn a_tf_enum_dtype_and_a_numpy_dtype_normalize_alike() {
        let tf = serde_json::json!("DT_UINT8");
        let numpy = serde_json::json!("uint8");
        assert_eq!(normalize_dtype(Some(&tf)), normalize_dtype(Some(&numpy)));
    }
}
