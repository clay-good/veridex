//! The ceiling on one source file read whole into memory.
//!
//! MCAP, ASAM MF4 and a rosbag2 `.db3` are random-access containers — the summary is at the end of
//! the file, the block graph is a web of offsets, SQLite's b-tree walk seeks — so each is read whole
//! by design. That makes the allocation the file's size, and a file far past what the machine can
//! hold does not fail with a verdict: a failed allocation aborts the process, and the OOM killer
//! does not wait for that. Either way the run dies with no report and no clue that size was the
//! problem. So the size is refused on `stat`, before the read.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use veridex_core::adapter::{
    default_registry, IngestError, IngestOptions, Source, DEFAULT_MAX_SOURCE_BYTES,
};

/// A ceiling below every fixture here, so the refusal is what is under test rather than the size.
fn capped(limit: u64) -> IngestOptions {
    IngestOptions {
        max_source_bytes: Some(limit),
        ..IngestOptions::default()
    }
}

fn ingest(path: &Path, options: &IngestOptions) -> Result<(), IngestError> {
    default_registry()
        .ingest(&Source::Local(path.to_path_buf()), options)
        .map(|_| ())
}

fn rosbag2_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rosbag2")
        .join(name)
}

/// A small, valid MCAP with one channel and one message.
fn write_mcap(dir: &Path) -> PathBuf {
    let mut out = Vec::new();
    {
        let mut writer = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let schema = writer
            .add_schema("sensor_msgs/msg/Imu", "ros2msg", b"")
            .expect("schema");
        let channel = writer
            .add_channel(schema, "/imu", "cdr", &BTreeMap::new())
            .expect("channel");
        writer
            .write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: channel,
                    sequence: 0,
                    log_time: 1_000_000_000,
                    publish_time: 1_000_000_000,
                },
                b"payload",
            )
            .expect("message");
        writer.finish().expect("finish");
    }
    let path = dir.join("recording.mcap");
    std::fs::write(&path, &out).expect("write");
    path
}

#[test]
fn an_mcap_past_the_ceiling_is_refused_by_name_and_pointed_at_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_mcap(dir.path());
    let size = std::fs::metadata(&path).unwrap().len();

    let err = ingest(&path, &capped(1)).expect_err("a file over the ceiling is refused");
    assert!(
        matches!(
            err,
            IngestError::SourceTooLarge { format_id, limit, size: reported, .. }
                if format_id == "mcap" && limit == 1 && reported == size
        ),
        "{err}"
    );
    let text = err.to_string();
    assert!(
        text.contains("--metadata-only") && text.contains("--max-source-bytes"),
        "the refusal has to say what to do about it: {text}"
    );

    // And the way out actually works: the summary read never touches the ceiling, because it never
    // holds the file. A hint naming an escape the format does not have would be worse than none.
    ingest(
        &path,
        &IngestOptions {
            metadata_only: true,
            ..capped(1)
        },
    )
    .expect("a metadata-only MCAP run reads the summary, not the file");

    // The default ceiling is far above a real recording, so nothing normal is refused by it.
    assert!(size < DEFAULT_MAX_SOURCE_BYTES);
    ingest(&path, &IngestOptions::default()).expect("an ordinary recording ingests");
}

#[test]
fn a_rosbag2_shard_past_the_ceiling_is_refused_before_any_shard_is_opened() {
    let bag = rosbag2_fixture("clean_rig");
    let err = ingest(&bag, &capped(1)).expect_err("a shard over the ceiling is refused");
    assert!(
        matches!(err, IngestError::SourceTooLarge { format_id, .. } if format_id == "rosbag2"),
        "{err}"
    );
    assert!(err.to_string().contains("metadata.yaml"), "{err}");

    ingest(
        &bag,
        &IngestOptions {
            metadata_only: true,
            ..capped(1)
        },
    )
    .expect("a metadata-only rosbag2 run opens no shard");
}

/// The MF4 reader holds the file even for a header-only run, so its refusal must not promise
/// `--metadata-only` as a way out. The check still has to come *before* the parse: a file refused on
/// size is refused whatever its contents are.
#[test]
fn an_mf4_past_the_ceiling_is_refused_before_it_is_parsed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("measurement.mf4");
    // An identification block is all detection reads; the rest of this file is nothing at all, so a
    // parse would fail — and must not be what happens.
    let mut bytes = vec![0u8; 64];
    bytes[0..8].copy_from_slice(b"MDF     ");
    bytes[8..16].copy_from_slice(b"4.10    ");
    bytes[28..30].copy_from_slice(&410u16.to_le_bytes());
    std::fs::write(&path, &bytes).unwrap();

    for options in [
        capped(1),
        IngestOptions {
            metadata_only: true,
            ..capped(1)
        },
    ] {
        let err = ingest(&path, &options).expect_err("a file over the ceiling is refused");
        assert!(
            matches!(err, IngestError::SourceTooLarge { format_id, .. } if format_id == "mf4"),
            "the size is refused before the parse, in both modes: {err}"
        );
        assert!(
            !err.to_string().contains("--metadata-only"),
            "an MF4 header-only run holds the file too, so it is not a way out: {err}"
        );
    }
}

#[test]
fn a_ceiling_of_none_removes_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_mcap(dir.path());
    ingest(
        &path,
        &IngestOptions {
            max_source_bytes: None,
            ..IngestOptions::default()
        },
    )
    .expect("no ceiling, no refusal");
}
