//! Tests for metadata-only ingestion: what the manifest alone can honestly answer, what it cannot,
//! and — the part that matters — that the checks which need frames abstain instead of reading their
//! absence as a defect, and that the resulting verdict is never mistaken for a full one.

use std::fs;
use std::path::{Path, PathBuf};
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
    // Two formats interleave their structure with their data — a CAN log is a stream of frames with
    // no inventory in front of it, and MDF4's channel groups are read through the same block walk as
    // its records — so there is no header set that describes the recording on its own. Reading
    // everything anyway and labelling it metadata-only would answer a question that was never asked.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 100 Speed: 8 ECU\n SG_ wheel : 0|16@1+ (0.01,0) [0|655] \"kph\" X\n",
    )
    .unwrap();
    fs::write(dir.path().join("drive.log"), "(1.000000) can0 064#0000\n").unwrap();
    let src = dir.path();
    match veridex_core::default_registry()
        .ingest(&Source::Local(src.to_path_buf()), &metadata_only())
    {
        Err(IngestError::NotImplemented { what, hint }) => {
            assert!(what.contains("metadata-only"), "{what}");
            // A CAN log has no episode axis either, so the hint must not send the reader to a
            // second refusal by suggesting a sample.
            assert!(!hint.contains("sample the dataset instead"), "{hint}");
            assert!(hint.contains("check it in full"), "{hint}");
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

#[test]
fn a_manifest_that_declares_an_episode_twice_is_refused() {
    // The lerobot#4143 class stated outright in the manifest, and the one form of it a run that
    // never reads a frame can prove. Keying the episode map on `episode_index` silently collapsed
    // the duplicate — last line wins — so a manifest declaring episode 1 with both length 10 and
    // length 99 produced a clean CDM carrying 99, while the cumulative boundaries LeRobot derives
    // from those lines are wrong for every episode after the duplicate.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 10, None);
    fs::write(
        dir.path().join("meta/episodes.jsonl"),
        "{\"episode_index\": 0, \"length\": 10}\n\
         {\"episode_index\": 1, \"length\": 10}\n\
         {\"episode_index\": 1, \"length\": 99}\n",
    )
    .unwrap();

    for options in [metadata_only(), IngestOptions::default()] {
        match ingest(dir.path(), &options) {
            Err(IngestError::Parse { message, .. }) => {
                assert!(message.contains("more than once"), "{message}")
            }
            other => panic!("expected a refusal, got ok={}", other.is_ok()),
        }
    }
}

#[test]
fn the_manifest_cannot_make_the_ingest_allocate_without_bound() {
    // No frame is read, so the frame budget — charged per frame — never fires, and nothing else
    // bounded what the manifest could make this build: one `Stream` per (episode x feature), both
    // factors straight from attacker-controlled text. 300k declared episodes against 60 features
    // measured 18M streams and 6 GB resident, from a file that costs nothing to write.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 10, None);
    let manifest: String = (0..50_000u64)
        .map(|e| format!("{{\"episode_index\": {e}, \"length\": 10}}\n"))
        .collect();
    fs::write(dir.path().join("meta/episodes.jsonl"), manifest).unwrap();

    // 50,000 episodes x 2 features = 100,000 streams, refused against a 1,000 ceiling.
    let tight = IngestOptions {
        metadata_only: true,
        max_frames: Some(1_000),
        ..IngestOptions::default()
    };
    match ingest(dir.path(), &tight) {
        Err(IngestError::FrameBudgetExceeded {
            format_id,
            requested,
            ..
        }) => {
            assert_eq!(format_id, "lerobot");
            assert_eq!(requested, 100_000);
        }
        other => panic!("expected the budget to refuse, got ok={}", other.is_ok()),
    }

    // Under the default ceiling the same manifest is ingested, so the guard bounds abuse without
    // refusing a genuinely large dataset.
    let ok = ingest(dir.path(), &metadata_only()).unwrap();
    assert_eq!(ok.dataset.episodes.len(), 50_000);
}

#[test]
fn every_format_that_supports_it_is_named_in_the_docs() {
    // Four documents tell a reader which formats can be checked from what they declare: the CLI's
    // help (which builds its list from the registry, so it cannot drift), and three prose files that
    // can. This is the guard for those three — the same guard `docs/checks.md` has against a new
    // check going undocumented.
    //
    // The mapping is here rather than on the adapter because prose names a format the way its users
    // do ("ROS 2 rosbag2"), and an id is not that. A new adapter fails this test until both the
    // mapping and the documents mention it, which is the point.
    let documented_as = |format_id: &str| -> &'static str {
        match format_id {
            "lerobot" => "LeRobot",
            "mcap" => "MCAP",
            "rosbag2" => "rosbag2",
            "rlds" => "RLDS",
            "hdf5" => "HDF5",
            "zarr" => "Zarr",
            "mf4" => "MF4",
            other => panic!("no documented name known for the `{other}` adapter — add one here, and name the format in the three documents below"),
        }
    };

    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    for doc in ["docs/partial-runs.md", "docs/checks.md", "README.md"] {
        let text = std::fs::read_to_string(format!("{root}/{doc}"))
            .unwrap_or_else(|e| panic!("{doc} is readable: {e}"));
        for format_id in veridex_core::default_registry().formats_supporting_metadata_only() {
            let name = documented_as(format_id);
            assert!(
                text.contains(name),
                "{doc} does not mention `{name}`, which supports --metadata-only"
            );
        }
    }
}

// ---- The invariant across every format that supports the flag ----

/// A dataset in one format, and the adapter id that reads it.
struct Sample2 {
    format: &'static str,
    path: PathBuf,
    /// Kept alive: several of these are written into a temporary directory.
    _keep: Option<tempfile::TempDir>,
}

fn repo_fixture(relative: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(relative)
}

/// One dataset per format that claims `--metadata-only`, so the invariant below is checked against
/// every one of them rather than against whichever the author happened to think of.
fn one_dataset_per_supporting_format() -> Vec<Sample2> {
    let lerobot = tempfile::tempdir().unwrap();
    write_dataset(lerobot.path(), 3, 20, None);
    let lerobot_path = lerobot.path().to_path_buf();

    let mcap = tempfile::tempdir().unwrap();
    let mcap_path = mcap.path().join("demo.mcap");
    std::fs::copy(
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../veridex-cli/tests/fixtures/demo.mcap"
        )),
        &mcap_path,
    )
    .expect("the CLI's committed MCAP fixture");

    vec![
        Sample2 {
            format: "lerobot",
            path: lerobot_path,
            _keep: Some(lerobot),
        },
        Sample2 {
            format: "mcap",
            path: mcap_path,
            _keep: Some(mcap),
        },
        Sample2 {
            format: "rosbag2",
            path: repo_fixture("rosbag2/clean_rig"),
            _keep: None,
        },
        Sample2 {
            format: "hdf5",
            path: repo_fixture("hdf5/robomimic_small.h5"),
            _keep: None,
        },
        Sample2 {
            format: "zarr",
            path: repo_fixture("zarr/dp_replay.zarr"),
            _keep: None,
        },
    ]
}

