//! Tests for the LeRobot v3 adapter, including the cross-format neutrality gate: the same logical
//! dataset ingested as LeRobot and as MCAP must yield equivalent CDMs.

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, FixedSizeListArray, Float32Array, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{Adapter, Detection, IngestOptions, Source};
use veridex_core::cdm::{Dataset, Modality};
use veridex_core::check::Severity;

/// Write a LeRobot v3 dataset directory with the given features and rows.
/// `rows` are (episode_index, timestamp_seconds).
fn write_lerobot(dir: &Path, features: &[(&str, &str)], fps: f64, rows: &[(i64, f64)]) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();

    let features_json: serde_json::Map<String, serde_json::Value> = features
        .iter()
        .map(|(name, dtype)| {
            (
                name.to_string(),
                serde_json::json!({ "dtype": dtype, "shape": [1] }),
            )
        })
        .collect();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
        "features": features_json,
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    write_frames_parquet(&dir.join("data/chunk-000/file-000.parquet"), rows);
}

/// Write one `.parquet` shard with the `episode_index` / `frame_index` / `timestamp` columns from
/// `rows`. The parent directory must already exist.
fn write_frames_parquet(path: &Path, rows: &[(i64, f64)]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
    ]));
    let eps: Vec<i64> = rows.iter().map(|(e, _)| *e).collect();
    let frames: Vec<i64> = rows.iter().enumerate().map(|(i, _)| i as i64).collect();
    let ts: Vec<f64> = rows.iter().map(|(_, t)| *t).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
        ],
    )
    .unwrap();

    let file = fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn ingest_lerobot(dir: &Path) -> Dataset {
    LeRobotAdapter
        .ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
        .expect("lerobot ingest")
        .dataset
}

/// Write a LeRobot dataset with a single `float32` feature column carrying real per-row values, so
/// the adapter's content-hash fingerprinting is exercised. `rows` are (episode_index, timestamp_s,
/// feature_value).
fn write_lerobot_with_values(dir: &Path, feature: &str, fps: f64, rows: &[(i64, f64, f32)]) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
        "features": { feature: { "dtype": "float32", "shape": [1] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new(feature, DataType::Float32, false),
    ]));
    let eps: Vec<i64> = rows.iter().map(|(e, _, _)| *e).collect();
    let frames: Vec<i64> = (0..rows.len() as i64).collect();
    let ts: Vec<f64> = rows.iter().map(|(_, t, _)| *t).collect();
    let vals: Vec<f32> = rows.iter().map(|(_, _, v)| *v).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
            Arc::new(Float32Array::from(vals)),
        ],
    )
    .unwrap();
    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// Build an MCAP with the same logical streams and timestamps.
fn write_mcap(path: &Path, channels: &[(&str, &str, &[u64])]) {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).unwrap();
        for (schema, topic, times) in channels {
            let sid = w.add_schema(schema, "ros2msg", b"").unwrap();
            let cid = w.add_channel(sid, topic, "cdr", &BTreeMap::new()).unwrap();
            for (seq, &t) in times.iter().enumerate() {
                w.write_to_known_channel(
                    &mcap::records::MessageHeader {
                        channel_id: cid,
                        sequence: seq as u32,
                        log_time: t,
                        publish_time: t,
                    },
                    b"x",
                )
                .unwrap();
            }
        }
        w.finish().unwrap();
    }
    fs::write(path, &out).unwrap();
}

fn ingest_mcap(path: &Path) -> Dataset {
    McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("mcap ingest")
        .dataset
}

/// (stream name, modality, frame timestamps).
type StreamSig = (String, Modality, Vec<i64>);
/// Per-episode structural signature: (episode index, its streams).
type EpisodeSig = (u64, Vec<StreamSig>);

/// The structural signature used to compare CDMs across formats: per episode, the sorted set of
/// (stream name, modality, frame timestamps). Excludes format-specific fields (clock id, declared
/// rate, provenance) per the ingestion spec's equivalence definition.
fn signature(d: &Dataset) -> Vec<EpisodeSig> {
    let mut eps: Vec<EpisodeSig> = d
        .episodes
        .iter()
        .map(|ep| {
            let mut streams: Vec<StreamSig> = ep
                .streams
                .iter()
                .map(|s| {
                    (
                        s.name.clone(),
                        s.modality,
                        s.frames.iter().map(|f| f.ts).collect(),
                    )
                })
                .collect();
            streams.sort_by(|a, b| a.0.cmp(&b.0));
            (ep.index, streams)
        })
        .collect();
    eps.sort_by_key(|(i, _)| *i);
    eps
}

