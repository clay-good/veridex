//! HDF5 adapter tests.
//!
//! The fixtures are **real `h5py` output**, committed under `tests/fixtures/hdf5/`, not files this
//! repository's own writer produced: a reader tested only against its own writer proves the two
//! agree, not that either matches the format. `tests/fixtures/hdf5/README.md` records the script
//! that generated each one.

use std::path::PathBuf;

use veridex_core::adapter::{
    default_registry, Adapter, Coverage, Detection, IngestError, IngestOptions, Sample, Source,
};
use veridex_core::cdm::{ClockKind, Modality};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hdf5")
        .join(name)
}

fn ingest(name: &str, options: IngestOptions) -> veridex_core::adapter::Ingested {
    default_registry()
        .ingest(&Source::Local(fixture(name)), &options)
        .expect("the fixture ingests")
}

#[test]
fn a_robomimic_file_becomes_episodes_and_streams() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let dataset = &ingested.dataset;

    assert_eq!(ingested.report.format_id, "hdf5");
    assert_eq!(dataset.episodes.len(), 2, "`/data` holds two demo groups");
    assert_eq!(
        dataset.episodes.iter().map(|e| e.index).collect::<Vec<_>>(),
        vec![0, 1],
        "the index comes from the trailing number in `demo_N`"
    );

    let first = &dataset.episodes[0];
    let names: Vec<&str> = first.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "actions",
            "dones",
            "obs/agentview_image",
            "obs/robot0_eef_pos",
            "rewards",
        ],
        "every array below the episode group becomes a stream, nested paths included"
    );
    assert!(
        first.streams.iter().all(|s| s.frames.len() == 5),
        "the first dimension of each array is its frame count"
    );
}

#[test]
fn shapes_types_and_modalities_come_from_the_file() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let episode = &ingested.dataset.episodes[0];
    let stream = |name: &str| {
        episode
            .streams
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("stream {name} exists"))
    };

    let actions = stream("actions");
    assert_eq!(actions.dtype.as_deref(), Some("float32"));
    assert_eq!(actions.shape, Some(vec![7]));
    assert_eq!(
        actions.modality,
        Modality::Action,
        "`actions` is the one name every robot dataset shares"
    );

    let image = stream("obs/agentview_image");
    assert_eq!(image.dtype.as_deref(), Some("uint8"));
    assert_eq!(image.shape, Some(vec![6, 8, 3]));
    assert_eq!(
        image.modality,
        Modality::Video,
        "a uint8 [H, W, 3] frame is an image by its structure, not by its name"
    );

    let dones = stream("dones");
    assert_eq!(dones.dtype.as_deref(), Some("int64"));
    assert_eq!(dones.shape, None, "a 1-D array has no per-frame shape");
    assert_eq!(
        dones.modality,
        Modality::ScalarState,
        "nothing else is inferred from a name"
    );
}

#[test]
fn a_gzip_chunked_image_array_reads_back_row_by_row() {
    // `obs/agentview_image` is chunked (2 rows per chunk) and deflate-filtered, so reading it
    // exercises the chunk index, the inflate path, and the chunk-to-row copy across an edge chunk
    // (5 rows into 2-row chunks leaves a half-full last one).
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let image = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "obs/agentview_image")
        .expect("the image stream exists");

    assert_eq!(image.frames.len(), 5);
    let hashes: Vec<_> = image
        .frames
        .iter()
        .map(|f| f.value_ref.content_hash.expect("rows are fingerprinted"))
        .collect();
    let distinct: std::collections::BTreeSet<_> = hashes.iter().collect();
    assert_eq!(
        distinct.len(),
        5,
        "each row hashes to its own value — a chunk read that returned the same bytes for every \
         row would collapse these"
    );
    assert_eq!(
        image.frames[0].value_ref.byte_len,
        Some(6 * 8 * 3),
        "a row is the product of the dimensions after the first"
    );
    assert_eq!(
        image.frames[0].value_ref.byte_offset, None,
        "a chunked row has no single address in the file"
    );
}

