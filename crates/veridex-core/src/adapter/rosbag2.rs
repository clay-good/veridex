//! ROS 2 **rosbag2** adapter: maps a `.db3` recording (the `sqlite3` storage plugin) into the CDM.
//!
//! rosbag2 is what a ROS 2 robot records by default, and the format most existing robot logs are
//! sitting in. Veridex already read MCAP — rosbag2's *other* storage plugin — so the message side of
//! this adapter is the same work: a topic is a [`Stream`], a message is a [`Frame`] stamped with the
//! bag's single log clock, and the few AV message *headers* are CDR-decoded into the rig CDM
//! (`PointCloud2` → point fields, `CameraInfo` + `TFMessage` → calibration, `Odometry` → ego poses).
//! The bulk payload is never decoded, only fingerprinted.
//!
//! What is new is the container. A bag is either a directory — a `metadata.yaml` beside one or more
//! `.db3` files, which is what `ros2 bag record` writes — or a single `.db3` handed over on its own.
//! Both are accepted, and the difference is disclosed rather than smoothed over: a bare `.db3` has no
//! manifest, so there is no declared message count to check the recording against and no recorder
//! identity to record, and the ingest report says which fields were therefore *omitted* instead of
//! leaving the reader to assume they were checked.
//!
//! Three things this adapter refuses to guess:
//!
//! - **Columns are bound by name, not position.** rosbag2 has added columns to `topics` across bag
//!   versions (`offered_qos_profiles`, `type_description_hash`). Reading column 3 because that is
//!   where `serialization_format` sat in version 4 would silently read a hash as a format in a
//!   version 9 bag, so the `CREATE TABLE` statement is parsed and columns are looked up by name.
//! - **A message on an undeclared topic is unread data, not a dropped row.** If `messages` references
//!   a `topic_id` that `topics` never declares, there is no honest stream to file it under —
//!   inventing one would name a topic the bag does not, and skipping it quietly would let a bag with
//!   half its rows unattributed produce the same verdict as an intact one. It is recorded in
//!   [`IngestReport::unread_sources`], which surfaces as a `COVERAGE.SOURCE_UNREAD` finding.
//! - **`relative_file_paths` is content, so it never leaves the bag.** The `.db3` files read are the
//!   ones found *in* the bag directory. A manifest entry naming a path with a directory component is
//!   not followed, and one naming a file that is not there is recorded as unread — a split recording
//!   missing its third shard is a coverage hole, and coverage is the one thing a verdict must not
//!   overstate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cdm::{
    Calibration, CameraIntrinsics, ClockKind, Dataset, EgoPose, Episode, Frame, Modality,
    PointField, Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope, Stream, Transform,
    ValueRef,
};

use super::sqlite::{SqliteDb, Value};
use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};

const FORMAT: &str = "rosbag2";

/// Every message in a bag is stamped by the recorder against one clock, so all streams share it.
/// Cross-stream skew is therefore inferred from duration drift, exactly as for MCAP.
const CLOCK_ID: &str = "rosbag2-log";

/// The bag versions this adapter reads. rosbag2's `metadata.yaml` stamps a `version`; every version
/// from 4 on keeps its recording in the `topics`/`messages` tables read here. A bag stamped with a
/// version outside this list is refused by name rather than read on the assumption that its schema
/// did not change.
const SUPPORTED_VERSIONS: &[&str] = &["4", "5", "6", "7", "8", "9"];

/// Adapter for ROS 2 rosbag2 recordings using the `sqlite3` storage plugin.
pub struct Rosbag2Adapter;

/// What a bag directory's `metadata.yaml` told us, as far as it could be read.
///
/// Every field is optional because every field is allowed to be absent, and an absent field is
/// reported as omitted rather than defaulted. Nothing here is inferred from the `.db3`.
#[derive(Default)]
struct BagManifest {
    version: Option<String>,
    storage_identifier: Option<String>,
    ros_distro: Option<String>,
    message_count: Option<u64>,
    relative_file_paths: Vec<String>,
    /// `zstd`, or absent for an uncompressed bag. rosbag2 writes the key with an empty value when
    /// nothing was compressed, which is read as absent.
    compression_format: Option<String>,
    /// `FILE` (the whole `.db3` compressed after writing) or `MESSAGE` (each message body compressed
    /// individually). Only the first is read; the second is refused by name.
    compression_mode: Option<String>,
}

/// Read the handful of scalar keys Veridex uses out of a rosbag2 `metadata.yaml`.
///
/// Deliberately not a YAML parser. rosbag2 writes this file from a fixed emitter, and the keys taken
/// here are top-level scalars under `rosbag2_bagfile_information` plus one list of strings. Anything
/// with a shape this reader is not certain of is left absent — a manifest value Veridex is unsure it
/// read correctly is worse than one it admits it does not have, because the declared message count
/// is compared against the recording and a misread would report a fault in a sound bag.
fn parse_manifest(text: &str) -> BagManifest {
    let mut m = BagManifest::default();
    let mut in_paths = false;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // The `relative_file_paths:` list runs until the next key at its own indentation.
        if in_paths {
            if let Some(item) = trimmed.strip_prefix("- ") {
                m.relative_file_paths.push(unquote(item).to_string());
                continue;
            }
            in_paths = false;
        }
        let Some((key, rest)) = trimmed.split_once(':') else {
            continue;
        };
        let value = unquote(rest.trim());
        // Only the keys directly under `rosbag2_bagfile_information` (one level of nesting) are
        // taken. `topics_with_message_count` also contains a `message_count:` per topic, four levels
        // deep; taking that as the bag's total would compare one topic's count against the whole
        // recording.
        if indent != 2 {
            continue;
        }
        match key {
            "version" if !value.is_empty() => m.version = Some(value.to_string()),
            "storage_identifier" if !value.is_empty() => {
                m.storage_identifier = Some(value.to_string())
            }
            "ros_distro" if !value.is_empty() => m.ros_distro = Some(value.to_string()),
            "compression_format" if !value.is_empty() => {
                m.compression_format = Some(value.to_string())
            }
            "compression_mode" if !value.is_empty() => m.compression_mode = Some(value.to_string()),
            "message_count" => m.message_count = value.parse().ok(),
            "relative_file_paths" => in_paths = true,
            _ => {}
        }
    }
    m
}

