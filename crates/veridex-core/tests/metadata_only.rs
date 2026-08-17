//! Tests for metadata-only ingestion: what the manifest alone can honestly answer, what it cannot,
//! and — the part that matters — that the checks which need frames abstain instead of reading their
//! absence as a defect, and that the resulting verdict is never mistaken for a full one.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::{Coverage, IngestError, IngestOptions, Sample, Source};
use veridex_core::certificate::document::Certificate;
use veridex_core::CoverageNote;

/// The options a `--metadata-only` run uses.
fn metadata_only() -> IngestOptions {
    IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    }
}

/// Write a LeRobot v3 dataset: `episodes` × `frames_per`, a correct `meta/episodes.jsonl`, a dataset
/// card carrying a license, and (optionally) a `meta/stats.json`.
fn write_dataset(dir: &Path, episodes: u64, frames_per: u64, stats: Option<serde_json::Value>) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();

    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10.0,
        "robot_type": "so100",
        "total_episodes": episodes,
        "total_frames": episodes * frames_per,
        "features": {
            "observation.state": { "dtype": "float32", "shape": [1] },
            "action": { "dtype": "float32", "shape": [1] },
        },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    let manifest: String = (0..episodes)
        .map(|e| format!("{{\"episode_index\": {e}, \"length\": {frames_per}}}\n"))
        .collect();
    fs::write(dir.join("meta/episodes.jsonl"), manifest).unwrap();
    fs::write(
        dir.join("README.md"),
        "---\nlicense: apache-2.0\n---\n\n# demo\n",
    )
    .unwrap();
    if let Some(stats) = stats {
        fs::write(
            dir.join("meta/stats.json"),
            serde_json::to_string_pretty(&stats).unwrap(),
        )
        .unwrap();
    }

    write_parquet(dir, episodes, frames_per);
}

fn write_parquet(dir: &Path, episodes: u64, frames_per: u64) {
    let mut eps = Vec::new();
    let mut frame_idx = Vec::new();
    let mut ts = Vec::new();
    let mut state = Vec::new();
    let mut action = Vec::new();
    for e in 0..episodes {
        for f in 0..frames_per {
            eps.push(e as i64);
            frame_idx.push(f as i64);
            ts.push(f as f64 / 10.0);
            state.push(e as f32);
            action.push(f as f32);
        }
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("observation.state", DataType::Float32, false),
        Field::new("action", DataType::Float32, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)) as ArrayRef,
            Arc::new(Int64Array::from(frame_idx)) as ArrayRef,
            Arc::new(Float64Array::from(ts)) as ArrayRef,
            Arc::new(Float32Array::from(state)) as ArrayRef,
            Arc::new(Float32Array::from(action)) as ArrayRef,
        ],
    )
    .unwrap();
    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn ingest(dir: &Path, options: &IngestOptions) -> Result<veridex_core::Ingested, IngestError> {
    veridex_core::default_registry().ingest(&Source::Local(dir.to_path_buf()), options)
}

fn check(dir: &Path, options: &IngestOptions) -> veridex_core::CheckOutput {
    veridex_core::run_check(
        &veridex_core::default_registry(),
        &Source::Local(dir.to_path_buf()),
        None,
        options,
    )
    .unwrap()
}

#[test]
fn the_manifest_alone_yields_the_declared_structure() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 20, None);

    let ingested = ingest(dir.path(), &metadata_only()).unwrap();
    assert_eq!(
        ingested.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: 3
        }
    );
    assert_eq!(ingested.dataset.episodes.len(), 3);
    for (i, ep) in ingested.dataset.episodes.iter().enumerate() {
        assert_eq!(ep.index, i as u64);
        // The manifest's declared length is carried; the frames it describes were not read.
        assert_eq!(ep.declared_frame_count, Some(20));
        assert_eq!(ep.start_ts, None);
        assert_eq!(ep.end_ts, None);
        let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["action", "observation.state"]);
        for s in &ep.streams {
            assert!(s.frames.is_empty(), "no payload should have been read");
            // Types and the declared rate come from the manifest, so they are covered.
            assert_eq!(s.dtype.as_deref(), Some("float32"));
            assert_eq!(s.shape.as_deref(), Some(&[1u64][..]));
            assert_eq!(s.declared_rate_hz, Some(10.0));
        }
    }
}

