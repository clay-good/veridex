//! Generate a small demo LeRobot v3 dataset for trying the CLI end-to-end.
//!
//! Writes a minimal on-disk LeRobot v3 layout (`meta/info.json` + one Parquet shard) with two
//! episodes. By default the second episode has an out-of-order timestamp — a real data corruption
//! that `veridex check` catches as `TEMPORAL.NON_MONOTONIC` — so there is something to find. Pass
//! `clean` as the second argument for a well-formed dataset.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_lerobot -- <output-dir> [clean]`
//!
//! Then: `veridex check <output-dir>`.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-lerobot".to_string());
    // Pass "clean" for a well-formed dataset; the default injects a non-monotonic timestamp.
    let clean = std::env::args().nth(2).as_deref() == Some("clean");
    let dir = Path::new(&dir);

    write_dataset(dir, clean);

    let what = if clean {
        "clean (well-formed)"
    } else {
        "broken (episode 1 has an out-of-order timestamp → TEMPORAL.NON_MONOTONIC)"
    };
    println!("Wrote {what} LeRobot v3 dataset to {}", dir.display());
    println!("Try:  veridex check {}", dir.display());
}

fn write_dataset(dir: &Path, clean: bool) {
    fs::create_dir_all(dir.join("meta")).expect("create meta/");
    fs::create_dir_all(dir.join("data/chunk-000")).expect("create data/");

    let fps = 30.0;
    // Two features, both frame-aligned scalars. Only structure/timestamps are read by Veridex.
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
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

    // Two episodes of 10 frames each at ~30 Hz. In the broken variant, one frame in episode 1 is
    // pushed before its predecessor, breaking timestamp monotonicity within that episode.
    let mut rows: Vec<(i64, f64)> = Vec::new();
    for ep in 0..2i64 {
        for f in 0..10i64 {
            let t = f as f64 / fps;
            rows.push((ep, t));
        }
    }
    if !clean {
        // Episode 1, frame 5: rewind the clock behind frame 4 (an out-of-order/duplicated frame).
        let idx = 10 + 5;
        rows[idx].1 = rows[idx - 1].1 - 0.010;
    }

    write_parquet(&dir.join("data/chunk-000/file-000.parquet"), &rows);
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
