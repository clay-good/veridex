//! Generate a small demo LeRobot v3 dataset for trying the CLI end-to-end.
//!
//! Writes a minimal on-disk LeRobot v3 layout (`meta/info.json` + one Parquet shard). Pick a variant
//! with the second argument:
//!
//! - (default) `broken` — two episodes; episode 1 has an out-of-order timestamp → `TEMPORAL.NON_MONOTONIC`.
//! - `clean` — a well-formed two-episode dataset with no findings.
//! - `truncated` — the manifest declares 20 frames but episode 1 was cut short (only 6 written),
//!   a realistic interrupted export → `STRUCTURAL.FRAME_COUNT_MISMATCH`.
//! - `boundary` — two well-formed 10-frame episodes, but `meta/episodes.jsonl` declares the wrong
//!   `length` for episode 1 (7, not 10). LeRobot derives cumulative boundaries from those lengths, so
//!   the corruption silently misplaces frames into the wrong episode — the lerobot#4143 class →
//!   `STRUCTURAL.EPISODE_BOUNDARY`.
//! - `jitter` — episode 1 has an irregular inter-frame spacing (alternating ~13 ms / ~53 ms) so its
//!   mean rate still looks like ~30 Hz and no single gap is large, yet the timeline is jittery →
//!   `TEMPORAL.JITTER`.
//! - `duplicate` — two episodes with byte-for-byte identical content (a re-upload) →
//!   `STRUCTURAL.DUPLICATE_EPISODE`.
//! - `short-episode` — five episodes; four are ~1 s captures and one was cut short right after it
//!   began (~0.07 s), a duration far below the dataset median → `TEMPORAL.EPISODE_DURATION_OUTLIER`.
//! - `saturated` — one 30-frame episode whose feature values sit pinned exactly at their maximum for
//!   24 of 30 frames (a clamped actuator against its stop) → `STATISTICAL.SATURATED`.
//! - `spike` — one 150-frame episode whose values hug zero except a single frame that jumps to 1000
//!   (a sensor glitch or unit error), an extreme >10σ from the mean → `STATISTICAL.OUTLIER`.
//! - `nan` — one 30-frame episode with a single NaN feature value (a failed sensor read) and no
//!   `meta/stats.json`, so only a recompute over the real cells sees it → `STATISTICAL.NON_FINITE_OBSERVED`.
//! - `multi-joint` — a 3-DoF `action` (a `FixedSizeList`) whose gripper (dimension 2) saturates while
//!   the arm joints sweep → `STATISTICAL.SATURATED` naming the dimension, which element 0 alone misses.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_lerobot -- <output-dir> [clean|truncated|boundary|jitter|short-episode|duplicate|saturated|spike|nan|multi-joint]`
//!
//! Then: `veridex check <output-dir>`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, FixedSizeListArray, Float32Array, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

/// Which demo dataset to write.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Clean,
    NonMonotonic,
    Truncated,
    Boundary,
    Jitter,
    ShortEpisode,
    Duplicate,
    Saturated,
    Spike,
    Nan,
    MultiJoint,
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-lerobot".to_string());
    let mode = match std::env::args().nth(2).as_deref() {
        Some("clean") => Mode::Clean,
        Some("truncated") => Mode::Truncated,
        Some("boundary") => Mode::Boundary,
        Some("jitter") => Mode::Jitter,
        Some("short-episode") => Mode::ShortEpisode,
        Some("duplicate") => Mode::Duplicate,
        Some("saturated") => Mode::Saturated,
        Some("spike") => Mode::Spike,
        Some("nan") => Mode::Nan,
        Some("multi-joint") => Mode::MultiJoint,
        _ => Mode::NonMonotonic,
    };
    let dir = Path::new(&dir);

    write_dataset(dir, mode);

    let what = match mode {
        Mode::Clean => "clean (well-formed)",
        Mode::NonMonotonic => "broken (episode 1 has an out-of-order timestamp → TEMPORAL.NON_MONOTONIC)",
        Mode::Truncated => {
            "truncated (manifest declares 20 frames, episode 1 cut short → STRUCTURAL.FRAME_COUNT_MISMATCH)"
        }
        Mode::Boundary => {
            "boundary (meta/episodes.jsonl declares the wrong length for episode 1 → STRUCTURAL.EPISODE_BOUNDARY)"
        }
        Mode::Jitter => {
            "jitter (episode 1 has an irregular inter-frame spacing → TEMPORAL.JITTER)"
        }
        Mode::ShortEpisode => {
            "short-episode (episode 4 was cut short right after it began → TEMPORAL.EPISODE_DURATION_OUTLIER)"
        }
        Mode::Duplicate => {
            "duplicate (episode 1 is a byte-for-byte re-upload of episode 0 → STRUCTURAL.DUPLICATE_EPISODE)"
        }
        Mode::Saturated => {
            "saturated (feature values pinned at their maximum for 24 of 30 frames → STATISTICAL.SATURATED)"
        }
        Mode::Spike => {
            "spike (a single frame jumps to 1000 while the rest hug zero → STATISTICAL.OUTLIER)"
        }
        Mode::Nan => {
            "nan (one frame's value is NaN, invisible to the absent stats.json → STATISTICAL.NON_FINITE_OBSERVED)"
        }
        Mode::MultiJoint => {
            "multi-joint (a 3-DoF `action` whose gripper — dimension 2 — saturates → STATISTICAL.SATURATED naming the dimension)"
        }
    };
    println!("Wrote {what} LeRobot v3 dataset to {}", dir.display());
    println!("Try:  veridex check {}", dir.display());
}