/// Strip one layer of matching quotes from a YAML scalar.
fn unquote(s: &str) -> &str {
    for q in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(q) && s.ends_with(q) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// The column names a `CREATE TABLE` statement declares, in order.
///
/// Enough of a parse to take the first identifier of each top-level comma-separated item inside the
/// outermost parentheses — which is the column name — while ignoring commas nested inside a type or
/// a constraint (`DECIMAL(10,2)`, `CHECK(a IN (1,2))`).
fn column_names(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };
    let body = &sql[open + 1..];
    let mut depth = 0usize;
    let mut items = vec![String::new()];
    for ch in body.chars() {
        match ch {
            '(' => {
                depth += 1;
                items.last_mut().expect("one item always exists").push(ch);
            }
            ')' if depth == 0 => break,
            ')' => {
                depth -= 1;
                items.last_mut().expect("one item always exists").push(ch);
            }
            ',' if depth == 0 => items.push(String::new()),
            _ => items.last_mut().expect("one item always exists").push(ch),
        }
    }
    items
        .iter()
        .filter_map(|item| {
            let name = item.split_whitespace().next()?;
            let name = name.trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']');
            (!name.is_empty()).then(|| name.to_ascii_lowercase())
        })
        .collect()
}

/// Locate a table by name and map the columns this adapter needs to their record positions.
fn column_index(columns: &[String], want: &str) -> Option<usize> {
    columns.iter().position(|c| c == want)
}

/// Whether an `offered_qos_profiles` value declares **transient-local** (latched) durability.
///
/// `Some(true)` for transient-local, `Some(false)` for volatile, `None` for anything else — a
/// profile that says nothing about durability, one this reader is not certain it understood, or
/// several publishers that disagree. Read rather than inferred, and read conservatively, because
/// this flag makes three checks abstain: a false `Some(true)` silences a sensor that genuinely died
/// after one sample, which is worse than leaving the flag unset and living with a warning.
///
/// rosbag2 has written this column three ways across bag versions — a YAML sequence, a JSON array,
/// and (recently) policy *names* rather than the `rmw` enum numbers — so the value is scanned for
/// `durability` and only the four spellings below are accepted.
pub(crate) fn declares_latched(qos: &str) -> Option<bool> {
    let hay = qos.to_ascii_lowercase();
    let mut verdict: Option<bool> = None;
    for (at, _) in hay.match_indices("durability") {
        let rest = &hay[at + "durability".len()..];
        // Step over a `durability_policy`-style suffix and the quoting a JSON key carries, then
        // the separator.
        let rest = rest.trim_start_matches(|c: char| {
            c.is_alphanumeric() || c == '_' || c == '"' || c == '\'' || c == ' '
        });
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let token: String = rest
            .trim_start_matches([' ', '"', '\''])
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        // rmw_qos_durability_policy_t: 1 = TRANSIENT_LOCAL, 2 = VOLATILE. 0 (system default) and 3
        // (unknown) say nothing, and neither does a spelling not listed here.
        let this = match token.as_str() {
            "1" | "transient_local" => Some(true),
            "2" | "volatile" => Some(false),
            _ => None,
        };
        match (verdict, this) {
            (_, None) => continue,
            (None, some) => verdict = some,
            // Two publishers offering different durability is not something to pick a winner from.
            (Some(a), Some(b)) if a != b => return None,
            _ => {}
        }
    }
    verdict
}

/// A topic row: what the bag declares about one recorded topic.
struct TopicRow {
    name: String,
    ros_type: String,
    serialization_format: Option<String>,
    /// Whether the topic's offered QoS declares transient-local (latched) durability.
    latched: Option<bool>,
}

/// A stream being accumulated across every `.db3` in the bag.
struct StreamBuilder {
    modality: Modality,
    ros_type: String,
    frames: Vec<Frame>,
    /// From the topic's recorded QoS durability, when it states one unambiguously.
    latched: Option<bool>,
    point_fields: Option<Vec<PointField>>,
    frame_id: Option<String>,
}

/// Everything one ingest accumulates while walking a bag's `.db3` files.
#[derive(Default)]
struct BagContents {
    streams: BTreeMap<String, StreamBuilder>,
    min_ts: Option<i64>,
    max_ts: Option<i64>,
    ego_poses: Vec<EgoPose>,
    intrinsics: BTreeMap<String, CameraIntrinsics>,
    transforms: BTreeMap<(String, String), Transform>,
    serialization_formats: BTreeSet<String>,
    /// Topic ids referenced by a message row that the `topics` table never declared, with how many
    /// messages each accounts for.
    orphan_topics: BTreeMap<i64, u64>,
}

fn parse_error(message: impl Into<String>) -> IngestError {
    IngestError::Parse {
        format_id: FORMAT,
        message: message.into(),
    }
}

/// Whether a shard's name says it is zstd-compressed.
///
/// `ros2 bag record --compression-mode file` writes the shard, then compresses the finished `.db3`
/// to `<shard>.db3.zstd` and deletes the original — which is how any recording large enough to care
/// about is stored, and the only thing that stood between Veridex and those bags.
fn is_zstd_shard(path: &Path) -> bool {
    // rosbag2 writes `.zstd`; the `zstd` CLI defaults to `.zst`, so a hand-compressed shard is
    // recognized too.
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("zstd") | Some("zst")
    ) && path
        .file_stem()
        .map(Path::new)
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        == Some("db3")
}

