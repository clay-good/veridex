//! LeRobot v3 adapter: maps a LeRobot dataset directory into the CDM (design D3).
//!
//! LeRobot is the beachhead format. A dataset is a directory with `meta/info.json` (declaring the
//! features, fps, and robot type) and Parquet data files under `data/` where **one row is one
//! frame** carrying every feature plus bookkeeping columns (`episode_index`, `timestamp`, …).
//!
//! Mapping:
//! - each declared feature (e.g. `observation.images.top`, `observation.state`, `action`) → a CDM
//!   [`Stream`], with modality inferred from the feature name and dtype;
//! - the per-row `timestamp` (seconds) → each frame's timestamp; rows are grouped by
//!   `episode_index` into [`Episode`]s. All features are frame-aligned, so within an episode every
//!   stream shares the same frame timestamps and the single LeRobot clock (`clock_id = "lerobot"`);
//! - `fps` → each stream's declared rate.
//!
//! Veridex reads timestamps and structure and **fingerprints** feature payloads — it hashes each
//! feature cell's raw value bytes into `frame.value_ref.content_hash` (a SHA-256, never an
//! interpretation of the values), so the CDM content hash is sensitive to actual frame content and
//! content-level checks (duplicate episodes) work. Cells whose type isn't a supported numeric
//! feature (e.g. an image feature stored outside the Parquet) are left unhashed, honestly.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::array::{
    Array, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, Int32Array, Int64Array,
    ListArray,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cdm::{
    Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, StreamStats, ValueRef,
};

use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};

const CLOCK_ID: &str = "lerobot";

/// A data feature resolved from `meta/info.json`: its name, inferred modality, and declared
/// dtype/shape (both preserved so the structural checks can verify cross-episode consistency).
type FeatureSpec = (String, Modality, Option<String>, Option<Vec<u64>>);

/// Bookkeeping columns that are not data features and never become streams.
const BOOKKEEPING: &[&str] = &[
    "timestamp",
    "frame_index",
    "episode_index",
    "index",
    "task_index",
];

#[derive(Debug, Deserialize)]
struct InfoJson {
    #[serde(default)]
    codebase_version: Option<String>,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    robot_type: Option<String>,
    #[serde(default)]
    total_episodes: Option<u64>,
    #[serde(default)]
    total_frames: Option<u64>,
    #[serde(default)]
    features: BTreeMap<String, FeatureInfo>,
}

#[derive(Debug, Deserialize)]
struct FeatureInfo {
    #[serde(default)]
    dtype: Option<String>,
    #[serde(default)]
    shape: Option<Vec<u64>>,
}

/// The LeRobot v3 adapter.
pub struct LeRobotAdapter;

impl LeRobotAdapter {
    fn info_path(dir: &Path) -> PathBuf {
        dir.join("meta").join("info.json")
    }
}

fn infer_modality(name: &str, dtype: Option<&str>) -> Modality {
    let n = name.to_ascii_lowercase();
    let d = dtype.unwrap_or("").to_ascii_lowercase();
    if n.contains("image") || d == "video" || d == "image" {
        Modality::Video
    } else if n.contains("audio") {
        Modality::Audio
    } else if n.contains("wrench")
        || n.contains("force")
        || n.contains("torque")
        || n.contains("tactile")
    {
        Modality::TactileForceTorque
    } else if n == "action" || n.starts_with("action") {
        Modality::Action
    } else {
        Modality::ScalarState
    }
}

/// The first scalar number reachable in a JSON value (descending into arrays). LeRobot stats are
/// per-dimension arrays; we summarize with the first dimension.
fn first_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Array(a) => a.iter().find_map(first_number),
        _ => None,
    }
}

/// Load per-feature stored statistics from `meta/stats.json`, if present. Missing or unparseable
/// stats are simply absent (never fabricated).
fn load_stats(dir: &Path) -> BTreeMap<String, StreamStats> {
    let mut out = BTreeMap::new();
    let Ok(bytes) = std::fs::read(dir.join("meta").join("stats.json")) else {
        return out;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        return out;
    };
    for (feature, stats) in map {
        let (Some(min), Some(max), Some(mean), Some(std)) = (
            stats.get("min").and_then(first_number),
            stats.get("max").and_then(first_number),
            stats.get("mean").and_then(first_number),
            stats.get("std").and_then(first_number),
        ) else {
            continue;
        };
        out.insert(
            feature,
            StreamStats {
                min,
                max,
                mean,
                std,
            },
        );
    }
    out
}