#[test]
fn no_stream_payload_is_read() {
    // The observable form of "reads no data": with the Parquet gone entirely, a metadata-only ingest
    // is unchanged, while a full one has nothing to ingest.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 20, None);
    let with_data = ingest(dir.path(), &metadata_only()).unwrap();

    fs::remove_dir_all(dir.path().join("data")).unwrap();
    let without_data = ingest(dir.path(), &metadata_only()).unwrap();
    assert_eq!(with_data.dataset, without_data.dataset);

    let full = ingest(dir.path(), &IngestOptions::default()).unwrap();
    assert!(
        full.dataset.episodes.is_empty(),
        "a full ingest of a dataset with no data files has no episodes — which is the point"
    );
}

#[test]
fn a_sound_dataset_produces_no_findings_about_the_frames_nobody_read() {
    // The failure this guards: `declared 20 frames but 0 were ingested`, `stream has no frames`, and
    // `manifest declares 60 frames but 0 were ingested` are all true of a metadata-only run over a
    // perfectly sound dataset. Reported, they would fail every dataset checked this way.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 20, None);

    let out = check(dir.path(), &metadata_only());
    let codes: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .map(|f| f.code.as_str())
        .collect();
    for forbidden in [
        "STRUCTURAL.EPISODE_BOUNDARY",
        "STRUCTURAL.EMPTY_STREAM",
        "STRUCTURAL.SINGLE_FRAME_STREAM",
        "STRUCTURAL.FRAME_COUNT_MISMATCH",
        "STRUCTURAL.EMPTY_DATASET",
    ] {
        assert!(
            !codes.contains(&forbidden),
            "{forbidden} is an artifact of the request, not a defect: {codes:?}"
        );
    }
}

#[test]
fn the_verdict_says_it_read_no_data() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 20, None);
    let out = check(dir.path(), &metadata_only());

    assert_eq!(
        out.verdict.coverage,
        CoverageNote::MetadataOnly {
            episodes_declared: 3
        }
    );
    assert!(out.verdict.coverage.is_partial());
    assert!(!out.verdict.coverage.frames_read());

    // It travels into every rendering, so nobody holding only the report can miss it.
    let json = veridex_core::render_json(&out.verdict, Some(out.trust));
    assert!(json.contains("\"metadata_only\""), "{json}");
    let terminal = veridex_core::render_terminal(&out.verdict, None, 5);
    assert!(terminal.contains("METADATA-ONLY"), "{terminal}");
    let html = veridex_core::render_html(&out.verdict, None);
    assert!(html.contains("METADATA-ONLY"), "{html}");
}

#[test]
fn a_metadata_only_run_cannot_be_certified() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 20, None);
    let out = check(dir.path(), &metadata_only());

    let err = Certificate::certifiable(&out.verdict)
        .expect_err("a certificate must not be issuable from a run that read no data");
    assert!(err.contains("metadata-only"), "{err}");
}

#[test]
fn the_stored_statistics_are_still_checked() {
    // `meta/stats.json` is manifest content, so its internal contradictions are exactly what this
    // mode can catch — without touching a byte of the data it summarizes.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(
        dir.path(),
        2,
        10,
        Some(serde_json::json!({
            "observation.state": { "min": [5.0], "max": [1.0], "mean": [3.0], "std": [0.5] },
            "action": { "min": [0.0], "max": [1.0], "mean": [0.5], "std": [0.1] },
        })),
    );

    let out = check(dir.path(), &metadata_only());
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code.starts_with("STATISTICAL.")),
        "an inverted stored range must be caught from the manifest alone: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_provenance_is_still_extracted() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 10, None);
    let ingested = ingest(dir.path(), &metadata_only()).unwrap();

    let keys: Vec<&str> = ingested.dataset.provenance[0]
        .elements
        .iter()
        .map(|e| e.key.as_str())
        .collect();
    assert!(keys.contains(&"license"), "{keys:?}");
    assert!(keys.contains(&"sensor"), "{keys:?}");
    assert!(keys.contains(&"source_format"), "{keys:?}");
}

#[test]
fn an_independent_declared_total_is_still_compared() {
    // `meta/episodes.jsonl` supplies the episode set, so `total_episodes` is a *second*, independent
    // assertion — and the two disagreeing is a manifest inconsistency this mode can prove.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);
    let info_path = dir.path().join("meta/info.json");
    let mut info: serde_json::Value =
        serde_json::from_slice(&fs::read(&info_path).unwrap()).unwrap();
    info["total_episodes"] = serde_json::json!(4);
    fs::write(&info_path, serde_json::to_string_pretty(&info).unwrap()).unwrap();

    let out = check(dir.path(), &metadata_only());
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "3 episodes declared line-by-line against a total of 4 is a real inconsistency"
    );
}

