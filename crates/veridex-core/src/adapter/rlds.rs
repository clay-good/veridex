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
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cdm::{
    Dataset, Episode, Frame, Label, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
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

/// One record's payload plus where it began, so a value reference can name it.
struct Record<'a> {
    payload: &'a [u8],
    ordinal: u64,
}

/// Walks the records of one shard, verifying both checksums.
struct RecordReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    ordinal: u64,
}

impl<'a> RecordReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        RecordReader {
            bytes,
            pos: 0,
            ordinal: 0,
        }
    }

    /// The next record, `None` at a clean end of file, or `Err` with what is wrong.
    fn next_record(&mut self) -> Option<Result<Record<'a>, String>> {
        if self.pos >= self.bytes.len() {
            return None;
        }
        // A record is: u64 length (LE), masked CRC-32C of those 8 bytes, payload, masked CRC-32C of
        // the payload. A trailing fragment shorter than a header is truncation, not a clean end.
        let header = match self.bytes.get(self.pos..self.pos + 12) {
            Some(h) => h,
            None => {
                return Some(Err(format!(
                    "truncated record header at byte {} ({} bytes remain, 12 needed)",
                    self.pos,
                    self.bytes.len() - self.pos
                )))
            }
        };
        let length = u64::from_le_bytes(header[0..8].try_into().expect("8 bytes"));
        let length_crc = u32::from_le_bytes(header[8..12].try_into().expect("4 bytes"));
        if mask_crc(crc32c(&header[0..8])) != length_crc {
            return Some(Err(format!(
                "record {} at byte {}: length-prefix checksum mismatch (the shard is corrupt)",
                self.ordinal, self.pos
            )));
        }
        // Checked against the file's own size before any slice is taken: `length` comes straight
        // from the file, so an absurd value must fail cleanly rather than wrap or panic.
        let start = self.pos + 12;
        let end = match usize::try_from(length)
            .ok()
            .and_then(|len| start.checked_add(len))
            .and_then(|end| end.checked_add(4))
        {
            Some(end) if end <= self.bytes.len() => end,
            _ => {
                return Some(Err(format!(
                    "record {} at byte {}: declares {length} payload bytes, past the end of a \
                     {}-byte shard",
                    self.ordinal,
                    self.pos,
                    self.bytes.len()
                )))
            }
        };
        let payload = &self.bytes[start..end - 4];
        let payload_crc = u32::from_le_bytes(self.bytes[end - 4..end].try_into().expect("4 bytes"));
        if mask_crc(crc32c(payload)) != payload_crc {
            return Some(Err(format!(
                "record {} at byte {}: payload checksum mismatch (the shard is corrupt)",
                self.ordinal, self.pos
            )));
        }
        let ordinal = self.ordinal;
        self.pos = end;
        self.ordinal += 1;
        Some(Ok(Record { payload, ordinal }))
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
    Floats(Vec<f32>),
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

