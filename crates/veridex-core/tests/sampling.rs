//! Tests for sampled ingestion: what a sample selects, what it costs, and — the part that matters —
//! that a verdict over a sample is never readable as a verdict over the whole dataset.

use std::collections::BTreeSet;
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

/// Write a LeRobot v3 dataset of `episodes` episodes × `frames_per` frames, with a correct
/// `meta/episodes.jsonl` manifest (the episode set a sampled ingest draws from).
fn write_dataset(dir: &Path, episodes: u64, frames_per: u64) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();

    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10.0,
        "robot_type": "so100",
        "total_episodes": episodes,
        "total_frames": episodes * frames_per,
        "features": { "observation.state": { "dtype": "float32", "shape": [1] } },
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

    let mut eps = Vec::new();
    let mut frame_idx = Vec::new();
    let mut ts = Vec::new();
    let mut vals = Vec::new();
    for e in 0..episodes {
        for f in 0..frames_per {
            eps.push(e as i64);
            frame_idx.push(f as i64);
            ts.push(f as f64 / 10.0);
            // A per-episode value, so dropping an episode changes the recomputed statistics.
            vals.push(e as f32);
        }
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("observation.state", DataType::Float32, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(eps)) as ArrayRef,
            Arc::new(Int64Array::from(frame_idx)) as ArrayRef,
            Arc::new(Float64Array::from(ts)) as ArrayRef,
            Arc::new(Float32Array::from(vals)) as ArrayRef,
        ],
    )
    .unwrap();
    let file = fs::File::create(dir.join("data/chunk-000/file-000.parquet")).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// A minimal one-channel MCAP recording — a format that ingests as a single episode.
fn write_mcap(path: &Path) {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(std::io::Cursor::new(&mut out)).unwrap();
        let sid = w.add_schema("sensor_msgs/Image", "ros2msg", b"").unwrap();
        let cid = w
            .add_channel(sid, "/camera", "cdr", &std::collections::BTreeMap::new())
            .unwrap();
        for seq in 0..5u32 {
            let t = seq as u64 * 100_000_000;
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: cid,
                    sequence: seq,
                    log_time: t,
                    publish_time: t,
                },
                b"x",
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    fs::write(path, &out).unwrap();
}

fn options(sample: Sample) -> IngestOptions {
    IngestOptions {
        sample,
        ..IngestOptions::default()
    }
}

fn check(dir: &Path, sample: Sample) -> Result<veridex_core::CheckOutput, IngestError> {
    let registry = veridex_core::default_registry();
    veridex_core::run_check(
        &registry,
        &Source::Local(dir.to_path_buf()),
        None,
        &options(sample),
    )
}

fn episode_indices(out: &veridex_core::CheckOutput) -> Vec<u64> {
    out.ingested
        .dataset
        .episodes
        .iter()
        .map(|e| e.index)
        .collect()
}

#[test]
fn first_episodes_ingests_exactly_those_episodes() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 5, 4);

    let out = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    assert_eq!(episode_indices(&out), vec![0, 1]);
    assert_eq!(
        out.ingested.report.coverage,
        Coverage::Sample {
            sample: Sample::FirstEpisodes(2),
            episodes_ingested: 2,
        }
    );
}

#[test]
fn a_sample_larger_than_the_dataset_is_the_whole_dataset_and_still_says_sample() {
    // Asking for 50 of 5 episodes is not an error, but it is also not a full-dataset verdict: the
    // request was still a sample, and the coverage note is about what was asked and honored.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 5, 4);

    let out = check(dir.path(), Sample::FirstEpisodes(50)).unwrap();
    assert_eq!(episode_indices(&out), vec![0, 1, 2, 3, 4]);
    assert!(out.verdict.coverage.is_partial());
}

#[test]
fn the_same_fraction_and_seed_always_draw_the_same_episodes() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 20, 3);

    let request = Sample::Fraction {
        fraction: 0.25,
        seed: 7,
    };
    let a = check(dir.path(), request.clone()).unwrap();
    let b = check(dir.path(), request).unwrap();
    assert_eq!(episode_indices(&a), episode_indices(&b));
    assert_eq!(a.verdict.result_content_hash, b.verdict.result_content_hash);
    // ceil(0.25 x 20) = 5.
    assert_eq!(episode_indices(&a).len(), 5);
}

#[test]
fn a_different_seed_draws_a_different_subset() {
    // Otherwise the seed is decoration and every "random" sample inspects the same episodes.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 40, 2);

    let draw = |seed| {
        episode_indices(
            &check(
                dir.path(),
                Sample::Fraction {
                    fraction: 0.25,
                    seed,
                },
            )
            .unwrap(),
        )
    };
    assert_ne!(draw(1), draw(2));
}

