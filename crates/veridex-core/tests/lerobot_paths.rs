//! Which files on disk a LeRobot manifest can make Veridex open, and which it cannot.
//!
//! Two opposite failures meet in the same code. A manifest is untrusted content: a feature key is a
//! JSON object key an attacker chooses, and it is joined onto the dataset directory to find that
//! feature's video, so it must never reach outside. But the ordinary on-disk shape of a downloaded
//! dataset — `huggingface_hub`'s `snapshot_download` — is a tree of symlinks into the blob cache, so
//! refusing every symlink refuses every real download.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::{Adapter, IngestOptions, Ingested, Source};
use veridex_core::cdm::MediaStatus;

/// A LeRobot dataset with one video feature named `feature` and one two-row episode.
fn write_dataset(dir: &Path, feature: &str) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 30.0,
        "features": { feature: { "dtype": "video", "shape": [480, 640, 3] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();
    write_parquet(&dir.join("data/chunk-000/file-000.parquet"));
}

fn write_parquet(path: &Path) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![0i64, 0])) as ArrayRef,
            Arc::new(Int64Array::from(vec![0i64, 1])),
            Arc::new(Float64Array::from(vec![0.0f64, 0.0333])),
        ],
    )
    .unwrap();
    let mut writer = ArrowWriter::try_new(fs::File::create(path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn ingest(dir: &Path) -> Ingested {
    LeRobotAdapter
        .ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
        .expect("lerobot ingest")
}

/// Bytes that parse as an MP4 well enough to be distinguishable from "unreadable" — if the adapter
/// ever reads the file, the status says so.
fn plausible_container() -> Vec<u8> {
    let mut out = 20u32.to_be_bytes().to_vec();
    out.extend_from_slice(b"ftypisom");
    out.extend_from_slice(b"\0\0\0\0isom");
    out
}

fn media_status(dir: &Path, feature: &str) -> MediaStatus {
    let ds = ingest(dir).dataset;
    ds.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == feature)
        .and_then(|s| s.media.as_ref())
        .map(|m| m.status.clone())
        .expect("the video feature carries media")
}

// ---- a manifest must not reach outside the dataset ----------------------------------------------

/// A feature key of `../../secret` used to be joined straight onto `<dir>/videos/`, and the file
/// that landed on was opened and its headers copied into the CDM — which is bound into the content
/// hash and the signed certificate. `MediaStatus` then reports missing vs. unreadable vs. read, so
/// the published verdict is an existence-and-content oracle over the host filesystem.
#[test]
fn a_relative_traversal_in_a_feature_name_is_refused_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("secret");
    fs::write(&outside, plausible_container()).unwrap();

    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "../../secret");

    match media_status(&dir, "../../secret") {
        MediaStatus::Unreadable { reason } => assert!(
            reason.contains("outside the dataset directory"),
            "the refusal must name why: {reason}"
        ),
        other => panic!("a traversing path must be refused, got {other:?}"),
    }
    // The file itself is untouched and, more to the point, unread: nothing about its contents
    // reached the CDM.
    assert!(outside.is_file());
}

/// An absolute feature name is worse than `..`: `Path::join` discards the base entirely, so the
/// manifest names any file on the host directly.
#[test]
fn an_absolute_feature_name_is_refused_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside.mp4");
    fs::write(&outside, plausible_container()).unwrap();

    let dir = tmp.path().join("dataset");
    let feature = outside.to_str().unwrap().to_string();
    write_dataset(&dir, &feature);

    match media_status(&dir, &feature) {
        MediaStatus::Unreadable { reason } => {
            assert!(reason.contains("outside the dataset directory"), "{reason}")
        }
        other => panic!("an absolute path must be refused, got {other:?}"),
    }
}

/// A symlink *inside* the dataset pointing out of it is the case the component filter cannot see,
/// which is why containment is re-checked after resolution.
#[test]
fn a_symlink_leading_out_of_the_dataset_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("secret.mp4");
    fs::write(&outside, plausible_container()).unwrap();

    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "cam");
    fs::create_dir_all(dir.join("videos/cam")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dir.join("videos/cam/episode_000000.mp4")).unwrap();

    #[cfg(unix)]
    match media_status(&dir, "cam") {
        MediaStatus::Unreadable { reason } => {
            assert!(reason.contains("outside the dataset directory"), "{reason}")
        }
        other => panic!("a symlink out of the dataset must be refused, got {other:?}"),
    }
}

// ---- but the shape a real download has on disk must resolve --------------------------------------

/// `snapshot_download` materializes every file as a symlink into the blob cache. Refusing those read
/// a sound dataset as having no video at all.
#[cfg(unix)]
#[test]
fn a_symlinked_media_file_inside_the_dataset_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "cam");

    // The blob cache lives inside the snapshot root, exactly as the hub client lays it out.
    let blobs = dir.join("blobs");
    fs::create_dir_all(&blobs).unwrap();
    let blob = blobs.join("deadbeef");
    fs::write(&blob, plausible_container()).unwrap();

    fs::create_dir_all(dir.join("videos/cam")).unwrap();
    std::os::unix::fs::symlink(&blob, dir.join("videos/cam/episode_000000.mp4")).unwrap();

    // Read or Unreadable are both fine here — what matters is that the file was found and opened.
    // Missing (the old behavior) and the containment refusal are not.
    match media_status(&dir, "cam") {
        MediaStatus::Missing => panic!("a symlinked video that is present was reported missing"),
        MediaStatus::Unreadable { reason } => assert!(
            !reason.contains("outside the dataset directory"),
            "a blob inside the dataset root is not outside it: {reason}"
        ),
        MediaStatus::Read | MediaStatus::Unattributable { .. } => {}
    }
}

/// The same for data: a symlinked shard is the normal case, and refusing it ingested zero episodes
/// while still claiming `Coverage::Full`.
#[cfg(unix)]
#[test]
fn a_symlinked_parquet_shard_inside_the_dataset_resolves() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "cam");

    // Move the real shard into a blob directory and leave a symlink where it was.
    let blobs = dir.join("blobs");
    fs::create_dir_all(&blobs).unwrap();
    let shard = dir.join("data/chunk-000/file-000.parquet");
    let blob = blobs.join("cafebabe");
    fs::rename(&shard, &blob).unwrap();
    std::os::unix::fs::symlink(&blob, &shard).unwrap();

    let ds = ingest(&dir).dataset;
    assert_eq!(ds.episodes.len(), 1, "the symlinked shard must be read");
    assert_eq!(ds.episodes[0].streams[0].frames.len(), 2);
}

/// A symlinked *directory* stays refused — one pointing at an ancestor would send the walk into
/// unbounded recursion, and surviving malformed input is the point.
#[cfg(unix)]
#[test]
fn a_symlinked_directory_pointing_at_an_ancestor_does_not_recurse() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "cam");
    std::os::unix::fs::symlink(&dir, dir.join("data/loop")).unwrap();

    // The point is that this returns at all.
    let ds = ingest(&dir).dataset;
    assert_eq!(ds.episodes.len(), 1);
}
