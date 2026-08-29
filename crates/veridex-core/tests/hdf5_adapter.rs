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

#[test]
fn chunks_that_split_inner_dimensions_assemble_the_right_row() {
    // `chunked_ragged.h5` chunks every axis smaller than the dataset, so a row is stitched from
    // several chunks and every axis has a ragged edge: `(5,6,8,3)` in `(2,3,5,2)` chunks, and
    // `(5,7,5)` in `(2,3,2)`. This is the case that catches a wrong destination stride, a bad
    // odometer carry, or missing edge clipping in `copy_chunk_into_row` — with a chunk shape equal
    // to the dataset shape on the inner axes, those mistakes are invisible.
    let ingested = ingest("chunked_ragged.h5", IngestOptions::default());
    assert_row_hashes(
        stream_of(&ingested, 0, "obs/agentview_image"),
        &[
            "7c90e5bcde9e838fc2ea6fd95e7344dc74c931bd0cddefb1edb7af151e37c3ad",
            "6a514b189d8db1e1b248feb889f609ce2907de527169a3ca3e213e3297d1e21d",
            "ab6c0b2ec8bf3f48c5522ab81fb51d1dd9073f29c8534e3aacca19c991e8258d",
            "55c47460d2cab02439142891b5fe6dd73d07effc9fcfd8227e7d303dbb3acaa1",
            "9600ff5d01472f113d173a3b49240960c7406f44e899f6a8e8e882080aba8d12",
        ],
    );
    // Rank 3, chunked on all three axes, uncompressed.
    assert_row_hashes(
        stream_of(&ingested, 0, "grid"),
        &[
            "e2107dd5a468deee906767b6f2cca0222dcdebc3a657d54f27fc51257f5dd486",
            "4a9bc7f1db75e5319e7a300a055c32847e415cdd7bb25f825d2de6e9e1299df4",
            "b6641dd2b0a8166a98fa0dfe27b7bbb5275bf4b396f63ce0a42f906e956b8215",
            "3fd08e4f73484df784c929a14a74a27f1bc2a4a6b89211e32a9c55c6ca7e5cfb",
            "cda5a74b5581e81169382578f86a59e2325df44944c209f5388bee6791d40e3e",
        ],
    );
    // Rank 1, chunked: a "row" is a single element, and 7 rows in 3-element chunks ends ragged.
    assert_row_hashes(
        stream_of(&ingested, 0, "scalar_series"),
        &[
            "0c9157e10a6a39cd2ce611562b0bb5135b5f3a66ca4e69c6760949eb29463249",
            "962c6503e5d82deadc4d3a263fee3e3f552015d98326556f1e9e5eb5c08548ca",
            "3d008cc2a04fb424ddfd4b5325a544a14b9b8f5abf58b26163740077839b6cb0",
            "4918f74327942562f39a87de5710fba0db7c392aa34c1b6dd9f5908d1b2581bf",
            "16c6048bf6605136cac18e8281be92243edd45f251084d0a4f0059d1ce235765",
            "f5152210196715e48bef981617d3a901b373875503e743b3085e51e3dd3fb11d",
            "61e3168d498e22a91bb6bfc0a605dc913d838ac4a31112a0205496ff718459a7",
        ],
    );
}

#[test]
fn every_unit_spelling_the_adapter_knows_scales_to_nanoseconds() {
    // Four episodes recording the same 150 ms span in ms, us, ns, and " S " (padded and
    // upper-case), plus a fifth declaring a unit that means nothing.
    let ingested = ingest("units_zoo.h5", IngestOptions::default());
    for index in 0..4 {
        let actions = stream_of(&ingested, index, "actions");
        assert_eq!(
            actions.clock_kind,
            ClockKind::Measured,
            "episode {index} declares its units"
        );
        assert_eq!(
            actions.frames.iter().map(|f| f.ts).collect::<Vec<_>>(),
            vec![0, 50_000_000, 100_000_000, 150_000_000],
            "episode {index} spans the same 150 ms whichever unit it is written in"
        );
    }
    let unknown = stream_of(&ingested, 4, "actions");
    assert_eq!(
        unknown.clock_kind,
        ClockKind::StepIndex,
        "an unrecognized unit is not a clock"
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("furlongs")),
        "the refusal names the unit the file declared: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_non_finite_timestamp_is_refused_rather_than_stamped() {
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("nan_timestamp.h5")),
            &IngestOptions::default(),
        )
        .expect_err("a NaN in the timeline is not a clock");
    match err {
        IngestError::Parse { message, .. } => assert!(
            message.contains("non-finite"),
            "the refusal says what was wrong: {message}"
        ),
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn a_timeline_that_covers_only_some_arrays_leaves_the_episode_unbounded() {
    // `timestamps` has 4 rows, `actions` has 6. Only the arrays the timeline describes are on
    // measured time; the episode's own bounds must not then be stated in nanoseconds, because
    // `Episode::duration_ns` would subtract them into a duration and the outlier check would
    // compare that against genuinely measured episodes.
    let ingested = ingest("mismatched_timeline.h5", IngestOptions::default());
    let episode = &ingested.dataset.episodes[0];

    assert_eq!(
        stream_of(&ingested, 0, "actions").clock_kind,
        ClockKind::StepIndex
    );
    assert_eq!(
        stream_of(&ingested, 0, "timestamps").clock_kind,
        ClockKind::Measured
    );
    assert_eq!((episode.start_ts, episode.end_ts), (None, None));
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("does not cover every array")),
        "the mixed timeline is disclosed: {:?}",
        ingested.report.unmapped_fields
    );

    // And neither declared count is comparable against an episode with no single length.
    assert_eq!(
        episode.declared_frame_count, None,
        "`num_samples` has no single actual count to be compared against here"
    );
    assert!(
        !ingested
            .dataset
            .metadata
            .iter()
            .any(|(k, _)| k == "declared_total_frames"),
        "nor does the dataset-wide total"
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .filter(|f| f.note.contains("different numbers of rows"))
            .count()
            >= 2,
        "both withheld counts say why"
    );
}

