//! Zarr adapter tests.
//!
//! The fixtures are **real `zarr` + `numcodecs` output**, committed under `tests/fixtures/zarr/`, and
//! the per-row SHA-256 values below come from Python itself: a reader tested only against its own
//! writer proves the two agree, not that either matches the format.
//! `tests/fixtures/zarr/generate_fixtures.py` regenerates them.

use std::path::PathBuf;

use veridex_core::adapter::{
    default_registry, Coverage, IngestError, IngestOptions, Ingested, Sample, Source,
};
use veridex_core::cdm::{ClockKind, Modality, Stream};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/zarr")
        .join(name)
}

fn ingest(name: &str, options: IngestOptions) -> Ingested {
    default_registry()
        .ingest(&Source::Local(fixture(name)), &options)
        .unwrap_or_else(|e| panic!("{name} ingests: {e}"))
}

fn refusal(name: &str) -> String {
    match default_registry().ingest(&Source::Local(fixture(name)), &IngestOptions::default()) {
        Err(IngestError::Parse { message, .. }) => message,
        Err(other) => panic!("{name}: expected a parse error, got {other:?}"),
        Ok(_) => panic!("{name}: expected a refusal, got a dataset"),
    }
}

fn stream_of<'a>(ingested: &'a Ingested, episode: u64, name: &str) -> &'a Stream {
    ingested
        .dataset
        .episodes
        .iter()
        .find(|e| e.index == episode)
        .and_then(|e| e.streams.iter().find(|s| s.name == name))
        .unwrap_or_else(|| panic!("episode {episode} has stream {name}"))
}

fn row_hashes(stream: &Stream) -> Vec<String> {
    stream
        .frames
        .iter()
        .map(|f| {
            f.value_ref
                .content_hash
                .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .expect("rows are fingerprinted")
        })
        .collect()
}

/// The SHA-256 of each row of `dp_replay.zarr`'s arrays, taken from Python.
const ACTION_ROWS: [&str; 10] = [
    "d9a1362eb38f1d838fb6999c04e7b4cad8d872a5df03175adaaf8da1d7f1fcc2",
    "ccf7e64bb0b0b19d13ac0e2e57a612aa674fc74de1a0307b7ed435e7d735ab9b",
    "8dca8907c6ff2ddb26ff934fa96a6a4a50164c14c65cff3bed107402ad6b0177",
    "3807b6e5ccdd86368a7a1277a20d0eab8851e7b38fb4d07e07c902eb6de08e2e",
    "0685058e7bb6640f345c600e722febab882aeba888985cb2d1a1d386ef9cf68b",
    "33ec31ecc360ccd8d14f0abe19b5914288fe8b3bbdfb1434f448cffa57cecdb3",
    "2d9b58b35a470072c53486ef0504ecfefd785da2dc860fbcec874e91b9487f6b",
    "e474bcd8720d45e834a0fe8aec6f9427657baa4282701a2480d0dbf74f6b1a09",
    "b490c16bf28dcda271ea485bbdb6a13c7ec81fdadf901e40faf0ed64d824d822",
    "d9755591cb58972fae1affe08987cf66d86f0022206000d87c333d9d18d545f1",
];
const IMG_ROWS: [&str; 10] = [
    "1e26d61128c756391587ef84f4e63e2850aa4050246c5e3ed5e02552d623fa7a",
    "143225f042b2d5dc762f4eb3346314fae7b1d9b9d43f6c7f811d89e16482e050",
    "162e3d2f4ddbdf87bd9868861f46a5a1c28f4e4814bcf7e83580b2d5e55f78f7",
    "83e532b220b285230bb43f5148f711da4e3b676191ae85d6e76ae84c0299c96a",
    "b018bb01e60cb9703bb9abbeabbc62e122e727c9edeee9a8e8f083de308274b4",
    "97dff063a4932de8b3d777c78c7dfbe1d058029c9d27f09efd58a5288018e9de",
    "eb886242eac10b7a9f92ff2e1610ec5db340682d55bfb8f70558797badb2f1c0",
    "306b44791aa3486dc24b34a90f123947ef5157b52c29853418a5bff23a37345e",
    "100628c08d77672fa20eb55f2266a033426765e27c5e666aacea36277c654b71",
    "19b133ad971106f8fabe4a0af31f5b6d11f4c15acf2052d1d89abc6db4a73a79",
];