#[test]
fn a_tiny_fraction_still_draws_at_least_one_episode() {
    // Rounding a fraction down to zero would ingest nothing and then report a clean verdict over it.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, 2);

    let out = check(
        dir.path(),
        Sample::Fraction {
            fraction: 0.0001,
            seed: 0,
        },
    )
    .unwrap();
    assert_eq!(episode_indices(&out).len(), 1);
}

#[test]
fn a_meaningless_sample_request_is_refused_not_silently_emptied() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 4, 2);

    for bad in [
        Sample::FirstEpisodes(0),
        Sample::Fraction {
            fraction: 0.0,
            seed: 0,
        },
        Sample::Fraction {
            fraction: -0.5,
            seed: 0,
        },
        Sample::Fraction {
            fraction: 1.5,
            seed: 0,
        },
        Sample::Fraction {
            fraction: f64::NAN,
            seed: 0,
        },
    ] {
        match check(dir.path(), bad.clone()) {
            Err(IngestError::InvalidSample { .. }) => {}
            other => panic!("expected {bad:?} to be refused, got {:?}", other.is_ok()),
        }
    }
}

#[test]
fn a_sampled_ingest_drops_the_dataset_level_declared_totals() {
    // The manifest totals describe the whole dataset. Left in place, every sample would be reported
    // as a truncated export — a fabricated finding, and the loudest kind.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 6, 5);

    let full = check(dir.path(), Sample::All).unwrap();
    assert!(full
        .verdict
        .findings
        .iter()
        .all(|f| f.code != "STRUCTURAL.FRAME_COUNT_MISMATCH"));

    let sampled = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    assert!(
        sampled
            .verdict
            .findings
            .iter()
            .all(|f| !f.code.starts_with("STRUCTURAL.FRAME_COUNT")
                && f.code != "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "a sample must not be reported as a truncated dataset: {:?}",
        sampled
            .verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect::<Vec<_>>()
    );
    let keys: BTreeSet<&str> = sampled
        .ingested
        .dataset
        .metadata
        .iter()
        .map(|(k, _)| k.as_str())
        .collect();
    assert!(!keys.contains("declared_episodes") && !keys.contains("declared_frames"));
}

#[test]
fn per_episode_declared_lengths_still_apply_under_a_sample() {
    // Dropping the *dataset* totals must not disarm the per-episode boundary check (lerobot#4143):
    // that assertion is about an episode that was ingested, so it is still testable.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 4, 3);
    // Corrupt episode 1's declared length; episodes 0 and 1 are what `FirstEpisodes(2)` selects.
    let manifest = "{\"episode_index\": 0, \"length\": 3}\n\
                    {\"episode_index\": 1, \"length\": 2}\n\
                    {\"episode_index\": 2, \"length\": 3}\n\
                    {\"episode_index\": 3, \"length\": 3}\n";
    fs::write(dir.path().join("meta/episodes.jsonl"), manifest).unwrap();

    let out = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_BOUNDARY"),
        "the corrupt per-episode length is still caught inside the sample"
    );
}

#[test]
fn the_verdict_and_the_rendered_reports_state_that_the_run_was_partial() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 8, 3);

    let out = check(dir.path(), Sample::FirstEpisodes(3)).unwrap();
    assert_eq!(
        out.verdict.coverage,
        CoverageNote::Sample {
            request: "first 3 episode(s) by index".into(),
            episodes_ingested: 3,
        }
    );

    let terminal = veridex_core::render_terminal(&out.verdict, None, 10);
    assert!(
        terminal.contains("Coverage: SAMPLE") && terminal.contains("3 episode(s) ingested"),
        "the terminal report states the run was partial:\n{terminal}"
    );

    let json = veridex_core::render_json(&out.verdict, None);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["verdict"]["coverage"]["kind"], "sample");
    assert_eq!(parsed["verdict"]["coverage"]["episodes_ingested"], 3);

    let html = veridex_core::render_html(&out.verdict, None);
    assert!(html.contains("Coverage: SAMPLE"));
}

#[test]
fn a_full_run_says_nothing_about_coverage() {
    // The note is for the exceptional case; a full check must not gain a banner it does not need.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 3, 3);

    let out = check(dir.path(), Sample::All).unwrap();
    assert_eq!(out.verdict.coverage, CoverageNote::Full);
    assert!(!veridex_core::render_terminal(&out.verdict, None, 10).contains("Coverage:"));
}