#[test]
fn everything_the_reader_skips_is_named_in_the_report() {
    // `disclosure.h5` holds, deliberately: a soft link, a hard link back to its own episode group,
    // a variable-length string array, a scalar array, a zero-row array, an array attribute, an
    // array beside the episode groups, and a committed datatype.
    let ingested = ingest("disclosure.h5", IngestOptions::default());
    let notes: Vec<String> = ingested
        .report
        .unmapped_fields
        .iter()
        .map(|f| format!("{} :: {}", f.source_path, f.note))
        .collect();
    let says = |needle: &str| {
        assert!(
            notes.iter().any(|n| n.contains(needle)),
            "no note mentions {needle}: {notes:#?}"
        )
    };
    says("soft or external link");
    says("linked more than once");
    says("variable-length array");
    says("scalar array");
    says("holds several values");
    says("neither a group nor a dataset");

    // An array beside the episode groups holds rows nothing read, which is a hole in *coverage* —
    // not a note about what the CDM can hold. It belongs in `unread_sources`, the only channel that
    // reaches the verdict.
    let unread: Vec<String> = ingested
        .report
        .unread_sources
        .iter()
        .map(|f| format!("{} :: {}", f.source_path, f.note))
        .collect();
    assert!(
        unread
            .iter()
            .any(|n| n.starts_with("/data/sibling_array ::") && n.contains("2 row(s)")),
        "the array beside the episode groups is not disclosed as unread: {unread:#?}"
    );

    // A zero-row array is a stream with no frames — a fact for the structural checks to report,
    // not something to drop.
    let empty = stream_of(&ingested, 0, "empty_rows");
    assert!(empty.frames.is_empty());
}

#[test]
fn attribute_values_survive_as_the_file_wrote_them() {
    let ingested = ingest("disclosure.h5", IngestOptions::default());
    let meta = |key: &str| {
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        meta("h5:offset_steps").as_deref(),
        Some("-7"),
        "a negative int32 attribute is sign-extended, not read as 4.29 billion"
    );
    let blob = meta("h5:notes_blob").expect("the long attribute is carried");
    assert!(
        blob.ends_with("… (truncated)") && blob.len() < 4200,
        "an attribute far larger than the cap is cut at a character boundary and says so \
         (len {})",
        blob.len()
    );
}

#[test]
fn an_image_is_recognized_by_structure_and_nothing_else_is() {
    let ingested = ingest("disclosure.h5", IngestOptions::default());
    assert_eq!(
        stream_of(&ingested, 0, "channels_7").modality,
        Modality::ScalarState,
        "a uint8 [4, 4, 7] frame is not an image: no camera writes 7 channels"
    );
    assert_eq!(
        stream_of(&ingested, 0, "float_image_shaped").modality,
        Modality::ScalarState,
        "image *shape* alone is not an image; the element type has to be uint8 as well"
    );
}

#[test]
fn ingestion_does_not_depend_on_the_order_the_file_lists_its_links() {
    // Groups written `demo_10`, `demo_2`, `demo_1`, and arrays written in reverse alphabetical
    // order, so link order differs from name order.
    let ingested = ingest("unsorted_names.h5", IngestOptions::default());
    assert_eq!(
        ingested
            .dataset
            .episodes
            .iter()
            .map(|e| e.index)
            .collect::<Vec<_>>(),
        vec![1, 2, 10],
        "episodes come out in index order, and 10 sorts after 2"
    );
    assert_eq!(
        ingested.dataset.episodes[0]
            .streams
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["actions", "obs/a_first", "obs/z_last", "rewards"],
        "streams come out in name order, not creation order"
    );
}

#[test]
fn colliding_or_digitless_group_names_fall_back_to_position() {
    // `run_1` and `other_1` both end in 1, so the trailing number cannot be the index — two
    // episodes sharing index 1 would be a duplicate this adapter invented.
    let ingested = ingest("colliding_names.h5", IngestOptions::default());
    assert_eq!(
        ingested
            .dataset
            .episodes
            .iter()
            .map(|e| e.index)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "indices fall back to position in name order"
    );
    let upstreams: Vec<String> = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .filter(|e| e.key == "upstream")
        .filter_map(|e| e.value.clone())
        .collect();
    assert!(
        upstreams.iter().any(|u| u.ends_with("/data/other_1"))
            && upstreams.iter().any(|u| u.ends_with("/data/run_1")),
        "the real group name is preserved, so a derived index is never mistaken for a stated one: \
         {upstreams:?}"
    );
}