#[test]
fn detects_lerobot_directory() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32")],
        10.0,
        &[(0, 0.0), (0, 0.1)],
    );
    assert!(matches!(
        LeRobotAdapter.detect(&Source::Local(dir.path().to_path_buf())),
        Detection::Yes { .. }
    ));
    // A directory without meta/info.json is not detected.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        LeRobotAdapter.detect(&Source::Local(empty.path().to_path_buf())),
        Detection::No
    );
}

#[test]
fn maps_features_to_streams_and_groups_by_episode() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[
            ("observation.images.top", "video"),
            ("observation.state", "float32"),
            ("action", "float32"),
        ],
        10.0,
        // episode 0: 3 frames; episode 1: 2 frames.
        &[(0, 0.0), (0, 0.1), (0, 0.2), (1, 0.0), (1, 0.1)],
    );
    let d = ingest_lerobot(dir.path());
    assert_eq!(d.episodes.len(), 2);

    let ep0 = &d.episodes[0];
    assert_eq!(ep0.index, 0);
    assert_eq!(ep0.streams.len(), 3, "one stream per data feature");
    let cam = ep0
        .streams
        .iter()
        .find(|s| s.name == "observation.images.top")
        .unwrap();
    assert_eq!(cam.modality, Modality::Video);
    assert_eq!(cam.declared_rate_hz, Some(10.0));
    assert_eq!(cam.frames.len(), 3);
    // 0.2 s => 200_000_000 ns
    assert_eq!(cam.frames[2].ts, 200_000_000);
    // Declared dtype/shape are read straight from meta/info.json (the fixture writes shape [1]).
    assert_eq!(cam.dtype.as_deref(), Some("video"));
    assert_eq!(cam.shape.as_deref(), Some(&[1u64][..]));

    let action = ep0.streams.iter().find(|s| s.name == "action").unwrap();
    assert_eq!(action.modality, Modality::Action);
    assert_eq!(action.dtype.as_deref(), Some("float32"));

    assert_eq!(d.episodes[1].streams[0].frames.len(), 2);
}

#[test]
fn provenance_records_robot_type_but_does_not_fabricate() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32")],
        10.0,
        &[(0, 0.0)],
    );
    let d = ingest_lerobot(dir.path());
    let sensor = d
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == "sensor")
        .unwrap();
    assert_eq!(sensor.value.as_deref(), Some("so100"));
}

#[test]
fn provenance_reads_license_from_the_dataset_card() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32")],
        10.0,
        &[(0, 0.0)],
    );
    // A Hugging Face dataset card: YAML frontmatter with a license, then prose.
    fs::write(
        dir.path().join("README.md"),
        "---\nlicense: apache-2.0\ntags:\n- robotics\n---\n\n# My dataset\n",
    )
    .unwrap();
    let d = ingest_lerobot(dir.path());
    let license = d
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == "license")
        .expect("license extracted from the card");
    assert_eq!(license.value.as_deref(), Some("apache-2.0"));
    assert_eq!(license.class, veridex_core::cdm::ProvenanceClass::Known);

    // With a real license, the completeness check must no longer report it missing.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(
        verdict
            .findings
            .iter()
            .all(|f| f.code != "PROVENANCE.MISSING_LICENSE"),
        "an extracted license clears the MISSING_LICENSE finding"
    );
}

#[test]
fn license_list_form_and_missing_card_are_handled() {
    // YAML list form: `license:` then `- <value>` on the next line.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(dir.path(), &[("s", "float32")], 10.0, &[(0, 0.0)]);
    fs::write(dir.path().join("README.md"), "---\nlicense:\n- mit\n---\n").unwrap();
    let d = ingest_lerobot(dir.path());
    let license = d
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == "license");
    assert_eq!(license.and_then(|e| e.value.as_deref()), Some("mit"));

    // No card at all: no license element is fabricated.
    let dir2 = tempfile::tempdir().unwrap();
    write_lerobot(dir2.path(), &[("s", "float32")], 10.0, &[(0, 0.0)]);
    let d2 = ingest_lerobot(dir2.path());
    assert!(d2
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .all(|e| e.key != "license"));
}