/// Recursively collect `.parquet` files under `dir`, in a deterministic sorted order.
fn find_parquet(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            find_parquet(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("parquet") {
            out.push(p);
        }
    }
}

fn column_i64(array: &dyn Array, row: usize) -> Option<i64> {
    if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
        Some(a.value(row))
    } else {
        array
            .as_any()
            .downcast_ref::<Int32Array>()
            .map(|a| a.value(row) as i64)
    }
}

fn column_f64(array: &dyn Array, row: usize) -> Option<f64> {
    if let Some(a) = array.as_any().downcast_ref::<Float64Array>() {
        Some(a.value(row))
    } else {
        array
            .as_any()
            .downcast_ref::<Float32Array>()
            .map(|a| a.value(row) as f64)
    }
}

/// Fingerprint one feature cell into a stable 32-byte digest, or `None` when the cell's type isn't a
/// supported numeric feature (e.g. an image feature stored outside the Parquet, or an encoded blob).
/// This is a hash of the raw value bytes — Veridex never interprets them.
fn hash_cell(array: &dyn Array, row: usize) -> Option<[u8; 32]> {
    let mut hasher = Sha256::new();
    feed_cell(array, row, &mut hasher).then(|| hasher.finalize().into())
}

/// Feed a cell's canonical little-endian bytes into `hasher`. Returns `false` for an unsupported
/// type, so the caller can abstain rather than fabricate a hash. Nested lists recurse; a null cell
/// contributes a distinct marker byte.
fn feed_cell(array: &dyn Array, row: usize, hasher: &mut Sha256) -> bool {
    let any = array.as_any();
    if array.is_null(row) {
        hasher.update([0xffu8]);
        return true;
    }
    if let Some(a) = any.downcast_ref::<Float32Array>() {
        hasher.update(a.value(row).to_le_bytes());
    } else if let Some(a) = any.downcast_ref::<Float64Array>() {
        hasher.update(a.value(row).to_le_bytes());
    } else if let Some(a) = any.downcast_ref::<Int64Array>() {
        hasher.update(a.value(row).to_le_bytes());
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        hasher.update(a.value(row).to_le_bytes());
    } else if let Some(a) = any.downcast_ref::<BooleanArray>() {
        hasher.update([a.value(row) as u8]);
    } else if let Some(a) = any.downcast_ref::<FixedSizeListArray>() {
        let child = a.value(row);
        for i in 0..child.len() {
            if !feed_cell(child.as_ref(), i, hasher) {
                return false;
            }
        }
    } else if let Some(a) = any.downcast_ref::<ListArray>() {
        let child = a.value(row);
        // Length-prefix variable-length lists so [[a],[b]] and [[a,b]] can't collide.
        hasher.update((child.len() as u64).to_le_bytes());
        for i in 0..child.len() {
            if !feed_cell(child.as_ref(), i, hasher) {
                return false;
            }
        }
    } else {
        return false;
    }
    true
}

/// The first scalar value reachable in a feature cell (descending into the first list element), or
/// `None` for a null or an unsupported type. LeRobot's stored `meta/stats.json` summarizes each
/// feature by its first dimension, so Veridex recomputes over the same first scalar to compare.
fn first_scalar(array: &dyn Array, row: usize) -> Option<f64> {
    if array.is_null(row) {
        return None;
    }
    let any = array.as_any();
    if let Some(a) = any.downcast_ref::<Float32Array>() {
        Some(a.value(row) as f64)
    } else if let Some(a) = any.downcast_ref::<Float64Array>() {
        Some(a.value(row))
    } else if let Some(a) = any.downcast_ref::<Int64Array>() {
        Some(a.value(row) as f64)
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        Some(a.value(row) as f64)
    } else if let Some(a) = any.downcast_ref::<BooleanArray>() {
        Some(a.value(row) as u8 as f64)
    } else if let Some(a) = any.downcast_ref::<FixedSizeListArray>() {
        let child = a.value(row);
        (!child.is_empty())
            .then(|| first_scalar(child.as_ref(), 0))
            .flatten()
    } else if let Some(a) = any.downcast_ref::<ListArray>() {
        let child = a.value(row);
        (!child.is_empty())
            .then(|| first_scalar(child.as_ref(), 0))
            .flatten()
    } else {
        None
    }
}