#[test]
fn dense_link_storage_is_refused_by_name_not_read_as_an_empty_group() {
    // 40 arrays in one group written with `libver='latest'` moves the links into a fractal heap.
    // Reading that group as empty would turn a 40-stream episode into a clean verdict over nothing.
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("dense_links.h5")),
            &IngestOptions::default(),
        )
        .expect_err("a group this reader cannot enumerate is not an empty group");
    match err {
        IngestError::Parse { message, .. } => {
            assert!(
                message.contains("fractal heap") && message.contains("invisible"),
                "the refusal names the structure and the consequence: {message}"
            );
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn a_chunk_that_inflates_past_the_budget_is_refused_while_decoding() {
    // 128 KB on disk, 94 MB inflated. The budget is charged on what each chunk's shape says it
    // must become, before the bytes are allocated.
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("bomb.h5")),
            &IngestOptions {
                max_decompression_ratio: Some(1),
                ..IngestOptions::default()
            },
        )
        .expect_err("a file that expands 700x is refused");
    assert!(
        matches!(
            err,
            IngestError::DecompressionBudgetExceeded { format_id, .. } if format_id == "hdf5"
        ),
        "got {err:?}"
    );
}

#[test]
fn a_corrupt_chunk_fails_its_own_checksum() {
    // `timed_rig.h5`'s `actions` arrays are fletcher32-protected. Flip one byte of the file's
    // chunk data and the checksum stored with it must catch it — silently reading a corrupted
    // chunk is how poisoned data reaches training.
    let bytes = std::fs::read(fixture("timed_rig.h5")).expect("read fixture");
    let dir = tempfile::tempdir().expect("temp dir");
    let mut caught = 0;
    // The chunk payloads sit past the header structures; patching a spread of offsets in the file's
    // second half hits chunk data (and, where it does not, produces some other honest error).
    for offset in (bytes.len() / 2..bytes.len()).step_by(97) {
        let mut patched = bytes.clone();
        patched[offset] ^= 0xFF;
        let path = dir.path().join(format!("patched-{offset}.h5"));
        std::fs::write(&path, &patched).expect("write");
        if let Err(IngestError::Parse { message, .. }) =
            default_registry().ingest(&Source::Local(path), &IngestOptions::default())
        {
            if message.contains("fletcher32") && message.contains("corrupt") {
                caught += 1;
            }
        }
    }
    assert!(
        caught > 0,
        "no single-byte corruption of the chunk data was caught by its stored checksum"
    );
}

#[test]
fn no_single_byte_corruption_panics_or_passes_silently() {
    // The one property that matters for a tool that runs in CI over untrusted files: whatever the
    // bytes are, the answer is a dataset or a named error — never a panic, a hang, or a verdict
    // over data that was never read.
    let bytes = std::fs::read(fixture("robomimic_small.h5")).expect("read fixture");
    let dir = tempfile::tempdir().expect("temp dir");
    for (n, offset) in (0..bytes.len()).step_by(311).enumerate() {
        let mut patched = bytes.clone();
        patched[offset] = patched[offset].wrapping_add(0x5A);
        let path = dir.path().join(format!("fuzz-{n}.h5"));
        std::fs::write(&path, &patched).expect("write");
        // Run it the way a CI gate would: a bounded budget, so a patched dimension field that now
        // declares millions of frames is refused on what it declares instead of being materialized.
        let options = IngestOptions {
            max_frames: Some(50_000),
            ..IngestOptions::default()
        };
        match default_registry().ingest(&Source::Local(path), &options) {
            Ok(ingested) => assert!(
                ingested
                    .dataset
                    .episodes
                    .iter()
                    .any(|e| !e.streams.is_empty()),
                "byte {offset} produced a dataset with no streams — a pass over nothing"
            ),
            // A patched signature stops being an HDF5 file at all, which the registry says rather
            // than guessing — also an acceptable answer.
            Err(IngestError::UnsupportedFormat { .. })
            | Err(IngestError::Parse { .. })
            | Err(IngestError::Io(_))
            | Err(IngestError::FrameBudgetExceeded { .. })
            | Err(IngestError::DecompressionBudgetExceeded { .. }) => {}
            Err(other) => panic!("byte {offset} produced an unexpected error: {other:?}"),
        }
    }
}

#[test]
fn the_step_index_span_is_the_first_and_last_step() {
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    let episode = &ingested.dataset.episodes[0];
    assert_eq!((episode.start_ts, episode.end_ts), (Some(0), Some(4)));
    assert_eq!(
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == "hdf5_superblock_version")
            .map(|(_, v)| v.as_str()),
        Some("0"),
        "h5py's default writes a version-0 superblock"
    );
}

#[test]
fn a_run_over_an_unmeasured_timeline_says_so_in_the_verdict() {
    // The report field is not what a user sees — the verdict is. A step-index dataset has to carry
    // its abstention all the way into the findings, or a clean temporal result reads as good timing.
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("robomimic_small.h5")),
        None,
        &IngestOptions::default(),
    )
    .expect("the pipeline runs");
    let verdict = &out.verdict;
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "TEMPORAL.UNMEASURED_CLOCK"),
        "the unmeasured clock reaches the findings"
    );
    assert!(
        !verdict
            .findings
            .iter()
            .any(|f| f.code.starts_with("TEMPORAL.CLOCK_SKEW")),
        "and nothing grades a clock the file never recorded"
    );
}