#[test]
fn reads_stored_stats_from_stats_json() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32")],
        10.0,
        &[(0, 0.0), (0, 0.1)],
    );
    // A constant (degenerate) stored stat for the feature.
    let stats = serde_json::json!({
        "observation.state": { "min": [2.0], "max": [2.0], "mean": [2.0], "std": [0.0] }
    });
    fs::write(
        dir.path().join("meta/stats.json"),
        serde_json::to_string(&stats).unwrap(),
    )
    .unwrap();

    let d = ingest_lerobot(dir.path());
    let s = &d.episodes[0].streams[0];
    let st = s.stats.expect("stats populated from stats.json");
    assert_eq!(st.min, 2.0);
    assert_eq!(st.std, 0.0);

    // The statistical check flags the constant stream as degenerate.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "STATISTICAL.DEGENERATE"));
}

#[test]
fn frames_are_aggregated_across_multiple_parquet_shards() {
    // Real LeRobot datasets spread episodes across many shards in separate chunk dirs. The adapter
    // must recurse into every chunk (find_parquet) and collect all episodes, not just the first
    // shard — a silent single-shard read would drop most of a real dataset.
    let dir = tempfile::tempdir().unwrap();
    // chunk-000 (written by write_lerobot): episodes 0 and 1.
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32")],
        10.0,
        &[(0, 0.0), (0, 0.1), (1, 0.0), (1, 0.1), (1, 0.2)],
    );
    // A second shard in a separate chunk dir: episode 2. Only reached if find_parquet recurses.
    fs::create_dir_all(dir.path().join("data/chunk-001")).unwrap();
    write_frames_parquet(
        &dir.path().join("data/chunk-001/file-000.parquet"),
        &[(2, 0.0), (2, 0.1)],
    );

    let d = ingest_lerobot(dir.path());
    // Every episode from both shards is present, in ascending index order.
    assert_eq!(
        d.episodes.iter().map(|e| e.index).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    // Each episode's frame count reflects the rows in whichever shard held it.
    assert_eq!(d.episodes[0].streams[0].frames.len(), 2);
    assert_eq!(d.episodes[1].streams[0].frames.len(), 3);
    assert_eq!(d.episodes[2].streams[0].frames.len(), 2);
}

#[test]
fn a_truncated_episode_is_flagged_as_a_duration_outlier_end_to_end() {
    // Five episodes at 30 Hz through the real adapter: four full ~1 s captures (30 frames) and one
    // cut short right after it began (3 frames, ~0.07 s). The short episode's rate, spacing, and
    // counts are all otherwise correct, so the duration-outlier check must be what catches it.
    let fps = 30.0;
    let mut rows: Vec<(i64, f64)> = Vec::new();
    for ep in 0..4i64 {
        for f in 0..30i64 {
            rows.push((ep, f as f64 / fps));
        }
    }
    for f in 0..3i64 {
        rows.push((4, f as f64 / fps));
    }

    let dir = tempfile::tempdir().unwrap();
    write_lerobot(dir.path(), &[("observation.state", "float32")], fps, &rows);
    let d = ingest_lerobot(dir.path());
    assert_eq!(d.episodes.len(), 5);

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let outlier = verdict
        .findings
        .iter()
        .find(|f| f.code == "TEMPORAL.EPISODE_DURATION_OUTLIER")
        .expect("the truncated episode is flagged as a duration outlier");
    assert!(matches!(
        outlier.location,
        veridex_core::check::Location::Episode { episode: 4 }
    ));
}

#[test]
fn same_logical_dataset_yields_equivalent_cdms_across_formats() {
    // One episode, two frame-aligned streams at 0.0/0.1/0.2 s.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32"), ("action", "float32")],
        10.0,
        &[(0, 0.0), (0, 0.1), (0, 0.2)],
    );
    let lerobot_cdm = ingest_lerobot(dir.path());

    let mcap_path = dir.path().join("equiv.mcap");
    let times: &[u64] = &[0, 100_000_000, 200_000_000];
    write_mcap(
        &mcap_path,
        &[
            ("sensor_msgs/msg/JointState", "observation.state", times),
            ("std_msgs/msg/Float64", "action", times),
        ],
    );
    let mcap_cdm = ingest_mcap(&mcap_path);

    // The neutrality proof: equivalent episodes, streams, modalities, and timestamps.
    assert_eq!(
        signature(&lerobot_cdm),
        signature(&mcap_cdm),
        "same logical dataset must produce equivalent CDMs across LeRobot and MCAP"
    );
}