/// Every stream in the dataset, as `episode/name dtype shape modality clock`.
fn structure(ingested: &veridex_core::Ingested) -> Vec<String> {
    let mut out: Vec<String> = ingested
        .dataset
        .episodes
        .iter()
        .flat_map(|e| {
            e.streams.iter().map(move |s| {
                format!(
                    "{}/{} {:?} {:?} {:?} {}",
                    e.index, s.name, s.dtype, s.shape, s.modality, s.clock_id
                )
            })
        })
        .collect();
    out.sort();
    out
}

#[test]
fn a_metadata_only_run_describes_the_same_dataset_minus_the_frames() {
    // The invariant the whole mode rests on: what it *does* say must agree with a full read. A
    // metadata-only run that named different streams, or gave them different types, would not be a
    // narrower answer to the same question — it would be a different answer, and the coverage note
    // would make it look like the first.
    //
    // Driven by the registry rather than by a list, so a seventh adapter claiming the flag fails
    // here until it has a dataset to check the invariant against.
    // Two exemptions, for two different reasons.
    //
    // RLDS is about the *fixture*: a full read of a TFDS export needs a real TFRecord shard, and the
    // writer for one lives in `rlds_adapter.rs`, deliberately as a second implementation of the wire
    // format. The same invariant is checked there, against a shard.
    //
    // MF4 is about the invariant itself, and the difference is honest: a full read drops a channel
    // whose data type this reader cannot decode, while the header tree still declares it. So a
    // metadata-only run legitimately names *more* streams than a full read finds — the file says
    // they are there, and only the decode step disagrees. `mdf4_adapter.rs` checks the shape a
    // metadata-only run does have, including on a compressed measurement, which a full read cannot
    // describe at all.
    const EXEMPT: &[&str] = &["rlds", "mf4"];

    let samples = one_dataset_per_supporting_format();
    for format in veridex_core::default_registry().formats_supporting_metadata_only() {
        if EXEMPT.contains(&format) {
            continue;
        }
        let sample = samples
            .iter()
            .find(|s| s.format == format)
            .unwrap_or_else(|| panic!("`{format}` claims --metadata-only but has no dataset here"));

        let full = ingest(&sample.path, &IngestOptions::default())
            .unwrap_or_else(|e| panic!("{format} full ingest: {e}"));
        let partial = ingest(&sample.path, &metadata_only())
            .unwrap_or_else(|e| panic!("{format} metadata-only ingest: {e}"));

        assert_eq!(
            structure(&partial),
            structure(&full),
            "{format}: the metadata-only run describes a different dataset"
        );
        assert!(
            partial
                .dataset
                .episodes
                .iter()
                .all(|e| e.streams.iter().all(|s| s.frames.is_empty())),
            "{format}: a metadata-only run read frames"
        );
        assert!(
            matches!(
                partial.report.coverage,
                veridex_core::adapter::Coverage::MetadataOnly { .. }
            ),
            "{format}: coverage is {:?}",
            partial.report.coverage
        );
    }
}