#[test]
fn the_registry_claims_an_hdf5_file_exactly_once() {
    let registry = default_registry();
    assert!(
        registry.supported_formats().contains(&"hdf5"),
        "the default registry carries the adapter: {:?}",
        registry.supported_formats()
    );
    // Autodetection resolving to exactly one adapter is what makes `veridex check <file>` work
    // without `--format`; two claimants would be an AmbiguousFormat error instead.
    let ingested = registry
        .ingest(
            &Source::Local(fixture("robomimic_small.h5")),
            &IngestOptions::default(),
        )
        .expect("exactly one adapter claims it");
    assert_eq!(ingested.report.format_id, "hdf5");
}

#[test]
fn a_deterministic_fraction_draws_the_same_episodes_every_time() {
    let draw = |seed: u64| {
        ingest(
            "unsorted_names.h5",
            IngestOptions {
                sample: Sample::Fraction {
                    fraction: 0.3,
                    seed,
                },
                ..IngestOptions::default()
            },
        )
        .dataset
        .episodes
        .iter()
        .map(|e| e.index)
        .collect::<Vec<_>>()
    };
    let first = draw(7);
    assert_eq!(first.len(), 1, "a third of three episodes is one episode");
    assert_eq!(
        first,
        draw(7),
        "the same seed always draws the same episode"
    );
    let others: Vec<Vec<u64>> = (0..8).map(draw).collect();
    assert!(
        others.iter().any(|d| *d != first),
        "and a different seed can draw a different one: {others:?}"
    );
}

#[test]
fn a_crafted_dimension_cannot_make_the_reader_synthesize_gigabytes() {
    // Byte 19282 of the fixture is the third byte of the image array's second dataspace dimension.
    // Adding 0x5A turns `6` into 5,898,246, which turns a 144-byte row into a 141 MB one — and a
    // chunked row is *assembled*, not read, so nothing about the file's own 23 KB size bounds it.
    // Before this was charged against the expansion budget, this one byte cost forty seconds and
    // still returned a dataset: four frames of mostly fill value, fingerprinted as content.
    let mut bytes = std::fs::read(fixture("robomimic_small.h5")).expect("read fixture");
    bytes[19282] = bytes[19282].wrapping_add(0x5A);
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("inflated-dimension.h5");
    std::fs::write(&path, &bytes).expect("write");

    let err = default_registry()
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect_err("a row larger than the whole budget is refused");
    assert!(
        matches!(
            err,
            IngestError::DecompressionBudgetExceeded { format_id, .. } if format_id == "hdf5"
        ),
        "got {err:?}"
    );
}

#[test]
fn recomputed_statistics_make_the_statistical_checks_live() {
    // Every fault in `statistical_faults.h5` sits in a *non-first* dimension, which is where a
    // recompute that only ever looks at element 0 gives false confidence: joint 6 is pinned at its
    // limit, joint 3 carries one 250x spike, and `obs/joint_state` hides a NaN in dimension 2.
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("statistical_faults.h5")),
        None,
        &IngestOptions::default(),
    )
    .expect("the pipeline runs");
    let codes: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .map(|f| f.code.as_str())
        .collect();
    for expected in [
        "STATISTICAL.NON_FINITE_OBSERVED",
        "STATISTICAL.SATURATED",
        "STATISTICAL.OUTLIER",
    ] {
        assert!(
            codes.contains(&expected),
            "{expected} did not fire — the statistical family is only live if the adapter \
             recomputes what it reads: {codes:?}"
        );
    }

    let ingested = ingest("statistical_faults.h5", IngestOptions::default());
    let actions = stream_of(&ingested, 0, "actions");
    assert!(
        actions.observed_stats.is_some(),
        "a 7-DoF action array is summarized"
    );
    assert_eq!(
        actions.observed_dim_stats.as_ref().map(|d| d.len()),
        Some(7),
        "one summary per degree of freedom, not one for the whole cell"
    );
    assert_eq!(
        actions.observed_saturation.map(|s| s.dim),
        Some(6),
        "the saturating dimension is named, so the finding points at the gripper"
    );
    assert_eq!(
        actions.observed_non_finite,
        Some(0),
        "read and clean is a different answer from never read"
    );
    assert!(
        actions.stats.is_none(),
        "HDF5 stores no statistics of its own, so there is nothing to compare against"
    );

    // A float depth map is far too wide for per-position statistics, but a NaN pixel still counts.
    let depth = stream_of(&ingested, 0, "obs/depth");
    assert_eq!(depth.observed_non_finite, Some(1));
    assert!(
        depth.observed_dim_stats.is_none() && depth.observed_stats.is_none(),
        "400 pixel positions get no per-position statistics"
    );
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("more than 256 values per frame") && f.contains("obs/depth")),
        "and the report names the arrays it did not summarize: {:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn an_integer_array_counts_as_read_and_clean() {
    // `dones` is int64: it cannot hold a NaN, so reading it is enough to say it is clean. A `None`
    // here would make the non-finite check abstain on a stream it had every reason to judge.
    let ingested = ingest("robomimic_small.h5", IngestOptions::default());
    assert_eq!(
        stream_of(&ingested, 0, "dones").observed_non_finite,
        Some(0)
    );
    assert_eq!(
        stream_of(&ingested, 0, "obs/agentview_image").observed_non_finite,
        Some(0),
        "a uint8 image is integer data too"
    );
}

