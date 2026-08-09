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
//! Veridex reads timestamps and structure, not feature payloads, so the feature array columns are
//! never decoded — only `episode_index` and `timestamp` are read from the Parquet.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::array::{Array, Float32Array, Float64Array, Int32Array, Int64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;

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

/// One row of a LeRobot data Parquet: its episode index, frame timestamp (ns), and `task_index`
/// if the column is present (unresolved to a string here — see [`load_tasks`]).
type Row = (u64, i64, Option<i64>);

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
            rows.push((ep, ts_ns, task_index));
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

        let mut episode_ts: BTreeMap<u64, Vec<i64>> = BTreeMap::new();
        // First `task_index` seen per episode (row order is deterministic: files are sorted). Most
        // episodes are single-task; if the task changes mid-episode we take the first, honestly.
        let mut episode_task_index: BTreeMap<u64, i64> = BTreeMap::new();
        for path in &parquet_files {
            for (ep, ts, task_index) in read_rows(path, fps)? {
                episode_ts.entry(ep).or_default().push(ts);
                if let Some(ti) = task_index {
                    episode_task_index.entry(ep).or_insert(ti);
                }
            }
        }

        let stats = load_stats(dir);
        // Resolve `task_index` -> task string via meta/tasks.jsonl (empty map if the file is absent).
        let tasks = load_tasks(dir);

        // Build episodes: one stream per feature, frames at the episode's row timestamps.
        let episodes: Vec<Episode> = episode_ts
            .into_iter()
            .map(|(index, timestamps)| {
                let start_ts = timestamps.iter().copied().min();
                let end_ts = timestamps.iter().copied().max();
                let streams = features
                    .iter()
                    .map(|(name, modality, dtype, shape)| Stream {
                        name: name.clone(),
                        modality: *modality,
                        declared_rate_hz: if fps > 0.0 { Some(fps) } else { None },
                        clock_id: CLOCK_ID.to_string(),
                        dtype: dtype.clone(),
                        shape: shape.clone(),
                        frames: timestamps
                            .iter()
                            .map(|ts| Frame {
                                ts: *ts,
                                value_ref: ValueRef {
                                    uri: name.clone(),
                                    byte_offset: None,
                                    byte_len: None,
                                    content_hash: None,
                                },
                            })
                            .collect(),
                        stats: stats.get(name).copied(),
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
            metadata: vec![
                ("source_format".into(), "lerobot".into()),
                (
                    "codebase_version".into(),
                    info.codebase_version.clone().unwrap_or_default(),
                ),
            ],
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
        ];
        let mut omitted_fields =
            vec!["video frame decoding (frames are timestamps, not pixels)".into()];
        // Task-string resolution is reported honestly by whether meta/tasks.jsonl was present.
        if tasks.is_empty() {
            omitted_fields.push("task strings (no meta/tasks.jsonl to resolve task_index)".into());
        } else {
            mapped_fields.push("task_index + meta/tasks.jsonl -> episode.task".into());
        }

        let report = IngestReport {
            format_id: "lerobot",
            source_version: info.codebase_version.clone(),
            coverage: Coverage::Full,
            mapped_fields,
            unmapped_fields: vec![UnmappedField {
                source_path: "feature array values".into(),
                note: "Veridex reads timestamps and structure, not feature payloads".into(),
            }],
            omitted_fields,
        };

        Ok(Ingested { dataset, report })
    }
}