fn write_dataset(dir: &Path, mode: Mode) {
    fs::create_dir_all(dir.join("meta")).expect("create meta/");
    fs::create_dir_all(dir.join("data/chunk-000")).expect("create data/");

    let fps = 30.0;
    // The one multi-DoF variant has its own info/parquet shape (a FixedSizeList feature).
    if mode == Mode::MultiJoint {
        write_multijoint_dataset(dir, fps);
        return;
    }
    // Build the per-frame rows and the manifest's declared counts for this mode.
    let (rows, declared_episodes, declared_frames) = build_rows(mode, fps);

    // Two features, both frame-aligned scalars. Veridex fingerprints their cell bytes into per-frame
    // content hashes (a SHA-256, never a decode), so content-level checks and the CDM hash reflect
    // actual frame content.
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
        "total_episodes": declared_episodes,
        "total_frames": declared_frames,
        "features": {
            "observation.state": { "dtype": "float32", "shape": [1] },
            "action": { "dtype": "float32", "shape": [1] },
        },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).expect("serialize info.json"),
    )
    .expect("write info.json");

    write_shared_meta(dir);
    write_parquet(&dir.join("data/chunk-000/file-000.parquet"), &rows);

    if mode == Mode::Boundary {
        // Both episodes actually hold 10 frames, but the per-episode manifest declares episode 1 as
        // 7 — a corrupted cumulative length. LeRobot would derive boundaries from these lengths and
        // misplace frames; `veridex check` catches the declared-vs-actual disagreement.
        let episodes = "\
            {\"episode_index\": 0, \"tasks\": [\"pick up the red cube\"], \"length\": 10}\n\
            {\"episode_index\": 1, \"tasks\": [\"pick up the red cube\"], \"length\": 7}\n";
        fs::write(dir.join("meta/episodes.jsonl"), episodes).expect("write episodes.jsonl");
    }
}

/// The tasks table (so the CLI resolves task strings) and a Hugging Face-style dataset card (so the
/// adapter extracts a real license into provenance) — shared by every variant.
fn write_shared_meta(dir: &Path) {
    fs::write(
        dir.join("meta/tasks.jsonl"),
        serde_json::json!({ "task_index": 0, "task": "pick up the red cube" }).to_string(),
    )
    .expect("write tasks.jsonl");
    fs::write(
        dir.join("README.md"),
        "---\nlicense: apache-2.0\ntags:\n- robotics\n- lerobot\n---\n\n# Demo dataset\n\nSynthetic \
         LeRobot v3 data generated by `make_demo_lerobot`.\n",
    )
    .expect("write README.md");
}