#[test]
fn lerobot_dataset_flows_through_the_full_check_pipeline() {
    // A clean two-episode LeRobot dataset, sampled at the declared rate, should ingest and pass the
    // standard engine end-to-end (ingest -> CDM -> every check). Exercises the LeRobot -> checks
    // path as a whole, not just the adapter in isolation.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("observation.state", "float32"), ("action", "float32")],
        10.0, // 10 Hz => 0.1 s spacing, matching the timestamps below
        &[(0, 0.0), (0, 0.1), (0, 0.2), (1, 0.0), (1, 0.1), (1, 0.2)],
    );
    let d = ingest_lerobot(dir.path());

    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());

    // Every standard check ran, none errored, and clean data yields no failing findings.
    assert_eq!(verdict.executed_checks.len(), 28);
    assert!(
        verdict.errored_checks.is_empty(),
        "no check should error on a well-formed dataset"
    );
    // No structural/temporal errors on clean data — the only non-info finding is the missing
    // license (a Warning by rubric), so the run passes with warnings rather than failing.
    assert!(
        !verdict
            .findings
            .iter()
            .any(|f| f.severity == Severity::Error),
        "a clean dataset should produce no error findings"
    );
    assert_eq!(
        verdict.status,
        veridex_core::Status::PassWithWarnings,
        "clean data with no declared license passes with a license warning"
    );
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "PROVENANCE.MISSING_LICENSE"),
        "the fixture declares no license, so the license warning must appear"
    );
}

/// Write a LeRobot v3 dataset whose data Parquet carries a `task_index` column, plus a
/// `meta/tasks.jsonl` mapping those indices to task strings. `rows` are (episode, ts_s, task_index).
fn write_lerobot_with_tasks(dir: &Path, fps: f64, rows: &[(i64, f64, i64)], tasks: &[(i64, &str)]) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();

    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": fps,
        "robot_type": "so100",
        "features": { "action": { "dtype": "float32", "shape": [1] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    let tasks_jsonl: String = tasks
        .iter()
        .map(|(i, t)| serde_json::json!({ "task_index": i, "task": t }).to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(dir.join("meta/tasks.jsonl"), tasks_jsonl).unwrap();

    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("task_index", DataType::Int64, false),
    ]));
    let eps: Vec<i64> = rows.iter().map(|(e, _, _)| *e).collect();
    let frames: Vec<i64> = (0..rows.len() as i64).collect();
    let ts: Vec<f64> = rows.iter().map(|(_, t, _)| *t).collect();
    let tix: Vec<i64> = rows.iter().map(|(_, _, ti)| *ti).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
            Arc::new(Int64Array::from(tix)),
        ],
    )
    .unwrap();
    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

#[test]
fn a_mid_episode_task_change_becomes_a_timestamped_language_label() {
    // Episode 0 starts on task 0 and switches to task 1 at ts 0.2s — a real multi-task episode.
    // The adapter surfaces the switch as one timestamped `language` label; the episode's primary
    // task stays the first. A single-task episode 1 produces no labels.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_tasks(
        dir.path(),
        10.0,
        &[
            (0, 0.0, 0),
            (0, 0.1, 0),
            (0, 0.2, 1),
            (0, 0.3, 1),
            (1, 0.0, 2),
            (1, 0.1, 2),
        ],
        &[(0, "grasp the cube"), (1, "lift the cube"), (2, "hold")],
    );
    let d = ingest_lerobot(dir.path());

    // Episode 0: primary task is the first; exactly one language label at the switch (ts 0.2s).
    assert_eq!(d.episodes[0].task.as_deref(), Some("grasp the cube"));
    let labels: Vec<_> = d.episodes[0]
        .labels
        .iter()
        .filter(|l| l.key == "language")
        .collect();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].value, "lift the cube");
    assert!(labels[0].ts.is_some());
    // Single-task episode 1 carries no transition labels.
    assert!(d.episodes[1].labels.is_empty());

    // The surfaced label is aligned within the episode, so the integrity check stays clean.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .all(|f| !f.code.starts_with("SEMANTIC.ANNOTATION")));
}