impl Rosbag2Adapter {
    /// Whether `path` is a `.db3` shard, compressed or not.
    fn is_db3(path: &Path) -> bool {
        path.is_file()
            && (path.extension().and_then(|e| e.to_str()) == Some("db3") || is_zstd_shard(path))
    }

    /// The `.db3` shards inside a bag directory, in **recording order**.
    ///
    /// Found by listing the directory, not by following `relative_file_paths`: the manifest is
    /// content, and a content-supplied path is never resolved out of the dataset.
    ///
    /// Order matters, and plain name order is wrong for it. `ros2 bag record --max-bag-size` splits
    /// a long recording into `bag_0.db3`, `bag_1.db3`, … `bag_11.db3`, and a lexicographic sort puts
    /// `bag_10` and `bag_11` ahead of `bag_2`. Frames are appended to their stream in the order the
    /// shards are read and the CDM preserves that order — deliberately, because reordering them
    /// would hide the out-of-order timestamps this tool exists to find — so a sound twelve-shard
    /// recording came back with two `TEMPORAL.NON_MONOTONIC` **errors** and two `TEMPORAL.GAP`
    /// warnings, and split recordings are the ordinary shape of any long bag.
    ///
    /// So the shards are ordered by [`natural_key`] — digit runs compared as numbers — and the
    /// caller then reorders by what the manifest lists, which is the bag's own record of the order
    /// it wrote them in. Taking an *ordering* from the manifest follows no path anywhere; only the
    /// file names already found by listing are ever opened.
    fn shards_in(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| Rosbag2Adapter::is_db3(p))
            .collect();
        out.sort_by_key(|p| natural_key(&display(p)));
        Ok(out)
    }

    /// Whether `dir` is a rosbag2 bag directory: it holds at least one `.db3` shard.
    ///
    /// The `metadata.yaml` is **not** required, and requiring it was wrong. rosbag2 writes the
    /// manifest when the recorder *closes*, so a bag that is still being recorded — a directory with
    /// a growing `rec_0.db3` and nothing else — was refused as an unrecognized format. That is
    /// exactly the case `veridex watch` exists for: re-validating a dataset while it records, which
    /// is where catching a clock skew is worth the most. A directory holding a `.db3` is
    /// unambiguous; no other adapter here claims one.
    ///
    /// What the manifest supplies is still reported as missing when it is absent, so nothing is
    /// assumed in its place: no declared message total to reconcile the recording against, no
    /// recording distribution, no shard order beyond the shards' own numbering.
    fn is_bag_dir(dir: &Path) -> bool {
        dir.is_dir() && Rosbag2Adapter::shards_in(dir).is_ok_and(|s| !s.is_empty())
    }
}

/// A sort key that compares digit runs as numbers, so `bag_2` precedes `bag_10`.
///
/// Each element is one run of the name: `(false, "", n)` for a run of digits, `(true, text, 0)` for
/// everything else. Digits sort before other text at the same position, which is arbitrary but
/// total — the property that matters is that the key is a pure function of the name, so the shard
/// order (and therefore the CDM content hash) is the same on every machine and every run.
fn natural_key(name: &str) -> Vec<(bool, String, u128)> {
    let mut out = Vec::new();
    let mut rest = name;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| c.is_ascii_digit());
        match digits {
            Some(0) => {
                let end = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                let (run, tail) = rest.split_at(end);
                // A run longer than u128 holds is not a shard index; comparing it as text keeps the
                // key total instead of saturating two different names to one value.
                match run.parse::<u128>() {
                    Ok(n) => out.push((false, String::new(), n)),
                    Err(_) => out.push((true, run.to_string(), 0)),
                }
                rest = tail;
            }
            Some(at) => {
                let (run, tail) = rest.split_at(at);
                out.push((true, run.to_string(), 0));
                rest = tail;
            }
            None => {
                out.push((true, rest.to_string(), 0));
                rest = "";
            }
        }
    }
    out
}

/// Reorder `found` into the order `listed` gives, appending anything the manifest does not name.
///
/// The manifest is the bag's own record of the order it wrote its shards in, which is the order
/// their messages have to be concatenated in. Only names are matched — a manifest entry naming a
/// path Veridex did not find is skipped here and separately disclosed as unread.
fn in_manifest_order(found: Vec<PathBuf>, listed: &[String]) -> Vec<PathBuf> {
    if listed.is_empty() {
        return found;
    }
    let mut remaining = found;
    let mut ordered = Vec::with_capacity(remaining.len());
    for want in listed {
        if let Some(at) = remaining.iter().position(|p| &display(p) == want) {
            ordered.push(remaining.remove(at));
        }
    }
    // Anything present but unlisted keeps its natural order, after what the manifest accounted for.
    ordered.extend(remaining);
    ordered
}

