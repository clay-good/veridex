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

/// Every array in `blosc_real.zarr`, with the rows Python reads back.
///
/// These arrays are large enough that blosc actually *compresses* them, and several are forced to
/// many blocks. That combination is what the first fixture set missed entirely: every blosc chunk in
/// it was small enough that blosc stored it verbatim (the MEMCPYED flag), so the codec ids, the block
/// offsets, the split streams, and the shuffle were all unexercised — and two of them were wrong.
const BLOSC_REAL: &[(&str, &[&str])] = &[
    (
        "lz4_shuf_multi",
        &[
            "aa4f38cd0b8d07ef988ee1172f9c11a73124c3da08fa5260bcbd089cd91aa449",
            "7b6fd13e4aa8a05e9b5cddb1efe85c8d1f1369e45c56c497f3ac1e0cdf681929",
            "4ee848761af02ed696e6ab844add89f9141c85e158ce8b9cbf23dad11c259dfe",
            "fc889f151246af05a5b6db7710ae4f2545689c3ca85659471ec9750d90f8bfb8",
            "6c3178d6ec29fc75ca21b3310aa649b1014d7e5d8040faca1712b9b69c384ddf",
            "a9f1fbd4055eb7db184ea7bdcb536f8249df3b30227830a62f17c48a02e403bc",
            "b436248aaf2cf8c58afad110bd1c405024c58f16229d37fbb77de86b01203b8f",
            "fe1934fc43c5e8e56c7a5bf8ad6c8ea8ccaa9bd260120679c750fdf0e561ba9d",
        ],
    ),
    (
        "zstd_shuf_multi",
        &[
            "2f12b71132df2a1d0476f254b94a22645007edd29eedd0c64974f6650785c391",
            "74523bf233f30b8a7ed2a253e3d9907f9a101a3b99b48f0e359d4e1cc7de4a9d",
            "a03c36aa5b4bc15314c6b3ce4b902a206a8cc7ea6e92d71ea95e6164556aab96",
            "ea0e10ff1de5b6dd8f91b58a56c1cac4e1b8e7022c7d319677444483d366349c",
            "2b978968d1da3f52e2fcc949e61209b7151fc2df17a39b719c945be274f36618",
            "adc38b2f17557371694eff3e46afc089f3e6d68580f0213004ce7ddab12dc39a",
            "0a060b8b21350c5ef108b4badd7e9d8e0c11758b284da4330fd2bd3a82347a5f",
            "df5af30bb138c14439e8a2bb5ca6b165a4631751a051dc10a4d3513f31edae8f",
        ],
    ),
    (
        "zlib_shuf_multi",
        &[
            "2f12b71132df2a1d0476f254b94a22645007edd29eedd0c64974f6650785c391",
            "74523bf233f30b8a7ed2a253e3d9907f9a101a3b99b48f0e359d4e1cc7de4a9d",
            "a03c36aa5b4bc15314c6b3ce4b902a206a8cc7ea6e92d71ea95e6164556aab96",
            "ea0e10ff1de5b6dd8f91b58a56c1cac4e1b8e7022c7d319677444483d366349c",
            "2b978968d1da3f52e2fcc949e61209b7151fc2df17a39b719c945be274f36618",
            "adc38b2f17557371694eff3e46afc089f3e6d68580f0213004ce7ddab12dc39a",
            "0a060b8b21350c5ef108b4badd7e9d8e0c11758b284da4330fd2bd3a82347a5f",
            "df5af30bb138c14439e8a2bb5ca6b165a4631751a051dc10a4d3513f31edae8f",
        ],
    ),
    (
        "lz4hc_shuf",
        &[
            "e70540b2a774257412c15b594eac07279f5fda53f7299308094d4038fefdd7f6",
            "b036ccaed3c80fa5f522547632839f25a5b32876f4601a96dd29b326a5dd02f7",
            "4e6ded3539409daa84cc5f5bca9ca1df77fbd03977cabf90b5b64b6bf59be51b",
            "d57329c2652fa94a0ede6a079a84b9664c90ca6dff56f53bf5365c12489c6a10",
            "a757bfc23db6c7c83fe51b96ffbbfd1820a3dd1cc9536cd2ada2051e9b36267b",
            "bcce9bfa0deee60091e34ec309ac9311006e308d060bd96bb2ef403a3efa8882",
            "92a4ee0e372af0557da7ddfb4ea8432738bc2e936720c21c98231438462bf4b9",
            "5c98899fe9a164dc99861677b358264c0f2c1398a430a74ccce7952990156a7e",
        ],
    ),
    (
        "zstd_noshuf",
        &[
            "a0645e14fa190697100d03136776796bd17b4e21b67f17046489f7ea91a337a4",
            "005f27976d491ec09edbd1d0cc1d475798d62661bb38461780f34ba4c8743b50",
            "13a784ea573d9afb966ef813b557d657918686961603c0d0a16a8153353e12d0",
            "f29099503fda41a257bc80b1e465c1b882ddea4bfcc401156bf70ac1ff36a2fc",
            "caec4314b2a8abeaea582d6e49205d35103e452e9fc6c920fac285198598521f",
            "366f804025fdb2a8fad0f456804b94d169984ab35e765841493fd3078bf799c7",
            "ded1805e60c00c700de0fc909107fd53924255e49a67ebfb184ba492901b5dae",
            "2da60b3b4587254b32f037f03224af3a85686c03f168c318b41e09e025eb5784",
        ],
    ),
    (
        "lz4_u1_multi",
        &[
            "0420e375f1385b8295a908d8d95bdbda755aa5b66a1ba63ec68be364c72e9910",
            "613e2c679171742dd1d816eef898f85150d6a99a48e0c6de26c92bb1f53bfa42",
            "747b596807b26fea4be5e88286e73973e438af672aec153c8c8b85c5549a210f",
            "f92aaeaa883cc2909c2a071af4abbe34c2ab1def37b24252f5038633da014a36",
            "1e506cc43280176ed8d47cb8cbde015c2458552c178813ee1ab930f96c2858ae",
            "30c1638bcda42d0361df90f73e3ca6232dc86548a95fdc3fac820015e1f02666",
            "df1cfc93f33b1e376ffa6ebfffc1d29a7fa31c494f6b8ad7cbcfe7b4596f0442",
            "979da8bca3f0baec954cdd6c7e2a06c1793f59dd0f6592243e1646d492239b97",
        ],
    ),
];

