//! What the LeRobot adapter refuses, and what it admits it did not read.
//!
//! Every case here used to succeed. Each one produced a dataset that looked complete and was not:
//! a fabricated clock, a phantom episode, a shrunken episode set, or a fidelity report claiming a
//! file it never read. "Never read silence as a pass" applies to the adapter first — a check cannot
//! recover a measurement the ingest threw away.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::{Adapter, IngestOptions, IngestReport, Ingested, Source};

fn write_info(dir: &Path) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 30.0,
        "features": { "observation.state": { "dtype": "float32", "shape": [1] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();
}

/// Write the shard with a caller-chosen `timestamp` column, so its Arrow type is the variable.
fn write_shard(dir: &Path, episodes: Vec<i64>, timestamp: (Field, ArrayRef)) {
    let rows = episodes.len();
    let (ts_field, ts_array) = timestamp;
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        ts_field,
        // The feature `write_info` declares. Written for real: a manifest declaring a
        // (non-video) feature the Parquet does not hold is its own defect, and these tests are
        // about the `timestamp` column's Arrow type.
        Field::new("observation.state", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(episodes)) as ArrayRef,
            Arc::new(Int64Array::from((0..rows as i64).collect::<Vec<_>>())),
            ts_array,
            Arc::new(Float64Array::from(
                (0..rows).map(|i| i as f64).collect::<Vec<_>>(),
            )),
        ],
    )
    .unwrap();
    let path = dir.join("data/chunk-000/file-000.parquet");
    let mut writer = ArrowWriter::try_new(fs::File::create(path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn float_ts(values: Vec<f64>) -> (Field, ArrayRef) {
    (
        Field::new("timestamp", DataType::Float64, false),
        Arc::new(Float64Array::from(values)) as ArrayRef,
    )
}

fn ingest(dir: &Path) -> Result<Ingested, veridex_core::adapter::IngestError> {
    LeRobotAdapter.ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
}

fn report(dir: &Path) -> IngestReport {
    ingest(dir).expect("ingest").report
}

// ---- a clock the dataset recorded is never replaced with one Veridex invented --------------------

/// An int64 `timestamp` (nanoseconds, which several exporters write) read as unreadable per-cell,
/// which is indistinguishable from a null cell — and a null cell legitimately falls back to
/// `frame_index / fps`. Applied to the whole column, that fallback discarded the real clock and
/// substituted a flawless 30 Hz ladder still labelled `ClockKind::Measured`, so a five-second
/// mid-episode gap and a duplicate frame both vanished and every temporal check passed.
#[test]
fn an_integer_timestamp_column_is_refused_not_silently_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(
        &dir,
        vec![0, 0, 0],
        (
            Field::new("timestamp", DataType::Int64, false),
            // 0 s, 5 s, then 1 µs later — an enormous gap and a near-duplicate.
            Arc::new(Int64Array::from(vec![0i64, 5_000_000_000, 5_000_001_000])) as ArrayRef,
        ),
    );

    let err = ingest(&dir).expect_err("an unreadable timestamp column must be refused");
    let text = err.to_string();
    assert!(
        text.contains("timestamp") && text.contains("Int64"),
        "the refusal must name the column and the type it found: {text}"
    );
}

/// The null-cell fallback it must not have broken: one absent cell in a genuine float column still
/// falls back to `frame_index / fps`, which is the behavior the adapter has always had.
#[test]
fn a_null_cell_in_a_float_timestamp_column_still_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    let ts = Float64Array::from(vec![Some(0.0), None, Some(0.0667)]);
    write_shard(
        &dir,
        vec![0, 0, 0],
        (
            Field::new("timestamp", DataType::Float64, true),
            Arc::new(ts) as ArrayRef,
        ),
    );

    let ds = ingest(&dir).expect("a null cell is not a refusal").dataset;
    let frames = &ds.episodes[0].streams[0].frames;
    assert_eq!(frames.len(), 3);
    // Row 1 fell back to frame_index 1 / 30 fps.
    assert_eq!(frames[1].ts, 33_333_333);
}