/// Write a copy of `name` with `patch` applied, and return the ingest result.
fn ingest_patched(
    name: &str,
    label: &str,
    patch: impl FnOnce(&mut Vec<u8>),
) -> Result<veridex_core::adapter::Ingested, IngestError> {
    let mut bytes = std::fs::read(fixture(name)).expect("read fixture");
    patch(&mut bytes);
    // Leaked deliberately: the returned result borrows nothing, but the file has to outlive the
    // call, and a test process is short-lived.
    let dir = Box::leak(Box::new(tempfile::tempdir().expect("temp dir")));
    let path = dir.path().join(format!("{label}.h5"));
    std::fs::write(&path, &bytes).expect("write");
    default_registry().ingest(&Source::Local(path), &IngestOptions::default())
}

fn parse_message(result: Result<veridex_core::adapter::Ingested, IngestError>) -> String {
    match result {
        Err(IngestError::Parse { message, .. }) => message,
        Err(other) => panic!("expected a parse error, got {other:?}"),
        Ok(_) => panic!("expected a refusal, got a dataset"),
    }
}

#[test]
fn each_structure_the_reader_cannot_read_names_itself() {
    // These messages are the user's only remedy, so they are worth asserting rather than assuming.
    // Every one is reached by corrupting the structure it describes in a copy of a real file.
    let find = |bytes: &[u8], needle: &[u8]| {
        bytes
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("fixture holds {}", String::from_utf8_lossy(needle)))
    };

    let message = parse_message(ingest_patched("robomimic_small.h5", "superblock", |b| {
        b[8] = 9; // the superblock version
    }));
    assert!(
        message.contains("superblock version 9"),
        "names the version it found: {message}"
    );

    for signature in [&b"TREE"[..], &b"HEAP"[..], &b"SNOD"[..]] {
        let name = String::from_utf8_lossy(signature).to_string();
        let message = parse_message(ingest_patched(
            "robomimic_small.h5",
            &format!("sig-{name}"),
            |b| {
                let at = find(b, signature);
                b[at] = b'X';
            },
        ));
        assert!(
            message.contains(&name),
            "a corrupt {name} block names the block: {message}"
        );
    }

    // A `GCOL` collection holds the variable-length string attributes; a broken one must not take
    // the whole ingest down, because an attribute is an annotation, not the data.
    let ingested = ingest_patched("robomimic_small.h5", "sig-GCOL", |b| {
        let at = find(b, b"GCOL");
        b[at] = b'X';
    })
    .expect("an unreadable attribute does not fail an otherwise sound dataset");
    assert!(
        ingested
            .dataset
            .metadata
            .iter()
            .all(|(k, _)| k != "h5:env_args"),
        "the attribute that could not be read is absent…"
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("global-heap object could not be read")),
        "…and the report says the attribute could not be *read*, not that it was some kind of \
         value the CDM cannot hold: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_sampled_hdf5_run_carries_its_coverage_and_cannot_be_certified() {
    let sampled = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("robomimic_small.h5")),
        None,
        &IngestOptions {
            sample: Sample::FirstEpisodes(1),
            ..IngestOptions::default()
        },
    )
    .expect("the sampled run completes");
    let full = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("robomimic_small.h5")),
        None,
        &IngestOptions::default(),
    )
    .expect("the full run completes");

    assert_ne!(
        sampled.verdict.result_content_hash, full.verdict.result_content_hash,
        "a run that saw one of two episodes must not share a verdict hash with one that saw both"
    );
    let err = veridex_core::certificate::Certificate::certifiable(&sampled.verdict)
        .expect_err("a certificate speaks for a dataset, not for the part of it that was read");
    assert!(err.contains("sampled run"), "{err}");
    assert!(veridex_core::certificate::Certificate::certifiable(&full.verdict).is_ok());
}

#[test]
fn a_cyclic_btree_is_a_parse_error_not_a_stack_overflow() {
    // `btree_cycle.h5` is `robomimic_small.h5` with one group B-tree node relabelled as interior
    // (level 1) and its first child pointer aimed back at the node itself. The walkers recurse on
    // interior nodes, so before the visited-set existed this recursed until the thread's stack was
    // gone — and a stack overflow *aborts the process* (SIGABRT, exit 134). A CI gate or a
    // validation service reading an untrusted file cannot report that; it just dies. Every other
    // corruption in this file's tests comes back as a named error, and so must this one.
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("btree_cycle.h5")),
            &IngestOptions::default(),
        )
        .expect_err("a self-referential B-tree node must be refused");
    match err {
        IngestError::Parse { message, .. } => {
            assert!(message.contains("cyclic"), "got {message}")
        }
        other => panic!("expected a parse error naming the cycle, got {other:?}"),
    }
}