/// Write a single-episode dataset whose one feature is a 3-DoF `action` (a `FixedSizeList<float32,
/// 3>`): the arm joints (dimensions 0 and 1) sweep freely while the gripper (dimension 2) sits pinned
/// exactly at its maximum for 24 of 30 frames — a clamped actuator. `veridex check` flags
/// `STATISTICAL.SATURATED` and names the offending dimension, which an element-0-only scan would miss.
fn write_multijoint_dataset(dir: &Path, fps: f64) {
    let n = 30usize;
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
        "total_episodes": 1,
        "total_frames": n,
        "features": { "action": { "dtype": "float32", "shape": [3] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).expect("serialize info.json"),
    )
    .expect("write info.json");
    write_shared_meta(dir);

    // Flattened 3-per-row values: [arm0, arm1, gripper]. The gripper pins at 1.0 for the first 24
    // frames, then relaxes so the stream isn't merely constant.
    let mut values: Vec<f32> = Vec::with_capacity(n * 3);
    for i in 0..n {
        let gripper = if i < 24 {
            1.0
        } else {
            1.0 - (i - 23) as f32 * 0.1
        };
        values.extend_from_slice(&[i as f32 * 0.5, -(i as f32) * 0.3, gripper]);
    }
    let item = Arc::new(Field::new("item", DataType::Float32, true));
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("action", DataType::FixedSizeList(item.clone(), 3), false),
    ]));
    let action: ArrayRef = Arc::new(FixedSizeListArray::new(
        item,
        3,
        Arc::new(Float32Array::from(values)),
        None,
    ));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![0i64; n])),
            Arc::new(Int64Array::from((0..n as i64).collect::<Vec<_>>())),
            Arc::new(Float64Array::from(
                (0..n).map(|i| i as f64 / fps).collect::<Vec<_>>(),
            )),
            action,
        ],
    )
    .expect("build record batch");
    let file =
        fs::File::create(dir.join("data/chunk-000/file-000.parquet")).expect("create parquet");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("parquet writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close parquet");
}

/// One demo frame: `(episode_index, timestamp_seconds, feature_value)`. The feature value is
/// fingerprinted into the frame content hash; it is globally unique per row in every variant except
/// `duplicate`, where episode 1 deliberately repeats episode 0's values (and timestamps) exactly.
type DemoRow = (i64, f64, f32);

