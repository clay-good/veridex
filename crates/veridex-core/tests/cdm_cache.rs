//! Validating a saved CDM: the incremental half of a run, and the three refusals that keep it from
//! becoming a way to launder a verdict.
//!
//! The claim under test is strong and narrow: for the content hash the cache names, a cached run's
//! verdict is **identical** to a full read's, because the CDM is the whole of what a read computes
//! and the verdict is a function of it. Everything else here is about what the cache cannot be used
//! to claim.

use std::path::PathBuf;

use veridex_core::adapter::{default_registry, IngestOptions, Source};
use veridex_core::cdm_cache::{self, CacheError};

fn demo(dir: &std::path::Path, variant: &str) -> PathBuf {
    let path = dir.join(format!("{variant}.mcap"));
    veridex_demo::mcap::write(&path, variant).expect("the demo recording");
    path
}

fn ingest(path: &std::path::Path, options: IngestOptions) -> veridex_core::Ingested {
    default_registry()
        .ingest(&Source::Local(path.to_path_buf()), &options)
        .expect("ingests")
}

/// The report a full read produces, and the one a cached run produces from the CDM that read built.
fn both_ways(variant: &str) -> (veridex_core::CheckOutput, veridex_core::CheckOutput) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = demo(dir.path(), variant);
    let ingested = ingest(&path, IngestOptions::default());
    let cache = cdm_cache::save(&ingested);
    let text = cdm_cache::render(&cache);

    let full = veridex_core::check_ingested(ingested, &veridex_core::RunConfig::default(), None);
    let loaded = cdm_cache::load(&text).expect("the cache loads");
    let from = veridex_core::CachedInput {
        veridex_version: loaded.veridex_version.clone(),
        cdm_content_hash: loaded.cdm_content_hash.clone(),
    };
    let cached = veridex_core::check_ingested_cached(
        cdm_cache::as_ingested(loaded),
        &veridex_core::RunConfig::default(),
        None,
        veridex_core::RunControl::default(),
        Some(from),
    );
    (full, cached)
}

#[test]
fn a_cached_run_is_the_full_runs_verdict_plus_the_fact_that_it_was_cached() {
    let (full, cached) = both_ways("av");

    // The binding is the same hash: this is what makes the two comparable at all.
    assert_eq!(
        cached.verdict.cdm_content_hash,
        full.verdict.cdm_content_hash
    );
    assert_eq!(cached.trust.score, full.trust.score, "the same score");
    assert_eq!(cached.verdict.status, full.verdict.status);
    assert_eq!(
        cached.verdict.executed_checks, full.verdict.executed_checks,
        "the same catalog ran"
    );
    assert_eq!(cached.verdict.coverage, full.verdict.coverage);

    // Findings: identical but for the one disclosure, which is the honest difference.
    let codes = |o: &veridex_core::CheckOutput| -> Vec<String> {
        o.verdict.findings.iter().map(|f| f.code.clone()).collect()
    };
    let added: Vec<String> = codes(&cached)
        .into_iter()
        .filter(|c| !codes(&full).contains(c))
        .collect();
    assert_eq!(added, ["COVERAGE.CACHED_CDM"]);
    let removed: Vec<String> = codes(&full)
        .into_iter()
        .filter(|c| !codes(&cached).contains(c))
        .collect();
    assert!(removed.is_empty(), "{removed:?}");

    // The disclosure names what a reader needs to judge it: the version that built the model and
    // the hash it is bound to.
    let note = cached
        .verdict
        .findings
        .iter()
        .find(|f| f.code == "COVERAGE.CACHED_CDM")
        .expect("disclosed");
    assert!(
        note.message.contains(&full.verdict.cdm_content_hash),
        "{}",
        note.message
    );
    assert_eq!(note.severity, veridex_core::check::Severity::Info);
}