// ---- an episode index is a real index -----------------------------------------------------------

/// `-1` is a sentinel some exporters write for an unassigned row. Cast straight to `u64` it became
/// 18446744073709551615 and those frames landed in a phantom episode that no declared length is ever
/// compared against — so the corrupt-manifest class this adapter exists to catch walked past.
#[test]
fn a_negative_episode_index_is_refused_not_wrapped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(&dir, vec![0, -1], float_ts(vec![0.0, 0.0333]));

    let err = ingest(&dir).expect_err("a negative episode index must be refused");
    let text = err.to_string();
    assert!(
        text.contains("-1") && text.contains("cannot be negative"),
        "the refusal must name the value: {text}"
    );
}

// ---- the fidelity report claims only what it did ------------------------------------------------

/// `meta/stats.json -> stream.stats` was pushed unconditionally, so a dataset with no stats file was
/// told the file had been read. The fidelity report is the artifact whose entire job is disclosing
/// what the run covered.
#[test]
fn an_absent_stats_file_is_reported_as_omitted_not_mapped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(&dir, vec![0, 0], float_ts(vec![0.0, 0.0333]));
    assert!(!dir.join("meta/stats.json").exists());

    let r = report(&dir);
    assert!(
        !r.mapped_fields.iter().any(|f| f.contains("stats.json")),
        "nothing read it: {:?}",
        r.mapped_fields
    );
    assert!(
        r.omitted_fields
            .iter()
            .any(|f| f.contains("stored summary statistics") && f.contains("no meta/stats.json")),
        "the omission must say why: {:?}",
        r.omitted_fields
    );
}

/// A corrupt stats file is worse than an absent one — it silently disables every stored-vs-observed
/// comparison — so it is distinguished from absence rather than lumped in with it.
#[test]
fn an_unparseable_stats_file_says_so_distinctly() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(&dir, vec![0, 0], float_ts(vec![0.0, 0.0333]));
    fs::write(dir.join("meta/stats.json"), "{ this is not json").unwrap();

    let r = report(&dir);
    assert!(!r.mapped_fields.iter().any(|f| f.contains("stats.json")));
    assert!(
        r.omitted_fields
            .iter()
            .any(|f| f.contains("could not be read")),
        "a corrupt file is not the same as an absent one: {:?}",
        r.omitted_fields
    );
}

/// A real stats file is still claimed.
#[test]
fn a_readable_stats_file_is_still_reported_as_mapped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(&dir, vec![0, 0], float_ts(vec![0.0, 0.0333]));
    let stats = serde_json::json!({
        "observation.state": { "min": [0.0], "max": [1.0], "mean": [0.5], "std": [0.1] }
    });
    fs::write(
        dir.join("meta/stats.json"),
        serde_json::to_string(&stats).unwrap(),
    )
    .unwrap();

    let r = report(&dir);
    assert!(
        r.mapped_fields
            .iter()
            .any(|f| f.contains("meta/stats.json -> stream.stats")),
        "{:?}",
        r.mapped_fields
    );
}

// ---- the declared episode set is not quietly shrunk ---------------------------------------------

/// Under `--metadata-only` this file *is* the episode set, so a line that will not parse read as a
/// smaller, perfectly clean dataset. It is the same corruption the duplicate-index arm already
/// refuses, arriving by a quieter route.
#[test]
fn an_unparseable_episodes_jsonl_line_is_refused_not_dropped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("ds");
    write_info(&dir);
    write_shard(&dir, vec![0, 1, 2], float_ts(vec![0.0, 0.0333, 0.0667]));
    fs::write(
        dir.join("meta/episodes.jsonl"),
        "{\"episode_index\": 0, \"length\": 1}\n\
         {\"episode_index\": 1, \"lenght\": 1}\n\
         {\"episode_index\": 2, \"length\": 1}\n",
    )
    .unwrap();

    let err = ingest(&dir).expect_err("a malformed manifest line must be refused");
    let text = err.to_string();
    assert!(
        text.contains("line 2"),
        "the refusal must name the line: {text}"
    );
}