/// Running accumulator for a feature's recomputed statistics (over its first scalar per row).
#[derive(Default, Clone, Copy)]
struct StatsAccum {
    count: u64,
    min: f64,
    max: f64,
    sum: f64,
    sum_sq: f64,
}

impl StatsAccum {
    fn push(&mut self, v: f64) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.count += 1;
        self.sum += v;
        self.sum_sq += v * v;
    }

    /// Finalize to a [`StreamStats`] (population std), or `None` if nothing was accumulated.
    fn finish(&self) -> Option<StreamStats> {
        if self.count == 0 {
            return None;
        }
        let n = self.count as f64;
        let mean = self.sum / n;
        let variance = (self.sum_sq / n - mean * mean).max(0.0);
        Some(StreamStats {
            min: self.min,
            max: self.max,
            mean,
            std: variance.sqrt(),
        })
    }
}

/// One row of a LeRobot data Parquet: episode index, frame timestamp (ns), `task_index` (if the
/// column is present; unresolved to a string here — see [`load_tasks`]), and per feature-value column
/// present in the Parquet a content hash (`None` if the type isn't hashable) and its first scalar
/// value (`None` if not numeric), used to recompute statistics.
type Row = (
    u64,
    i64,
    Option<i64>,
    Vec<(String, Option<[u8; 32]>, Option<f64>)>,
);

/// Read (episode_index, timestamp_ns, task_index) for every row of a Parquet file, in row order.
fn read_rows(path: &Path, fps: f64) -> Result<Vec<Row>, IngestError> {
    let file = File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .and_then(|b| b.build())
        .map_err(|e| IngestError::Parse {
            format_id: "lerobot",
            message: format!("{}: {e}", path.display()),
        })?;

    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| IngestError::Parse {
            format_id: "lerobot",
            message: format!("{}: {e}", path.display()),
        })?;
        let ep_col = batch
            .column_by_name("episode_index")
            .ok_or_else(|| IngestError::Parse {
                format_id: "lerobot",
                message: format!("{}: missing `episode_index` column", path.display()),
            })?;
        let ts_col = batch.column_by_name("timestamp");
        let frame_col = batch.column_by_name("frame_index");
        let task_col = batch.column_by_name("task_index");

        // Feature-value columns are every column that isn't bookkeeping; their cells are fingerprinted
        // per row so content-level checks and the CDM hash reflect actual frame content.
        let feature_cols: Vec<(String, &dyn Array)> = batch
            .schema()
            .fields()
            .iter()
            .enumerate()
            .filter(|(_, f)| !BOOKKEEPING.contains(&f.name().as_str()))
            .map(|(i, f)| (f.name().clone(), batch.column(i).as_ref()))
            .collect();

        for row in 0..batch.num_rows() {
            let ep = column_i64(ep_col.as_ref(), row).ok_or_else(|| IngestError::Parse {
                format_id: "lerobot",
                message: format!("{}: episode_index is not an integer column", path.display()),
            })? as u64;

            // Prefer the recorded timestamp; fall back to frame_index / fps.
            let ts_ns = if let Some(ts) = ts_col.as_ref().and_then(|c| column_f64(c.as_ref(), row))
            {
                (ts * 1_000_000_000.0).round() as i64
            } else if let Some(fi) = frame_col.as_ref().and_then(|c| column_i64(c.as_ref(), row)) {
                if fps <= 0.0 {
                    return Err(IngestError::Parse {
                        format_id: "lerobot",
                        message: "no `timestamp` column and fps is unset; cannot derive timestamps"
                            .into(),
                    });
                }
                ((fi as f64) * 1_000_000_000.0 / fps).round() as i64
            } else {
                return Err(IngestError::Parse {
                    format_id: "lerobot",
                    message: format!("{}: no `timestamp` or `frame_index` column", path.display()),
                });
            };
            let task_index = task_col.as_ref().and_then(|c| column_i64(c.as_ref(), row));
            let feature_values: Vec<(String, Option<[u8; 32]>, Option<f64>)> = feature_cols
                .iter()
                .map(|(name, col)| (name.clone(), hash_cell(*col, row), first_scalar(*col, row)))
                .collect();
            rows.push((ep, ts_ns, task_index, feature_values));
        }
    }
    Ok(rows)
}