#[test]
fn task_index_is_resolved_to_episode_task_via_tasks_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_tasks(
        dir.path(),
        10.0,
        &[(0, 0.0, 0), (0, 0.1, 0), (1, 0.0, 1), (1, 0.1, 1)],
        &[(0, "pick up the red cube"), (1, "hold")],
    );
    let ingested = LeRobotAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("lerobot ingest");
    let d = &ingested.dataset;

    // Each episode's task_index resolved to its string.
    assert_eq!(d.episodes[0].task.as_deref(), Some("pick up the red cube"));
    assert_eq!(d.episodes[1].task.as_deref(), Some("hold"));

    // The fidelity report records the resolution as a mapped field, not omitted.
    assert!(ingested
        .report
        .mapped_fields
        .iter()
        .any(|f| f.contains("meta/tasks.jsonl -> episode.task")));

    // The placeholder task ("hold") is surfaced by the semantic check via the real adapter path.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(d);
    let verdict = engine.run(d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "SEMANTIC.PLACEHOLDER_TASK"));
}

#[test]
fn missing_tasks_jsonl_leaves_tasks_unresolved() {
    // The standard fixture writes no task_index column and no tasks.jsonl: tasks stay None and the
    // omission is reported honestly rather than fabricated.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot(
        dir.path(),
        &[("action", "float32")],
        10.0,
        &[(0, 0.0), (0, 0.1)],
    );
    let ingested = LeRobotAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("lerobot ingest");
    assert!(ingested.dataset.episodes[0].task.is_none());
    assert!(ingested
        .report
        .omitted_fields
        .iter()
        .any(|f| f.contains("no meta/tasks.jsonl")));
}

#[test]
fn feature_cells_are_fingerprinted_into_frame_content_hashes() {
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_values(
        dir.path(),
        "observation.state",
        10.0,
        &[(0, 0.0, 1.0), (0, 0.1, 2.0), (0, 0.2, 3.0)],
    );
    let d = ingest_lerobot(dir.path());
    let stream = &d.episodes[0].streams[0];
    assert_eq!(stream.name, "observation.state");
    // Every frame carries a content hash, and distinct feature values hash distinctly.
    let hashes: Vec<[u8; 32]> = stream
        .frames
        .iter()
        .map(|f| {
            f.value_ref
                .content_hash
                .expect("frame carries a content hash")
        })
        .collect();
    assert_eq!(hashes.len(), 3);
    assert!(
        hashes[0] != hashes[1] && hashes[1] != hashes[2],
        "different feature values must hash differently"
    );
}

#[test]
fn duplicate_episodes_are_detected_end_to_end_via_lerobot_content_hashes() {
    // Episodes 0 and 2 have identical timestamps AND feature values (a re-upload); episode 1 shares
    // the timing but has different values. Only the true content duplicates are flagged.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_values(
        dir.path(),
        "observation.state",
        10.0,
        &[
            (0, 0.0, 1.0),
            (0, 0.1, 2.0),
            (1, 0.0, 9.0),
            (1, 0.1, 8.0),
            (2, 0.0, 1.0),
            (2, 0.1, 2.0),
        ],
    );
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let dup: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STRUCTURAL.DUPLICATE_EPISODE")
        .collect();
    assert_eq!(dup.len(), 1, "the re-uploaded episode pair is flagged once");
    assert!(dup[0].message.contains('0') && dup[0].message.contains('2'));
    assert!(!dup[0].message.contains('1'));
}

#[test]
fn tampering_a_feature_value_changes_the_lerobot_cdm_hash() {
    // Two datasets identical in structure and timestamps; one feature value differs. Because feature
    // cells are fingerprinted into the CDM, the content hash must differ — so a tampered dataset can
    // no longer verify against a certificate bound to the original.
    let d0 = tempfile::tempdir().unwrap();
    write_lerobot_with_values(d0.path(), "action", 10.0, &[(0, 0.0, 1.0), (0, 0.1, 2.0)]);
    let d1 = tempfile::tempdir().unwrap();
    write_lerobot_with_values(d1.path(), "action", 10.0, &[(0, 0.0, 1.0), (0, 0.1, 2.5)]);
    let mut a = ingest_lerobot(d0.path());
    let mut b = ingest_lerobot(d1.path());
    a.id = "fixed".into();
    b.id = "fixed".into();
    assert_ne!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b),
        "a changed feature value must change the CDM hash"
    );
}