fn parse_map_entry(entry: &[u8]) -> Option<(String, FeatureValues<'_>)> {
    let mut buf = Buf::new(entry);
    let mut key: Option<String> = None;
    let mut values: Option<FeatureValues<'_>> = None;
    while !buf.done() {
        let (field, wire) = buf.tag()?;
        match (field, wire) {
            (1, 2) => key = Some(String::from_utf8(buf.length_delimited()?.to_vec()).ok()?),
            (2, 2) => values = Some(parse_feature(buf.length_delimited()?)?),
            _ => buf.skip(wire)?,
        }
    }
    Some((key?, values?))
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
                out = Some(FeatureValues::Bytes(entries));
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
                                    .push(f32::from_le_bytes(chunk.try_into().expect("4 bytes")));
                            }
                        }
                        // … and unpacked, which the wire format still permits.
                        (1, 5) => {
                            let raw = list.take(4)?;
                            entries.push(f32::from_le_bytes(raw.try_into().expect("4 bytes")));
                        }
                        _ => list.skip(wire)?,
                    }
                }
                out = Some(FeatureValues::Floats(entries));
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
                out = Some(FeatureValues::Ints(entries));
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
    if let Some(dict) = node.get("featuresDict").and_then(|d| d.get("features")) {
        if let Some(map) = dict.as_object() {
            // `serde_json`'s default map is ordered, so the walk is deterministic.
            for (name, child) in map {
                walk_features(child, &join_path(path, name), in_sequence, leaves, unmapped);
            }
        }
        return;
    }
    if let Some(sequence) = node.get("sequence") {
        if in_sequence {
            // A sequence inside a sequence is ragged: TFDS serializes it with side tables this
            // parser does not read, so nothing under it is claimed.
            unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note: "a nested (ragged) sequence is serialized with row-length side tables that \
                       this adapter does not read; no stream is built from it"
                    .into(),
            });
            return;
        }
        if let Some(inner) = sequence.get("feature") {
            walk_features(inner, path, true, leaves, unmapped);
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
        let elem_len = shape
            .as_ref()
            .map(|dims| dims.iter().product::<u64>())
            .filter(|n| *n > 0);
        if elem_len.is_none() {
            unmapped.push(UnmappedField {
                source_path: path.to_string(),
                note:
                    "the declared shape has an unknown or zero dimension, so the values per step \
                       cannot be derived; no stream is built from it"
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
    if let Some(class_label) = node.get("classLabel") {
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            dtype: normalize_dtype(class_label.get("dtype")).or(Some("int64".into())),
            shape: None,
            elem_len: Some(1),
            modality: Modality::ScalarState,
        });
        return;
    }
    if let Some(scalar) = node.get("scalar") {
        leaves.push(Leaf {
            path: path.to_string(),
            per_step: in_sequence,
            dtype: normalize_dtype(scalar.get("dtype")),
            shape: None,
            elem_len: Some(1),
            modality: modality_for(path, Modality::ScalarState),
        });
        return;
    }
    unmapped.push(UnmappedField {
        source_path: path.to_string(),
        note: "feature class is not one the CDM represents (not a tensor, image, text, scalar, or \
               class label); no stream is built from it"
            .into(),
    });
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
                    .is_some_and(|n| n.contains(".tfrecord"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    shards.sort();
    shards
}

/// A path relative to the dataset root, with forward slashes on every platform — it is bound into
/// the content hash, so it must not vary with the host.
fn relative_uri(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
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

        // The declared episode total, from the per-split shard lengths. Present in every TFDS
        // manifest this adapter has a reason to trust; absent only in a hand-built directory.
        let declared_episodes: Option<u64> = info.splits.as_ref().and_then(|splits| {
            let mut total = 0u64;
            let mut saw_any = false;
            for split in splits {
                let lengths = split.shard_lengths.as_ref()?;
                for length in lengths {
                    total = total.saturating_add(json_u64(length)?);
                    saw_any = true;
                }
            }
            saw_any.then_some(total)
        });

        // Resolve the sampling request into concrete episode ordinals before any shard is read.
        // `FirstEpisodes` needs no manifest — the first n ordinals are the first n records — while a
        // random draw has to rank the whole episode set, which only the declared shard lengths give.
        let selected: Option<BTreeSet<u64>> = if options.sample.is_partial() {
            match &options.sample {
                Sample::FirstEpisodes(n) => Some((0..*n).collect()),
                _ => {
                    let Some(total) = declared_episodes.filter(|t| *t > 0) else {
                        return Err(IngestError::SamplingUnsupported {
                            format_id: FORMAT_ID,
                            reason: "dataset_info.json declares no split shard lengths, so which \
                                     episodes exist is not known without reading every shard"
                                .into(),
                        });
                    };
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
        let mut episode_ordinal: u64 = 0;

        'shards: for shard in &shards {
            let bytes = std::fs::read(shard).map_err(|e| IngestError::Io(e.to_string()))?;
            let shard_uri = relative_uri(dir, shard);
            let mut reader = RecordReader::new(&bytes);
            while let Some(record) = reader.next_record() {
                let record = record.map_err(|e| parse_error(format!("{shard_uri}: {e}")))?;
                let index = episode_ordinal;
                episode_ordinal += 1;
                if let Some(sel) = &selected {
                    if !sel.contains(&index) {
                        // Not selected: the record was framed and checksummed, but nothing in it is
                        // parsed or allocated.
                        if last_selected.is_some_and(|last| index > last) {
                            break 'shards;
                        }
                        continue;
                    }
                }
                let build = build_episode(
                    index,
                    &record,
                    &shard_uri,
                    &step_leaves,
                    &mut seen_keys,
                    &mut absent_leaves,
                    &mut budget,
                )?;
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
                if let Some(file) = &b.source_file {
                    provenance.push(Provenance {
                        scope: ProvenanceScope::Episode(b.index),
                        elements: vec![ProvenanceElement {
                            key: "upstream".into(),
                            value: Some(file.clone()),
                            class: ProvenanceClass::Known,
                        }],
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
        // The declared episode total is an assertion about the whole dataset, so it is only
        // comparable against a full ingest. Under a sample, what is comparable is the count the
        // sample itself selected.
        match &selected {
            Some(sel) => metadata.push((
                crate::cdm::META_DECLARED_EPISODES.to_string(),
                sel.len().to_string(),
            )),
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
            id: info.name.clone().unwrap_or_else(|| {
                dir.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(FORMAT_ID)
                    .to_string()
            }),
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
            "tfrecord masked CRC-32C -> verified on every record".into(),
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
            (None, None) => {
                omitted_fields.push("split shard lengths (dataset_info.json declares none)".into())
            }
        }
        unmapped_fields.push(UnmappedField {
            source_path: "tf.train.Example feature values".into(),
            note: "step values are fingerprinted (hashed) into content_hash, never decoded or \
                   interpreted"
                .into(),
        });
        // Reconcile declared features against the keys the records actually carried.
        let declared: BTreeSet<&str> = step_leaves.iter().map(|l| l.path.as_str()).collect();
        for key in &seen_keys {
            if !declared.contains(key.as_str()) && key.starts_with(STEPS_PREFIX) {
                unmapped_fields.push(UnmappedField {
                    source_path: format!("tf.train.Example key `{key}`"),
                    note: "present in the records but not declared in features.json; not \
                           represented as a stream"
                        .into(),
                });
            }
        }
        for path in &absent_leaves {
            omitted_fields.push(format!(
                "feature `{path}` declared in features.json but absent from the records (no frames \
                 ingested)"
            ));
        }

        let report = IngestReport {
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

/// Turn one record into an episode: derive the step count from every step feature, require the
/// answers to agree, and build one stream per feature.
#[allow(clippy::too_many_arguments)]
fn build_episode(
    index: u64,
    record: &Record<'_>,
    shard_uri: &str,
    step_leaves: &[&Leaf],
    seen_keys: &mut BTreeSet<String>,
    absent_leaves: &mut BTreeSet<String>,
    budget: &mut FrameBudget,
) -> Result<EpisodeBuild, IngestError> {
    let values = parse_example(record.payload).ok_or_else(|| {
        parse_error(format!(
            "{shard_uri} record {}: not a decodable tf.train.Example",
            record.ordinal
        ))
    })?;
    for key in values.keys() {
        seen_keys.insert(key.clone());
    }

    // Derive each step feature's step count. RLDS serializes every step of a feature into one list,
    // so the count is the list length divided by the values one step holds.
    let mut counts: Vec<(&Leaf, u64)> = Vec::new();
    for leaf in step_leaves {
        let Some(feature) = values.get(&leaf.path) else {
            absent_leaves.insert(leaf.path.clone());
            continue;
        };
        let elem = leaf
            .elem_len
            .expect("leaves without an element size are unmapped");
        let len = feature.len();
        if len % elem != 0 {
            return Err(parse_error(format!(
                "{shard_uri} record {}: feature `{}` holds {len} {} values, not a whole multiple of \
                 the {elem} values per step that features.json declares — the record contradicts \
                 its own schema",
                record.ordinal,
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
            "{shard_uri} record {}: feature `{}` holds {n} steps but `{}` holds {steps}; the \
             record's step features disagree on the length of the episode",
            record.ordinal, leaf.path, first_leaf.path,
        )));
    }

    // Charge the budget on the frames this record is about to materialize, before allocating them.
    budget.take(FORMAT_ID, (counts.len() as u64).saturating_mul(steps))?;

    let mut streams = Vec::with_capacity(counts.len());
    let mut task = None;
    let mut labels = Vec::new();
    for (leaf, _) in &counts {
        let feature = &values[&leaf.path];
        let elem = leaf.elem_len.expect("checked above") as usize;
        let frames = (0..steps as usize)
            .map(|step| Frame {
                ts: step as i64,
                value_ref: ValueRef {
                    uri: format!("{shard_uri}#{}/{}", record.ordinal, leaf.path),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: Some(feature.hash_range(step * elem, (step + 1) * elem)),
                },
            })
            .collect();
        // The natural-language instruction is both a stream (it has a per-step value) and the
        // episode's task. Only text that actually decodes becomes a task — Veridex never invents an
        // instruction out of bytes it cannot read.
        if leaf.is_instruction() {
            let mut previous: Option<String> = None;
            for step in 0..steps as usize {
                let Some(text) = feature
                    .bytes_at(step)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                else {
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
            point_fields: None,
            media: None,
            frame_id: None,
        });
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