#[test]
fn coverage_is_bound_into_the_verdict_hash() {
    // Two runs that saw different amounts of the same dataset must not share a result hash — the
    // hash is what a certificate signs over.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 5, 3);

    let all = check(dir.path(), Sample::All).unwrap();
    let sampled = check(dir.path(), Sample::FirstEpisodes(5)).unwrap();
    // Both runs saw all five episodes, so the findings and the score are the same...
    assert_eq!(all.verdict.status, sampled.verdict.status);
    // ...but the verdicts are not interchangeable.
    assert_ne!(
        all.verdict.result_content_hash,
        sampled.verdict.result_content_hash
    );

    // The coverage note alone moves the hash, with nothing else about the run changed — so a
    // certificate signed over it cannot be re-read as covering more than it did.
    let engine = veridex_core::checks::default_engine().unwrap();
    let d = &all.ingested.dataset;
    let hash = veridex_core::content_hash(d);
    let config = veridex_core::RunConfig::default();
    let full = engine.run(d, hash, &config);
    let partial = engine.run_over(
        d,
        hash,
        &config,
        CoverageNote::Sample {
            request: "first 5 episode(s) by index".into(),
            episodes_ingested: 5,
        },
    );
    // The partial run's findings are the full run's plus its own disclosure that it was partial —
    // which is what carries the coverage into SARIF, `diff`, and the certificate's findings summary,
    // none of which read the `coverage` field.
    let disclosure: Vec<&veridex_core::check::Finding> = partial
        .findings
        .iter()
        .filter(|f| f.code == "COVERAGE.SAMPLE")
        .collect();
    assert_eq!(disclosure.len(), 1, "{:?}", partial.findings);
    assert_eq!(disclosure[0].severity, veridex_core::check::Severity::Info);
    let partial_without: Vec<_> = partial
        .findings
        .iter()
        .filter(|f| f.code != "COVERAGE.SAMPLE")
        .cloned()
        .collect();
    assert_eq!(full.findings, partial_without);
    assert_ne!(full.result_content_hash, partial.result_content_hash);
    // An info-severity disclosure never turns a sound dataset into a failing one.
    assert_eq!(full.status, partial.status);
}

#[test]
fn a_sampled_verdict_cannot_be_certified() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 6, 3);

    let sampled = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    let err = Certificate::certifiable(&sampled.verdict)
        .expect_err("a certificate must not be issued from a partial run");
    assert!(err.contains("sampled run"), "{err}");

    let full = check(dir.path(), Sample::All).unwrap();
    assert!(Certificate::certifiable(&full.verdict).is_ok());
}

#[test]
fn the_episode_set_falls_back_to_the_declared_total_when_there_is_no_manifest() {
    // meta/episodes.jsonl is optional in practice; info.json's total_episodes is the other place the
    // episode set is declared, and LeRobot numbers episodes 0..total.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 4, 3);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();

    let out = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    assert_eq!(episode_indices(&out), vec![0, 1]);
}