#[test]
fn a_replay_buffer_becomes_one_episode_per_boundary() {
    // `episode_ends = [4, 10]`: episode 0 is rows 0..4 and episode 1 is rows 4..10.
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    assert_eq!(ingested.report.format_id, "zarr");
    assert_eq!(
        ingested
            .dataset
            .episodes
            .iter()
            .map(|e| (e.index, e.streams.len(), e.streams[0].frames.len()))
            .collect::<Vec<_>>(),
        vec![(0, 3, 4), (1, 3, 6)],
        "the boundaries are the episode structure"
    );
    let names: Vec<&str> = ingested.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["action", "img", "state"]);
    assert!(ingested
        .report
        .mapped_fields
        .iter()
        .any(|f| f.contains("episode_ends")));
}

#[test]
fn every_episode_gets_the_rows_its_boundary_names() {
    // Sliced, not re-read: episode 1's frames must be rows 4..10 of the flat array, in order.
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    assert_eq!(
        row_hashes(stream_of(&ingested, 0, "action")),
        ACTION_ROWS[..4],
        "blosc + lz4 + byte shuffle, over rows 0..4"
    );
    assert_eq!(
        row_hashes(stream_of(&ingested, 1, "action")),
        ACTION_ROWS[4..],
        "and rows 4..10 for the second episode"
    );
    // `img` is chunked (3, 2, 4, 2) over a (10, 4, 6, 3) array, so every row is stitched from six
    // chunks with a ragged edge on three axes.
    assert_eq!(row_hashes(stream_of(&ingested, 0, "img")), IMG_ROWS[..4]);
    assert_eq!(row_hashes(stream_of(&ingested, 1, "img")), IMG_ROWS[4..]);
}

#[test]
fn every_codec_decodes_to_the_same_bytes() {
    // One array per codec, all holding identical values. Any codec that decoded wrongly would break
    // the equality — and there is no plausible way to be wrong and still agree with the others.
    let ingested = ingest("codec_zoo.zarr", IngestOptions::default());
    let expected = [
        "af961e9b2afd84544c71b51334ffc9792538e210eb1c0eadba8fdb1495558223",
        "cbe6972b5d452a3e09aaa8f18b46d0381b5158181a48b479e4615f248cfd9f0a",
        "68a4bec3b8d9e2490b26a49b6314c118dd827e7adf5e60392af13beb7b8a6828",
        "8fd80fedea10b95da5a34d97ebaf8a49df762a1983f0fcead9ac9f517f3c0a2e",
        "c6a3dada6cf20a1bc71d738656ece7f19021ae335c9ff104a9228604e3612691",
        "76416ac3772a40d552f9b1614fc0999799de23ca4f59881da9948b50952ebef0",
    ];
    for codec in [
        "none",
        "zlib",
        "gzip",
        "zstd",
        "lz4",
        "blosc_lz4_shuffle",
        "blosc_zstd_noshuffle",
        "blosc_zlib_shuffle",
    ] {
        assert_eq!(
            row_hashes(stream_of(&ingested, 0, codec)),
            expected,
            "codec {codec} decoded to different bytes than Python wrote"
        );
    }
}

#[test]
fn a_codec_this_reader_cannot_apply_is_refused_by_name() {
    // Reading a compressed array through the wrong codec does not fail — it yields plausible-looking
    // numbers. So an unsupported codec has to be a refusal that names itself and says what to do.
    let message = refusal("blosclz.zarr");
    assert!(
        message.contains("blosclz") && message.contains("cname"),
        "{message}"
    );
    let message = refusal("bitshuffle.zarr");
    assert!(
        message.contains("bit shuffle") && message.contains("shuffle=1"),
        "{message}"
    );
    let message = refusal("fortran_order.zarr");
    assert!(
        message.contains("order") && message.contains("C order"),
        "{message}"
    );
}

#[test]
fn a_boundary_that_contradicts_the_data_is_refused_not_clamped() {
    let message = refusal("ends_backwards.zarr");
    assert!(
        message.contains("non-decreasing"),
        "a boundary that goes backwards is not an episode: {message}"
    );
    let message = refusal("ends_past_rows.zarr");
    assert!(
        message.contains("past the") && message.contains("row"),
        "a boundary past the end of the arrays it indexes: {message}"
    );
}