#[test]
fn a_fault_in_the_dataset_survives_the_round_trip() {
    // A cache of a broken recording is still a verdict about a broken recording: the point of the
    // thing is re-running the catalog, so the findings have to come back.
    let (full, cached) = both_ways("av-dead-lidar");
    let has = |o: &veridex_core::CheckOutput, code: &str| {
        o.verdict.findings.iter().any(|f| f.code == code)
    };
    assert!(has(&full, "AUTONOMY.POINT_CLOUD_EMPTY"));
    assert!(has(&cached, "AUTONOMY.POINT_CLOUD_EMPTY"));
    assert_eq!(cached.verdict.counts.error, full.verdict.counts.error);
}

#[test]
fn an_edited_cache_does_not_load() {
    // The refusal that makes the file usable as input at all. A cache is a verdict's whole
    // evidence, so a file that no longer hashes to the hash it states is not evidence of anything.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo(dir.path(), "av-dead-lidar"), IngestOptions::default());
    let text = cdm_cache::render(&cdm_cache::save(&ingested));

    // Soften the fault the way someone wanting a clean report would: make the empty sweeps full.
    let edited = text.replace("\"empty\": 11", "\"empty\": 0");
    assert_ne!(edited, text, "the fixture must actually be edited");
    match cdm_cache::load(&edited) {
        Err(CacheError::HashMismatch { .. }) => {}
        other => panic!("an edited cache must not load: {other:?}"),
    }
}

#[test]
fn a_cache_from_another_encoding_version_is_refused_rather_than_validated() {
    // A content hash means something only within one encoding. A cache written under another one
    // would validate into a verdict bound to a hash this build cannot reproduce — which is the
    // wording of tampering, about a file that is merely old.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo(dir.path(), "av"), IngestOptions::default());
    let mut cache = cdm_cache::save(&ingested);
    cache.cdm_encoding_version += 1;
    match cdm_cache::load(&cdm_cache::render(&cache)) {
        Err(CacheError::EncodingChanged { found, expected }) => {
            assert_eq!(found, expected + 1);
        }
        other => panic!("an older encoding must be refused: {other:?}"),
    }
}

#[test]
fn a_file_that_is_not_a_cache_is_refused_by_name() {
    for text in ["{}", "not json", "[1, 2, 3]"] {
        match cdm_cache::load(text) {
            Err(CacheError::NotACache(_)) => {}
            other => panic!("`{text}` must not load: {other:?}"),
        }
    }
    // A report is the file most likely to be handed over by mistake, and it is not a cache.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo(dir.path(), "av"), IngestOptions::default());
    let out = veridex_core::check_ingested(ingested, &veridex_core::RunConfig::default(), None);
    let report = veridex_core::render_json(&out.verdict, Some(out.trust));
    assert!(matches!(
        cdm_cache::load(&report),
        Err(CacheError::NotACache(_))
    ));
}

#[test]
fn a_partial_read_saves_as_exactly_as_partial_a_cache() {
    // A cache of a metadata-only read must not validate as a whole-dataset run: the coverage is
    // the ingest's own answer, carried through the file rather than re-derived from it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = demo(dir.path(), "av");
    let ingested = ingest(
        &path,
        IngestOptions {
            metadata_only: true,
            ..IngestOptions::default()
        },
    );
    let cache = cdm_cache::save(&ingested);
    let loaded = cdm_cache::load(&cdm_cache::render(&cache)).expect("loads");
    let out = veridex_core::check_ingested_cached(
        cdm_cache::as_ingested(loaded),
        &veridex_core::RunConfig::default(),
        None,
        veridex_core::RunControl::default(),
        None,
    );
    assert!(matches!(
        out.verdict.coverage,
        veridex_core::CoverageNote::MetadataOnly { .. }
    ));
    assert!(out
        .verdict
        .findings
        .iter()
        .any(|f| f.code == "COVERAGE.METADATA_ONLY"));
    // …and it still cannot be certified, for the same reason the read could not be.
    assert!(veridex_core::certificate::Certificate::certifiable(&out.verdict).is_err());
}