/// Read one `.db3` into `contents`, charging the ingest budgets as rows arrive.
fn read_shard(
    path: &Path,
    contents: &mut BagContents,
    frames: &mut super::FrameBudget,
    bytes_budget: &mut super::DecompressionBudget,
) -> Result<(), IngestError> {
    let raw = std::fs::read(path).map_err(|e| IngestError::Io(e.to_string()))?;
    // A compressed shard is unpacked under the same budget that bounds every other container in this
    // crate, and bounded *during* the read rather than charged after it: the cap handed to the
    // decoder is what the budget has left, so a zstd bomb is stopped by the bound instead of merely
    // billed for once the memory is gone. The whole `.db3` has to be resident either way — SQLite is
    // a random-access format and the b-tree walk seeks — so this is one allocation, not a stream.
    let bytes = if is_zstd_shard(path) {
        let cap = bytes_budget.remaining().unwrap_or(u64::MAX);
        let decoder = zstd::stream::read::Decoder::new(raw.as_slice()).map_err(|e| {
            parse_error(format!(
                "{}: the zstd stream could not be opened: {e}",
                display(path)
            ))
        })?;
        let mut out = Vec::new();
        // `cap + 1` so a stream that would exactly exhaust the budget is still distinguishable from
        // one that overruns it, which the charge below then refuses by name.
        std::io::copy(
            &mut std::io::Read::take(decoder, cap.saturating_add(1)),
            &mut out,
        )
        .map_err(|e| {
            parse_error(format!(
                "{}: the zstd stream failed to decompress: {e}",
                display(path)
            ))
        })?;
        bytes_budget.take(FORMAT, out.len() as u64)?;
        out
    } else {
        raw
    };
    let db = SqliteDb::open(&bytes).map_err(|e| parse_error(format!("{}: {e}", display(path))))?;

    // --- topics ------------------------------------------------------------------------------
    let (topics_root, topics_sql) = db
        .table_def("topics")
        .map_err(|e| parse_error(format!("{}: {e}", display(path))))?
        .ok_or_else(|| {
            parse_error(format!(
                "{}: no `topics` table — this is a SQLite database but not a rosbag2 bag",
                display(path)
            ))
        })?;
    let cols = column_names(&topics_sql);
    let (i_id, i_name, i_type) = (
        column_index(&cols, "id"),
        column_index(&cols, "name"),
        column_index(&cols, "type"),
    );
    let i_fmt = column_index(&cols, "serialization_format");
    let i_qos = column_index(&cols, "offered_qos_profiles");
    let (Some(i_id), Some(i_name), Some(i_type)) = (i_id, i_name, i_type) else {
        return Err(parse_error(format!(
            "{}: the `topics` table declares columns {cols:?}, which is not a rosbag2 topics table",
            display(path)
        )));
    };
    let mut topics: BTreeMap<i64, TopicRow> = BTreeMap::new();
    db.scan_table(topics_root, &mut |rowid, row| {
        // `id INTEGER PRIMARY KEY` is a rowid *alias*: SQLite stores NULL in the record and the
        // value lives in the cell's rowid. Reading the column alone finds NULL for every row of
        // every real bag, which orphans every message in the file — so the rowid is what the column
        // falls back to.
        let id = row.get(i_id).and_then(Value::as_int).unwrap_or(rowid);
        let (Some(name), Some(ros_type)) = (
            row.get(i_name).and_then(Value::as_text),
            row.get(i_type).and_then(Value::as_text),
        ) else {
            // A topic row missing the columns that identify it cannot name a stream. Skipping it
            // would silently orphan its messages; the messages pass below then records exactly that.
            return Ok(());
        };
        topics.insert(
            id,
            TopicRow {
                name: name.to_string(),
                ros_type: ros_type.to_string(),
                serialization_format: i_fmt
                    .and_then(|i| row.get(i))
                    .and_then(Value::as_text)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string),
                latched: i_qos
                    .and_then(|i| row.get(i))
                    .and_then(Value::as_text)
                    .and_then(declares_latched),
            },
        );
        Ok(())
    })
    .map_err(|e| parse_error(format!("{}: topics: {e}", display(path))))?;

    for topic in topics.values() {
        if let Some(fmt) = &topic.serialization_format {
            contents.serialization_formats.insert(fmt.clone());
        }
    }

    // --- messages ----------------------------------------------------------------------------
    let (messages_root, messages_sql) = db
        .table_def("messages")
        .map_err(|e| parse_error(format!("{}: {e}", display(path))))?
        .ok_or_else(|| {
            parse_error(format!(
                "{}: no `messages` table — this is a SQLite database but not a rosbag2 bag",
                display(path)
            ))
        })?;
    let mcols = column_names(&messages_sql);
    let (Some(i_topic), Some(i_ts), Some(i_data)) = (
        column_index(&mcols, "topic_id"),
        column_index(&mcols, "timestamp"),
        column_index(&mcols, "data"),
    ) else {
        return Err(parse_error(format!(
            "{}: the `messages` table declares columns {mcols:?}, which is not a rosbag2 messages \
             table",
            display(path)
        )));
    };

    // The visitor returns a `SqliteError`, so an ingest-budget refusal is carried out through the
    // scan in this slot and re-raised after it, rather than being flattened into a parse error that
    // would say the file was malformed when it was merely large.
    let mut budget_error: Option<IngestError> = None;
    let scan = db.scan_table(messages_root, &mut |_rowid, row| {
        let (Some(topic_id), Some(ts)) = (
            row.get(i_topic).and_then(Value::as_int),
            row.get(i_ts).and_then(Value::as_int),
        ) else {
            return Ok(());
        };
        let data = row.get(i_data).and_then(Value::as_blob).unwrap_or(&[]);

        let Some(topic) = topics.get(&topic_id) else {
            *contents.orphan_topics.entry(topic_id).or_insert(0) += 1;
            return Ok(());
        };

        if let Err(e) = frames
            .take(FORMAT, 1)
            .and_then(|()| bytes_budget.take(FORMAT, data.len() as u64))
        {
            budget_error = Some(e);
            return Err(super::sqlite::SqliteError("ingest budget exceeded".into()));
        }

        contents.min_ts = Some(contents.min_ts.map_or(ts, |m: i64| m.min(ts)));
        contents.max_ts = Some(contents.max_ts.map_or(ts, |m: i64| m.max(ts)));

        let builder = contents
            .streams
            .entry(topic.name.clone())
            .or_insert_with(|| StreamBuilder {
                modality: super::mcap::infer_modality(&topic.ros_type, &topic.name),
                ros_type: topic.ros_type.clone(),
                frames: Vec::new(),
                latched: topic.latched,
                point_fields: None,
                frame_id: None,
            });
        builder.frames.push(Frame {
            ts,
            value_ref: ValueRef {
                uri: topic.name.clone(),
                byte_offset: None,
                byte_len: Some(data.len() as u64),
                // The serialized message's bytes, hashed — never decoded. This is what gives the
                // content-level checks (duplicate episodes, stuck streams) something exact.
                content_hash: Some(Sha256::digest(data).into()),
            },
        });

        if builder.frame_id.is_none() {
            builder.frame_id = super::cdr::decode_header_frame_id(data);
        }

        let ty = &topic.ros_type;
        if super::mcap::schema_is(ty, "PointCloud2") {
            if builder.point_fields.is_none() {
                builder.point_fields = super::cdr::decode_point_cloud2_fields(data);
            }
        } else if super::mcap::schema_is(ty, "CameraInfo") {
            if !contents.intrinsics.contains_key(&topic.name) {
                if let Some(ci) = super::cdr::decode_camera_info(data, &topic.name) {
                    contents.intrinsics.insert(topic.name.clone(), ci);
                }
            }
        } else if super::mcap::schema_is(ty, "Odometry") {
            if let Some(pose) = super::cdr::decode_odometry_pose(data) {
                contents.ego_poses.push(EgoPose { ts, pose });
            }
        } else if super::mcap::schema_is(ty, "TFMessage") {
            if let Some(edges) = super::cdr::decode_tf_message(data) {
                for t in edges {
                    contents
                        .transforms
                        .entry((t.parent_frame.clone(), t.child_frame.clone()))
                        .or_insert(t);
                }
            }
        }
        Ok(())
    });
    if let Some(e) = budget_error {
        return Err(e);
    }
    scan.map_err(|e| parse_error(format!("{}: messages: {e}", display(path))))?;
    Ok(())
}