/// Write a `meta/stats.json` declaring one feature's stored [min, max] (mean/std filled plausibly).
fn write_stats(dir: &Path, feature: &str, min: f64, max: f64) {
    let stats = serde_json::json!({
        feature: { "min": [min], "max": [max], "mean": [(min + max) / 2.0], "std": [0.0] }
    });
    fs::write(dir.join("meta/stats.json"), stats.to_string()).unwrap();
}

#[test]
fn stored_stats_that_dont_bound_the_data_are_flagged_stale() {
    // Actual feature values span [1, 5]; the stored stats claim a too-narrow [1, 3], so a real value
    // (5) falls outside the stored range → STATISTICAL.STATS_STALE.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_values(
        dir.path(),
        "observation.state",
        10.0,
        &[(0, 0.0, 1.0), (0, 0.1, 3.0), (0, 0.2, 5.0)],
    );
    write_stats(dir.path(), "observation.state", 1.0, 3.0);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let stale: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.STATS_STALE")
        .collect();
    assert_eq!(stale.len(), 1, "the out-of-range stored stats are flagged");
    assert_eq!(stale[0].severity, Severity::Error);
}

#[test]
fn an_actuator_pinned_at_its_limit_is_flagged_saturated() {
    // 24 of 30 samples sit exactly at 1.0 (the action clamped against its stop); the rest vary.
    // >50% pinned at one distinct extreme over ≥20 samples → STATISTICAL.SATURATED, end to end.
    let dir = tempfile::tempdir().unwrap();
    let mut rows: Vec<(i64, f64, f32)> = Vec::new();
    for i in 0..30i64 {
        let ts = i as f64 * 0.1;
        let value = if i < 24 { 1.0 } else { -(i as f32) * 0.01 };
        rows.push((0, ts, value));
    }
    write_lerobot_with_values(dir.path(), "action", 10.0, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let sat: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.SATURATED")
        .collect();
    assert_eq!(sat.len(), 1, "the pinned actuator is flagged once");
    assert_eq!(sat[0].severity, Severity::Warning);
    assert!(sat[0].message.contains("maximum"));
}

#[test]
fn a_lone_value_spike_is_flagged_an_outlier() {
    // 150 frames hugging zero with one spike to 1000. A single point among N sits at most
    // sqrt(N-1) std out, so 150 frames put this ~12σ past the mean → STATISTICAL.OUTLIER, end to end
    // through the adapter's recomputed stats.
    let dir = tempfile::tempdir().unwrap();
    let mut rows: Vec<(i64, f64, f32)> = Vec::new();
    for i in 0..150i64 {
        let value = if i == 75 { 1000.0 } else { i as f32 * 0.001 };
        rows.push((0, i as f64 * 0.033, value));
    }
    write_lerobot_with_values(dir.path(), "action", 30.0, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let out: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.OUTLIER")
        .collect();
    assert_eq!(out.len(), 1, "the lone spike is flagged");
    assert_eq!(out[0].severity, Severity::Warning);
    assert!(out[0].message.contains("maximum"));
}

#[test]
fn a_nan_feature_value_is_flagged_non_finite_end_to_end() {
    // 30 frames, one whose value is NaN (a failed sensor read). No stats.json is written, so the
    // stored-stats NON_FINITE check has nothing to inspect; only the adapter's recompute over the
    // real cells sees it → STATISTICAL.NON_FINITE_OBSERVED, end to end.
    let dir = tempfile::tempdir().unwrap();
    let mut rows: Vec<(i64, f64, f32)> = Vec::new();
    for i in 0..30i64 {
        let value = if i == 15 { f32::NAN } else { i as f32 };
        rows.push((0, i as f64 * 0.1, value));
    }
    write_lerobot_with_values(dir.path(), "action", 10.0, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let nf: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.NON_FINITE_OBSERVED")
        .collect();
    assert_eq!(nf.len(), 1, "the NaN value is flagged once");
    assert_eq!(nf[0].severity, Severity::Error);
    // The stored-stats NON_FINITE check must not fire: there is no stats.json to inspect.
    assert!(verdict
        .findings
        .iter()
        .all(|f| f.code != "STATISTICAL.NON_FINITE"));
}

#[test]
fn all_finite_values_do_not_flag_non_finite() {
    // A clean stream: the adapter records Some(0), which must not fire.
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<(i64, f64, f32)> = (0..20i64).map(|i| (0, i as f64 * 0.1, i as f32)).collect();
    write_lerobot_with_values(dir.path(), "action", 10.0, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .all(|f| f.code != "STATISTICAL.NON_FINITE_OBSERVED"));
}

/// Write a LeRobot dataset with one multi-dimensional feature: a `FixedSizeList<Float32, width>`
/// column, `rows[i]` supplying the `width` scalars of frame `i` (all in episode 0).
fn write_lerobot_vector_feature(dir: &Path, feature: &str, width: i32, rows: &[Vec<f32>]) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10.0,
        "robot_type": "so100",
        "features": { feature: { "dtype": "float32", "shape": [width] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    let item = Arc::new(Field::new("item", DataType::Float32, true));
    let list_ty = DataType::FixedSizeList(item.clone(), width);
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new(feature, list_ty, false),
    ]));
    let eps: Vec<i64> = vec![0; rows.len()];
    let frames: Vec<i64> = (0..rows.len() as i64).collect();
    let ts: Vec<f64> = (0..rows.len()).map(|i| i as f64 * 0.1).collect();
    let flat: Vec<f32> = rows.iter().flatten().copied().collect();
    let values: ArrayRef = Arc::new(Float32Array::from(flat));
    let list = FixedSizeListArray::new(item, width, values, None);
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)),
            Arc::new(Int64Array::from(frames)),
            Arc::new(Float64Array::from(ts)),
            Arc::new(list),
        ],
    )
    .unwrap();
    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