#[test]
fn blosc_arrays_that_are_really_compressed_decode_correctly() {
    // Two bugs lived here. The codec ids were read off blosc's public compcode table instead of the
    // *compformat* field in the header, so a `zstd` chunk went to the zlib inflater and a `zlib`
    // chunk was refused as `snappy` — every store using either was unreadable. And the byte shuffle
    // was undone once over the whole chunk instead of per block, which is only equivalent when there
    // is one block: with more, every value came back scrambled, with no error at all.
    let ingested = ingest("blosc_real.zarr", IngestOptions::default());
    for (label, expected) in BLOSC_REAL {
        assert_eq!(
            row_hashes(stream_of(&ingested, 0, label)),
            *expected,
            "{label} decoded to different bytes than Python wrote"
        );
    }
}

#[test]
fn a_unicode_dtype_is_sized_in_bytes_not_characters() {
    // NumPy's `U` size is a character count and each character is four bytes, so `<U5` is a 20-byte
    // element. Reading 5 made every chunk the wrong size and refused the whole store.
    let ingested = ingest("dtype_edges.zarr", IngestOptions::default());
    let labels = stream_of(&ingested, 0, "labels");
    assert_eq!(labels.dtype.as_deref(), Some("string[5]"));
    assert_eq!(labels.frames[0].value_ref.byte_len, Some(20));
    // Taken from `array[i:i+1].tobytes()`, not `array[i].tobytes()`: extracting a single element of
    // a `<U5` array yields a NumPy scalar whose width shrinks to its own content, so the latter hashes
    // 12 bytes for `"abc"` where the array stores 20. The stored width is what a reader sees.
    assert_eq!(
        row_hashes(labels),
        [
            "5ac05fc946ac1e134d4aff902b475f0eee0102db829be96901f48e076d473217",
            "276bb5c7b30dd9ad496b6f391b65ce86f167b57ab8e444410f86d3b0cc46220e",
            "0be8638f8759241eecb590ad38a411c429cd26d1a75e86d1263088e423913c88",
            "afa70f32ccca9325286da833899d714681c40233c42ef9f13242c88c8d509c2a",
        ]
    );
}

