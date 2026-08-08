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
                metadata: vec![],
                provenance: vec![],
                episodes: vec![],
            },
            report: IngestReport {
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
