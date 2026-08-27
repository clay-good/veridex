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

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use arrow::array::{
    Array, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, Int16Array, Int32Array,
    Int64Array, ListArray, UInt16Array, UInt32Array, UInt64Array,
};
use arrow::datatypes::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, DimStats, Episode, Frame, Label, Media, MediaParams, MediaStatus, Modality,
    Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope, Saturation, Stream,
    StreamStats, ValueRef,
};

use super::stats::FeatureAccum;
use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Sample,
    Source, UnmappedField,
};

const CLOCK_ID: &str = "lerobot";

/// Ceiling on the episode set a sampled ingest will derive from `info.json`'s declared
/// `total_episodes` alone.
///
/// That number is a handful of bytes in a manifest, and the index set it implies is materialized
/// before either ingest budget is constructed — so nothing else bounds it. Chosen far above any real
/// dataset (LeRobot datasets run to thousands of episodes) so it only ever refuses a manifest that is
/// lying, and even then points at the fix: `meta/episodes.jsonl`, whose cost is bounded by its size.
const MAX_DECLARED_EPISODES_FOR_SAMPLING: u64 = 1_000_000;

/// Seconds to nanoseconds, saturating at the `i64` bounds rather than wrapping.
///
/// A `timestamp` cell is a number from an untrusted file. `as i64` on a value past the range is
/// saturating in Rust, but the multiply can still overflow to infinity first, and a NaN would cast to
/// `0` — a fabricated "start of recording" that reads as an ordinary timestamp. Non-finite inputs are
/// filtered out by the caller (the row contributes no frame); this handles the merely enormous.
/// Mirrors `mdf4::seconds_to_ns`, which had this guard first.
fn seconds_to_ns(seconds: f64) -> i64 {
    (seconds * 1_000_000_000.0)
        .round()
        .clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

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
    /// LeRobot records a video feature's encoding here, as flat `video.*` keys
    /// (`video.codec`, `video.fps`, `video.height`, `video.width`, …).
    #[serde(default)]
    info: Option<BTreeMap<String, serde_json::Value>>,
    /// The name of each axis of `shape` (e.g. `["height", "width", "channel"]`). The manifest states
    /// its own dimension order here, so a channel-first feature is not read as channel-last.
    #[serde(default)]
    names: Option<Vec<String>>,
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
    /// Why the maps are as full or as empty as they are. The ingest report claimed
    /// `meta/stats.json -> stream.stats` unconditionally, so a dataset with no stats file at all —
    /// or a corrupt one, which silently disables every stored-vs-observed comparison — was told it
    /// had been read. The fidelity report is the artifact whose whole job is disclosing what the
    /// run did and did not cover; an unearned claim in it is worse than a missing one.
    source: StatsSource,
}

/// What `meta/stats.json` turned out to be.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum StatsSource {
    /// Parsed, and at least one feature yielded a usable summary.
    Read,
    /// No `meta/stats.json` on disk.
    #[default]
    Absent,
    /// The file exists but is not a JSON object.
    Unparseable,
}