#[test]
fn a_width_the_reader_cannot_decode_abstains_instead_of_reporting_clean() {
    // `half` is float16 and holds a NaN and an infinity. Nothing here decodes a 2-byte float, so
    // reporting `Some(0)` non-finite would satisfy STATISTICAL.NON_FINITE_OBSERVED on an array full
    // of them — "read and clean" is a claim, and it has to be earned.
    let ingested = ingest("dtype_edges.zarr", IngestOptions::default());
    let half = stream_of(&ingested, 0, "half");
    assert_eq!(half.dtype.as_deref(), Some("float16"));
    assert_eq!(
        half.observed_non_finite, None,
        "never read is not the same answer as read and clean"
    );
    assert!(half.observed_stats.is_none() && half.observed_dim_stats.is_none());
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.source_path == "half" && f.note.contains("not decoded")),
        "and the report says the values were not summarized: {:?}",
        ingested.report.unmapped_fields
    );
    // The float32 array beside it is still summarized, so this is an abstention, not a shutdown.
    assert!(stream_of(&ingested, 0, "action").observed_stats.is_some());
}

#[test]
fn each_episode_of_a_group_layout_holds_only_its_own_arrays() {
    // Two groups, three and four rows. Before this was fixed every episode carried every group's
    // arrays — so each episode held its neighbour's frames, truncated to its own length, and the
    // whole store still certified as a pass.
    let ingested = ingest("group_layout.zarr", IngestOptions::default());
    assert_eq!(ingested.dataset.episodes.len(), 2);
    for episode in &ingested.dataset.episodes {
        assert_eq!(
            episode
                .streams
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            vec!["action", "obs/state"],
            "a stream is named below its own episode's group, so the same sensor is the same name in \
             every episode — which is what makes the cross-episode checks comparable"
        );
    }
    assert_eq!(
        row_hashes(stream_of(&ingested, 0, "action")),
        [
            "22b6f43bd8d27738d3213f29e96b62d01d9d6c0ab4f9732aaae803186f51eab7",
            "2fd848aa90e817e10e20985de4e8ac6a09b0fe70623d6b952e46800be6b025b9",
            "84c4426d4f7c3fb46daf2e28151e2adbdb53091cd52dfbc76c7edf418b1ff65d",
        ]
    );
    assert_eq!(
        row_hashes(stream_of(&ingested, 1, "action")),
        [
            "1ec4ebd1e506bbcaaa7bad31e5e40f9d139ca4ca33745c956a3c0dcba8f38d9e",
            "0e602bdf1fd8e3e685fa6ed695073f45ab129c211b997462f5b2e4c51b896477",
            "4f4ef093487eed5d3029312ff74ed6d2818f6984903d4a3c1b01778908b2db9d",
            "0b1000a1f2af3bc8f4d5c7dddeb34d0ad634abf9af8336ca2cb06f09df626da0",
        ],
        "and episode 1's own four rows, not episode 0's three"
    );
}

#[test]
fn one_episodes_timestamps_do_not_become_another_episodes_clock() {
    // `ep_0` records a timeline; `ep_1` records none. Taking the first timeline found in the store
    // stamped episode 1 with episode 0's nanoseconds — fabricated time, feeding every rate, gap, and
    // skew verdict downstream.
    let ingested = ingest("group_time.zarr", IngestOptions::default());
    assert_eq!(
        stream_of(&ingested, 0, "action").clock_kind,
        ClockKind::Measured
    );
    assert_eq!(
        stream_of(&ingested, 1, "action").clock_kind,
        ClockKind::StepIndex,
        "an episode that recorded no time is on a step index"
    );
    assert_eq!(ingested.dataset.episodes[1].start_ts, Some(0));
    assert_ne!(
        ingested.dataset.episodes[0].start_ts,
        ingested.dataset.episodes[1].start_ts
    );
}

#[test]
fn a_single_array_store_is_one_episode_with_one_stream() {
    let ingested = ingest("bare_array.zarr", IngestOptions::default());
    assert_eq!(ingested.dataset.episodes.len(), 1);
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 1);
    assert_eq!(ingested.dataset.episodes[0].streams[0].frames.len(), 5);
}