/// A chunked dataset that is only partly written has no index entry for the unwritten chunks; HDF5
/// defines those regions as the dataset's declared fill value, and that is what `h5py` returns. The
/// reader zero-initialized the row instead, so every unwritten element came back as 0 — silently,
/// with `coverage: Full` and nothing in `unmapped_fields`. The written chunks decoded correctly, so
/// only the invented part was wrong, which is the hardest kind of wrong to notice.
///
/// The hashes below are `sha256(h5py_row.tobytes())`, taken from `h5py` reading this same fixture.
#[test]
fn unwritten_regions_read_as_the_declared_fill_value_not_as_zeros() {
    let ingested = ingest("partial_fill.h5", IngestOptions::default());

    // Only columns 0..5 of 20 were written; the rest is fill 7.5.
    assert_row_hashes(
        stream_of(&ingested, 0, "actions"),
        &[
            "773362c3f1fb07c58c60bf29f131a65c2a20b7dc93847350c9f5e61e943fd103",
            "76ab32749f98a51c09f1372c1145fd82410585ddde97f14bb429b007bc3be59f",
            "90245e59ddf2e8ad8559aab1f92d0bf4c30a576ab719925bcadc939dcfe38408",
            "dfe318b1d409538bb42a328ca3b565c082f2e78bd4eeea24bfc79c19d009499b",
        ],
    );
}

/// Rows 2 and 3 of `obs` are covered by *no* chunk at all — a logger that pre-allocated 4 steps and
/// wrote 2. `h5py` reads them as the fill value. The reader refused the whole file with "the
/// dataset's chunk index is incomplete", blaming a file that was complete and correct — and doing so
/// inconsistently with the partly-written row above, which it fabricated without complaint.
#[test]
fn a_wholly_unwritten_row_is_the_fill_value_not_a_corrupt_index() {
    let ingested = ingest("partial_fill.h5", IngestOptions::default());
    assert_row_hashes(
        stream_of(&ingested, 0, "obs"),
        &[
            "8a31a40ecac0ceb4d87b30bd156ca7a547e8e33dc071454b765fbc777d1c34a1",
            "8a31a40ecac0ceb4d87b30bd156ca7a547e8e33dc071454b765fbc777d1c34a1",
            "f48af7dd3f3adb7dcc618687a0a2a61b8df98065ca0c180875e8ae65a88e28e4",
            "f48af7dd3f3adb7dcc618687a0a2a61b8df98065ca0c180875e8ae65a88e28e4",
        ],
    );
}

/// The worst instance of the same bug: with `fillvalue=nan` and a partial write, the fabricated
/// zeros made `observed_non_finite` report `Some(0)` — which means "every value was read and every
/// one was finite". `STATISTICAL.NON_FINITE_OBSERVED` then returned a confident clean answer over
/// eight NaNs it never read.
#[test]
fn a_nan_fill_value_is_seen_by_the_non_finite_check() {
    let ingested = ingest("partial_fill.h5", IngestOptions::default());
    let s = stream_of(&ingested, 0, "nanfill");
    assert_row_hashes(
        s,
        &[
            "31f7fdde50e0a38c241f71e37ba65be3f2cb4d79d47a47ebc65cce5932834eb8",
            "31f7fdde50e0a38c241f71e37ba65be3f2cb4d79d47a47ebc65cce5932834eb8",
            "31f7fdde50e0a38c241f71e37ba65be3f2cb4d79d47a47ebc65cce5932834eb8",
            "31f7fdde50e0a38c241f71e37ba65be3f2cb4d79d47a47ebc65cce5932834eb8",
        ],
    );
    assert_eq!(
        s.observed_non_finite,
        Some(8),
        "two NaN columns over four rows must be counted, not read as zeros"
    );
}

/// The frame budget's contract is to refuse on what a file *declares*, before anything is
/// allocated — and the timeline read was the one path that never charged it. A 5,000,000-row `time`
/// array was read in full and only then measured against the limit, so the correct refusal arrived
/// after the memory had already been spent. (The audit measured the extreme form on a hostile file:
/// a datatype patched to zero width makes the row reader non-terminating, and a 24 MB input grew
/// past 2.6 GB with no ceiling and no error.)
///
/// The fixture is 7.8 KB and declares 5M rows, so a run that returns promptly is itself the
/// assertion: it cannot have read them.
#[test]
fn the_frame_budget_refuses_a_declared_timeline_before_reading_it() {
    let started = std::time::Instant::now();
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("huge_timeline.h5")),
            &IngestOptions {
                max_frames: Some(10),
                ..IngestOptions::default()
            },
        )
        .expect_err("a budget of 10 frames cannot hold a declared 5,000,000-row timeline");
    assert!(
        matches!(err, IngestError::FrameBudgetExceeded { format_id, .. } if format_id == "hdf5"),
        "got {err:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the refusal must come from the declared row count, not from reading the rows"
    );
}