#[test]
fn sampling_with_no_declared_episode_set_is_refused_not_quietly_downgraded() {
    // With neither source of the episode set, sampling would have to read everything — the cost it
    // exists to avoid. Reading it all and calling the result a sample would be a lie about the work
    // done; reading it all and calling it full would be a lie about the request.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 4, 3);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();
    let info = fs::read_to_string(dir.path().join("meta/info.json")).unwrap();
    let mut info: serde_json::Value = serde_json::from_str(&info).unwrap();
    info.as_object_mut().unwrap().remove("total_episodes");
    fs::write(
        dir.path().join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    match check(dir.path(), Sample::FirstEpisodes(2)) {
        Err(IngestError::SamplingUnsupported { format_id, reason }) => {
            assert_eq!(format_id, "lerobot");
            assert!(reason.contains("no episode set"), "{reason}");
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
    // The same dataset checks fine without a sampling request.
    assert!(check(dir.path(), Sample::All).is_ok());
}

#[test]
fn a_single_episode_format_refuses_a_sample_rather_than_returning_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo.mcap");
    write_mcap(&path);

    let registry = veridex_core::default_registry();
    let source = Source::Local(path);
    match registry.ingest(&source, &options(Sample::FirstEpisodes(1))) {
        Err(IngestError::SamplingUnsupported { format_id, .. }) => assert_eq!(format_id, "mcap"),
        other => panic!("expected mcap to refuse a sample, got ok={}", other.is_ok()),
    }
    assert!(registry.ingest(&source, &options(Sample::All)).is_ok());
}

#[test]
fn options_the_implementation_does_not_honor_are_refused() {
    // A remote source is spec'd but unbuilt. Accepting it and reading a local file instead would
    // hand back a verdict the caller would read as something else.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 2);
    let registry = veridex_core::default_registry();

    // A metadata-only *sample* asks for two different partial coverages at once, and one verdict
    // carries only one — so the combination is refused rather than silently resolved.
    let both = IngestOptions {
        metadata_only: true,
        sample: veridex_core::adapter::Sample::FirstEpisodes(1),
        ..IngestOptions::default()
    };
    match registry.ingest(&Source::Local(dir.path().to_path_buf()), &both) {
        Err(IngestError::InvalidSample { reason }) => assert!(reason.contains("metadata-only")),
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }

    match registry.ingest(
        &Source::Remote("lerobot/some-dataset".into()),
        &IngestOptions::default(),
    ) {
        Err(IngestError::NotImplemented { what, .. }) => assert!(what.contains("remote")),
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn sampling_charges_the_frame_budget_only_for_the_episodes_it_keeps() {
    // The point of sampling is that the unselected episodes cost nothing. A budget that the full
    // dataset exceeds and the sample does not is the observable form of that.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, 10); // 100 rows x 1 feature = 100 frames.
    let registry = veridex_core::default_registry();
    let source = Source::Local(dir.path().to_path_buf());

    let tight = |sample| IngestOptions {
        sample,
        max_frames: Some(50),
        ..IngestOptions::default()
    };
    assert!(
        matches!(
            registry.ingest(&source, &tight(Sample::All)),
            Err(IngestError::FrameBudgetExceeded { .. })
        ),
        "the full dataset is over the budget"
    );
    assert!(
        registry
            .ingest(&source, &tight(Sample::FirstEpisodes(2)))
            .is_ok(),
        "a two-episode sample is not charged for the eight it skipped"
    );
}

#[test]
fn the_draw_does_not_depend_on_how_the_episode_set_was_discovered() {
    // `select` is called with a set; it must not matter that the set arrived in some other order.
    let forward: BTreeSet<u64> = (0..30).collect();
    let backward: BTreeSet<u64> = (0..30).rev().collect();
    let request = Sample::Fraction {
        fraction: 0.3,
        seed: 42,
    };
    assert_eq!(request.select(&forward), request.select(&backward));
}

#[test]
fn a_lying_episode_total_cannot_exhaust_memory_before_a_byte_is_read() {
    // `total_episodes` is a few bytes in an untrusted manifest, and the index set it implies used to
    // be materialized before either ingest budget existed. `u64::MAX` panicked on capacity overflow;
    // merely enormous values were a straight OOM that still returned Ok.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 2);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();
    let info = fs::read_to_string(dir.path().join("meta/info.json")).unwrap();
    let mut info: serde_json::Value = serde_json::from_str(&info).unwrap();
    info["total_episodes"] = serde_json::json!(u64::MAX);
    fs::write(
        dir.path().join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    // A `Fraction` draw needs the whole set, so it is the expensive arm.
    match check(
        dir.path(),
        Sample::Fraction {
            fraction: 0.5,
            seed: 0,
        },
    ) {
        Err(IngestError::SamplingUnsupported { reason, .. }) => {
            assert!(reason.contains("ceiling"), "{reason}");
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
    // `FirstEpisodes` never needs more than the first n indices, so it stays cheap and succeeds.
    let out = check(dir.path(), Sample::FirstEpisodes(2)).unwrap();
    assert_eq!(episode_indices(&out), vec![0, 1]);
}

#[test]
fn a_sample_still_catches_episodes_the_manifest_declares_but_the_data_lacks() {
    // Dropping the dataset-level totals under a sample must not drop the assertion that *is* in the
    // sample's scope: the sample selected N episodes, and only 2 materialized. That is the check that
    // catches "the episode set the manifest declares does not exist in the data".
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 2, 3);
    fs::remove_file(dir.path().join("meta/episodes.jsonl")).unwrap();
    let info = fs::read_to_string(dir.path().join("meta/info.json")).unwrap();
    let mut info: serde_json::Value = serde_json::from_str(&info).unwrap();
    info["total_episodes"] = serde_json::json!(10); // data holds 2
    fs::write(
        dir.path().join("meta/info.json"),
        serde_json::to_string_pretty(&info).unwrap(),
    )
    .unwrap();

    let out = check(dir.path(), Sample::FirstEpisodes(10)).unwrap();
    assert_eq!(episode_indices(&out), vec![0, 1]);
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code.starts_with("STRUCTURAL.EPISODE_COUNT")),
        "8 selected episodes never materialized: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_coverage_banner_carries_no_floating_point_noise() {
    // The banner is printed in every report and bound into the verdict hash, so `0.29 * 100.0`
    // rendering as 28.999999999999996 is user-facing.
    for (fraction, want) in [(0.29, "29%"), (0.5, "50%"), (0.125, "12.5%"), (1.0, "100%")] {
        let d = Sample::Fraction { fraction, seed: 3 }.describe();
        assert!(d.starts_with(want), "fraction {fraction} rendered as {d}");
    }
}
