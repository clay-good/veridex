//! Behavior tests for the adapter contract and registry.

use veridex_core::adapter::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
};
use veridex_core::cdm::Dataset;

/// A minimal adapter that recognizes `Source::Local` paths ending in `.fake`.
struct FakeAdapter;

impl Adapter for FakeAdapter {
    fn format_id(&self) -> &'static str {
        "fake"
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["1"]
    }

    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(p) if p.extension().and_then(|e| e.to_str()) == Some("fake") => {
                Detection::Yes {
                    version: Some("1".into()),
                }
            }
            _ => Detection::No,
        }
    }

    fn ingest(&self, _source: &Source, _options: &IngestOptions) -> Result<Ingested, IngestError> {
        Ok(Ingested {
            dataset: Dataset {
                id: "fake/ds".into(),
                calibration: None,
                metadata: vec![],
                provenance: vec![],
                episodes: vec![],
            },
            report: IngestReport {
                unread_sources: Vec::new(),
                format_id: "fake",
                source_version: Some("1".into()),
                coverage: Coverage::Full,
                mapped_fields: vec!["episodes".into()],
                unmapped_fields: vec![],
                omitted_fields: vec![],
            },
        })
    }
}

fn registry() -> veridex_core::AdapterRegistry {
    let mut reg = veridex_core::AdapterRegistry::new();
    reg.register(Box::new(FakeAdapter));
    reg
}

/// Create a real (empty) file with the given name inside a fresh temp dir, returning both so the
/// dir stays alive for the duration of the test. Ingest now requires the source path to exist.
fn temp_file(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, b"").unwrap();
    (dir, path)
}

#[test]
fn recognized_source_ingests() {
    let reg = registry();
    let (_dir, path) = temp_file("dataset.fake");
    let out = reg
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect("should ingest");
    assert_eq!(out.dataset.id, "fake/ds");
    assert_eq!(out.report.format_id, "fake");
    assert_eq!(out.report.coverage, Coverage::Full);
}

#[test]
fn unsupported_format_is_rejected_clearly_and_lists_supported() {
    let reg = registry();
    let (_dir, path) = temp_file("dataset.rlds");
    let src = Source::Local(path);
    let err = reg.ingest(&src, &IngestOptions::default()).unwrap_err();
    match err {
        IngestError::UnsupportedFormat { supported } => {
            assert_eq!(supported, vec!["fake"]);
        }
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
    // The error message must name the supported formats so users are not left guessing.
    let msg = reg
        .ingest(&src, &IngestOptions::default())
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("fake"),
        "error should list supported formats: {msg}"
    );
}

#[test]
fn missing_path_is_reported_as_not_found_not_unsupported_format() {
    let reg = registry();
    // A path that does not exist — even with a recognized extension — is a not-found error, so a
    // mistyped path is not misreported as an unrecognized format.
    let src = Source::Local("/nonexistent/dataset.fake".into());
    let err = reg.ingest(&src, &IngestOptions::default()).unwrap_err();
    match err {
        IngestError::SourceNotFound(p) => {
            assert_eq!(p, std::path::PathBuf::from("/nonexistent/dataset.fake"));
        }
        other => panic!("expected SourceNotFound, got {other:?}"),
    }
    // ingest_as guards the same way.
    let err2 = reg
        .ingest_as("fake", &src, &IngestOptions::default())
        .unwrap_err();
    assert!(matches!(err2, IngestError::SourceNotFound(_)));
}

#[test]
fn supported_formats_reflects_registrations() {
    assert_eq!(registry().supported_formats(), vec!["fake"]);
}

#[test]
fn an_ingest_refuses_to_materialize_more_frames_than_its_budget() {
    // The product of streams × samples is quadratic in file-controlled numbers, so ingestion must
    // refuse on what a file declares rather than being OOM-killed inside a CI gate.
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use veridex_core::adapter::mcap::McapAdapter;
    use veridex_core::adapter::{Adapter, IngestError, IngestOptions, Source};

    let mut bytes = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut bytes)).expect("writer");
        let schema = w
            .add_schema("std_msgs/msg/String", "ros2msg", b"")
            .expect("s");
        let channel = w
            .add_channel(schema, "/t", "cdr", &BTreeMap::new())
            .expect("c");
        for i in 0..50u64 {
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: channel,
                    sequence: i as u32,
                    log_time: i * 1_000_000,
                    publish_time: i * 1_000_000,
                },
                b"x",
            )
            .expect("m");
        }
        w.finish().expect("finish");
    }
    let mut f = tempfile::Builder::new()
        .suffix(".mcap")
        .tempfile()
        .expect("temp");
    std::io::Write::write_all(&mut f, &bytes).expect("write");
    let path = f.into_temp_path();
    let source = Source::Local(path.to_path_buf());

    // The default budget is far above this file.
    assert!(McapAdapter
        .ingest(&source, &IngestOptions::default())
        .is_ok());

    // A budget below what the file holds is refused with a clear error, not a partial ingest.
    let opts = IngestOptions {
        max_frames: Some(10),
        ..IngestOptions::default()
    };
    let err = McapAdapter
        .ingest(&source, &opts)
        .expect_err("over budget must fail");
    assert!(
        matches!(err, IngestError::FrameBudgetExceeded { limit: 10, .. }),
        "{err:?}"
    );

    // And `None` removes the limit entirely, for a genuinely large dataset.
    let opts = IngestOptions {
        max_frames: None,
        ..IngestOptions::default()
    };
    assert!(McapAdapter.ingest(&source, &opts).is_ok());
}