/// Two file-driven arithmetic overflows, each a debug-build panic and a release-build wrong answer.
///
/// The repo sets no `[profile]` overrides, so `cargo test` and every dev build have overflow checks
/// on — these aborted the process outright. In release they wrap silently, which is worse in one of
/// the two cases: `attr.dims.iter().product()` over `(2^63, 2)` wrapped to 0, `.max(1)` made it 1,
/// and a 2^64-element attribute was read as a single scalar and folded into provenance, task, or
/// units. A file from a stranger must not be able to do either.
#[test]
fn a_file_supplied_size_that_overflows_is_refused_not_a_panic() {
    // `attr_dims_overflow.h5` is `robomimic_small.h5` with a (2, 2) attribute's dataspace rewritten
    // to (2^63, 2); `gcol_size_overflow.h5` has a global-heap object size set to u64::MAX, which
    // overflowed inside `next_multiple_of(8)` before the surrounding `checked_add` ever saw it.
    for name in ["attr_dims_overflow.h5", "gcol_size_overflow.h5"] {
        // Returning at all is the assertion. Either outcome is sound: the file is refused, or the
        // overflowing field is treated as unreadable and the rest of the file still reads.
        match default_registry().ingest(&Source::Local(fixture(name)), &IngestOptions::default()) {
            Ok(ingested) => assert!(
                !ingested.dataset.episodes.is_empty(),
                "{name}: an ingest that succeeded must have read something"
            ),
            Err(IngestError::Parse { message, .. }) => {
                assert!(!message.is_empty(), "{name}: the refusal names something")
            }
            Err(other) => panic!("{name}: expected a parse error, got {other:?}"),
        }
    }
}

/// Assembling rows must not cost `O(rows x chunks)`.
///
/// `chunked_row` found the chunks covering a row by filtering the *entire* chunk index, once per
/// row. For a dataset chunked one row at a time — `chunks=(1, ...)`, the ordinary shape a per-step
/// robot logger writes — the chunk count scales with the row count, so that is quadratic. The two
/// bounds meant to contain it, the frame budget on rows and `MAX_CHUNKS_PER_DATASET` on chunks,
/// bound them independently: nothing bounded the product, and nothing bounds CPU time at all.
///
/// Measured on the release binary before the fix: a 6.3 MB file (100,000 rows, 100,000 chunks) cost
/// **70 seconds** of CPU, and 12.5 MB did not finish in eight minutes. After: 0.12 s and 0.22 s —
/// linear, because the index is a `BTreeMap` keyed by origin and the covering origins are a
/// contiguous range in it.
///
/// This fixture is 16,000 rows and 16,000 chunks, sized so the quadratic version blows the ceiling
/// by a wide margin in a debug test build while staying small enough to be a unit test. A 4,000-row
/// fixture did *not* discriminate — the old code read it in 2.4 s, comfortably inside any ceiling
/// that would not flake — which is the trap a timing test of this kind sets for itself.
#[test]
fn a_dataset_chunked_one_row_at_a_time_reads_in_linear_time() {
    let started = std::time::Instant::now();
    let ingested = ingest("many_chunks.h5", IngestOptions::default());
    let elapsed = started.elapsed();

    let stream = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "actions")
        .expect("the actions stream");
    assert_eq!(stream.frames.len(), 16_000, "every row is read");

    assert!(
        elapsed < std::time::Duration::from_secs(20),
        "reading 16,000 one-row chunks took {elapsed:?} — the per-row rescan is back"
    );
}

// ---- Reading the file's headers without reading a chunk ----

fn metadata_only() -> IngestOptions {
    IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    }
}

#[test]
fn the_headers_alone_yield_the_episodes_streams_and_types() {
    // An HDF5 file keeps its structure in the container, but not in its data: the group tree, each
    // array's datatype and shape, and every attribute are headers. Reading them describes the file
    // without touching a chunk.
    let summary = ingest("robomimic_small.h5", metadata_only());
    let full = ingest("robomimic_small.h5", IngestOptions::default());

    assert_eq!(
        summary.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: full.dataset.episodes.len() as u64
        }
    );
    // Every episode's streams, rendered as text: name, datatype, per-row shape.
    fn shape(i: &veridex_core::adapter::Ingested) -> Vec<String> {
        i.dataset
            .episodes
            .iter()
            .flat_map(|e| {
                e.streams
                    .iter()
                    .map(move |s| format!("{}/{} {:?} {:?}", e.index, s.name, s.dtype, s.shape))
            })
            .collect()
    }
    assert_eq!(
        shape(&summary),
        shape(&full),
        "the episodes, streams, datatypes and shapes all come from headers a full read uses too"
    );
    assert!(summary
        .dataset
        .episodes
        .iter()
        .all(|e| e.streams.iter().all(|s| s.frames.is_empty())));
    // Every statistic HDF5 offers is recomputed from values, and no value was read. `None`, not
    // `Some(0)`: that difference is what tells a clean stream from an unread one.
    assert!(summary
        .dataset
        .episodes
        .iter()
        .all(|e| e.streams.iter().all(|s| s.observed_non_finite.is_none())));
    assert!(
        !summary
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("content_hash") || f.contains("recomputed")),
        "a run that read no row must not claim to have hashed or summarized one: {:?}",
        summary.report.mapped_fields
    );
}

#[test]
fn the_clock_reported_is_the_files_own_not_a_consequence_of_reading_nothing() {
    // Whether the file records measured time — a timestamp array that declares its units — is in
    // the headers, so it is knowable without a chunk. Reporting a step index here would be this
    // run's abstention dressed up as a fact about the file, bound into the content hash as one.
    let timed = ingest("timed_rig.h5", metadata_only());
    let actions = timed.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "actions")
        .expect("actions");
    assert_eq!(actions.clock_id, "hdf5-time");
    assert_eq!(actions.clock_kind, ClockKind::Measured);
    assert!(actions.frames.is_empty(), "declared, not read");

    // A timestamp array that states no units is still not a clock.
    let untimed = ingest("untimed_units.h5", metadata_only());
    assert!(untimed.dataset.episodes[0]
        .streams
        .iter()
        .all(|s| s.clock_kind == ClockKind::StepIndex));
}

