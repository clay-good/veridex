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
    write_parquet(&dir.join("data/chunk-000/file-000.parquet"), feature);
}

/// The declared feature gets a real column: a manifest declaring one the Parquet does not hold is
/// itself a defect Veridex reports (`STRUCTURAL.EMPTY_STREAM` over an unread source), and these
/// tests are about where the *video files* resolve, not about that.
fn write_parquet(path: &Path, feature: &str) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new(feature, DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![0i64, 0])) as ArrayRef,
            Arc::new(Int64Array::from(vec![0i64, 1])),
            Arc::new(Float64Array::from(vec![0.0f64, 0.0333])),
            Arc::new(Float64Array::from(vec![1.0f64, 2.0])),
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

/// A shard symlinked *out* of the dataset is not the dataset's data, and must not be read.
///
/// `path_is_inside` (then `media_path_is_inside`) guarded exactly this for video, and
/// `probe_stream_media` was its only caller — the data walk was documented as "inside by
/// construction", which following symlinks had already made untrue. So a published dataset could
/// ship `data/chunk-000/file-000.parquet -> /home/victim/payroll.parquet` and anyone running
/// `veridex check` on it read that file: its columns' min/max/mean/std and a SHA-256 per cell went
/// into the CDM, which is content-hashed, printed, and signed into a certificate the victim might
/// then hand to someone else.
#[cfg(unix)]
#[test]
fn a_shard_symlinked_out_of_the_dataset_is_not_read() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("dataset");
    write_dataset(&dir, "cam");

    // The victim's file lives entirely outside the dataset, as it would on a real machine.
    let outside = tmp.path().join("victim");
    fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("payroll.parquet");
    let shard = dir.join("data/chunk-000/file-000.parquet");
    fs::rename(&shard, &secret).unwrap();
    std::os::unix::fs::symlink(&secret, &shard).unwrap();

    let ingested: Ingested = LeRobotAdapter
        .ingest(&Source::Local(dir.clone()), &IngestOptions::default())
        .expect("a dataset whose shard points outside still ingests, with nothing from it");

    let frames: usize = ingested
        .dataset
        .episodes
        .iter()
        .flat_map(|e| e.streams.iter())
        .map(|s| s.frames.len())
        .sum();
    assert_eq!(
        frames, 0,
        "not one row of a file outside the dataset may reach the CDM"
    );
    // Disclosed by name, not silently skipped: otherwise this reads as a dataset that merely holds
    // fewer episodes than its manifest declares.
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|f| f.note.contains("outside the dataset directory")),
        "the escape must be reported: {:?}",
        ingested.report.unread_sources
    );

    // ...and the disclosure has to reach the verdict, which is the whole reason it is recorded.
    // `unread_sources` used to share a vector with the benign "the CDM has no shape for this" notes,
    // and that vector is rendered by `inspect` alone. `check`, `certify`, and `diff` all consume a
    // `Verdict`, which never saw it — so a dataset with one of its two shards pointed out of the
    // directory produced the same `coverage: Full`, the same findings, the same score, and a
    // certifiable verdict naming the whole dataset over the half that was read.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run_over_with_unread(
        &ingested.dataset,
        hash,
        &veridex_core::RunConfig::default(),
        veridex_core::engine::CoverageNote::Full,
        &ingested.report.unread_sources,
    );
    let unread = verdict
        .findings
        .iter()
        .find(|f| f.code == "COVERAGE.SOURCE_UNREAD")
        .expect(
            "the unread shard must be a finding: findings are the only channel that reaches \
                 SARIF, the diff, and the certificate",
        );
    assert!(
        unread.message.contains(".parquet"),
        "the finding must name the file that was not read: {}",
        unread.message
    );
}