/// Load `meta/tasks.jsonl`, mapping each `task_index` to its natural-language task string. Absent or
/// unreadable file yields an empty map (tasks simply stay unresolved). Malformed lines are skipped.
fn load_tasks(dir: &Path) -> BTreeMap<i64, String> {
    #[derive(Deserialize)]
    struct TaskRow {
        task_index: i64,
        task: String,
    }
    let mut out = BTreeMap::new();
    let Ok(contents) = std::fs::read_to_string(dir.join("meta").join("tasks.jsonl")) else {
        return out;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<TaskRow>(line) {
            out.insert(row.task_index, row.task);
        }
    }
    out
}

impl Adapter for LeRobotAdapter {
    fn format_id(&self) -> &'static str {
        "lerobot"
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["3.0"]
    }

    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(dir) if LeRobotAdapter::info_path(dir).is_file() => {
                Detection::Yes { version: None }
            }
            _ => Detection::No,
        }
    }

    fn ingest(&self, source: &Source, _options: &IngestOptions) -> Result<Ingested, IngestError> {
        let dir = match source {
            Source::Local(p) => p,
            Source::Remote(_) => {
                return Err(IngestError::Parse {
                    format_id: "lerobot",
                    message: "remote LeRobot ingestion is not supported in v0.1".into(),
                })
            }
        };

        let info_bytes = std::fs::read(LeRobotAdapter::info_path(dir))
            .map_err(|e| IngestError::Io(e.to_string()))?;
        let info: InfoJson =
            serde_json::from_slice(&info_bytes).map_err(|e| IngestError::Parse {
                format_id: "lerobot",
                message: format!("meta/info.json: {e}"),
            })?;

        let fps = info.fps.unwrap_or(0.0);

        // Data features become streams; bookkeeping columns do not. Each carries its declared dtype
        // and shape from info.json so the structural checks can verify cross-episode consistency.
        let features: Vec<FeatureSpec> = info
            .features
            .iter()
            .filter(|(name, _)| !BOOKKEEPING.contains(&name.as_str()))
            .map(|(name, f)| {
                (
                    name.clone(),
                    infer_modality(name, f.dtype.as_deref()),
                    f.dtype.clone(),
                    f.shape.clone(),
                )
            })
            .collect();

        // Read every data Parquet, grouping row timestamps by episode.
        let mut parquet_files = Vec::new();
        find_parquet(&dir.join("data"), &mut parquet_files);

        // Per episode: rows in read order, each a (timestamp, feature-name -> content hash) pair.
        type RowContent = (i64, BTreeMap<String, Option<[u8; 32]>>);
        let mut episode_rows: BTreeMap<u64, Vec<RowContent>> = BTreeMap::new();
        // First `task_index` seen per episode (row order is deterministic: files are sorted). Most
        // episodes are single-task; if the task changes mid-episode we take the first, honestly.
        let mut episode_task_index: BTreeMap<u64, i64> = BTreeMap::new();
        // Dataset-level recomputed statistics per feature (LeRobot's stored stats are dataset-level).
        let mut observed: BTreeMap<String, StatsAccum> = BTreeMap::new();
        for path in &parquet_files {
            for (ep, ts, task_index, feature_values) in read_rows(path, fps)? {
                let mut hashes = BTreeMap::new();
                for (name, hash, scalar) in feature_values {
                    if let Some(v) = scalar {
                        if v.is_finite() {
                            observed.entry(name.clone()).or_default().push(v);
                        }
                    }
                    hashes.insert(name, hash);
                }
                episode_rows.entry(ep).or_default().push((ts, hashes));
                if let Some(ti) = task_index {
                    episode_task_index.entry(ep).or_insert(ti);
                }
            }
        }
        let observed_stats: BTreeMap<String, StreamStats> = observed
            .iter()
            .filter_map(|(name, acc)| acc.finish().map(|s| (name.clone(), s)))
            .collect();

        let stats = load_stats(dir);
        // Resolve `task_index` -> task string via meta/tasks.jsonl (empty map if the file is absent).
        let tasks = load_tasks(dir);

        // Build episodes: one stream per feature, frames at the episode's row timestamps, each frame
        // carrying that feature's per-row content hash (when the feature is a hashable Parquet column).
        let episodes: Vec<Episode> = episode_rows
            .into_iter()
            .map(|(index, rows)| {
                let start_ts = rows.iter().map(|(ts, _)| *ts).min();
                let end_ts = rows.iter().map(|(ts, _)| *ts).max();
                let streams = features
                    .iter()
                    .map(|(name, modality, dtype, shape)| Stream {
                        name: name.clone(),
                        modality: *modality,
                        declared_rate_hz: if fps > 0.0 { Some(fps) } else { None },
                        clock_id: CLOCK_ID.to_string(),
                        dtype: dtype.clone(),
                        shape: shape.clone(),
                        frames: rows
                            .iter()
                            .map(|(ts, hashes)| Frame {
                                ts: *ts,
                                value_ref: ValueRef {
                                    uri: name.clone(),
                                    byte_offset: None,
                                    byte_len: None,
                                    content_hash: hashes.get(name).copied().flatten(),
                                },
                            })
                            .collect(),
                        stats: stats.get(name).copied(),
                        observed_stats: observed_stats.get(name).copied(),
                    })
                    .collect();
                // Resolve this episode's task string, if its task_index maps to one.
                let task = episode_task_index
                    .get(&index)
                    .and_then(|ti| tasks.get(ti))
                    .cloned();
                Episode {
                    index,
                    start_ts,
                    end_ts,
                    streams,
                    task,
                    labels: vec![],
                }
            })
            .collect();

        // Provenance: extract what info.json actually records; never infer.
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some("lerobot".into()),
            class: ProvenanceClass::Known,
        }];
        if let Some(robot) = &info.robot_type {
            elements.push(ProvenanceElement {
                key: "sensor".into(),
                value: Some(robot.clone()),
                class: ProvenanceClass::Known,
            });
        }

        let dataset = Dataset {
            id: dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("lerobot")
                .to_string(),
            metadata: {
                let mut m = vec![
                    ("source_format".into(), "lerobot".into()),
                    (
                        "codebase_version".into(),
                        info.codebase_version.clone().unwrap_or_default(),
                    ),
                ];
                // Record the declared counts so checks can catch a truncated export.
                if let Some(total) = info.total_episodes {
                    m.push((
                        crate::cdm::META_DECLARED_EPISODES.to_string(),
                        total.to_string(),
                    ));
                }
                if let Some(total) = info.total_frames {
                    m.push((
                        crate::cdm::META_DECLARED_FRAMES.to_string(),
                        total.to_string(),
                    ));
                }
                m
            },
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements,
            }],
            episodes,
        };

        let mut mapped_fields = vec![
            "features -> streams".into(),
            "timestamp -> frame.ts".into(),
            "episode_index -> episode".into(),
            "fps -> stream.declared_rate_hz".into(),
            "feature.dtype -> stream.dtype".into(),
            "feature.shape -> stream.shape".into(),
            "robot_type -> provenance.sensor".into(),
            "meta/stats.json -> stream.stats".into(),
            "feature cell bytes -> frame.value_ref.content_hash (SHA-256)".into(),
        ];
        let mut omitted_fields =
            vec!["video frame decoding (frames are timestamps, not pixels)".into()];
        // Task-string resolution is reported honestly by whether meta/tasks.jsonl was present.
        if tasks.is_empty() {
            omitted_fields.push("task strings (no meta/tasks.jsonl to resolve task_index)".into());
        } else {
            mapped_fields.push("task_index + meta/tasks.jsonl -> episode.task".into());
        }
        if info.total_episodes.is_some() {
            mapped_fields.push("total_episodes -> declared episode-count check".into());
        }
        if info.total_frames.is_some() {
            mapped_fields.push("total_frames -> declared frame-count check".into());
        }

        let report = IngestReport {
            format_id: "lerobot",
            source_version: info.codebase_version.clone(),
            coverage: Coverage::Full,
            mapped_fields,
            unmapped_fields: vec![UnmappedField {
                source_path: "feature array values".into(),
                note:
                    "feature payloads are fingerprinted (hashed) into content_hash, never decoded \
                       or interpreted"
                        .into(),
            }],
            omitted_fields,
        };

        Ok(Ingested { dataset, report })
    }
}