/// Build the per-frame rows for a mode, plus the episode and frame counts the manifest should
/// declare. All variants but `truncated` declare exactly what they write; `truncated` deliberately
/// over-declares so the declared/actual mismatch fires.
fn build_rows(mode: Mode, fps: f64) -> (Vec<DemoRow>, u64, u64) {
    if mode == Mode::Duplicate {
        // Two episodes with byte-for-byte identical content: same timestamps AND same feature values.
        // A re-upload or a bad merge. Only STRUCTURAL.DUPLICATE_EPISODE fires.
        let mut rows: Vec<DemoRow> = Vec::new();
        for ep in 0..2i64 {
            for f in 0..10i64 {
                rows.push((ep, f as f64 / fps, f as f32));
            }
        }
        return (rows, 2, 20);
    }

    if mode == Mode::Saturated {
        // One 30-frame episode at ~30 Hz whose feature values sit pinned exactly at 1.0 for 24 of the
        // 30 frames — an actuator clamped against its stop — then vary for the last 6 (so the stream
        // isn't merely constant). 80% pinned at the maximum, well past the 50% default → SATURATED.
        // (`action` mirrors the value column, so both scalar streams saturate.)
        let rows: Vec<DemoRow> = (0..30i64)
            .map(|f| {
                let v = if f < 24 {
                    1.0
                } else {
                    1.0 - (f - 23) as f32 * 0.1
                };
                (0, f as f64 / fps, v)
            })
            .collect();
        return (rows, 1, 30);
    }

    if mode == Mode::Spike {
        // One 150-frame episode at ~30 Hz. Values hug zero (tiny distinct steps) except frame 75,
        // which jumps to 1000 — a sensor glitch or a unit error. A single point among N can sit at
        // most sqrt(N-1) std from the mean, so 150 frames put this spike ~12σ out, past the 10σ
        // default → STATISTICAL.OUTLIER (and nothing else: one frame is far too few to saturate).
        let rows: Vec<DemoRow> = (0..150i64)
            .map(|f| {
                let v = if f == 75 { 1000.0 } else { f as f32 * 0.001 };
                (0, f as f64 / fps, v)
            })
            .collect();
        return (rows, 1, 150);
    }

    if mode == Mode::Nan {
        // One 30-frame episode at ~30 Hz whose values step distinctly except frame 15, whose value is
        // NaN — a failed sensor read. No `meta/stats.json` is written, so the stored-stats
        // NON_FINITE check has nothing to inspect; only a recompute over the real cells sees it →
        // STATISTICAL.NON_FINITE_OBSERVED. (`action` mirrors the value, so both streams carry it.)
        let rows: Vec<DemoRow> = (0..30i64)
            .map(|f| {
                let v = if f == 15 { f32::NAN } else { f as f32 };
                (0, f as f64 / fps, v)
            })
            .collect();
        return (rows, 1, 30);
    }

    // Give every row a globally-unique value so no two episodes are accidental content duplicates.
    let value = |idx: usize| idx as f32;

    if mode == Mode::ShortEpisode {
        // Five episodes at ~30 Hz. Four are full ~1 s captures (30 frames → ~0.97 s span); episode 4
        // was cut short right after it began (3 frames → ~0.07 s span), more than 10x below the ~0.97 s
        // median. Its rate, spacing, and counts are all otherwise correct, so the only finding is
        // TEMPORAL.EPISODE_DURATION_OUTLIER.
        let mut ts: Vec<(i64, f64)> = Vec::new();
        for ep in 0..4i64 {
            for f in 0..30i64 {
                ts.push((ep, f as f64 / fps));
            }
        }
        for f in 0..3i64 {
            ts.push((4, f as f64 / fps));
        }
        let rows: Vec<DemoRow> = ts
            .into_iter()
            .enumerate()
            .map(|(i, (ep, t))| (ep, t, value(i)))
            .collect();
        let frames = rows.len() as u64;
        return (rows, 5, frames);
    }

    // Two episodes at ~30 Hz. Episode 0 always has 10 frames. Episode 1 has 10 too, except in the
    // truncated variant where it was cut short to 6 — fewer than the 20 frames info.json declares.
    let ep1_frames = if mode == Mode::Truncated { 6 } else { 10 };
    let mut ts: Vec<(i64, f64)> = Vec::new();
    for f in 0..10i64 {
        ts.push((0, f as f64 / fps));
    }
    if mode == Mode::Jitter {
        // Episode 1: irregular spacing (alternating ~13 ms / ~53 ms). The mean rate stays ~30 Hz
        // (so TEMPORAL.RATE is quiet) and no single interval reaches the gap threshold, but the
        // coefficient of variation is high → TEMPORAL.JITTER.
        let mut t = 0.0f64;
        for f in 0..10i64 {
            ts.push((1, t));
            t += if f % 2 == 0 { 0.013 } else { 0.053 };
        }
    } else {
        for f in 0..ep1_frames {
            ts.push((1, f as f64 / fps));
        }
    }
    if mode == Mode::NonMonotonic {
        // Episode 1, frame 5: rewind the clock behind frame 4 (an out-of-order/duplicated frame).
        let idx = 10 + 5;
        ts[idx].1 = ts[idx - 1].1 - 0.010;
    }
    let rows: Vec<DemoRow> = ts
        .into_iter()
        .enumerate()
        .map(|(i, (ep, t))| (ep, t, value(i)))
        .collect();
    // Every two-episode variant declares 20 frames; truncated under-writes to trigger the mismatch.
    (rows, 2, 20)
}

/// Write the per-frame Parquet table: bookkeeping columns plus the two feature-value columns
/// (`observation.state`, `action`) whose cell bytes Veridex fingerprints into frame content hashes.
fn write_parquet(path: &Path, rows: &[DemoRow]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("task_index", DataType::Int64, false),
        Field::new("observation.state", DataType::Float32, false),
        Field::new("action", DataType::Float32, false),
    ]));
    let eps: Vec<i64> = rows.iter().map(|(e, _, _)| *e).collect();
    let frames: Vec<i64> = (0..rows.len() as i64).collect();
    let ts: Vec<f64> = rows.iter().map(|(_, t, _)| *t).collect();
    let task_index: Vec<i64> = vec![0; rows.len()];
    let state: Vec<f32> = rows.iter().map(|(_, _, v)| *v).collect();
    // `action` mirrors `state` here; distinct content isn't needed to demonstrate the checks.
    let action: Vec<f32> = state.clone();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
            Arc::new(Int64Array::from(task_index)),
            Arc::new(Float32Array::from(state)),
            Arc::new(Float32Array::from(action)),
        ],
    )
    .expect("build record batch");

    let file = fs::File::create(path).expect("create parquet file");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("parquet writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close parquet");
}