#[test]
fn an_empty_boundary_stays_an_empty_episode() {
    // `episode_ends = [3, 3, 6]` declares three episodes, one of which spans nothing. Dropping it
    // would leave the ingested count disagreeing with the declared one for no stated reason; keeping
    // it lets STRUCTURAL.EMPTY_EPISODE say exactly what is wrong.
    let ingested = ingest("ends_empty.zarr", IngestOptions::default());
    assert_eq!(
        ingested
            .dataset
            .episodes
            .iter()
            .map(|e| (e.index, e.streams.len()))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 0), (2, 1)]
    );
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(fixture("ends_empty.zarr")),
        None,
        &IngestOptions::default(),
    )
    .expect("the pipeline runs");
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EMPTY_EPISODE"),
        "the empty episode is named: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| f.code.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn hostile_metadata_is_refused_rather_than_trusted() {
    // A dtype width that overflows the arithmetic derived from it. Five characters of JSON used to
    // abort the process with a multiply overflow in a checked build.
    let message = refusal("huge_dtype.zarr");
    assert!(
        message.contains("ceiling"),
        "an absurd element width is bounded, not multiplied: {message}"
    );
    // A zero-length dimension after the first means every row holds no values, and every one of them
    // fingerprints to the hash of nothing — a dataset with no data must not read back as a pass.
    let message = refusal("zero_width.zarr");
    assert!(message.contains("zero-length dimension"), "{message}");
    // An unreadable `.zattrs` is not an absent one: the licence, the task, and a timeline's units all
    // live there.
    let message = refusal("bad_attrs.zarr");
    assert!(
        message.contains(".zattrs"),
        "a corrupt attributes file is named: {message}"
    );
}

#[test]
fn a_declared_chunk_with_no_chunk_files_cannot_allocate_gigabytes() {
    // `chunks: [1, 1000000000]` with no chunk files on disk. Every row is a fill chunk, and the fill
    // path was the one allocation that was never charged — a 350-byte store that climbed past 3 GB.
    let err = default_registry()
        .ingest(
            &Source::Local(fixture("fill_bomb.zarr")),
            &IngestOptions::default(),
        )
        .expect_err("a metadata-only allocation is still an allocation");
    assert!(
        matches!(
            err,
            IngestError::DecompressionBudgetExceeded { format_id, .. } if format_id == "zarr"
        ),
        "got {err:?}"
    );
}

#[test]
fn a_symlink_out_of_the_store_is_not_followed() {
    // A store that links to a directory outside itself would otherwise have that directory's bytes
    // read, hashed, and signed into a certificate as part of the dataset — and a link to its own
    // parent makes the walk exponential. The LeRobot and CAN adapters already refuse this.
    let dir = tempfile::tempdir().expect("temp dir");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).expect("mkdir");
    std::fs::copy(
        fixture("bare_array.zarr").join(".zarray"),
        outside.join(".zarray"),
    )
    .expect("copy .zarray");
    for chunk in ["0.0", "1.0", "2.0"] {
        let from = fixture("bare_array.zarr").join(chunk);
        if from.exists() {
            std::fs::copy(&from, outside.join(chunk)).expect("copy chunk");
        }
    }
    let store = dir.path().join("store.zarr");
    std::fs::create_dir_all(&store).expect("mkdir");
    std::fs::write(store.join(".zgroup"), br#"{"zarr_format": 2}"#).expect("write");
    std::fs::copy(
        fixture("dp_replay.zarr").join("data").join(".zgroup"),
        store.join(".zgroup"),
    )
    .ok();
    std::os::unix::fs::symlink(&outside, store.join("leaked")).expect("symlink");

    let result = default_registry().ingest(&Source::Local(store), &IngestOptions::default());
    match result {
        Ok(ingested) => {
            assert!(
                ingested
                    .dataset
                    .episodes
                    .iter()
                    .flat_map(|e| e.streams.iter())
                    .all(|s| s.name != "leaked"),
                "the linked directory's data must not be in the CDM"
            );
            assert!(ingested
                .report
                .unmapped_fields
                .iter()
                .any(|f| f.note.contains("symbolic link")));
        }
        // A store whose only entry was the link has nothing left to read, which is also correct — as
        // long as the refusal is not a dataset.
        Err(IngestError::Parse { message, .. }) => {
            assert!(!message.is_empty(), "the refusal names something");
        }
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}