#[test]
fn rows_past_the_last_boundary_belong_to_no_episode_and_are_disclosed() {
    // 8 rows, boundaries [4, 6]: rows 6..8 belong to no episode. Attaching them to the last one
    // would hide exactly the off-by-one a replay buffer gets wrong.
    let ingested = ingest("ends_short.zarr", IngestOptions::default());
    assert_eq!(
        ingested
            .dataset
            .episodes
            .iter()
            .map(|e| e.streams[0].frames.len())
            .collect::<Vec<_>>(),
        vec![4, 2]
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("past the last episode boundary")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_timeline_becomes_a_clock_only_when_its_units_are_declared() {
    let timed = ingest("timed.zarr", IngestOptions::default());
    let action = stream_of(&timed, 1, "action");
    assert_eq!(action.clock_kind, ClockKind::Measured);
    assert_eq!(action.clock_id, "zarr-time");
    assert_eq!(
        action.frames.iter().map(|f| f.ts).collect::<Vec<_>>(),
        vec![150_000_000, 200_000_000, 250_000_000],
        "the second episode's rows carry their own recorded times, in nanoseconds"
    );

    let untimed = ingest("untimed.zarr", IngestOptions::default());
    assert_eq!(
        stream_of(&untimed, 0, "action").clock_kind,
        ClockKind::StepIndex,
        "seconds or nanoseconds is not stated, so this is not a clock"
    );
    assert!(untimed
        .report
        .unmapped_fields
        .iter()
        .any(|f| f.note.contains("units")));
}

#[test]
fn a_store_with_no_recorded_time_is_on_a_step_index_that_restarts_each_episode() {
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    let second = stream_of(&ingested, 1, "action");
    assert_eq!(
        second.frames.iter().map(|f| f.ts).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5],
        "an episode's steps count from zero, not from its offset into the replay buffer"
    );
    assert!(ingested
        .dataset
        .episodes
        .iter()
        .all(|e| e.duration_ns().is_none()));
}

#[test]
fn types_shapes_and_modalities_come_from_the_zarray() {
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    let action = stream_of(&ingested, 0, "action");
    assert_eq!(action.dtype.as_deref(), Some("float32"));
    assert_eq!(action.shape, Some(vec![2]));
    assert_eq!(action.modality, Modality::Action);

    let state = stream_of(&ingested, 0, "state");
    assert_eq!(state.dtype.as_deref(), Some("float64"));
    assert_eq!(state.modality, Modality::ScalarState);

    let img = stream_of(&ingested, 0, "img");
    assert_eq!(img.dtype.as_deref(), Some("uint8"));
    assert_eq!(img.shape, Some(vec![4, 6, 3]));
    assert_eq!(img.modality, Modality::Video);
}

#[test]
fn values_are_summarized_so_the_statistical_checks_apply() {
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    let state = stream_of(&ingested, 0, "state");
    assert!(state.observed_stats.is_some());
    assert_eq!(state.observed_dim_stats.as_ref().map(|d| d.len()), Some(5));
    assert_eq!(state.observed_non_finite, Some(0));
    assert!(
        state.stats.is_none(),
        "Zarr stores no statistics of its own, so there is nothing to compare against"
    );
}

#[test]
fn attributes_become_metadata_and_provenance() {
    let ingested = ingest("dp_replay.zarr", IngestOptions::default());
    let meta = |key: &str| {
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(meta("source_format"), Some("zarr"));
    assert_eq!(meta("zarr_format"), Some("2"));
    assert_eq!(meta("declared_total_episodes"), Some("2"));
    assert_eq!(
        ingested.dataset.episodes[0].task.as_deref(),
        Some("push the block")
    );
    assert!(ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .any(|e| e.key == "author"));
}

#[test]
fn the_same_store_ingests_to_the_same_content_hash() {
    let a = ingest("dp_replay.zarr", IngestOptions::default());
    let b = ingest("dp_replay.zarr", IngestOptions::default());
    assert_eq!(
        veridex_core::canonical::content_hash(&a.dataset),
        veridex_core::canonical::content_hash(&b.dataset)
    );
}

#[test]
fn a_sample_is_honored_and_reported_as_partial() {
    let ingested = ingest(
        "dp_replay.zarr",
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
            episodes_ingested: 1
        }
    );
    assert_eq!(
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == "declared_total_episodes")
            .map(|(_, v)| v.as_str()),
        Some("1"),
        "under a sample the comparable count is what the sample selected, not the whole store"
    );
}

#[test]
fn the_frame_budget_refuses_before_it_allocates() {
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("dp_replay.zarr")),
            &IngestOptions {
                max_frames: Some(2),
                ..IngestOptions::default()
            },
        )
        .expect_err("a budget of 2 frames cannot hold this store");
    assert!(
        matches!(err, IngestError::FrameBudgetExceeded { format_id, .. } if format_id == "zarr"),
        "got {err:?}"
    );
}