#[test]
fn a_nan_in_a_non_first_dimension_is_flagged_non_finite() {
    // A 3-DoF feature whose element 0 is always healthy but joint 1 goes NaN on one frame. Since the
    // recompute scans every dimension, the buried NaN is still flagged — the common multi-DoF case a
    // first-scalar-only scan would miss entirely.
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<Vec<f32>> = (0..25i64)
        .map(|i| {
            let j1 = if i == 12 { f32::NAN } else { i as f32 * 0.2 };
            vec![i as f32, j1, i as f32 * -0.1]
        })
        .collect();
    write_lerobot_vector_feature(dir.path(), "observation.state", 3, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let nf: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.NON_FINITE_OBSERVED")
        .collect();
    assert_eq!(nf.len(), 1, "the NaN buried in joint 1 is flagged");
    assert_eq!(nf[0].severity, Severity::Error);
}

#[test]
fn a_saturating_gripper_at_the_last_dimension_is_flagged() {
    // A 3-DoF action whose first two joints sweep freely but the gripper (element 2) is pinned at
    // 1.0 for 24 of 30 frames — the most common real saturation, and one a first-scalar-only read
    // (element 0 varies) would miss entirely. The per-dimension recompute catches it.
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<Vec<f32>> = (0..30i64)
        .map(|i| {
            let gripper = if i < 24 {
                1.0
            } else {
                1.0 - (i - 23) as f32 * 0.1
            };
            vec![i as f32 * 0.5, -(i as f32) * 0.3, gripper]
        })
        .collect();
    write_lerobot_vector_feature(dir.path(), "action", 3, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let sat: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.SATURATED")
        .collect();
    assert_eq!(sat.len(), 1, "the pinned gripper dimension is flagged");
    assert!(sat[0].message.contains("maximum"));
    assert!(
        sat[0].message.contains("dimension 2"),
        "the finding names the saturating dimension: {}",
        sat[0].message
    );
}

#[test]
fn a_varied_actuator_is_not_saturated() {
    // Same stream, values spread across its range — no single rail dominates → no SATURATED.
    let dir = tempfile::tempdir().unwrap();
    let mut rows: Vec<(i64, f64, f32)> = Vec::new();
    for i in 0..30i64 {
        rows.push((0, i as f64 * 0.1, i as f32 * 0.03));
    }
    write_lerobot_with_values(dir.path(), "action", 10.0, &rows);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .all(|f| f.code != "STATISTICAL.SATURATED"));
}

#[test]
fn stored_stats_that_bound_the_data_are_clean() {
    // Correct (containing) stored stats → no STATS_STALE.
    let dir = tempfile::tempdir().unwrap();
    write_lerobot_with_values(
        dir.path(),
        "observation.state",
        10.0,
        &[(0, 0.0, 1.0), (0, 0.1, 3.0), (0, 0.2, 5.0)],
    );
    write_stats(dir.path(), "observation.state", 0.0, 10.0);
    let d = ingest_lerobot(dir.path());
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .all(|f| f.code != "STATISTICAL.STATS_STALE"));
}