#[test]
fn a_file_that_records_no_time_is_on_a_step_index_clock() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let episode = &ingested.dataset.episodes[0];

    assert!(
        episode
            .streams
            .iter()
            .all(|s| s.clock_kind == ClockKind::StepIndex && s.clock_id == "hdf5-step-index"),
        "robomimic records no timestamps, so nothing here is measured time"
    );
    assert!(
        episode.streams.iter().all(|s| s.declared_rate_hz.is_none()),
        "HDF5 declares no rate, and Veridex never invents one"
    );
    assert!(
        episode.duration_ns().is_none(),
        "a step index is not a duration"
    );
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("step index")),
        "the report says the timeline is an index, not a clock"
    );
}

#[test]
fn attributes_become_metadata_provenance_and_the_declared_counts() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let dataset = &ingested.dataset;

    let meta = |key: &str| {
        dataset
            .metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(meta("source_format"), Some("hdf5"));
    assert_eq!(
        meta("h5:env_args").map(|v| v.contains("Lift")),
        Some(true),
        "a variable-length string attribute resolves through the global heap"
    );
    assert_eq!(
        meta("declared_total_frames"),
        Some("9"),
        "`/data`'s `total` attribute is the source's own frame assertion"
    );
    assert_eq!(
        dataset.episodes[0].declared_frame_count,
        Some(5),
        "`num_samples` is the episode's own assertion about its length"
    );
    assert_eq!(
        dataset.episodes[0].task.as_deref(),
        Some("lift the cube"),
        "the instruction attribute becomes the episode's task"
    );

    let author = dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .find(|e| e.key == "author");
    assert!(
        author.is_some(),
        "a root `author` attribute is a lineage fact, not free-form metadata"
    );
    let upstream = dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .find(|e| e.key == "upstream")
        .and_then(|e| e.value.clone())
        .unwrap_or_default();
    assert!(
        upstream.ends_with("/data/demo_0") || upstream.ends_with("/data/demo_1"),
        "each episode records the group it came from, so a derived index is never mistaken for a \
         stated one: {upstream}"
    );
}

#[test]
fn the_same_bytes_ingest_to_the_same_content_hash() {
    let a = ingest("robomimic_small.h5", IngestOptions::default());
    let b = ingest("robomimic_small.h5", IngestOptions::default());
    assert_eq!(
        veridex_core::canonical::content_hash(&a.dataset),
        veridex_core::canonical::content_hash(&b.dataset),
        "ingestion is deterministic"
    );
}

#[test]
fn a_sample_is_honored_and_labeled_as_partial() {
    let ingested = ingest(
        "robomimic_small.h5",
        IngestOptions {
            sample: Sample::FirstEpisodes(1),
            ..IngestOptions::default()
        },
    );
    assert_eq!(ingested.dataset.episodes.len(), 1);
    assert_eq!(
        ingested.report.coverage,
        Coverage::Sample {
            sample: Sample::FirstEpisodes(1),
            episodes_ingested: 1,
        }
    );
    assert!(
        !ingested
            .dataset
            .metadata
            .iter()
            .any(|(k, _)| k == "declared_total_frames"),
        "the dataset-wide frame total is not comparable against one sampled episode"
    );
}

#[test]
fn the_frame_budget_refuses_before_it_allocates() {
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("robomimic_small.h5")),
            &IngestOptions {
                max_frames: Some(3),
                ..IngestOptions::default()
            },
        )
        .expect_err("a budget of 3 frames cannot hold this file");
    assert!(
        matches!(err, IngestError::FrameBudgetExceeded { format_id, .. } if format_id == "hdf5"),
        "got {err:?}"
    );
}

