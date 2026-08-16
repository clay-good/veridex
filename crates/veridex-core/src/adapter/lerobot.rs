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
    Array, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, Int16Array, Int32Array,
    Int64Array, ListArray, UInt16Array, UInt32Array, UInt64Array,
};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cdm::{
    Dataset, DimStats, Episode, Frame, Label, Modality, Provenance, ProvenanceClass,
    ProvenanceElement, ProvenanceScope, Saturation, Stream, StreamStats, ValueRef,
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

/// The flat list of numbers in a JSON value: a scalar becomes a 1-element vec, a (possibly nested)
/// array flattens in order. `None` if it holds no numbers. Used to read LeRobot's per-dimension stat
/// arrays (`min: [a, b, c]`).
fn number_list(v: &serde_json::Value) -> Option<Vec<f64>> {
    match v {
        serde_json::Value::Number(n) => n.as_f64().map(|x| vec![x]),
        serde_json::Value::Array(a) => {
            let nums: Vec<f64> = a.iter().filter_map(number_list).flatten().collect();
            (!nums.is_empty()).then_some(nums)
        }
        _ => None,
    }
}

/// Stored statistics loaded from `meta/stats.json`: the element-0 summary per feature (for the
/// scalar checks) and, for multi-dimensional features, the full per-dimension breakdown.
#[derive(Default)]
struct StoredStats {
    scalar: BTreeMap<String, StreamStats>,
    per_dim: BTreeMap<String, Vec<DimStats>>,
}

/// Load per-feature stored statistics from `meta/stats.json`, if present. Missing or unparseable
/// stats are simply absent (never fabricated). Element-0 backs the scalar checks; the full
/// per-dimension arrays (when the feature is multi-DoF) back per-dimension stored-vs-observed.
fn load_stats(dir: &Path) -> StoredStats {
    let mut out = StoredStats::default();
    let Ok(bytes) = std::fs::read(dir.join("meta").join("stats.json")) else {
        return out;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        return out;
    };
    for (feature, stats) in map {
        let (Some(min), Some(max), Some(mean), Some(std)) = (
            stats.get("min").and_then(number_list),
            stats.get("max").and_then(number_list),
            stats.get("mean").and_then(number_list),
            stats.get("std").and_then(number_list),
        ) else {
            continue;
        };
        // Element 0 is the scalar summary the existing element-0 checks compare against.
        out.scalar.insert(
            feature.clone(),
            StreamStats {
                min: min[0],
                max: max[0],
                mean: mean[0],
                std: std[0],
            },
        );
        // A multi-DoF feature (all four arrays present and same length > 1) gets a per-dimension
        // breakdown; ragged or scalar stats stay element-0 only.
        let width = min.len();
        if width > 1
            && [max.len(), mean.len(), std.len()]
                .iter()
                .all(|&l| l == width)
        {
            let dims: Vec<DimStats> = (0..width)
                .map(|i| DimStats {
                    dim: i as u64,
                    stats: StreamStats {
                        min: min[i],
                        max: max[i],
                        mean: mean[i],
                        std: std[i],
                    },
                })
                .collect();
            out.per_dim.insert(feature, dims);
        }
    }
    out
}

/// Whether a declared LeRobot `codebase_version` is one this adapter supports. Normalizes an optional
/// leading `v` (LeRobot writes `"v3.0"`; the supported list is `"3.0"`) and matches on the major
/// version so a compatible minor revision (`v3.1`) is accepted while a different major (`v2.0`) is not.
fn version_supported(declared: &str, supported: &[&str]) -> bool {
    fn major(s: &str) -> &str {
        s.split('.').next().unwrap_or(s)
    }
    let norm = declared.trim().trim_start_matches(['v', 'V']);
    supported
        .iter()
        .any(|s| norm == *s || major(norm) == major(s))
}

/// Recursively collect `.parquet` files under `dir`, in a deterministic sorted order.
fn find_parquet(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        // Use `symlink_metadata` (does not follow) so a symlinked directory pointing at an ancestor
        // can't send this into unbounded recursion — Veridex's job is to survive malformed input.
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            find_parquet(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("parquet") {
            out.push(p);
        }
    }
}

fn column_i64(array: &dyn Array, row: usize) -> Option<i64> {
    // A null cell is absent data, not zero: `PrimitiveArray::value` ignores the validity bitmap and
    // would return a garbage `0`, silently fabricating episode/frame/task indices. Abstain instead.
    if array.is_null(row) {
        return None;
    }
    let any = array.as_any();
    // LeRobot v3 writes int64 index columns today, but accept the other integer widths a valid
    // export could use rather than falsely rejecting the dataset. An unsigned value above `i64::MAX`
    // can't be represented, so abstain on it (yields a clean parse error upstream) instead of wrapping.
    if let Some(a) = any.downcast_ref::<Int64Array>() {
        Some(a.value(row))
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        Some(a.value(row) as i64)
    } else if let Some(a) = any.downcast_ref::<Int16Array>() {
        Some(a.value(row) as i64)
    } else if let Some(a) = any.downcast_ref::<UInt64Array>() {
        i64::try_from(a.value(row)).ok()
    } else if let Some(a) = any.downcast_ref::<UInt32Array>() {
        Some(a.value(row) as i64)
    } else {
        any.downcast_ref::<UInt16Array>()
            .map(|a| a.value(row) as i64)
    }
}

fn column_f64(array: &dyn Array, row: usize) -> Option<f64> {
    // See `column_i64`: a null cell reads as a bogus `0.0` unless we check the bitmap first, which
    // would fabricate a `ts = 0` mid-stream and bypass the frame_index/fps fallback.
    if array.is_null(row) {
        return None;
    }
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

/// Collect **every leaf scalar** of one feature cell, in dimension order. Element `i` of `out` is
/// dimension `i` of the cell: a scalar column yields one value, a `FixedSizeList`/`List` yields one
/// per element. This is the multi-DoF generalization that lets the recompute see past element 0 — a
/// saturating gripper or a NaN buried in joint 6 is invisible to a first-scalar-only read.
///
/// A null **leaf** yields `None` — absent data that still holds its dimension slot, so a dropout in
/// joint 1 does not shift joints 2..N down a dimension and corrupt their per-dimension stats. (An
/// entirely-null container cell contributes no dimensions, exactly as a missing feature would.)
fn cell_scalars(array: &dyn Array, row: usize, out: &mut Vec<Option<f64>>) {
    let any = array.as_any();
    if let Some(a) = any.downcast_ref::<Float32Array>() {
        out.push((!a.is_null(row)).then(|| a.value(row) as f64));
    } else if let Some(a) = any.downcast_ref::<Float64Array>() {
        out.push((!a.is_null(row)).then(|| a.value(row)));
    } else if let Some(a) = any.downcast_ref::<Int64Array>() {
        out.push((!a.is_null(row)).then(|| a.value(row) as f64));
    } else if let Some(a) = any.downcast_ref::<Int32Array>() {
        out.push((!a.is_null(row)).then(|| a.value(row) as f64));
    } else if let Some(a) = any.downcast_ref::<BooleanArray>() {
        out.push((!a.is_null(row)).then(|| a.value(row) as u8 as f64));
    } else if let Some(a) = any.downcast_ref::<FixedSizeListArray>() {
        if a.is_null(row) {
            return;
        }
        let child = a.value(row);
        for i in 0..child.len() {
            cell_scalars(child.as_ref(), i, out);
        }
    } else if let Some(a) = any.downcast_ref::<ListArray>() {
        if a.is_null(row) {
            return;
        }
        let child = a.value(row);
        for i in 0..child.len() {
            cell_scalars(child.as_ref(), i, out);
        }
    }
    // Any other type contributes no scalars.
}

/// Per-feature recomputed accumulators: one [`StatsAccum`] per dimension, plus a non-finite tally
/// pooled across all dimensions. Dimension 0 backs `observed_stats` (mirroring the element-0 stored
/// `stats.json` that `STATISTICAL.STATS_STALE` validates); saturation is judged across every dimension.
#[derive(Default, Clone)]
struct FeatureAccum {
    dims: Vec<StatsAccum>,
    /// NaN / ±inf scalars seen across all dimensions (kept out of the per-dimension stats).
    non_finite: u64,
}

impl FeatureAccum {
    /// Feed one cell's dimension-ordered scalars: finite values grow their dimension's accumulator;
    /// non-finite values are tallied and held out (a NaN would poison the summary). A `None` leaf is
    /// absent data — it is skipped, but its position is still consumed so later dimensions stay aligned.
    fn push_cell(&mut self, scalars: &[Option<f64>]) {
        for (dim, v) in scalars.iter().enumerate() {
            let Some(v) = *v else { continue };
            if v.is_finite() {
                if dim >= self.dims.len() {
                    self.dims.resize(dim + 1, StatsAccum::default());
                }
                self.dims[dim].push(v);
            } else {
                self.non_finite += 1;
            }
        }
    }

    /// Element-0 stats, for stored-vs-observed comparison against the element-0 stored stats.
    fn stats(&self) -> Option<StreamStats> {
        self.dims.first().and_then(StatsAccum::finish)
    }

    /// The saturation summary of the dimension most likely to be flagged — the non-constant dimension
    /// with the highest pinned fraction — so a saturating gripper at element 6 is caught, not just
    /// element 0. Falls back to dimension 0 (constant → the check skips it, DEGENERATE's concern), so
    /// a scalar stream reports exactly as before.
    fn saturation(&self) -> Option<Saturation> {
        let frac = |s: &Saturation| {
            let n = s.sample_count as f64;
            if n == 0.0 {
                0.0
            } else {
                s.at_min.max(s.at_max) as f64 / n
            }
        };
        // Tag each dimension's summary with its own index so the winning dimension is named.
        let with_dim = |(i, a): (usize, &StatsAccum)| {
            a.finish_saturation().map(|mut s| {
                s.dim = i as u64;
                s
            })
        };
        self.dims
            .iter()
            .enumerate()
            .filter_map(with_dim)
            .filter(|s| s.min != s.max)
            .max_by(|a, b| {
                frac(a)
                    .partial_cmp(&frac(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or_else(|| self.dims.iter().enumerate().next().and_then(with_dim))
    }
}

/// Running accumulator for one dimension's recomputed statistics. Mean and variance use Welford's
/// online algorithm rather than accumulating `sum`/`sum_sq`: real robot signals often ride a large DC
/// offset (encoder counts ~1e6 with sub-unit variance), where `E[x²]−E[x]²` suffers catastrophic
/// cancellation and can clamp a real variance to 0 (a spurious `DEGENERATE`). Welford is single-pass
/// and numerically stable.
#[derive(Default, Clone, Copy)]
struct StatsAccum {
    count: u64,
    min: f64,
    max: f64,
    /// Running mean (Welford).
    mean: f64,
    /// Running sum of squared deviations from the mean (Welford's M2); population variance is `m2/n`.
    m2: f64,
    /// Values seen exactly equal to the running `min` / `max`. Reset to 1 whenever a new extreme
    /// appears, so at the end they count values equal to the *final* min/max in a single pass —
    /// no need to retain the values (mirrors the streaming stats above).
    at_min: u64,
    at_max: u64,
}

impl StatsAccum {
    fn push(&mut self, v: f64) {
        if self.count == 0 {
            self.min = v;
            self.max = v;
            self.at_min = 1;
            self.at_max = 1;
        } else {
            if v < self.min {
                self.min = v;
                self.at_min = 1;
            } else if v == self.min {
                self.at_min += 1;
            }
            if v > self.max {
                self.max = v;
                self.at_max = 1;
            } else if v == self.max {
                self.at_max += 1;
            }
        }
        self.count += 1;
        // Welford update.
        let delta = v - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = v - self.mean;
        self.m2 += delta * delta2;
    }

    /// Finalize to a [`StreamStats`] (population std), or `None` if nothing was accumulated.
    fn finish(&self) -> Option<StreamStats> {
        if self.count == 0 {
            return None;
        }
        let variance = (self.m2 / self.count as f64).max(0.0);
        Some(StreamStats {
            min: self.min,
            max: self.max,
            mean: self.mean,
            std: variance.sqrt(),
        })
    }

    /// Finalize the pinned-at-extreme counts, or `None` if nothing was accumulated.
    fn finish_saturation(&self) -> Option<Saturation> {
        if self.count == 0 {
            return None;
        }
        Some(Saturation {
            sample_count: self.count,
            at_min: self.at_min,
            at_max: self.at_max,
            min: self.min,
            max: self.max,
            dim: 0, // set by FeatureAccum::saturation, which knows the dimension index
        })
    }
}

/// One feature's per-row extraction: `(name, content hash — `None` if the type isn't hashable, the
/// cell's dimension-ordered scalars — empty if not numeric, `None` per absent leaf)`. The scalars
/// drive the per-dimension statistics recompute (see [`FeatureAccum`]).
type FeatureValue = (String, Option<[u8; 32]>, Vec<Option<f64>>);

type Row = (u64, i64, Option<i64>, Vec<FeatureValue>);

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
            let feature_values: Vec<FeatureValue> = feature_cols
                .iter()
                .map(|(name, col)| {
                    let mut scalars = Vec::new();
                    cell_scalars(*col, row, &mut scalars);
                    (name.clone(), hash_cell(*col, row), scalars)
                })
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

/// Load `meta/episodes.jsonl`, mapping each `episode_index` to the frame count (`length`) the
/// manifest declares for it. This is the metadata whose corruption is the lerobot#4143 class: when a
/// per-episode length is wrong, LeRobot's cumulative boundaries misplace frames into the wrong
/// episode. The structural check compares this declared length against the frames actually ingested.
/// Absent or unreadable file yields an empty map (the check simply has nothing to compare); malformed
/// lines are skipped.
fn load_episode_lengths(dir: &Path) -> BTreeMap<u64, u64> {
    #[derive(Deserialize)]
    struct EpisodeRow {
        episode_index: u64,
        length: u64,
    }
    let mut out = BTreeMap::new();
    let Ok(contents) = std::fs::read_to_string(dir.join("meta").join("episodes.jsonl")) else {
        return out;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<EpisodeRow>(line) {
            out.insert(row.episode_index, row.length);
        }
    }
    out
}

/// Read the SPDX license from a Hugging Face dataset card's YAML frontmatter (`README.md`), the place
/// LeRobot datasets actually record it (`meta/info.json` carries none). Only the leading `---`-fenced
/// block is inspected, and only the `license:` key — either a scalar (`license: apache-2.0`) or the
/// first item of a YAML list. Returns `None` when there is no card, no frontmatter, or no license, so
/// a missing license stays honestly missing (the completeness check then reports it). This is a
/// deliberately minimal parse — no YAML dependency for one well-known field.
fn load_card_license(dir: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(dir.join("README.md")).ok()?;
    let mut lines = contents.lines();
    // Frontmatter must be the very first non-empty content and open with a `---` fence.
    if lines.by_ref().find(|l| !l.trim().is_empty())?.trim() != "---" {
        return None;
    }
    let mut in_license_list = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break; // end of frontmatter
        }
        if in_license_list {
            // First item of a `license:`-introduced YAML list.
            if let Some(item) = trimmed.strip_prefix('-') {
                let v = clean_yaml_scalar(item);
                if !v.is_empty() {
                    return Some(v);
                }
            }
            // A non-item line ends the list without a value.
            in_license_list = false;
        }
        if let Some(rest) = trimmed.strip_prefix("license:") {
            let v = clean_yaml_scalar(rest);
            if v.is_empty() {
                in_license_list = true; // list form: value(s) on following lines
            } else {
                return Some(v);
            }
        }
    }
    None
}

/// Trim a minimal YAML scalar: surrounding whitespace, a trailing `# comment`, and matching quotes.
fn clean_yaml_scalar(s: &str) -> String {
    let mut v = s.trim();
    if let Some(hash) = v.find(" #") {
        v = v[..hash].trim();
    }
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v = &v[1..v.len() - 1];
    }
    v.to_string()
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

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
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

        // Reject a recognized-but-unsupported layout cleanly rather than misparsing it as v3 (a v2.x
        // export still has a `meta/info.json`, so `detect` matches it). Only reject when the manifest
        // actually declares a version we don't support; an absent version is abstained on, not failed.
        if let Some(declared) = info.codebase_version.as_deref() {
            if !version_supported(declared, self.supported_versions()) {
                return Err(IngestError::UnsupportedVersion {
                    format_id: "lerobot",
                    version: Some(declared.to_string()),
                    supported: self.supported_versions(),
                });
            }
        }

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
        // First `task_index` seen per episode (row order is deterministic: files are sorted) — this
        // is the episode's primary task string.
        let mut episode_task_index: BTreeMap<u64, i64> = BTreeMap::new();
        // Task *transitions* within an episode: the (timestamp, task_index) at each row whose task
        // differs from the previous row's — a mid-episode instruction change. These become timestamped
        // `language` annotations the semantic checks verify. Single-task episodes produce none, so a
        // dataset without mid-episode task changes is unaffected.
        let mut episode_task_events: BTreeMap<u64, Vec<(i64, i64)>> = BTreeMap::new();
        let mut last_task_index: BTreeMap<u64, i64> = BTreeMap::new();
        // Every row becomes one frame per declared feature, and `info.json` declares the features
        // independently of what the Parquet actually holds — so a few-KB manifest naming 50k features
        // multiplies against every row. Charge the budget as rows arrive, before the frames are built.
        let mut budget = super::FrameBudget::new(options);
        let per_row = features.len().max(1) as u64;
        // Dataset-level recomputed statistics per feature (LeRobot's stored stats are dataset-level).
        let mut observed: BTreeMap<String, FeatureAccum> = BTreeMap::new();
        for path in &parquet_files {
            for (ep, ts, task_index, feature_values) in read_rows(path, fps)? {
                budget.take("lerobot", per_row)?;
                let mut hashes = BTreeMap::new();
                for (name, hash, scalars) in feature_values {
                    // Feed every dimension of the cell: finite values grow their dimension's stats,
                    // non-finite values (a NaN in any joint) are tallied out. `or_default` keeps the
                    // entry alive even for an all-null/non-numeric cell so "values were read" holds.
                    observed
                        .entry(name.clone())
                        .or_default()
                        .push_cell(&scalars);
                    hashes.insert(name, hash);
                }
                episode_rows.entry(ep).or_default().push((ts, hashes));
                if let Some(ti) = task_index {
                    episode_task_index.entry(ep).or_insert(ti);
                    // Record a transition when this row's task differs from the episode's previous row.
                    match last_task_index.insert(ep, ti) {
                        Some(prev) if prev != ti => {
                            episode_task_events.entry(ep).or_default().push((ts, ti));
                        }
                        _ => {}
                    }
                }
            }
        }
        let observed_stats: BTreeMap<String, StreamStats> = observed
            .iter()
            .filter_map(|(name, acc)| acc.stats().map(|s| (name.clone(), s)))
            .collect();
        let observed_saturation: BTreeMap<String, Saturation> = observed
            .iter()
            .filter_map(|(name, acc)| acc.saturation().map(|s| (name.clone(), s)))
            .collect();
        // Every feature whose scalars were read gets a non-finite count (0 when all were finite), so
        // the check can distinguish "clean data" from "values never read" (a `None`).
        let observed_non_finite: BTreeMap<String, u64> = observed
            .iter()
            .map(|(name, acc)| (name.clone(), acc.non_finite))
            .collect();
        // Per-dimension stats, only for multi-DoF features — the extreme-outlier check scans them so a
        // spike in a non-first joint is caught. Scalar features carry their one dimension in
        // `observed_stats` already, so they get `None` here (no duplication).
        let observed_dim_stats: BTreeMap<String, Vec<DimStats>> = observed
            .iter()
            .filter(|(_, acc)| acc.dims.len() > 1)
            .filter_map(|(name, acc)| {
                let dims: Vec<DimStats> = acc
                    .dims
                    .iter()
                    .enumerate()
                    .filter_map(|(i, a)| {
                        a.finish().map(|stats| DimStats {
                            dim: i as u64,
                            stats,
                        })
                    })
                    .collect();
                (!dims.is_empty()).then(|| (name.clone(), dims))
            })
            .collect();

        let stats = load_stats(dir);
        // Resolve `task_index` -> task string via meta/tasks.jsonl (empty map if the file is absent).
        let tasks = load_tasks(dir);
        // Declared per-episode frame counts from meta/episodes.jsonl (empty if absent) — the manifest
        // assertion the boundary check tests against the frames actually ingested (lerobot#4143).
        let declared_lengths = load_episode_lengths(dir);

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
                        stats: stats.scalar.get(name).copied(),
                        dim_stats: stats.per_dim.get(name).cloned(),
                        observed_stats: observed_stats.get(name).copied(),
                        observed_saturation: observed_saturation.get(name).copied(),
                        observed_non_finite: observed_non_finite.get(name).copied(),
                        observed_dim_stats: observed_dim_stats.get(name).cloned(),
                        // LeRobot is a manipulation format: no point-cloud streams.
                        point_fields: None,
                    })
                    .collect();
                // Resolve this episode's task string, if its task_index maps to one.
                let task = episode_task_index
                    .get(&index)
                    .and_then(|ti| tasks.get(ti))
                    .cloned();
                // Surface each mid-episode task change as a timestamped `language` annotation. Only
                // resolvable task indices become labels (Veridex never invents an instruction from a
                // bare index); the semantic checks then verify their integrity.
                let labels = episode_task_events
                    .get(&index)
                    .into_iter()
                    .flatten()
                    .filter_map(|(ts, ti)| {
                        tasks.get(ti).map(|value| Label {
                            key: "language".into(),
                            value: value.clone(),
                            ts: Some(*ts),
                        })
                    })
                    .collect();
                Episode {
                    index,
                    start_ts,
                    end_ts,
                    streams,
                    task,
                    labels,
                    // LeRobot carries no ego-vehicle trajectory.
                    ego_poses: None,
                    declared_frame_count: declared_lengths.get(&index).copied(),
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
        // The dataset card (README.md frontmatter) is where LeRobot records the license.
        let card_license = load_card_license(dir);
        if let Some(license) = &card_license {
            elements.push(ProvenanceElement {
                key: "license".into(),
                value: Some(license.clone()),
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
            // LeRobot is a manipulation format with no sensor-rig calibration.
            calibration: None,
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
        if card_license.is_some() {
            mapped_fields.push("README.md license -> provenance.license".into());
        }
        if info.total_episodes.is_some() {
            mapped_fields.push("total_episodes -> declared episode-count check".into());
        }
        if info.total_frames.is_some() {
            mapped_fields.push("total_frames -> declared frame-count check".into());
        }
        if !declared_lengths.is_empty() {
            mapped_fields.push("meta/episodes.jsonl length -> episode.declared_frame_count".into());
        }

        // Reconcile the Parquet columns actually present against the info.json feature declarations so
        // neither direction is dropped silently (the fidelity requirement). `observed` is keyed by the
        // real non-bookkeeping Parquet columns; `declared_names` is what info.json declared. Both loops
        // iterate sorted collections so the report is deterministic.
        let declared_names: std::collections::BTreeSet<&str> =
            features.iter().map(|(n, ..)| n.as_str()).collect();
        let mut unmapped_fields = vec![UnmappedField {
            source_path: "feature array values".into(),
            note: "feature payloads are fingerprinted (hashed) into content_hash, never decoded \
                   or interpreted"
                .into(),
        }];
        for col in observed.keys() {
            if !declared_names.contains(col.as_str()) {
                unmapped_fields.push(UnmappedField {
                    source_path: format!("data column `{col}`"),
                    note: "present in the Parquet data but not declared in meta/info.json \
                           features; not represented as a stream"
                        .into(),
                });
            }
        }
        for name in &declared_names {
            if !observed.contains_key(*name) {
                omitted_fields.push(format!(
                    "feature `{name}` declared in meta/info.json but absent from the Parquet data \
                     (no values ingested)"
                ));
            }
        }

        let report = IngestReport {
            format_id: "lerobot",
            source_version: info.codebase_version.clone(),
            coverage: Coverage::Full,
            mapped_fields,
            unmapped_fields,
            omitted_fields,
        };

        Ok(Ingested { dataset, report })
    }
}
