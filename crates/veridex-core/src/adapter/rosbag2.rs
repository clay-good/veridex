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

/// A topic row: what the bag declares about one recorded topic.
struct TopicRow {
    name: String,
    ros_type: String,
    serialization_format: Option<String>,
}

/// A stream being accumulated across every `.db3` in the bag.
struct StreamBuilder {
    modality: Modality,
    ros_type: String,
    frames: Vec<Frame>,
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

impl Rosbag2Adapter {
    /// Whether `path` is a `.db3` file.
    fn is_db3(path: &Path) -> bool {
        path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("db3")
    }

    /// The `.db3` files inside a bag directory, in name order.
    ///
    /// Found by listing the directory, not by following `relative_file_paths`: the manifest is
    /// content, and a content-supplied path is never resolved out of the dataset.
    fn shards_in(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| Rosbag2Adapter::is_db3(p))
            .collect();
        out.sort();
        Ok(out)
    }

    /// Whether `dir` looks like a rosbag2 bag directory: a `metadata.yaml` beside at least one
    /// `.db3`. Both are required — a `metadata.yaml` alone belongs to some other tool, and a
    /// directory of `.db3` files with no manifest is not something to claim as a bag by autodetection
    /// (point Veridex at the file, or pass `--format rosbag2`).
    fn is_bag_dir(dir: &Path) -> bool {
        dir.is_dir()
            && dir.join("metadata.yaml").is_file()
            && Rosbag2Adapter::shards_in(dir).is_ok_and(|s| !s.is_empty())
    }
}

/// Read one `.db3` into `contents`, charging the ingest budgets as rows arrive.
fn read_shard(
    path: &Path,
    contents: &mut BagContents,
    frames: &mut super::FrameBudget,
    bytes_budget: &mut super::DecompressionBudget,
) -> Result<(), IngestError> {
    let bytes = std::fs::read(path).map_err(|e| IngestError::Io(e.to_string()))?;
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
            let text = std::fs::read_to_string(path.join("metadata.yaml"))
                .map_err(|e| IngestError::Io(e.to_string()))?;
            (
                Rosbag2Adapter::shards_in(path).map_err(|e| IngestError::Io(e.to_string()))?,
                parse_manifest(&text),
                super::dataset_id_from_path(path, FORMAT),
            )
        } else {
            let id = path
                .canonicalize()
                .ok()
                .as_deref()
                .and_then(Path::file_stem)
                .or_else(|| path.file_stem())
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

        let mut unmapped_fields = vec![
            UnmappedField {
                source_path: "topics.offered_qos_profiles".into(),
                note: "the CDM has no shape for a per-topic QoS profile (reliability, durability, \
                       history depth)"
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