/// The SHA-256 of each row, taken from `h5py` itself (`sha256(array[i].tobytes())`).
///
/// This is the test that the reader decodes the *file*, not merely something self-consistent: the
/// expected values come from the reference implementation, so a chunk assembled in the wrong order,
/// an inflate that dropped a byte, or a shuffle left un-permuted all fail here.
fn assert_row_hashes(stream: &veridex_core::cdm::Stream, expected: &[&str]) {
    let actual: Vec<String> = stream
        .frames
        .iter()
        .map(|f| {
            f.value_ref
                .content_hash
                .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .expect("rows are fingerprinted")
        })
        .collect();
    assert_eq!(actual, expected, "stream {}", stream.name);
}

fn stream_of<'a>(
    ingested: &'a veridex_core::adapter::Ingested,
    episode: u64,
    name: &str,
) -> &'a veridex_core::cdm::Stream {
    ingested
        .dataset
        .episodes
        .iter()
        .find(|e| e.index == episode)
        .and_then(|e| e.streams.iter().find(|s| s.name == name))
        .unwrap_or_else(|| panic!("episode {episode} has stream {name}"))
}

#[test]
fn rows_decode_to_the_bytes_h5py_wrote() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    assert_row_hashes(
        stream_of(&ingested, 0, "actions"),
        &[
            "49874f4f0a22a1a4897fcfc6993fa37af672cedccb4dc4924731245457082966",
            "f04e904939036e5e26ac9d0c9827fcbf754787f783e75fb5dedbee7cf974ce89",
            "c590b4a874070906af16f01d650b235257f3d036e75d4c41211b720fcb21c040",
            "6f6dab177ba1e75c9b772374efed36155a49d4aab2c4f0e93ab60c685025dea3",
            "c56e700cdc074d7ad9720d91ecb7c93fee43764b84076492e6936f3035a6105c",
        ],
    );
    // Chunked (2 rows per chunk) and deflate-filtered, over 5 rows — so the last chunk is a partial
    // one and every row is assembled, not copied whole.
    assert_row_hashes(
        stream_of(&ingested, 0, "obs/agentview_image"),
        &[
            "ea1dd17b6d16915c57a448a9934c76826758460d31fb9a60425cb334291e4f30",
            "e7044181c037aa730cc4e2126d4dea446845429629832de3b7b4aee425189e1c",
            "6fed23be802407be7463ac03ca73dcbb1c71afb686a75a0a4597ef9cd9fb2178",
            "d6ad6668b703d960ce642309156aad0d751950911be5ec031546cb69d43e5c34",
            "334f3179ac36f82c21258155f3891002a4524604a64575812e7961d34eae7ef7",
        ],
    );
}

#[test]
fn shuffle_fletcher32_and_big_endian_arrays_decode_to_the_same_bytes() {
    let ingested = ingest("timed_rig.h5", IngestOptions::default());
    // gzip + shuffle + fletcher32, chunked two rows at a time.
    assert_row_hashes(
        stream_of(&ingested, 0, "actions"),
        &[
            "02eb63763eae551363ef45434da0677a3cff4d88be99ea2e1e055d0a91c1af7d",
            "c2ba7f8224e4f43719d89f4c66c6dbc49ea4162abdfdd151bdd43a64d333375d",
            "ddf9e2fe71b649afc532ba427535466d7507c4655d649f793abc3e1cff4a8ef7",
            "2e018550dc93c93a877020bee7673c3acb195c41412f48abd27dc56c039cb246",
        ],
    );
    // A big-endian float array: its bytes are carried through as stored, and its declared type says
    // the width the file states.
    let joints = stream_of(&ingested, 0, "joint_pos");
    assert_eq!(joints.dtype.as_deref(), Some("float32"));
    assert_row_hashes(
        joints,
        &[
            "45ddc384c1e2dd02f89079ca779a9ead11096deca6333261693f0f811cd97aad",
            "21e905b388ed1fd9161327b21a8ceff6bea2697cba8593f976790971990fe568",
            "137806a49983605ba37839fc2745fba6652b86a953927027aca0c07686441b0d",
            "cbf9b37b583cadd764af12b0bef9c5fc4469a3cf19e07e3003a31896950504db",
        ],
    );
}

