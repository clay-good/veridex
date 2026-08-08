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

#[test]
fn recognized_source_ingests() {
    let reg = registry();
    let src = Source::Local("dataset.fake".into());
    let out = reg
        .ingest(&src, &IngestOptions::default())
        .expect("should ingest");
    assert_eq!(out.dataset.id, "fake/ds");
    assert_eq!(out.report.format_id, "fake");
    assert_eq!(out.report.coverage, Coverage::Full);
}

#[test]
fn unsupported_format_is_rejected_clearly_and_lists_supported() {
    let reg = registry();
    let src = Source::Local("dataset.rlds".into());
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
fn supported_formats_reflects_registrations() {
    assert_eq!(registry().supported_formats(), vec!["fake"]);
}