// ---- half a dataset ------------------------------------------------------------------------------

/// A CAN dataset is a **pair**, and either half alone is a file Veridex can name but not check.
///
/// Pointing at the log is the ordinary first-use mistake — a recorder hands you a `.blf`, and the
/// database lives somewhere else entirely — and the answer was "no adapter recognized the source",
/// which is not true of a file whose magic the CAN adapter reads on sight. A reader one `mv` away
/// from a working command was told to go read a list of nine format names.
#[test]
fn a_can_log_without_its_database_is_named_rather_than_called_unrecognized() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("drive.blf");
    // The file signature alone is what the hint keys on: this is not a readable recording, and the
    // point is that Veridex still knows what kind of file it is looking at.
    std::fs::write(&log, b"LOGG").unwrap();

    let hints = veridex_core::default_registry().incomplete_hints(&Source::Local(log));
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(hints[0].contains("drive.blf"), "{}", hints[0]);
    assert!(hints[0].contains("Vector BLF"), "{}", hints[0]);
    // What to do about it, not only what it is.
    assert!(hints[0].contains(".dbc"), "{}", hints[0]);
    assert!(hints[0].contains("directory"), "{}", hints[0]);
}

/// The other half. A `.dbc` describes a bus and records none of it, so it is a database rather than
/// a dataset — and saying so is more use than a list of formats.
#[test]
fn a_database_without_its_log_is_named_too() {
    let dir = tempfile::tempdir().unwrap();
    let dbc = dir.path().join("vehicle.dbc");
    std::fs::write(&dbc, "BO_ 256 EngineData: 8 ECU\n").unwrap();

    let hints = veridex_core::default_registry().incomplete_hints(&Source::Local(dbc));
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(hints[0].contains("vehicle.dbc"), "{}", hints[0]);
    assert!(hints[0].contains("signal database"), "{}", hints[0]);
}

/// And the direction that matters more: a file nothing recognizes gets **no** hint. A hint that
/// fires on anything is noise on every genuine unsupported-format error, which is the case the
/// message above is already correct about.
#[test]
fn a_file_no_adapter_recognizes_gets_no_hint() {
    let dir = tempfile::tempdir().unwrap();
    let odd = dir.path().join("notes.txt");
    std::fs::write(&odd, "this is not a recording of anything").unwrap();
    assert!(veridex_core::default_registry()
        .incomplete_hints(&Source::Local(odd))
        .is_empty());
}

/// A complete CAN dataset is a directory, and a directory is never half of one — so the hint stays
/// silent on the very thing it is telling the caller to build.
#[test]
fn a_complete_dataset_directory_gets_no_hint() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 ECU\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("drive.log"),
        "(1.0) can0 100#0000000000000000\n",
    )
    .unwrap();
    assert!(veridex_core::default_registry()
        .incomplete_hints(&Source::Local(dir.path().to_path_buf()))
        .is_empty());
}

/// A LeRobot dataset is a *directory*, and its pieces are the three things a reader points at
/// instead: the `meta/` folder, the manifest inside it, and a data shard. Each was answered with
/// "no adapter recognized the source" and a list of nine format names, for a dataset sitting one
/// directory away.
#[test]
fn the_meta_folder_of_a_lerobot_dataset_names_the_dataset_above_it() {
    let dir = tempfile::tempdir().unwrap();
    let meta = dir.path().join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    std::fs::write(meta.join("info.json"), "{\"codebase_version\":\"v3.0\"}").unwrap();

    let hints = veridex_core::default_registry().incomplete_hints(&Source::Local(meta));
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(hints[0].contains("meta/"), "{}", hints[0]);
    // It names where to point instead, which is the whole reason for saying anything.
    assert!(
        hints[0].contains(&dir.path().display().to_string()),
        "{}",
        hints[0]
    );
}

#[test]
fn a_lerobot_manifest_file_names_the_dataset_two_levels_up() {
    let dir = tempfile::tempdir().unwrap();
    let meta = dir.path().join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    let info = meta.join("info.json");
    std::fs::write(&info, "{\"codebase_version\":\"v3.0\"}").unwrap();

    let hints = veridex_core::default_registry().incomplete_hints(&Source::Local(info));
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(hints[0].contains("info.json"), "{}", hints[0]);
    assert!(
        hints[0].contains(&dir.path().display().to_string()),
        "{}",
        hints[0]
    );
}

/// Parquet is not LeRobot's alone, so the hint says what the file would mean *if* it is a shard
/// rather than asserting that it is.
#[test]
fn a_parquet_shard_is_named_as_one_file_of_a_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let shard = dir.path().join("file-000.parquet");
    std::fs::write(&shard, b"PAR1").unwrap();

    let hints = veridex_core::default_registry().incomplete_hints(&Source::Local(shard));
    assert_eq!(hints.len(), 1, "{hints:?}");
    assert!(
        hints[0].contains("If it is a LeRobot data shard"),
        "{}",
        hints[0]
    );
}

/// And the direction that keeps it from becoming noise: an ordinary directory that is not part of a
/// dataset gets no hint. A directory with no manifest in it is not a `meta/` folder.
#[test]
fn an_ordinary_directory_gets_no_lerobot_hint() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "nothing to do with robots").unwrap();
    assert!(veridex_core::default_registry()
        .incomplete_hints(&Source::Local(dir.path().to_path_buf()))
        .is_empty());
}