/// A run that opened no payload cannot tell a dataset with no calibration or lineage from one whose
/// calibration and lineage live in the payloads it declined to read. Reporting them missing there
/// measures the request rather than the data — the defect `autonomy.calibration-completeness` was
/// fixed for, arriving through provenance instead.
///
/// Concretely: a ROS 2 bag carries its transform tree and camera intrinsics in message bodies, so a
/// full run records `calibration` provenance from them and a metadata-only run has none — and
/// reported the fully calibrated bag as missing it.
#[test]
fn a_payload_derived_provenance_element_is_not_called_missing_when_no_payload_was_read() {
    let bag = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rosbag2/clean_rig");
    let code = "PROVENANCE.MISSING_CALIBRATION";
    let has = |options: &IngestOptions| {
        veridex_core::pipeline::run_check(
            &veridex_core::default_registry(),
            &Source::Local(bag.clone()),
            None,
            options,
        )
        .expect("the run completes")
        .verdict
        .findings
        .iter()
        .any(|f| f.code == code)
    };

    assert!(
        !has(&metadata_only()),
        "the bag's calibration is in its message bodies, which this run did not open"
    );
    assert!(
        !has(&IngestOptions::default()),
        "and a full run reads that calibration, so it is not missing either"
    );

    // The check still speaks about it where the answer is real: a CDM with no calibration at all,
    // from a run that did read frames.
    let empty = veridex_core::cdm::Dataset {
        id: "no-provenance".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![],
        calibration: None,
    };
    let findings = veridex_core::check::Check::run_in(
        &veridex_core::checks::provenance::ProvenanceCompleteness,
        &empty,
        &veridex_core::check::CheckContext { frames_read: true },
    );
    assert!(findings.iter().any(|f| f.code == code));
    let narrowed = veridex_core::check::Check::run_in(
        &veridex_core::checks::provenance::ProvenanceCompleteness,
        &empty,
        &veridex_core::check::CheckContext { frames_read: false },
    );
    assert!(
        !narrowed.iter().any(|f| f.code == code),
        "and stays silent about it where no payload was read"
    );
    // Everything a manifest supplies is judged in both modes: its absence means the same thing.
    for still in [
        "PROVENANCE.MISSING_LICENSE",
        "PROVENANCE.MISSING_SENSOR",
        "PROVENANCE.MISSING_CLOCK",
        "PROVENANCE.MISSING_ANNOTATOR",
    ] {
        assert!(
            narrowed.iter().any(|f| f.code == still),
            "{still} is read from a manifest, so a narrow run still judges it"
        );
    }
}