/// Load per-feature stored statistics from `meta/stats.json`, if present. Missing or unparseable
/// stats are simply absent (never fabricated). Element-0 backs the scalar checks; the full
/// per-dimension arrays (when the feature is multi-DoF) back per-dimension stored-vs-observed.
fn load_stats(dir: &Path) -> StoredStats {
    let mut out = StoredStats::default();
    let Ok(bytes) = std::fs::read(dir.join("meta").join("stats.json")) else {
        out.source = StatsSource::Absent;
        return out;
    };
    let Ok(serde_json::Value::Object(map)) = serde_json::from_slice::<serde_json::Value>(&bytes)
    else {
        out.source = StatsSource::Unparseable;
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
    // A file that parsed but named no feature Veridex could summarize is, for every purpose
    // downstream, the same as one that would not parse: nothing to compare against.
    out.source = if out.scalar.is_empty() {
        StatsSource::Unparseable
    } else {
        StatsSource::Read
    };
    out
}

/// What the ingest report should say about `meta/stats.json`, as `(mapped, omitted)`.
fn stats_fidelity(stats: &StoredStats) -> (Option<String>, Option<String>) {
    match stats.source {
        StatsSource::Read => (Some("meta/stats.json -> stream.stats".into()), None),
        StatsSource::Absent => (
            None,
            Some(
                "stored summary statistics (no meta/stats.json; the stored-vs-observed checks \
                 have nothing to compare against)"
                    .into(),
            ),
        ),
        StatsSource::Unparseable => (
            None,
            Some(
                "stored summary statistics (meta/stats.json could not be read as per-feature \
                 min/max/mean/std; the stored-vs-observed checks have nothing to compare against)"
                    .into(),
            ),
        ),
    }
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
///
/// `escaped` collects shards that resolve outside `dataset_root`, so the caller can disclose them
/// rather than let the dataset merely look empty.
///
/// The walk follows symlinked files because that is the shape `snapshot_download` writes, and the
/// component filter in [`path_is_inside`] cannot see through one — so containment is re-checked
/// after resolution, here, on the way in. Without it a published dataset could ship
/// `data/chunk-000/file-000.parquet -> /home/victim/payroll.parquet` and anyone who ran `veridex
/// check` on it read that file: its columns' min/max/mean/std and a SHA-256 per cell went into the
/// CDM, which is content-hashed, printed, and **signed into a certificate** the victim might then
/// hand to someone else. `path_is_inside` already guarded exactly this for media (`probe_stream_media`
/// was its only caller); the data walk was documented as "inside by construction", which following
/// symlinks had already made untrue.
fn find_parquet(
    dataset_root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
    escaped: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // Numeric order, not lexicographic: rows land in their episode in the order the shards are read,
    // so `file-10.parquet` ahead of `file-1.parquet` puts a sound dataset's frames out of order and
    // reports it as `TEMPORAL.NON_MONOTONIC`. LeRobot's own exporter zero-pads, which hides this
    // until a re-export or a conversion script does not. See `super::natural_key`.
    paths.sort_by_key(|p| super::natural_key(&p.to_string_lossy()));
    for p in paths {
        let Some(kind) = walk_entry(&p) else { continue };
        if kind == EntryKind::Dir {
            find_parquet(dataset_root, &p, out, escaped);
        } else if p.extension().and_then(|e| e.to_str()) == Some("parquet") {
            if path_is_inside(dataset_root, &p) {
                out.push(p);
            } else {
                escaped.push(p);
            }
        }
    }
}

/// What a directory entry is, for the two dataset walks — or `None` for one they must not descend.
#[derive(PartialEq, Eq, Clone, Copy)]
enum EntryKind {
    Dir,
    File,
}

/// Classify one entry, following a symlink to a **file** but never to a directory.
///
/// `huggingface_hub`'s `snapshot_download` materializes every file in a repo as a symlink into the
/// blob cache, so a symlinked `.parquet` or `.mp4` is the ordinary on-disk shape of a downloaded
/// LeRobot dataset, not a hostile one. Refusing them read a normal download as zero episodes at
/// `Coverage::Full`, and reported every present video as `VIDEO.MEDIA_MISSING`.
///
/// A symlinked *directory* stays refused: one pointing at an ancestor sends the walk into unbounded
/// recursion, and surviving malformed input is the point. Following a file cannot recurse.
fn walk_entry(path: &Path) -> Option<EntryKind> {
    // `symlink_metadata` first, so the link itself is what decides — a link to a directory is
    // refused on the strength of its target's kind, without ever descending it.
    let link = std::fs::symlink_metadata(path).ok()?;
    if !link.file_type().is_symlink() {
        return if link.is_dir() {
            Some(EntryKind::Dir)
        } else if link.is_file() {
            Some(EntryKind::File)
        } else {
            // A fifo, socket, or device is not dataset content; opening one can block forever.
            None
        };
    }
    // A broken link resolves to nothing and is skipped.
    let target = std::fs::metadata(path).ok()?;
    target.is_file().then_some(EntryKind::File)
}

/// Container extensions Veridex knows how to read headers from (all ISO base media format).
const MEDIA_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov"];

/// Where a video feature's media files live, resolved from the `videos/` tree.
#[derive(Default)]
struct VideoIndex {
    /// feature key → episode index → media file path. Populated only for the one-file-per-episode
    /// layout, which is the only one from which a file can be attributed to an episode.
    per_episode: BTreeMap<String, BTreeMap<u64, PathBuf>>,
    /// Feature keys whose media exists but is not laid out one file per episode (LeRobot v3 may
    /// concatenate many episodes into one file). Reported as unmapped, never guessed at: attributing
    /// a shared file's frames to one episode would invent the very number the checks compare.
    unresolvable: BTreeSet<String>,
}

/// Index the `videos/` tree for the declared video features.
///
/// A file belongs to feature `k` when `k` is one of its path components under `videos/` — the
/// layout LeRobot writes, where the dotted feature key is a directory name — and to episode `n` when
/// its file stem is `episode_<n>`. Anything else is recorded as unresolvable rather than matched
/// loosely.
fn index_videos(dir: &Path, video_features: &BTreeSet<&str>) -> VideoIndex {
    let mut index = VideoIndex::default();
    if video_features.is_empty() {
        return index;
    }
    let root = dir.join("videos");
    let mut files = Vec::new();
    find_media(&root, &mut files);
    for path in files {
        let Ok(relative) = path.strip_prefix(&root) else {
            continue;
        };
        let Some(key) = relative
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .find(|c| video_features.contains(c))
        else {
            continue;
        };
        match path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("episode_"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            Some(episode) => {
                index
                    .per_episode
                    .entry(key.to_string())
                    .or_default()
                    .insert(episode, path);
            }
            None => {
                index.unresolvable.insert(key.to_string());
            }
        }
    }
    index
}

/// Recursively collect media files under `dir`, in a deterministic sorted order. Mirrors
/// [`find_parquet`], including its symlink handling.
fn find_media(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort_by_key(|p| super::natural_key(&p.to_string_lossy()));
    for p in paths {
        let Some(kind) = walk_entry(&p) else { continue };
        if kind == EntryKind::Dir {
            find_media(&p, out);
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.as_str()))
        {
            out.push(p);
        }
    }
}

/// The encoding a feature's `info` block declares, falling back to the feature's `shape` for
/// resolution — LeRobot writes a video feature's shape as `[height, width, channels]`, so a manifest
/// that omits `video.width`/`video.height` still states the resolution, once.
fn declared_media_params(
    info: Option<&BTreeMap<String, serde_json::Value>>,
    shape: Option<&[u64]>,
    names: Option<&[String]>,
) -> MediaParams {
    let get = |k: &str| info.and_then(|m| m.get(k));
    // A pixel count is a positive whole number. `as u64` saturates, so a corrupt `-1` would become
    // `0` and be compared as a real resolution; a non-positive or non-integral value is no
    // declaration at all.
    let pixels = |k: &str| {
        get(k)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n >= 1.0 && n.fract() == 0.0)
            .map(|n| n as u64)
    };
    let from_shape = |axis: &str| axis_from_shape(shape, names, axis);
    MediaParams {
        codec: get("video.codec")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        width: pixels("video.width").or_else(|| from_shape("width")),
        height: pixels("video.height").or_else(|| from_shape("height")),
        fps: get("video.fps").and_then(|v| v.as_f64()),
    }
}