#[test]
fn a_timestamp_array_with_declared_units_becomes_measured_time() {
    let ingested = ingest("timed_rig.h5", IngestOptions::default());
    let actions = stream_of(&ingested, 0, "actions");
    assert_eq!(actions.clock_kind, ClockKind::Measured);
    assert_eq!(actions.clock_id, "hdf5-time");
    assert_eq!(
        actions.frames.iter().map(|f| f.ts).collect::<Vec<_>>(),
        vec![0, 50_000_000, 100_000_000, 150_000_000],
        "`units = s` scales the array's seconds into nanoseconds"
    );

    let episode = &ingested.dataset.episodes[1];
    assert_eq!(
        episode.start_ts,
        Some(1_000_000_000),
        "the second episode's own timeline starts one second in"
    );
    assert_eq!(episode.duration_ns(), Some(150_000_000));
    assert!(ingested
        .report
        .mapped_fields
        .iter()
        .any(|f| f.contains("units")));
}

#[test]
fn a_timestamp_array_without_units_is_refused_as_a_clock() {
    let ingested = ingest("untimed_units.h5", IngestOptions::default());
    let actions = stream_of(&ingested, 0, "actions");
    assert_eq!(
        actions.clock_kind,
        ClockKind::StepIndex,
        "seconds or nanoseconds is not stated, and guessing would fabricate every rate derived \
         from it"
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.source_path == "time" && f.note.contains("units")),
        "the omission is disclosed, not silent: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_file_whose_arrays_sit_at_the_root_is_one_episode() {
    let ingested = ingest("flat_single_episode.h5", IngestOptions::default());
    assert_eq!(ingested.dataset.episodes.len(), 1);
    let names: Vec<&str> = ingested.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["actions", "observations"]);
    assert_eq!(ingested.report.coverage, Coverage::Full);
}

#[test]
fn a_libver_latest_file_is_read_or_refused_by_name() {
    // h5py's `libver='latest'` writes a version-3 superblock and version-2 object headers, and may
    // index chunks with structures this reader does not implement. Either it reads, or it says what
    // it found — what it must never do is return a dataset missing whatever it skipped.
    let result = default_registry().ingest(
        &Source::Local(fixture("libver_latest.h5")),
        &IngestOptions::default(),
    );
    match result {
        Ok(ingested) => {
            assert_eq!(ingested.dataset.episodes.len(), 1);
            assert_eq!(stream_of(&ingested, 0, "actions").frames.len(), 3);
            assert_eq!(
                ingested.report.source_version.as_deref(),
                Some("superblock v3")
            );
        }
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                !message.is_empty(),
                "a refusal has to name what it could not read"
            );
        }
        Err(other) => panic!("expected either a read or a parse error, got {other:?}"),
    }
}

#[test]
fn a_file_that_is_not_hdf5_is_not_claimed() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("not-really.h5");
    std::fs::write(&path, b"\x89HDX\r\n\x1a\nrest of a file").expect("write");
    assert_eq!(
        veridex_core::adapter::hdf5::Hdf5Adapter.detect(&Source::Local(path)),
        Detection::No,
        "the extension does not make a file HDF5; the signature does"
    );
}

#[test]
fn a_truncated_file_is_a_parse_error_not_a_clean_verdict() {
    let bytes = std::fs::read(fixture("robomimic_small.h5")).expect("read fixture");
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("truncated.h5");
    std::fs::write(&path, &bytes[..bytes.len() / 2]).expect("write");

    let err = default_registry()
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect_err("half a file is not a dataset");
    assert!(
        matches!(err, IngestError::Parse { format_id, .. } if format_id == "hdf5"),
        "got {err:?}"
    );
}