#[test]
fn the_registry_claims_a_zarr_store_exactly_once() {
    let registry = default_registry();
    assert!(registry.supported_formats().contains(&"zarr"));
    let ingested = registry
        .ingest(
            &Source::Local(fixture("dp_replay.zarr")),
            &IngestOptions::default(),
        )
        .expect("exactly one adapter claims a Zarr store");
    assert_eq!(ingested.report.format_id, "zarr");
}

#[test]
fn a_zarr_store_flows_through_the_whole_pipeline() {
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("dp_replay.zarr")),
        None,
        &IngestOptions::default(),
    )
    .expect("the pipeline runs");
    // Clean data: nothing structural or temporal may fire, and the unmeasured clock must be stated.
    let noise: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .map(|f| f.code.as_str())
        .filter(|c| !c.starts_with("PROVENANCE."))
        .filter(|c| *c != "TEMPORAL.UNMEASURED_CLOCK")
        .collect();
    assert!(
        noise.is_empty(),
        "clean store, unexpected findings: {noise:?}"
    );
    assert!(out
        .verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.UNMEASURED_CLOCK"));
}

#[test]
fn a_v3_store_is_refused_by_version_not_reported_as_unknown() {
    // Zarr v3 renames every metadata file and changes the codec model. Recognizing it and refusing by
    // version is the difference between "re-save this as v2" and "we have no idea what this is".
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("v3_store.zarr")),
            &IngestOptions::default(),
        )
        .expect_err("a v3 store is not readable here");
    match err {
        IngestError::UnsupportedVersion {
            format_id,
            version,
            supported,
        } => {
            assert_eq!(format_id, "zarr");
            assert_eq!(version.as_deref(), Some("3"));
            assert_eq!(supported, ["2"]);
        }
        other => panic!("expected an unsupported-version error, got {other:?}"),
    }
}

#[test]
fn an_unwritten_chunk_reads_as_the_declared_fill_value() {
    // `sparse_fill.zarr` writes rows 0..2 and 4..6 of a float array whose `fill_value` is `"NaN"`,
    // and rows 0..2 of an int array whose fill is `-1`. The chunks covering the gaps are not in the
    // store at all. Reading them as zeros would turn missing data into plausible data — and would
    // hide the NaNs that `STATISTICAL.NON_FINITE_OBSERVED` exists to catch.
    let ingested = ingest("sparse_fill.zarr", IngestOptions::default());
    assert_eq!(
        row_hashes(stream_of(&ingested, 0, "state")),
        [
            "dc91ce9a50ddc828740aa26743716897fdb2bb64f1db662fe263a59be56145ae",
            "bed9efba025f2da91e4ece76e380f86ca1cd1765aea7f5bb87f607b547061efa",
            "241808cce19b49683d2308412efef71f1f4c7dcf2627039cba044bf44f6e3533",
            "241808cce19b49683d2308412efef71f1f4c7dcf2627039cba044bf44f6e3533",
            "9bb4833ece80484c0fb4bcbd256d3ad2a68476fdbce3a92298484932837041c7",
            "ac6a48691a9269689d056bda012b4cbbfb30c82b85662b4aef6bee596167613d",
        ],
        "every row matches what Python reads back, fill included"
    );
    assert_eq!(
        row_hashes(stream_of(&ingested, 0, "count")),
        [
            "e8613f5a5bc9f9feeda32a8e7c80b69dd4878e47b6a91723fb15eb84236b6a2b",
            "dc765660b06ee03dd16fd7ca5b957e8c805161ac2c4af28c5a100ab2ab432ca1",
            "ad95131bc0b799c0b1af477fb14fcf26a6a9f76079e48bf090acb7e8367bfd0e",
            "ad95131bc0b799c0b1af477fb14fcf26a6a9f76079e48bf090acb7e8367bfd0e",
            "ad95131bc0b799c0b1af477fb14fcf26a6a9f76079e48bf090acb7e8367bfd0e",
            "ad95131bc0b799c0b1af477fb14fcf26a6a9f76079e48bf090acb7e8367bfd0e",
        ],
        "a non-zero integer fill is the value the store declared, not zero"
    );
    // And the fill is not merely bytes: the NaNs it introduces are counted as what they are.
    assert_eq!(
        stream_of(&ingested, 0, "state").observed_non_finite,
        Some(4),
        "four NaN values across the two unwritten rows"
    );
}