#[test]
fn a_tautological_declared_total_is_not_reported_as_a_verified_one() {
    // Without `meta/episodes.jsonl` the episode set *is* `total_episodes`, so comparing the two is
    // `n == n`: a check that cannot fail, whose pass would read as something having been verified.
    // The claim is withheld and the omission stated instead.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();

    let ingested = ingest(dir.path(), &metadata_only()).unwrap();
    assert_eq!(ingested.dataset.episodes.len(), 3);
    assert!(
        !ingested
            .dataset
            .metadata
            .iter()
            .any(|(k, _)| k == veridex_core::cdm::META_DECLARED_EPISODES),
        "the episode set came from that very number; it is not evidence about itself"
    );
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("could not fail")),
        "{:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn a_dataset_that_declares_no_episode_set_is_refused_not_passed() {
    // With no manifest to read, a metadata-only ingest would yield an empty dataset — and silence
    // read as a pass is what this codebase refuses to do.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();
    let info_path = dir.path().join("meta/info.json");
    let mut info: serde_json::Value =
        serde_json::from_slice(&fs::read(&info_path).unwrap()).unwrap();
    info.as_object_mut().unwrap().remove("total_episodes");
    fs::write(&info_path, serde_json::to_string_pretty(&info).unwrap()).unwrap();

    match ingest(dir.path(), &metadata_only()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("no episode set"), "{message}")
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn a_format_that_cannot_honor_it_is_refused_by_name() {
    // Every other adapter keeps its structure inside the container. Reading everything anyway and
    // labelling it metadata-only would answer a question that was never asked.
    let src = Path::new("tests/fixtures/zarr/dp_replay.zarr");
    match veridex_core::default_registry()
        .ingest(&Source::Local(src.to_path_buf()), &metadata_only())
    {
        Err(IngestError::NotImplemented { what, .. }) => {
            assert!(what.contains("metadata-only"), "{what}")
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn a_metadata_only_sample_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);
    let both = IngestOptions {
        metadata_only: true,
        sample: Sample::FirstEpisodes(1),
        ..IngestOptions::default()
    };
    match ingest(dir.path(), &both) {
        Err(IngestError::InvalidSample { reason }) => {
            assert!(reason.contains("metadata-only"), "{reason}")
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn a_metadata_only_verdict_is_a_different_verdict_from_a_full_one() {
    // Same dataset, two runs: the hashes must differ, because they are claims about different
    // things. A metadata-only verdict that hashed like a full one could be presented as one.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);

    let partial = check(dir.path(), &metadata_only());
    let full = check(dir.path(), &IngestOptions::default());
    assert_ne!(
        partial.verdict.cdm_content_hash,
        full.verdict.cdm_content_hash
    );
    assert_ne!(
        partial.verdict.result_content_hash,
        full.verdict.result_content_hash
    );
}

#[test]
fn the_same_manifest_ingests_to_the_same_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 10, None);
    let a = check(dir.path(), &metadata_only());
    let b = check(dir.path(), &metadata_only());
    assert_eq!(a.verdict.cdm_content_hash, b.verdict.cdm_content_hash);
    assert_eq!(a.verdict.result_content_hash, b.verdict.result_content_hash);
}

#[test]
fn what_it_cannot_see_it_does_not_claim_to_have_seen() {
    // A corrupted per-episode length is the lerobot#4143 class, and it is provable only against the
    // frames. A full check catches it; a metadata-only one must not — and must not be readable as
    // having checked for it either, which is what the coverage note carries.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 10, None);
    fs::write(
        dir.path().join("meta/episodes.jsonl"),
        "{\"episode_index\": 0, \"length\": 10}\n{\"episode_index\": 1, \"length\": 99}\n",
    )
    .unwrap();

    let full = check(dir.path(), &IngestOptions::default());
    assert!(
        full.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_BOUNDARY"),
        "a full check proves the boundary is wrong"
    );

    let partial = check(dir.path(), &metadata_only());
    assert!(
        !partial
            .verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_BOUNDARY"),
        "a metadata-only check has no frames to prove it against"
    );
    assert!(!partial.verdict.coverage.frames_read());
}
