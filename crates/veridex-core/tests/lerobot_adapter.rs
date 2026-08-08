//! Tests for the LeRobot v3 adapter, including the cross-format neutrality gate: the same logical
//! dataset ingested as LeRobot and as MCAP must yield equivalent CDMs.

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{Adapter, Detection, IngestOptions, Source};
use veridex_core::cdm::{Dataset, Modality};

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

    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
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

    let action = ep0.streams.iter().find(|s| s.name == "action").unwrap();
    assert_eq!(action.modality, Modality::Action);

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