/// The `height` or `width` dimension of a feature's declared `shape`, or `None` when the manifest
/// does not make the axis order plain.
///
/// The manifest's own `names` decide it when present — LeRobot writes `["height", "width",
/// "channel"]`, and channel-first datasets exist. Without `names`, the order is only assumed when
/// the shape is unambiguously channel-last (a trailing dimension of 1, 3, or 4 is a channel count,
/// never a frame dimension). Anything else yields nothing rather than a guess: a fabricated
/// "declared 480x3" is worse than no comparison at all.
fn axis_from_shape(shape: Option<&[u64]>, names: Option<&[String]>, axis: &str) -> Option<u64> {
    let shape = shape?;
    if let Some(names) = names {
        if names.len() == shape.len() {
            let idx = names
                .iter()
                .position(|n| n.trim().to_ascii_lowercase().starts_with(axis))?;
            return shape.get(idx).copied();
        }
    }
    let channels_last = match shape.len() {
        2 => true,
        3 => matches!(shape[2], 1 | 3 | 4),
        _ => false,
    };
    if !channels_last {
        return None;
    }
    match axis {
        "height" => shape.first().copied(),
        "width" => shape.get(1).copied(),
        _ => None,
    }
}

/// Where an absent episode's media file would have been, modelled on the files that *are* there.
///
/// A missing-file finding is only actionable if it names the path the dataset would really use, and
/// that path is not guessable: LeRobot nests videos under chunk directories, the zero-padding varies,
/// and the extension may be any of [`MEDIA_EXTENSIONS`]. So it is copied from a sibling episode of
/// the same feature — the file next to the one that is missing — rather than invented. `by_episode`
/// is non-empty by construction (the feature is in per-episode layout because a file resolved), but
/// the fallback stays total rather than relying on that.
fn expected_sibling_path(by_episode: &BTreeMap<u64, PathBuf>, episode: u64) -> PathBuf {
    let Some((_, sibling)) = by_episode.iter().next() else {
        return PathBuf::from(format!("episode_{episode:06}.mp4"));
    };
    let padding = sibling
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_prefix("episode_"))
        .map_or(6, |digits| digits.len());
    let extension = sibling
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp4");
    let name = format!("episode_{episode:0padding$}.{extension}");
    match sibling.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

/// Read the media file backing one episode of one video stream, and record what it holds.
///
/// A file the index resolved but that is gone from disk between indexing and reading is
/// [`MediaStatus::Missing`], the same as one that was never there — either way the dataset claims
/// imagery it cannot produce.
/// Whether `expected` really names a file inside the dataset, so Veridex may open it.
///
/// The per-episode paths come from walking `videos/`, so they are inside by construction. The two
/// *fabricated* paths are not. `expected_sibling_path` builds a name, and the no-file-resolved
/// fallback joins a **feature key read from `meta/info.json`** — an untrusted string — onto the
/// dataset directory. `Path::join` neither rejects `..` nor resists an absolute argument, which
/// discards the base entirely: a manifest declaring a feature named `../../../../etc/shadow`, or an
/// absolute path outright, had Veridex open that file and copy its real headers into the CDM. Since
/// `Media` is bound into the content hash and the signed certificate, and `MediaStatus` distinguishes
/// missing from unreadable from read, that is a published existence-and-content oracle over the
/// whole filesystem of anyone who checks the dataset.
fn path_is_inside(dataset_root: &Path, expected: &Path) -> bool {
    use std::path::Component;
    // Lexical first: the path must be the root plus ordinary components only. This is what rejects
    // both `..` (which `strip_prefix` leaves in place) and an absolute path (which does not carry
    // the root as a prefix at all).
    let Ok(rel) = expected.strip_prefix(dataset_root) else {
        return false;
    };
    if !rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return false;
    }
    // The component filter cannot see through a symlink, and `find_media` now follows symlinked
    // files (the shape `snapshot_download` writes), so re-check containment after resolution. A path
    // that resolves to nothing is reported `Missing` from the lexical form without being opened.
    match (dataset_root.canonicalize(), expected.canonicalize()) {
        (Ok(root), Ok(path)) => path.starts_with(&root),
        _ => true,
    }
}