/// A path as it should appear in an error message: the file name, which is what identifies a shard
/// inside a bag, rather than the caller's whole path.
fn display(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("<db3>")
        .to_string()
}

impl Adapter for Rosbag2Adapter {
    fn format_id(&self) -> &'static str {
        FORMAT
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        SUPPORTED_VERSIONS
    }

    fn detect(&self, source: &Source) -> Detection {
        let Source::Local(path) = source else {
            return Detection::No;
        };
        if Rosbag2Adapter::is_bag_dir(path) {
            // `None` when the bag is still recording and has written no manifest yet — the honest
            // answer, and distinct from a bag that names a version.
            let version = std::fs::read_to_string(path.join("metadata.yaml"))
                .ok()
                .and_then(|t| parse_manifest(&t).version);
            return Detection::Yes { version };
        }
        if Rosbag2Adapter::is_db3(path) {
            // A bare `.db3` carries no manifest, so there is no version to report. Saying `None`
            // rather than guessing "4" is the difference between "the bag does not say" and "the bag
            // says 4".
            return Detection::Yes { version: None };
        }
        Detection::No
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        // A bag is one continuous recording, so there is no episode axis to sample along.
        super::reject_sampling(FORMAT, options)?;
        let Source::Local(path) = source else {
            return Err(IngestError::NotImplemented {
                what: "remote rosbag2 ingestion",
                hint: "fetch the bag locally and check the path",
            });
        };

        let is_dir = Rosbag2Adapter::is_bag_dir(path);
        let (shards, manifest, dataset_id) = if is_dir {
            // Absent while the recorder is still running: read it if it is there, and report what it
            // would have supplied as omitted if it is not.
            let manifest = match std::fs::read_to_string(path.join("metadata.yaml")) {
                Ok(text) => parse_manifest(&text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BagManifest::default(),
                Err(e) => return Err(IngestError::Io(e.to_string())),
            };
            let found =
                Rosbag2Adapter::shards_in(path).map_err(|e| IngestError::Io(e.to_string()))?;
            let ordered = in_manifest_order(found, &manifest.relative_file_paths);
            (ordered, manifest, super::dataset_id_from_path(path, FORMAT))
        } else {
            let resolved = path.canonicalize().ok();
            let named = resolved.as_deref().unwrap_or(path);
            // `shard_0.db3.zstd`'s stem is `shard_0.db3`; the dataset is `shard_0`, and a compressed
            // bag must not be identified differently from the same bag uncompressed.
            let id = Path::new(named.file_stem().unwrap_or_default())
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(FORMAT)
                .to_string();
            (vec![path.clone()], BagManifest::default(), id)
        };

        if let Some(version) = &manifest.version {
            if !SUPPORTED_VERSIONS.contains(&version.as_str()) {
                return Err(IngestError::UnsupportedVersion {
                    format_id: FORMAT,
                    version: Some(version.clone()),
                    supported: SUPPORTED_VERSIONS,
                });
            }
        }
        // Per-message compression puts a zstd frame in every `data` blob. Veridex would still read
        // the bag — the tables are plain — but every frame's content hash would fingerprint a
        // compressed body rather than the message, and no AV header would decode, so the rig CDM
        // would come back empty from a bag that is full. That is a wrong answer, not a missing one,
        // so the bag is refused by name.
        if let Some(mode) = &manifest.compression_mode {
            if !mode.eq_ignore_ascii_case("file") {
                return Err(parse_error(format!(
                    "the bag declares compression mode `{mode}`; this adapter reads `FILE` mode \
                     (whole-shard compression). Re-record or convert with `ros2 bag convert` to \
                     file-mode or uncompressed storage"
                )));
            }
        }
        if let Some(fmt) = &manifest.compression_format {
            if !fmt.eq_ignore_ascii_case("zstd") {
                return Err(parse_error(format!(
                    "the bag declares compression format `{fmt}`; this adapter decompresses `zstd`"
                )));
            }
        }
        // A bag recorded through a different storage plugin keeps its messages somewhere this
        // adapter does not read. Refuse it by name rather than reporting an empty recording.
        if let Some(storage) = &manifest.storage_identifier {
            if !storage.eq_ignore_ascii_case("sqlite3") {
                return Err(parse_error(format!(
                    "the bag declares storage `{storage}`; this adapter reads the `sqlite3` storage \
                     plugin (an MCAP-backed bag is read by pointing Veridex at its .mcap file)"
                )));
            }
        }

        let source_bytes: u64 = shards
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let mut frames = super::FrameBudget::new(options);
        let mut bytes_budget = super::DecompressionBudget::new(options, source_bytes);

        let mut contents = BagContents::default();
        for shard in &shards {
            read_shard(shard, &mut contents, &mut frames, &mut bytes_budget)?;
        }

        // Data the bag points at that this ingest did not read. Both arms are coverage holes, so
        // both travel into the verdict as `COVERAGE.SOURCE_UNREAD` rather than staying in a note
        // only `inspect` would print.
        let mut unread_sources = Vec::new();
        let present: BTreeSet<String> = shards.iter().map(|p| display(p)).collect();
        for listed in &manifest.relative_file_paths {
            if listed.contains('/') || listed.contains('\\') {
                unread_sources.push(UnmappedField {
                    source_path: format!("metadata.yaml relative_file_paths[{listed}]"),
                    note: "the manifest names a path with a directory component; Veridex reads only \
                           the .db3 files inside the bag directory and does not follow a path out of \
                           it"
                        .into(),
                });
            } else if !present.contains(listed) {
                unread_sources.push(UnmappedField {
                    source_path: format!("metadata.yaml relative_file_paths[{listed}]"),
                    note: "the manifest lists this shard but it is not in the bag directory — its \
                           messages were not read"
                        .into(),
                });
            }
        }
        // A SQLite sidecar beside a shard holds committed transactions the main file does not yet.
        // This reader walks the shard's own pages — it does not replay a write-ahead log or roll back
        // a hot journal — so a bag caught mid-recording under `journal_mode=WAL` has messages that
        // exist, are committed, and were not read. Silence there is the shape of defect this whole
        // crate is built to refuse: a report that speaks for a recording it only partly saw.
        for shard in &shards {
            for (suffix, what) in [
                ("-wal", "write-ahead log"),
                ("-journal", "rollback journal"),
            ] {
                let sidecar = shard.with_file_name(format!("{}{suffix}", display(shard)));
                let has_content = std::fs::metadata(&sidecar).is_ok_and(|m| m.len() > 0);
                if has_content {
                    unread_sources.push(UnmappedField {
                        source_path: display(&sidecar),
                        note: format!(
                            "a SQLite {what} sits beside this shard, holding transactions the \
                             `.db3` itself does not carry — a recording still in progress. Veridex \
                             reads the shard's committed pages and does not replay it, so those \
                             messages were not read; check the bag again once the recorder has \
                             closed it"
                        ),
                    });
                }
            }
        }

        for (topic_id, count) in &contents.orphan_topics {
            unread_sources.push(UnmappedField {
                source_path: format!("messages.topic_id={topic_id}"),
                note: format!(
                    "{count} message(s) reference a topic the `topics` table does not declare; \
                     there is no topic name to file them under, so they were not read"
                ),
            });
        }
        let has_point_fields = contents.streams.values().any(|s| s.point_fields.is_some());
        let ros_types: BTreeSet<String> = contents
            .streams
            .values()
            .map(|s| s.ros_type.clone())
            .collect();
        let cdm_streams: Vec<Stream> = contents
            .streams
            .into_iter()
            .map(|(name, b)| Stream {
                name,
                modality: b.modality,
                // rosbag2 records a QoS profile per topic, not a nominal sample rate.
                declared_rate_hz: None,
                clock_id: CLOCK_ID.to_string(),
                clock_kind: ClockKind::Measured,
                dtype: None,
                shape: None,
                frames: b.frames,
                stats: None,
                dim_stats: None,
                // Message payloads are opaque bytes: fingerprinted, never decoded into values.
                observed_stats: None,
                observed_saturation: None,
                observed_non_finite: None,
                observed_dim_stats: None,
                latched: b.latched,
                point_fields: b.point_fields,
                media: None,
                frame_id: b.frame_id,
            })
            .collect();

        // The manifest's own total, checked against what the recording actually yielded. A bag whose
        // writer was killed mid-flush leaves a `.db3` short of the count `metadata.yaml` closed with,
        // and reading it as a complete recording is precisely the "silence reads as a pass" failure
        // this tool exists to prevent.
        let orphaned: u64 = contents.orphan_topics.values().sum();
        let ingested: u64 = cdm_streams
            .iter()
            .map(|s| s.frames.len() as u64)
            .sum::<u64>()
            + orphaned;
        let mut count_mismatch = None;
        if let Some(declared) = manifest.message_count {
            match declared.cmp(&ingested) {
                std::cmp::Ordering::Greater => unread_sources.push(UnmappedField {
                    source_path: "metadata.yaml message_count".into(),
                    note: format!(
                        "the manifest declares {declared} message(s) but {ingested} were read — \
                         {} are missing from the bag's .db3 file(s)",
                        declared - ingested
                    ),
                }),
                // The other direction is not unread data: every message present was read. It is the
                // manifest that is wrong, which is worth saying but is not a coverage hole.
                std::cmp::Ordering::Less => {
                    count_mismatch = Some(UnmappedField {
                        source_path: "metadata.yaml message_count".into(),
                        note: format!(
                            "the manifest declares {declared} message(s) but {ingested} were read; \
                             the CDM records the recording, and the manifest's disagreeing total is \
                             not represented in it"
                        ),
                    })
                }
                std::cmp::Ordering::Equal => {}
            }
        }

        let calibration = if contents.transforms.is_empty() && contents.intrinsics.is_empty() {
            None
        } else {
            Some(Calibration {
                transforms: contents.transforms.into_values().collect(),
                intrinsics: contents.intrinsics.into_values().collect(),
            })
        };
        let ego_poses = if contents.ego_poses.is_empty() {
            None
        } else {
            let mut poses = contents.ego_poses;
            poses.sort_by_key(|p| p.ts);
            Some(poses)
        };

        let mut metadata = vec![("source_format".into(), FORMAT.to_string())];
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some(FORMAT.to_string()),
            class: ProvenanceClass::Known,
        }];
        if let Some(v) = &manifest.version {
            metadata.push(("rosbag2_version".into(), v.clone()));
        }
        if let Some(s) = &manifest.storage_identifier {
            metadata.push(("rosbag2_storage".into(), s.clone()));
        }
        if let Some(fmt) = &manifest.compression_format {
            metadata.push(("rosbag2_compression".into(), fmt.clone()));
        }
        if let Some(distro) = &manifest.ros_distro {
            metadata.push(("ros_distro".into(), distro.clone()));
            // The distribution that recorded the bag is who produced it, as far as the bag says.
            // Read verbatim from the manifest, so `Known`.
            elements.push(ProvenanceElement {
                key: "recorder".into(),
                value: Some(format!("rosbag2 ({distro})")),
                class: ProvenanceClass::Known,
            });
        }
        for fmt in &contents.serialization_formats {
            metadata.push(("serialization_format".into(), fmt.clone()));
        }

        let dataset = Dataset {
            id: dataset_id,
            metadata,
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements,
            }],
            episodes: vec![Episode {
                index: 0,
                start_ts: contents.min_ts,
                end_ts: contents.max_ts,
                streams: cdm_streams,
                task: None,
                labels: Vec::new(),
                ego_poses,
                // Deliberately not the manifest's `message_count`. That is a bag-wide total across
                // every topic, while `declared_frame_count` is what one episode's streams are each
                // expected to hold — the boundary check would compare 363 messages against the
                // longest stream's 200 frames and fail a sound bag. The manifest total is
                // reconciled against the recording above instead, where a shortfall is what it
                // actually is: messages the bag says exist that this run did not read.
                declared_frame_count: None,
            }],
            calibration,
        };

        let mut mapped_fields = vec![
            "topics.name -> stream.name".into(),
            "topics.type -> stream.modality".into(),
            "messages.timestamp -> frame.ts".into(),
            "messages.data.len -> frame.value_ref.byte_len".into(),
            "messages.data -> frame.value_ref.content_hash (SHA-256)".into(),
        ];
        if has_point_fields {
            mapped_fields.push("PointCloud2.fields -> stream.point_fields".into());
        }
        if dataset.calibration.is_some() {
            mapped_fields.push("CameraInfo.k/d + TFMessage -> dataset.calibration".into());
        }
        if dataset.episodes.iter().any(|e| e.ego_poses.is_some()) {
            mapped_fields.push("Odometry.pose -> episode.ego_poses".into());
        }
        if manifest.ros_distro.is_some() {
            mapped_fields.push("metadata.yaml ros_distro -> provenance.recorder".into());
        }
        if dataset
            .episodes
            .iter()
            .flat_map(|e| &e.streams)
            .any(|s| s.latched.is_some())
        {
            mapped_fields.push("topics.offered_qos_profiles durability -> stream.latched".into());
        }

        let mut unmapped_fields = vec![
            UnmappedField {
                source_path: "topics.offered_qos_profiles".into(),
                note:
                    "only the durability policy is read (into `Stream::latched`); the CDM has no \
                       shape for the rest of a QoS profile — reliability, history depth, deadline, \
                       lifespan, liveliness"
                        .into(),
            },
            UnmappedField {
                source_path: "messages.id".into(),
                note: "the bag's per-message rowid is not represented in the CDM".into(),
            },
        ];
        if let Some(mismatch) = count_mismatch {
            unmapped_fields.push(mismatch);
        }
        if !ros_types.is_empty() {
            unmapped_fields.push(UnmappedField {
                source_path: "message payloads".into(),
                note: format!(
                    "message bodies are fingerprinted, never decoded; only the AV message headers \
                     are read ({} ROS type(s) present)",
                    ros_types.len()
                ),
            });
        }

        let mut omitted_fields = vec![
            "episode-segmentation (a bag is one continuous recording; the whole bag is one episode)"
                .into(),
            "declared-rate (rosbag2 declares QoS, not a nominal sample rate)".into(),
        ];
        if !is_dir {
            omitted_fields.push(
                "metadata.yaml (a bare .db3 was checked, so the bag declares no message count to \
                 compare against, no storage identifier, and no recorder)"
                    .into(),
            );
        } else if !path.join("metadata.yaml").is_file() {
            // One line, not three: with no manifest at all, naming each key it would have carried
            // reads as three separate gaps in a bag that has one.
            omitted_fields.push(
                "metadata.yaml (the bag has not written one — rosbag2 writes the manifest when the \
                 recorder closes, so this is what a recording in progress looks like; there is no \
                 declared message count to reconcile the recording against and no recorder identity)"
                    .into(),
            );
        } else {
            if manifest.message_count.is_none() {
                omitted_fields.push(
                    "metadata.yaml message_count (the manifest declares no total, so the recording \
                     was not reconciled against one)"
                        .into(),
                );
            }
            if manifest.ros_distro.is_none() {
                omitted_fields.push(
                    "metadata.yaml ros_distro (the manifest names no recording distribution)"
                        .into(),
                );
            }
        }

        let report = IngestReport {
            format_id: FORMAT,
            source_version: manifest.version.clone(),
            coverage: Coverage::Full,
            mapped_fields,
            unmapped_fields,
            unread_sources,
            omitted_fields,
        };
        Ok(Ingested { dataset, report })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_ignores_commas_inside_a_type() {
        let cols = column_names(
            "CREATE TABLE t(id INTEGER PRIMARY KEY, price DECIMAL(10,2), name TEXT NOT NULL)",
        );
        assert_eq!(cols, vec!["id", "price", "name"]);
    }

    #[test]
    fn column_names_reads_the_rosbag2_topics_table() {
        let cols = column_names(
            "CREATE TABLE topics(id INTEGER PRIMARY KEY, name TEXT NOT NULL, type TEXT NOT NULL, \
             serialization_format TEXT NOT NULL, offered_qos_profiles TEXT NOT NULL)",
        );
        assert_eq!(column_index(&cols, "serialization_format"), Some(3));
        // The column that moved between bag versions is found by name, so a version that inserts a
        // column ahead of it does not shift what is read.
        let v9 = column_names(
            "CREATE TABLE topics(id INTEGER PRIMARY KEY, name TEXT NOT NULL, type TEXT NOT NULL, \
             type_description_hash TEXT NOT NULL, serialization_format TEXT NOT NULL, \
             offered_qos_profiles TEXT NOT NULL)",
        );
        assert_eq!(column_index(&v9, "serialization_format"), Some(4));
    }

    #[test]
    fn shards_sort_by_their_number_not_their_spelling() {
        let mut names: Vec<&str> = vec![
            "bag_10.db3",
            "bag_2.db3",
            "bag_1.db3",
            "bag_11.db3",
            "bag_0.db3",
        ];
        names.sort_by_key(|n| natural_key(n));
        assert_eq!(
            names,
            vec![
                "bag_0.db3",
                "bag_1.db3",
                "bag_2.db3",
                "bag_10.db3",
                "bag_11.db3"
            ]
        );
        // A digit run too long for a u128 must still order deterministically rather than collapsing
        // two different names onto one key.
        let huge = "9".repeat(60);
        assert_ne!(
            natural_key(&format!("a{huge}0")),
            natural_key(&format!("a{huge}1"))
        );
    }

    #[test]
    fn the_manifest_orders_the_shards_it_names_and_keeps_the_rest() {
        let found: Vec<PathBuf> = ["b.db3", "a.db3", "z.db3"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let listed = vec![
            "a.db3".to_string(),
            "b.db3".to_string(),
            "gone.db3".to_string(),
        ];
        let ordered: Vec<String> = in_manifest_order(found, &listed)
            .iter()
            .map(|p| display(p))
            .collect();
        // The manifest's order wins for what it names; a shard it does not name still gets read,
        // after them; a shard it names but that is absent is skipped here (and disclosed as unread).
        assert_eq!(ordered, vec!["a.db3", "b.db3", "z.db3"]);
    }

    #[test]
    fn qos_durability_is_read_only_where_it_is_unambiguous() {
        // The two spellings rosbag2 has written across bag versions: the `rmw` enum number and, more
        // recently, the policy name.
        assert_eq!(
            declares_latched("- history: 3\n  depth: 0\n  reliability: 1\n  durability: 1\n"),
            Some(true)
        );
        assert_eq!(
            declares_latched("- history: 3\n  durability: 2\n"),
            Some(false)
        );
        assert_eq!(
            declares_latched("[{\"durability\": \"transient_local\"}]"),
            Some(true)
        );
        assert_eq!(
            declares_latched("[{\"durability\": \"volatile\"}]"),
            Some(false)
        );
        assert_eq!(
            declares_latched("durability_policy: transient_local"),
            Some(true)
        );

        // Anything else leaves the flag unset rather than guessed: this suppresses three checks, so
        // a wrong `Some(true)` silences a sensor that genuinely died after one sample.
        assert_eq!(declares_latched(""), None);
        assert_eq!(
            declares_latched("- reliability: 1\n"),
            None,
            "no durability at all"
        );
        assert_eq!(
            declares_latched("durability: 0"),
            None,
            "system default says nothing"
        );
        assert_eq!(
            declares_latched("durability: 3"),
            None,
            "unknown says nothing"
        );
        assert_eq!(declares_latched("durability: sometimes"), None);
        // Two publishers disagreeing is not something to pick a winner from.
        assert_eq!(declares_latched("- durability: 1\n- durability: 2\n"), None);
        // Two publishers agreeing is still an answer.
        assert_eq!(
            declares_latched("- durability: 1\n- durability: 1\n"),
            Some(true)
        );
    }

    #[test]
    fn the_manifest_reads_only_top_level_keys() {
        let text = "\
rosbag2_bagfile_information:
  version: 5
  storage_identifier: sqlite3
  message_count: 700
  topics_with_message_count:
    - topic_metadata:
        name: /lidar/points
      message_count: 20
  ros_distro: humble
";
        let m = parse_manifest(text);
        assert_eq!(m.version.as_deref(), Some("5"));
        assert_eq!(m.storage_identifier.as_deref(), Some("sqlite3"));
        assert_eq!(m.ros_distro.as_deref(), Some("humble"));
        // 700, not the 20 that one topic declares four levels deeper.
        assert_eq!(m.message_count, Some(700));
    }

    #[test]
    fn the_manifest_reads_the_shard_list() {
        let text = "\
rosbag2_bagfile_information:
  version: 5
  relative_file_paths:
    - bag_0.db3
    - bag_1.db3
  message_count: 3
";
        let m = parse_manifest(text);
        assert_eq!(m.relative_file_paths, vec!["bag_0.db3", "bag_1.db3"]);
        assert_eq!(m.message_count, Some(3), "the list ends at the next key");
    }
}