#[test]
fn a_metadata_only_run_reads_no_chunk() {
    // Proved rather than asserted, and precisely: find a byte whose corruption a *full* read catches
    // as a chunk checksum failure — so that byte is chunk data, not a header — and check that the
    // header-only run of the same file is untouched by it.
    let bytes = std::fs::read(fixture("timed_rig.h5")).expect("read fixture");
    let dir = tempfile::tempdir().expect("temp dir");
    let intact = dir.path().join("intact.h5");
    std::fs::write(&intact, &bytes).unwrap();
    let expected = veridex_core::canonical::content_hash(
        &default_registry()
            .ingest(&Source::Local(intact), &metadata_only())
            .expect("headers describe the file")
            .dataset,
    );

    let mut proved = 0;
    for offset in (bytes.len() / 2..bytes.len()).step_by(97) {
        let mut patched = bytes.clone();
        patched[offset] ^= 0xFF;
        // One name for both reads: the dataset id comes from the file name, so two paths would
        // differ in the CDM for a reason that has nothing to do with what was read.
        let path = dir.path().join("intact.h5");
        std::fs::write(&path, &patched).expect("write");
        let full =
            default_registry().ingest(&Source::Local(path.clone()), &IngestOptions::default());
        let Err(IngestError::Parse { message, .. }) = &full else {
            continue;
        };
        if !(message.contains("fletcher32") && message.contains("corrupt")) {
            continue;
        }
        // That byte is chunk data. The header-only run must not notice.
        let summary = default_registry()
            .ingest(&Source::Local(path), &metadata_only())
            .unwrap_or_else(|e| panic!("corrupting chunk data broke the header-only read: {e}"));
        assert_eq!(
            veridex_core::canonical::content_hash(&summary.dataset),
            expected,
            "corrupting a byte only a full read can see changed what the header-only read produced"
        );
        proved += 1;
    }
    assert!(
        proved > 0,
        "no byte was found that a full read catches as chunk corruption, so this proves nothing"
    );
}

/// A `robomimic` file is not only `/data`: it carries `/mask` (which demonstrations are in the train
/// split), and hand-rolled collectors park reward tables and raw logs at the root the same way. None
/// of it is under an episode group, so none of it is read — and being unread, it has to reach the
/// *verdict*, not a note only `inspect` prints.
#[test]
fn what_the_root_holds_beside_the_episodes_is_disclosed_as_unread() {
    let ingested = ingest("root_siblings.h5", IngestOptions::default());
    let unread: Vec<&str> = ingested
        .report
        .unread_sources
        .iter()
        .map(|f| f.source_path.as_str())
        .collect();
    assert_eq!(
        unread,
        ["/logs", "/mask", "/reward_model"],
        "a group of arrays, an array, and a group whose arrays are two levels down are each data \
         the file holds and this run did not read"
    );

    // The other half of the line: an object with no rows under it is a note about shape, not a hole
    // in coverage. Calling it unread would fire `COVERAGE.SOURCE_UNREAD` over nothing.
    let unmapped: Vec<&str> = ingested
        .report
        .unmapped_fields
        .iter()
        .map(|f| f.source_path.as_str())
        .collect();
    for quiet in ["/notes", "/zero_rows", "/scalar_at_root"] {
        assert!(
            unmapped.contains(&quiet) && !unread.contains(&quiet),
            "{quiet} holds no rows, so it is unmapped rather than unread: {unmapped:?}"
        );
    }
    assert_eq!(
        ingested.report.coverage,
        Coverage::Full,
        "the episodes themselves were read in full; what is missing is disclosed as a finding"
    );
}

#[test]
fn an_unread_root_object_reaches_the_verdict_not_only_the_ingest_report() {
    let outcome = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("root_siblings.h5")),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    let finding = outcome
        .verdict
        .findings
        .iter()
        .find(|f| f.code == "COVERAGE.SOURCE_UNREAD")
        .expect("the unread root objects surface as a coverage finding");
    assert_eq!(finding.severity, veridex_core::check::Severity::Warning);
    for named in ["/mask", "/reward_model", "/logs"] {
        assert!(
            finding.message.contains(named),
            "the finding names every unread source: {}",
            finding.message
        );
    }

    // A file with nothing beside its episodes must not carry the finding at all.
    let clean = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("robomimic_small.h5")),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    assert!(
        !clean
            .verdict
            .findings
            .iter()
            .any(|f| f.code == "COVERAGE.SOURCE_UNREAD"),
        "a file whose root holds only `/data` has no unread source"
    );
}

/// The disclosure is read from object headers, so declining to open a chunk cannot hide it.
#[test]
fn a_metadata_only_run_discloses_the_same_unread_root_objects() {
    let full = ingest("root_siblings.h5", IngestOptions::default());
    let headers = ingest("root_siblings.h5", metadata_only());
    let paths = |i: &veridex_core::adapter::Ingested| -> Vec<String> {
        i.report
            .unread_sources
            .iter()
            .map(|f| f.source_path.clone())
            .collect()
    };
    assert_eq!(paths(&full), paths(&headers));
}