fn probe_stream_media(dataset_root: &Path, expected: &Path, declared: MediaParams) -> Media {
    // Joined with `/` explicitly rather than through `Path::display`: the uri is bound into the CDM
    // content hash, and a platform separator would make the same dataset hash differently on Windows
    // than on Linux.
    let uri = expected
        .strip_prefix(dataset_root)
        .unwrap_or(expected)
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/");
    if !path_is_inside(dataset_root, expected) {
        // Named rather than quietly treated as absent: the dataset did not merely fail to ship a
        // video, its manifest asked Veridex to read somewhere it has no business reading, and a
        // reader deciding whether to trust this dataset should be told that outright.
        return Media {
            uri,
            declared,
            status: MediaStatus::Unreadable {
                reason: "the manifest places this path outside the dataset directory, which \
                         Veridex will not open"
                    .to_string(),
            },
            observed: MediaParams::default(),
            frame_count: None,
        };
    }
    if !expected.is_file() {
        return Media {
            uri,
            declared,
            status: MediaStatus::Missing,
            observed: MediaParams::default(),
            frame_count: None,
        };
    }
    match crate::media::probe_mp4(expected) {
        Ok(probe) => Media {
            uri,
            declared,
            status: MediaStatus::Read,
            observed: probe.params,
            frame_count: probe.frame_count,
        },
        Err(reason) => Media {
            uri,
            declared,
            status: MediaStatus::Unreadable { reason },
            observed: MediaParams::default(),
            frame_count: None,
        },
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

/// One feature's per-row extraction: `(name, content hash — `None` if the type isn't hashable, the
/// cell's dimension-ordered scalars — empty if not numeric, `None` per absent leaf)`. The scalars
/// drive the per-dimension statistics recompute (see [`FeatureAccum`]).
type FeatureValue = (String, Option<[u8; 32]>, Vec<Option<f64>>);

type Row = (u64, i64, Option<i64>, Vec<FeatureValue>);

/// Read (episode_index, timestamp_ns, task_index) for every row of a Parquet file, in row order.
///
/// Both budgets are charged **per batch, as it is decoded**, not by the caller once the rows are
/// home. Parquet is compressed and its row count is a file-declared number: a 50 KB zstd file was
/// measured expanding to 1.26 GB resident, and charging afterwards means the error arrives only once
/// the memory is already spent.
///
/// `selected`, when present, restricts the rows read to those episodes: unselected rows are dropped
/// before their feature cells are hashed and before they are charged to the frame budget, which is
/// what makes a sampled ingest cheaper than a full one rather than merely narrower.
fn read_rows(
    path: &Path,
    fps: f64,
    declared_per_row: u64,
    selected: Option<&BTreeSet<u64>>,
    frames: &mut super::FrameBudget,
    expansion: &mut super::DecompressionBudget,
) -> Result<Vec<Row>, IngestError> {
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
        // A `timestamp` column Veridex cannot read is refused here rather than per row, because per
        // row it is indistinguishable from a null cell — and a null cell legitimately falls back to
        // `frame_index / fps`. Applied to a whole column of the wrong type, that fallback threw the
        // recorded clock away and substituted a mathematically perfect 1/fps ladder, still labelled
        // `ClockKind::Measured`. Every temporal check then ran against a synthetic timeline and
        // passed unconditionally: a five-second mid-episode gap certified clean. The fallback is for
        // a column that is *absent*, not one that is present and unreadable.
        if let Some(col) = ts_col {
            if !matches!(col.data_type(), DataType::Float64 | DataType::Float32) {
                return Err(IngestError::Parse {
                    format_id: "lerobot",
                    message: format!(
                        "{}: `timestamp` is a {} column; Veridex reads LeRobot timestamps as \
                         float seconds and will not substitute frame_index/fps for a clock the \
                         dataset actually recorded",
                        path.display(),
                        col.data_type()
                    ),
                });
            }
        }
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

        // Resolve each row's episode first, so a sampled-out row is dropped before it is hashed and
        // before it is charged. Reading one integer column is cheap next to fingerprinting every
        // feature cell in the row, which is what the charge is protecting against.
        let mut kept: Vec<(usize, u64)> = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let ep = column_i64(ep_col.as_ref(), row).ok_or_else(|| IngestError::Parse {
                format_id: "lerobot",
                message: format!("{}: episode_index is not an integer column", path.display()),
            })?;
            // `as u64` turned a negative index — `-1` is a sentinel some exporters write for an
            // unassigned row — into 18446744073709551615, and those frames were attributed to a
            // phantom episode that no declared length is ever compared against, so the corrupt
            // manifest class this adapter exists to catch walked straight past. Two different
            // sentinels also merged into one episode.
            let ep = u64::try_from(ep).map_err(|_| IngestError::Parse {
                format_id: "lerobot",
                message: format!(
                    "{}: episode_index is {ep}; an episode index cannot be negative",
                    path.display()
                ),
            })?;
            if selected.map_or(true, |s| s.contains(&ep)) {
                kept.push((row, ep));
            }
        }

        // Charge before the batch's rows are turned into `Row`s. The per-row cost is the larger of
        // what `info.json` declares and what the Parquet actually holds — a manifest declaring zero
        // features still pays for a 50,000-column file.
        let per_row = declared_per_row.max(feature_cols.len() as u64).max(1);
        frames.take("lerobot", (kept.len() as u64).saturating_mul(per_row))?;
        expansion.take("lerobot", batch.get_array_memory_size() as u64)?;

        for (row, ep) in kept {
            // Prefer the recorded timestamp; fall back to frame_index / fps.
            let ts_ns = if let Some(ts) = ts_col
                .as_ref()
                .and_then(|c| column_f64(c.as_ref(), row))
                .filter(|t| t.is_finite())
            {
                seconds_to_ns(ts)
            } else if let Some(fi) = frame_col.as_ref().and_then(|c| column_i64(c.as_ref(), row)) {
                if fps <= 0.0 {
                    return Err(IngestError::Parse {
                        format_id: "lerobot",
                        message: "no `timestamp` column and fps is unset; cannot derive timestamps"
                            .into(),
                    });
                }
                seconds_to_ns(fi as f64 / fps)
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
fn load_episode_lengths(dir: &Path) -> Result<BTreeMap<u64, u64>, IngestError> {
    #[derive(Deserialize)]
    struct EpisodeRow {
        episode_index: u64,
        length: u64,
    }
    let mut out = BTreeMap::new();
    let Ok(contents) = std::fs::read_to_string(dir.join("meta").join("episodes.jsonl")) else {
        return Ok(out);
    };
    for (number, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A line that will not parse used to be skipped without a word, which shrinks the declared
        // episode set — and under `--metadata-only` this file *is* the episode set, so a truncated
        // or partly-corrupt manifest read as a smaller, perfectly clean dataset. That is the same
        // corruption the duplicate-index arm below refuses, arriving by a quieter route.
        let row = serde_json::from_str::<EpisodeRow>(line).map_err(|e| IngestError::Parse {
            format_id: "lerobot",
            message: format!(
                "meta/episodes.jsonl line {}: {e}; every line must declare an `episode_index` and \
                 a `length`, and a line that does not is an episode this manifest silently drops",
                number + 1
            ),
        })?;
        {
            // A repeated `episode_index` is refused rather than collapsed. Keying a map on it made
            // the second line silently overwrite the first, so a manifest declaring episode 1 with
            // both length 10 and length 99 produced a CDM carrying 99 and no complaint — while the
            // cumulative boundaries LeRobot derives from those lines are, by construction, wrong for
            // every episode after the duplicate. That is the lerobot#4143 class stated outright in
            // the manifest, and it is the one form of it a run that never reads a frame can prove.
            if let Some(previous) = out.insert(row.episode_index, row.length) {
                return Err(IngestError::Parse {
                    format_id: "lerobot",
                    message: format!(
                        "meta/episodes.jsonl declares episode_index {} more than once (lengths {} \
                         and {}); the cumulative episode boundaries derived from it cannot be \
                         correct — re-export the manifest from the source shards",
                        row.episode_index, previous, row.length
                    ),
                });
            }
        }
    }
    Ok(out)
}

/// Ingest a LeRobot dataset from its `meta/` manifest alone, without opening a Parquet or video file.
///
/// What this covers, honestly: the declared episode set and its per-episode lengths, every feature's
/// name/dtype/shape/modality and declared rate, the stored summary statistics, and the provenance the
/// dataset card and `info.json` record. What it does not cover is everything the data itself would
/// answer — no timestamps, no values, no content hashes, no media. Every episode therefore carries
/// zero frames *by request*, which is why the run is labelled [`Coverage::MetadataOnly`]: the checks
/// that reason about frames are told to abstain rather than reading that absence as a defect, and a
/// certificate cannot be issued from it.
fn ingest_metadata_only(
    dir: &Path,
    info: &InfoJson,
    features: &[FeatureSpec],
    fps: f64,
    declared_lengths: &BTreeMap<u64, u64>,
    options: &IngestOptions,
) -> Result<Ingested, IngestError> {
    // The episode set has to come from the manifest, since reading the data is exactly what this
    // mode does not do. `meta/episodes.jsonl` is the good source: it names each episode and its
    // declared length. Failing that, `info.json`'s `total_episodes` implies indices `0..total` —
    // bounded by the same ceiling the sampled path uses, because the number is a handful of bytes in
    // a file that may be lying. With neither, there is no episode set at all, and inventing one (or
    // returning an empty dataset that reads as a clean pass) is refused.
    let episode_indices: BTreeSet<u64> = if !declared_lengths.is_empty() {
        declared_lengths.keys().copied().collect()
    } else if let Some(total) = info.total_episodes.filter(|t| *t > 0) {
        if total > MAX_DECLARED_EPISODES_FOR_SAMPLING {
            return Err(IngestError::Parse {
                format_id: "lerobot",
                message: format!(
                    "meta/info.json declares {total} episodes, over the \
                     {MAX_DECLARED_EPISODES_FOR_SAMPLING} ceiling for deriving the episode set from \
                     a declared total alone — ship meta/episodes.jsonl to check a dataset this \
                     large with --metadata-only"
                ),
            });
        }
        (0..total).collect()
    } else {
        return Err(IngestError::Parse {
            format_id: "lerobot",
            message: "the dataset declares no episode set (no meta/episodes.jsonl and no \
                      total_episodes in meta/info.json), so a metadata-only check has nothing to \
                      check — run a full check instead"
                .into(),
        });
    };

    // A metadata-only ingest reads no frames, so the frame budget — which is charged per frame —
    // never fires, and nothing else bounds what the manifest can make this allocate. It builds one
    // `Stream` per (episode × feature), and *both* factors come straight from attacker-controlled
    // text: an 11 MB `meta/episodes.jsonl` of 300,000 lines against 60 declared features measured
    // 18,000,000 streams and 6 GB of resident memory, a 520x amplification from a file that costs
    // nothing to write. Charge that product against the same budget the frame count uses, before a
    // single `Stream` is built — a stream and a frame are the same order of allocation, so the same
    // ceiling is the right one, and `--max-frames` raises both together.
    let declared_streams = (episode_indices.len() as u64).saturating_mul(features.len() as u64);
    let mut budget = super::FrameBudget::new(options);
    budget.take("lerobot", declared_streams)?;

    let stats = load_stats(dir);
    let card_license = load_card_license(dir);

    let episodes: Vec<Episode> = episode_indices
        .iter()
        .map(|&index| Episode {
            index,
            // No frame was read, so there is no measured window. Left `None` rather than derived
            // from the declared length and fps, which would be a timeline Veridex made up.
            start_ts: None,
            end_ts: None,
            streams: features
                .iter()
                .map(|(name, modality, dtype, shape)| Stream {
                    name: name.clone(),
                    modality: *modality,
                    declared_rate_hz: if fps > 0.0 { Some(fps) } else { None },
                    clock_id: CLOCK_ID.to_string(),
                    // The clock is still the measured LeRobot clock; this run simply did not read
                    // it. `ClockKind::Measured` describes the source, not the ingest, and the
                    // temporal checks abstain here for want of frames rather than for want of a
                    // clock — which the coverage note, not the clock kind, is what states.
                    clock_kind: ClockKind::Measured,
                    dtype: dtype.clone(),
                    shape: shape.clone(),
                    frames: Vec::new(),
                    // Stored statistics are manifest content, so they are covered: the stats
                    // sanity checks (inverted range, non-finite, mean outside range) run here.
                    stats: stats.scalar.get(name).copied(),
                    dim_stats: stats.per_dim.get(name).cloned(),
                    // Everything below is recomputed from values, and no value was read.
                    observed_stats: None,
                    observed_saturation: None,
                    observed_non_finite: None,
                    observed_dim_stats: None,
                    media: None,
                    latched: None,
                    point_fields: None,
                    frame_id: None,
                })
                .collect(),
            // Task strings are resolved from a per-row `task_index` in the Parquet, which this mode
            // does not read. Left absent rather than guessed; the task checks abstain on `None`.
            task: None,
            labels: Vec::new(),
            ego_poses: None,
            declared_frame_count: declared_lengths.get(&index).copied(),
        })
        .collect();

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
    if let Some(license) = &card_license {
        elements.push(ProvenanceElement {
            key: "license".into(),
            value: Some(license.clone()),
            class: ProvenanceClass::Known,
        });
    }

    // Whether the declared episode total is worth recording as a claim to check against depends on
    // where the episode set came from. When `meta/episodes.jsonl` supplied it, `total_episodes` is an
    // *independent* second assertion, and the two disagreeing is a real manifest inconsistency —
    // exactly what this mode exists to catch. When `total_episodes` supplied the set itself, the
    // comparison is `n == n`: a check that cannot fail, whose pass would then be reported as if
    // something had been verified.
    let declared_total_is_independent = !declared_lengths.is_empty();
    let mut metadata = vec![
        ("source_format".into(), "lerobot".into()),
        (
            "codebase_version".into(),
            info.codebase_version.clone().unwrap_or_default(),
        ),
    ];
    if declared_total_is_independent {
        if let Some(total) = info.total_episodes {
            metadata.push((
                crate::cdm::META_DECLARED_EPISODES.to_string(),
                total.to_string(),
            ));
        }
    }

    let dataset = Dataset {
        id: crate::adapter::dataset_id_from_path(dir, "lerobot"),
        metadata,
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        }],
        episodes,
        calibration: None,
    };

    let mut mapped_fields = vec![
        "features -> streams".into(),
        "fps -> stream.declared_rate_hz".into(),
        "feature.dtype -> stream.dtype".into(),
        "feature.shape -> stream.shape".into(),
        "robot_type -> provenance.sensor".into(),
    ];
    let (stats_mapped, stats_omitted) = stats_fidelity(&stats);
    mapped_fields.extend(stats_mapped);
    if !declared_lengths.is_empty() {
        mapped_fields.push("meta/episodes.jsonl -> episodes + declared_frame_count".into());
    } else {
        mapped_fields.push("total_episodes -> episode set (0..total)".into());
    }
    if declared_total_is_independent && info.total_episodes.is_some() {
        mapped_fields.push("total_episodes -> declared episode-count check".into());
    }
    if card_license.is_some() {
        mapped_fields.push("README.md license -> provenance.license".into());
    }

    let mut omitted_fields = vec![
        "frame timestamps, feature values, and content hashes (no Parquet was read)".into(),
        "video container headers (no media file was opened)".into(),
        "task strings (task_index lives in the Parquet data)".into(),
        "total_frames (a claim about frames, and no frame was read)".into(),
    ];
    omitted_fields.extend(stats_omitted);
    if !declared_total_is_independent && info.total_episodes.is_some() {
        omitted_fields.push(
            "total_episodes -> declared episode-count check (the episode set was derived from that \
             same number, so the comparison could not fail)"
                .into(),
        );
    }

    let report = IngestReport {
        unread_sources: Vec::new(),
        format_id: "lerobot",
        source_version: info.codebase_version.clone(),
        coverage: Coverage::MetadataOnly {
            episodes_declared: dataset.episodes.len() as u64,
        },
        mapped_fields,
        unmapped_fields: vec![UnmappedField {
            source_path: "data/**.parquet".into(),
            note: "stream payloads were not read: this is a metadata-only ingest".into(),
        }],
        omitted_fields,
    };

    Ok(Ingested { dataset, report })
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

    /// LeRobot writes its structure down beside the data, in `meta/`: the features and their
    /// dtypes/shapes in `info.json`, the episode set and per-episode lengths in `episodes.jsonl`,
    /// the summary statistics in `stats.json`, the license in the dataset card. All of it can be
    /// checked without opening a Parquet file.
    fn supports_metadata_only(&self) -> bool {
        true
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

        // What the manifest declares about each video feature's encoding, to compare against the
        // media files themselves. Keyed by feature name; only video features have one.
        let declared_media: BTreeMap<String, MediaParams> = info
            .features
            .iter()
            // `dtype: "video"` — and only that — means "this feature's pixels live in video files".
            // A feature whose *name* merely contains `image`, or whose dtype is `image` (individual
            // files) or a numeric array (stored inline in the Parquet), has no video file to find,
            // and expecting one would accuse a sound dataset of losing something it never had.
            .filter(|(name, f)| {
                !BOOKKEEPING.contains(&name.as_str())
                    && f.dtype.as_deref().map(str::trim) == Some("video")
            })
            .map(|(name, f)| {
                (
                    name.clone(),
                    declared_media_params(f.info.as_ref(), f.shape.as_deref(), f.names.as_deref()),
                )
            })
            .collect();

        // Declared per-episode frame counts from meta/episodes.jsonl (empty if absent) — the manifest
        // assertion the boundary check tests against the frames actually ingested (lerobot#4143), and
        // the episode index set a sampled ingest draws from.
        let declared_lengths = load_episode_lengths(dir)?;

        // A metadata-only ingest answers a different question — "does the manifest hold together?" —
        // and answers it without opening a single Parquet or video file. It returns here.
        if options.metadata_only {
            return ingest_metadata_only(dir, &info, &features, fps, &declared_lengths, options);
        }

        // Resolve a sampling request into the concrete episode indices to keep, *before* reading any
        // Parquet, so the unselected episodes cost nothing. The draw is made over the manifest's
        // episode set: the dataset-level statistics are accumulated as rows arrive, so which episodes
        // count has to be known going in, not filtered out afterwards.
        //
        // The set comes from `meta/episodes.jsonl` when present, else from `info.json`'s
        // `total_episodes` (LeRobot numbers episodes `0..total`). With neither, the episode set is
        // only knowable by reading everything — the cost sampling exists to avoid — so that
        // combination is refused rather than silently degraded into a full read.
        let selected: Option<BTreeSet<u64>> = if options.sample.is_partial() {
            let available: BTreeSet<u64> = if !declared_lengths.is_empty() {
                declared_lengths.keys().copied().collect()
            } else if let Some(total) = info.total_episodes.filter(|t| *t > 0) {
                match &options.sample {
                    // Only the first `n` indices can ever be selected, so a large declared total need
                    // not be materialized to answer this — and cannot be used to exhaust memory.
                    Sample::FirstEpisodes(n) => (0..total.min(*n)).collect(),
                    // The random draw ranks the whole set, so it is the arm that must materialize it.
                    // `total_episodes` is a number in a few-hundred-byte manifest and this runs before
                    // either ingest budget exists, so nothing else bounds it: `u64::MAX` panicked on
                    // capacity overflow, and merely enormous values were a straight OOM that still
                    // returned Ok. Past the ceiling the manifest must be backed by
                    // `meta/episodes.jsonl`, whose cost is bounded by its own size on disk.
                    _ => {
                        if total > MAX_DECLARED_EPISODES_FOR_SAMPLING {
                            return Err(IngestError::SamplingUnsupported {
                                format_id: "lerobot",
                                reason: format!(
                                    "meta/info.json declares {total} episodes, over the \
                                     {MAX_DECLARED_EPISODES_FOR_SAMPLING} ceiling for deriving the \
                                     episode set from a declared total alone — ship \
                                     meta/episodes.jsonl to sample a dataset this large"
                                ),
                            });
                        }
                        (0..total).collect()
                    }
                }
            } else {
                return Err(IngestError::SamplingUnsupported {
                    format_id: "lerobot",
                    reason: "the dataset declares no episode set (no meta/episodes.jsonl and no \
                             total_episodes in meta/info.json), so which episodes exist is not \
                             known without reading the whole dataset"
                        .into(),
                });
            };
            Some(options.sample.select(&available))
        } else {
            None
        };

        // Read every data Parquet, grouping row timestamps by episode.
        let mut parquet_files = Vec::new();
        let mut escaped_shards = Vec::new();
        find_parquet(
            dir,
            &dir.join("data"),
            &mut parquet_files,
            &mut escaped_shards,
        );

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
        // Compressed expansion is bounded across the whole dataset, sized on the Parquet bytes it
        // actually holds, since every file's rows accumulate into the same in-memory CDM.
        let parquet_bytes: u64 = parquet_files
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
        let mut expansion = super::DecompressionBudget::new(options, parquet_bytes);
        // Dataset-level recomputed statistics per feature (LeRobot's stored stats are dataset-level).
        let mut observed: BTreeMap<String, FeatureAccum> = BTreeMap::new();
        for path in &parquet_files {
            for (ep, ts, task_index, feature_values) in read_rows(
                path,
                fps,
                per_row,
                selected.as_ref(),
                &mut budget,
                &mut expansion,
            )? {
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
            .map(|(name, acc)| (name.clone(), acc.non_finite()))
            .collect();
        // Per-dimension stats, only for multi-DoF features — the extreme-outlier check scans them so a
        // spike in a non-first joint is caught. Scalar features carry their one dimension in
        // `observed_stats` already, so they get `None` here (no duplication).
        let observed_dim_stats: BTreeMap<String, Vec<DimStats>> = observed
            .iter()
            .filter_map(|(name, acc)| acc.dim_stats().map(|dims| (name.clone(), dims)))
            .collect();

        let stats = load_stats(dir);
        // Resolve `task_index` -> task string via meta/tasks.jsonl (empty map if the file is absent).
        let tasks = load_tasks(dir);

        // Resolve and read the media file behind each video stream. A LeRobot video feature keeps its
        // timeline in the Parquet and its pixels in `videos/`, and nothing reconciles the two — so
        // Veridex reads each container's headers (never a pixel) and hands both halves to the
        // `video.*` checks. Only the episodes actually ingested are probed, so a sample stays cheap.
        let video_index = index_videos(dir, &declared_media.keys().map(|s| s.as_str()).collect());
        let mut media: BTreeMap<(String, u64), Media> = BTreeMap::new();
        for (feature, declared) in &declared_media {
            if video_index.unresolvable.contains(feature) {
                // This feature has media Veridex cannot attribute to an episode (a v3 layout that
                // concatenates episodes into one file). Nothing is asserted about it — including
                // about the episodes that *did* resolve, since the rest of its frames plainly live
                // in the file that could not be split.
                //
                // Skipping outright left `media: None`, which is exactly what a non-video feature
                // carries — so the whole video family iterated past these streams and emitted
                // nothing at all, for a stream whose files might hold no container. The abstention
                // is now recorded on the stream, where a check can see it and say so. It only
                // reached the ingest report before, which `veridex check` never prints.
                for episode in episode_rows.keys() {
                    media.insert(
                        (feature.clone(), *episode),
                        Media {
                            uri: format!("videos/**/{feature}/"),
                            declared: declared.clone(),
                            status: MediaStatus::Unattributable {
                                reason: "media files are not laid out one per episode \
                                         (episode_<n>.<ext>)"
                                    .into(),
                            },
                            observed: MediaParams::default(),
                            frame_count: None,
                        },
                    );
                }
                continue;
            }
            let by_episode = video_index.per_episode.get(feature);
            for episode in episode_rows.keys() {
                // A feature declared `dtype: "video"` says its pixels live in video files. One that
                // is absent is a real gap — the episode's rows claim imagery the dataset does not
                // hold — and that is just as true when *no* file resolved as when one did: an
                // un-pulled LFS pointer or an interrupted download leaves the whole tree empty, and
                // reading that as "nothing to check" is reading silence as a pass.
                let expected = match by_episode {
                    Some(by_episode) => by_episode
                        .get(episode)
                        .cloned()
                        .unwrap_or_else(|| expected_sibling_path(by_episode, *episode)),
                    None => dir.join("videos").join(feature),
                };
                media.insert(
                    (feature.clone(), *episode),
                    probe_stream_media(dir, &expected, declared.clone()),
                );
            }
        }

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
                        // Real recorded timestamps: every temporal check applies.
                        clock_kind: ClockKind::Measured,
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
                        media: media.get(&(name.clone(), index)).cloned(),
                        // LeRobot is a manipulation format: no point-cloud streams.
                        latched: None,
                        point_fields: None,
                        // LeRobot declares no sensor coordinate frames (it is not a spatially-calibrated rig).
                        frame_id: None,
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
            id: crate::adapter::dataset_id_from_path(dir, "lerobot"),
            metadata: {
                let mut m = vec![
                    ("source_format".into(), "lerobot".into()),
                    (
                        "codebase_version".into(),
                        info.codebase_version.clone().unwrap_or_default(),
                    ),
                ];
                // Record the declared counts so checks can catch a truncated export. Under a sample
                // the manifest's dataset-level totals describe the whole dataset, so comparing them
                // against a deliberately partial ingest would report every sample as a truncation —
                // but the count of episodes the sample *selected* is a claim about the sample itself,
                // and comparing it against what materialized is the assertion that catches "the
                // episode set the manifest declares does not exist in the data". Keep that one.
                if let Some(sel) = &selected {
                    m.push((
                        crate::cdm::META_DECLARED_EPISODES.to_string(),
                        sel.len().to_string(),
                    ));
                }
                if selected.is_none() {
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
            "feature cell bytes -> frame.value_ref.content_hash (SHA-256)".into(),
        ];
        let mut omitted_fields =
            vec!["video frame decoding (frames are timestamps, not pixels)".into()];
        let (stats_mapped, stats_omitted) = stats_fidelity(&stats);
        mapped_fields.extend(stats_mapped);
        omitted_fields.extend(stats_omitted);
        if !media.is_empty() {
            mapped_fields.push(
                "videos/**.mp4 container headers -> stream.media (frame count, resolution, codec, \
                 rate)"
                    .into(),
            );
        }
        if declared_media
            .values()
            .any(|p| p != &MediaParams::default())
        {
            mapped_fields.push("feature.info video.* -> stream.media.declared".into());
        }
        // Task-string resolution is reported honestly by whether meta/tasks.jsonl was present.
        if tasks.is_empty() {
            omitted_fields.push("task strings (no meta/tasks.jsonl to resolve task_index)".into());
        } else {
            mapped_fields.push("task_index + meta/tasks.jsonl -> episode.task".into());
        }
        if card_license.is_some() {
            mapped_fields.push("README.md license -> provenance.license".into());
        }
        // The dataset-level declared totals are only comparable against a full ingest; under a sample
        // they are reported as omitted rather than as mapped-but-unused.
        if selected.is_some() {
            omitted_fields.push(
                "total_episodes / total_frames (dataset-level totals are not comparable against a \
                 sampled ingest)"
                    .into(),
            );
        } else {
            if info.total_episodes.is_some() {
                mapped_fields.push("total_episodes -> declared episode-count check".into());
            }
            if info.total_frames.is_some() {
                mapped_fields.push("total_frames -> declared frame-count check".into());
            }
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
        // Named rather than quietly skipped: the dataset did not merely fail to ship a shard, it
        // asked Veridex to read somewhere it has no business reading, and a reader deciding whether
        // to trust this dataset should be told that outright. Without this the run would just look
        // like a dataset with fewer episodes than its manifest declares.
        //
        // Recorded as an *unread source* rather than an unmapped field. Both were once the same
        // vector, and that vector reaches `inspect` alone -- so this notice, which is a hole in the
        // run's coverage, never reached the verdict that `check`, `certify`, and `diff` read.
        let mut unread_sources = Vec::new();
        for path in &escaped_shards {
            unread_sources.push(UnmappedField {
                source_path: path
                    .strip_prefix(dir)
                    .unwrap_or(path)
                    .components()
                    .filter_map(|c| c.as_os_str().to_str())
                    .collect::<Vec<_>>()
                    .join("/"),
                note: "this data shard resolves outside the dataset directory, so it was not read \
                       — a dataset is built only from its own files"
                    .into(),
            });
        }
        for feature in &video_index.unresolvable {
            unmapped_fields.push(UnmappedField {
                source_path: format!("videos/**/{feature}/"),
                note:
                    "media files are not laid out one per episode (episode_<n>.<ext>), so no file \
                       can be attributed to an episode; the video checks abstain for this stream"
                        .into(),
            });
        }
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
            unread_sources,
            format_id: "lerobot",
            source_version: info.codebase_version.clone(),
            // Coverage is what was actually ingested, not what was asked for: an episode the manifest
            // lists but the data does not hold never becomes an episode, so it is not counted here.
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
