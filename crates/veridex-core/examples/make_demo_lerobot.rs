//! Generate a small demo LeRobot v3 dataset for trying the CLI end-to-end.
//!
//! Writes a minimal on-disk LeRobot v3 layout (`meta/info.json` + one Parquet shard). Pick a variant
//! with the second argument:
//!
//! - (default) `broken` — two episodes; episode 1 has an out-of-order timestamp → `TEMPORAL.NON_MONOTONIC`.
//! - `clean` — a well-formed two-episode dataset with no findings.
//! - `truncated` — the manifest declares 20 frames but episode 1 was cut short (only 6 written),
//!   a realistic interrupted export → `STRUCTURAL.FRAME_COUNT_MISMATCH`.
//! - `jitter` — episode 1 has an irregular inter-frame spacing (alternating ~13 ms / ~53 ms) so its
//!   mean rate still looks like ~30 Hz and no single gap is large, yet the timeline is jittery →
//!   `TEMPORAL.JITTER`.
//! - `short-episode` — five episodes; four are ~1 s captures and one was cut short right after it
//!   began (~0.07 s), a duration far below the dataset median → `TEMPORAL.EPISODE_DURATION_OUTLIER`.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_lerobot -- <output-dir> [clean|truncated|jitter|short-episode]`
//!
//! Then: `veridex check <output-dir>`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

/// Which demo dataset to write.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Clean,
    NonMonotonic,
    Truncated,
    Jitter,
    ShortEpisode,
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-lerobot".to_string());
    let mode = match std::env::args().nth(2).as_deref() {
        Some("clean") => Mode::Clean,
        Some("truncated") => Mode::Truncated,
        Some("jitter") => Mode::Jitter,
        Some("short-episode") => Mode::ShortEpisode,
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
        Mode::Jitter => {
            "jitter (episode 1 has an irregular inter-frame spacing → TEMPORAL.JITTER)"
        }
        Mode::ShortEpisode => {
            "short-episode (episode 4 was cut short right after it began → TEMPORAL.EPISODE_DURATION_OUTLIER)"
        }
    };
    println!("Wrote {what} LeRobot v3 dataset to {}", dir.display());
    println!("Try:  veridex check {}", dir.display());
}

fn write_dataset(dir: &Path, mode: Mode) {
    fs::create_dir_all(dir.join("meta")).expect("create meta/");
    fs::create_dir_all(dir.join("data/chunk-000")).expect("create data/");

    let fps = 30.0;
    // Build the per-frame rows and the manifest's declared counts for this mode.
    let (rows, declared_episodes, declared_frames) = build_rows(mode, fps);

    // Two features, both frame-aligned scalars. Only structure/timestamps are read by Veridex.
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

    // A tasks table so the CLI shows resolved task strings; both episodes share task 0.
    fs::write(
        dir.join("meta/tasks.jsonl"),
        serde_json::json!({ "task_index": 0, "task": "pick up the red cube" }).to_string(),
    )
    .expect("write tasks.jsonl");

    write_parquet(&dir.join("data/chunk-000/file-000.parquet"), &rows);
}

/// Build the per-frame `(episode_index, timestamp_seconds)` rows for a mode, plus the episode and
/// frame counts the manifest should declare. All variants but `truncated` declare exactly what they
/// write; `truncated` deliberately over-declares so the declared/actual mismatch fires.
fn build_rows(mode: Mode, fps: f64) -> (Vec<(i64, f64)>, u64, u64) {
    if mode == Mode::ShortEpisode {
        // Five episodes at ~30 Hz. Four are full ~1 s captures (30 frames → ~0.97 s span); episode 4
        // was cut short right after it began (3 frames → ~0.07 s span), more than 10x below the ~0.97 s
        // median. Its rate, spacing, and counts are all otherwise correct, so the only finding is
        // TEMPORAL.EPISODE_DURATION_OUTLIER.
        let mut rows: Vec<(i64, f64)> = Vec::new();
        for ep in 0..4i64 {
            for f in 0..30i64 {
                rows.push((ep, f as f64 / fps));
            }
        }
        for f in 0..3i64 {
            rows.push((4, f as f64 / fps));
        }
        let frames = rows.len() as u64;
        return (rows, 5, frames);
    }

    // Two episodes at ~30 Hz. Episode 0 always has 10 frames. Episode 1 has 10 too, except in the
    // truncated variant where it was cut short to 6 — fewer than the 20 frames info.json declares.
    let ep1_frames = if mode == Mode::Truncated { 6 } else { 10 };
    let mut rows: Vec<(i64, f64)> = Vec::new();
    for f in 0..10i64 {
        rows.push((0, f as f64 / fps));
    }
    if mode == Mode::Jitter {
        // Episode 1: irregular spacing (alternating ~13 ms / ~53 ms). The mean rate stays ~30 Hz
        // (so TEMPORAL.RATE is quiet) and no single interval reaches the gap threshold, but the
        // coefficient of variation is high → TEMPORAL.JITTER.
        let mut t = 0.0f64;
        for f in 0..10i64 {
            rows.push((1, t));
            t += if f % 2 == 0 { 0.013 } else { 0.053 };
        }
    } else {
        for f in 0..ep1_frames {
            rows.push((1, f as f64 / fps));
        }
    }
    if mode == Mode::NonMonotonic {
        // Episode 1, frame 5: rewind the clock behind frame 4 (an out-of-order/duplicated frame).
        let idx = 10 + 5;
        rows[idx].1 = rows[idx - 1].1 - 0.010;
    }
    // Every two-episode variant declares 20 frames; truncated under-writes to trigger the mismatch.
    (rows, 2, 20)
}

/// Write the per-frame Parquet table: `episode_index`, `frame_index`, `timestamp` (seconds).
fn write_parquet(path: &Path, rows: &[(i64, f64)]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("task_index", DataType::Int64, false),
    ]));
    let eps: Vec<i64> = rows.iter().map(|(e, _)| *e).collect();
    let frames: Vec<i64> = (0..rows.len() as i64).collect();
    let ts: Vec<f64> = rows.iter().map(|(_, t)| *t).collect();
    let task_index: Vec<i64> = vec![0; rows.len()];
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
            Arc::new(Int64Array::from(task_index)),
        ],
    )
    .expect("build record batch");

    let file = fs::File::create(path).expect("create parquet file");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("parquet writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close parquet");
}
