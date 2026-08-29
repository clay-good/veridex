//! ROS 2 rosbag2 (`.db3`) adapter tests.
//!
//! Every fixture under `tests/fixtures/rosbag2/` is written by **Python's `sqlite3` module** — real
//! SQLite, from a writer with nothing to do with this repository. That is deliberate:
//! `adapter/sqlite.rs` is a hand-written reader, and a reader tested only against a writer from the
//! same head proves the two agree with each other, not that either agrees with the format.
//! `tests/fixtures/rosbag2/generate_fixtures.py` regenerates them.
//!
//! The golden SHA-256s pinned below were computed by that Python, over the bytes it inserted. They
//! are the proof that this reader assembles a payload correctly — including one spread across an
//! overflow-page chain, where an off-by-one in the local/overflow split produces bytes that still
//! look like a message.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

use veridex_core::adapter::{
    default_registry, Adapter, Coverage, Detection, IngestError, IngestOptions, Ingested, Sample,
    Source,
};
use veridex_core::cdm::Modality;
use veridex_core::check::Check;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rosbag2")
}

fn ingest(rel: &str) -> Ingested {
    ingest_with(rel, &IngestOptions::default()).expect("the fixture ingests")
}

fn ingest_with(rel: &str, options: &IngestOptions) -> Result<Ingested, IngestError> {
    default_registry().ingest(&Source::Local(fixtures().join(rel)), options)
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_bag_directory_maps_every_topic_to_a_stream() {
    let out = ingest("clean_rig");
    assert_eq!(out.report.format_id, "rosbag2");
    assert_eq!(out.report.source_version.as_deref(), Some("5"));
    assert_eq!(out.report.coverage, Coverage::Full);
    assert_eq!(out.dataset.id, "clean_rig");
    assert_eq!(out.dataset.episodes.len(), 1, "a bag is one recording");

    let ep = &out.dataset.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "/camera/front/camera_info",
            "/camera/front/image_raw",
            "/imu/data",
            "/lidar/points",
            "/odom",
            "/tf_static",
        ]
    );
    // Every message in the bag became a frame, and none was lost or double-counted.
    let frames: usize = ep.streams.iter().map(|s| s.frames.len()).sum();
    assert_eq!(frames, 401, "the fixture holds 401 messages");
    assert!(ep.streams.iter().all(|s| s.clock_id == "rosbag2-log"));
}

#[test]
fn the_ros_message_type_selects_the_modality() {
    let out = ingest("clean_rig");
    let by_name = |n: &str| {
        out.dataset.episodes[0]
            .streams
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("stream {n}"))
            .modality
    };
    assert_eq!(by_name("/lidar/points"), Modality::PointCloud);
    assert_eq!(by_name("/imu/data"), Modality::Imu);
    assert_eq!(by_name("/odom"), Modality::EgoPose);
    assert_eq!(by_name("/camera/front/image_raw"), Modality::Video);
    // A CameraInfo channel is a camera's calibration, not its imagery. Typing it `Video` made it a
    // sensor for `AUTONOMY.RIG_SYNC`, which then compared a latched or 1 Hz calibration topic's span
    // against a LiDAR's and failed a synchronized rig. Its content still reaches the CDM — as
    // `Dataset::calibration`, which is where intrinsics belong.
    assert_eq!(
        by_name("/camera/front/camera_info"),
        Modality::ScalarState,
        "a CameraInfo topic is telemetry about a camera, not a camera"
    );
}

#[test]
fn a_rig_recorded_with_the_node_chatter_beside_it_is_still_a_synchronized_rig() {
    // `housekeeping/` is the clean rig plus what every `ros2 bag record -a` also captures: /rosout,
    // /parameter_events, /diagnostics. Nothing about the rig changed, so nothing about the rig's
    // sync verdict should — and before the sensor filter it did: `AUTONOMY.RIG_SYNC` failed at
    // error severity naming `/rosout` as the sensor that drifted.
    let dataset = ingest("housekeeping").dataset;
    let findings = veridex_core::checks::autonomy::RigSync::default().run(&dataset);
    assert!(
        findings.is_empty(),
        "a synchronized rig must not be failed by the topics recorded beside it: {:?}",
        findings.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
}

#[test]
fn the_av_message_headers_populate_the_rig_cdm() {
    let out = ingest("clean_rig");

    // PointCloud2 -> per-point field layout, read from the message header, never the points.
    let lidar = out.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/lidar/points")
        .expect("the lidar stream");
    let fields = lidar.point_fields.as_ref().expect("decoded point fields");
    let field_names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(field_names, vec!["x", "y", "z", "intensity", "ring"]);
    assert_eq!(fields[0].dtype.as_deref(), Some("float32"));
    assert_eq!(fields[4].dtype.as_deref(), Some("uint16"));

    // Every header-first message names the frame its data is expressed in.
    assert_eq!(lidar.frame_id.as_deref(), Some("lidar_link"));

    // TFMessage -> the transform tree; CameraInfo -> intrinsics.
    let calib = out
        .dataset
        .calibration
        .as_ref()
        .expect("decoded calibration");
    let edges: Vec<(&str, &str)> = calib
        .transforms
        .iter()
        .map(|t| (t.parent_frame.as_str(), t.child_frame.as_str()))
        .collect();
    assert!(edges.contains(&("base_link", "lidar_link")), "{edges:?}");
    assert!(edges.contains(&("base_link", "camera_front")), "{edges:?}");
    assert_eq!(calib.intrinsics.len(), 1);
    assert_eq!(calib.intrinsics[0].fx, 1080.5);
    assert_eq!(calib.intrinsics[0].cx, 960.0);

    // A bag that carries its own transform tree and intrinsics identifies the calibration that
    // produced it, and that is provenance — the values are in the CDM and in its content hash.
    let calibration = out.dataset.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "calibration")
        .expect("in-band calibration is recorded as provenance");
    assert_eq!(
        calibration.class,
        veridex_core::cdm::ProvenanceClass::Known,
        "extracted content, not a name-shaped guess"
    );
    assert!(
        calibration
            .value
            .as_deref()
            .unwrap_or_default()
            .contains("in-band"),
        "{:?}",
        calibration.value
    );

    // Odometry -> the ego trajectory, in timestamp order.
    let poses = out.dataset.episodes[0]
        .ego_poses
        .as_ref()
        .expect("decoded ego poses");
    assert_eq!(poses.len(), 100);
    assert!(poses.windows(2).all(|w| w[0].ts <= w[1].ts));
}

#[test]
fn a_message_payload_is_fingerprinted_byte_for_byte() {
    let out = ingest("clean_rig");
    let lidar = out.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/lidar/points")
        .expect("the lidar stream");
    let first = &lidar.frames[0];
    assert_eq!(first.value_ref.uri, "/lidar/points");
    assert_eq!(first.value_ref.byte_len, Some(313));
    // Golden, from Python's sqlite3 over the bytes it inserted.
    assert_eq!(
        hex(&first.value_ref.content_hash.expect("a content hash")),
        "bd65d2a9cca4beb5fa30aceb83e124d606598d20e0685f7f79c5c6fcf5fa7599"
    );
}

#[test]
fn a_payload_spread_across_overflow_pages_is_reassembled_exactly() {
    // 36 KB messages in a 4 KB-page database: each one keeps a computed prefix on its own page and
    // the rest on a chain of overflow pages. An off-by-one in that split still yields plausible
    // bytes, so the only test worth running is against hashes an independent writer produced.
    let out = ingest("overflow.db3");
    let stream = &out.dataset.episodes[0].streams[0];
    assert_eq!(stream.frames.len(), 2);
    let hashes: Vec<String> = stream
        .frames
        .iter()
        .map(|f| hex(&f.value_ref.content_hash.expect("a content hash")))
        .collect();
    assert_eq!(
        hashes,
        vec![
            "1a84521e983237640547cce523e9d95da9ed3a3fd6d38455b58335d88611f0b7".to_string(),
            "3c1d97643503596945350adff3ce9a0baf35569b6cb1c7226e193503377833af".to_string(),
        ]
    );
    assert!(stream
        .frames
        .iter()
        .all(|f| f.value_ref.byte_len == Some(36169)));
    // The header at the front of that reassembled payload still parses, which it would not if the
    // prefix had been taken from the wrong offset.
    assert_eq!(stream.frame_id.as_deref(), Some("lidar_link"));
}

#[test]
fn a_split_recording_is_read_in_recording_order_not_name_order() {
    // `ros2 bag record --max-bag-size` rolls a long bag into `split_0.db3` … `split_11.db3`, and a
    // lexicographic sort puts `_10` and `_11` ahead of `_2`. Frames are appended to their stream in
    // the order the shards are read, and the CDM preserves that order deliberately — reordering them
    // would hide the out-of-order timestamps this tool exists to find. So reading the shards by name
    // returned a sound twelve-shard recording with two `TEMPORAL.NON_MONOTONIC` errors and two
    // `TEMPORAL.GAP` warnings, and split recordings are the ordinary shape of any long bag.
    let ep = &ingest("split").dataset.episodes[0];
    for stream in &ep.streams {
        let ts: Vec<i64> = stream.frames.iter().map(|f| f.ts).collect();
        assert!(
            ts.windows(2).all(|w| w[0] < w[1]),
            "stream `{}` is out of order at {:?}",
            stream.name,
            ts.windows(2)
                .position(|w| w[0] >= w[1])
                .map(|i| (ts[i], ts[i + 1]))
        );
    }
    // All twelve shards were read, not just the ones that sorted first.
    let frames: usize = ep.streams.iter().map(|s| s.frames.len()).sum();
    assert_eq!(frames, 12 * 110);
}

#[test]
fn a_split_recording_reads_the_same_whichever_order_the_directory_lists_it() {
    // The shard order is a pure function of the names — natural order, then the order the manifest
    // records — so the content hash does not depend on the order the filesystem happened to return
    // its entries in. Two reads of the same bag must agree, which is what a certificate over a split
    // recording depends on.
    let a = veridex_core::canonical::content_hash(&ingest("split").dataset);
    let b = veridex_core::canonical::content_hash(&ingest("split").dataset);
    assert_eq!(a, b);
}

#[test]
fn a_compressed_bag_reads_to_the_same_recording_as_the_uncompressed_one() {
    // `ros2 bag record --compression-mode file --compression-format zstd` compresses the finished
    // shard to `.db3.zstd` and deletes the original, which is how any recording large enough to care
    // about is stored. `compressed_rig/` and `clean_rig/` hold the identical messages; only the
    // storage differs, so only the dataset's name may differ.
    let plain = ingest("clean_rig").dataset;
    let packed = ingest("compressed_rig").dataset;
    assert_eq!(packed.id, "compressed_rig");
    assert_eq!(
        packed.episodes[0].streams, plain.episodes[0].streams,
        "compression is storage, not content: every stream, frame, timestamp and content hash \
         must come back identical"
    );
    assert_eq!(packed.calibration, plain.calibration);
    assert_eq!(packed.episodes[0].ego_poses, plain.episodes[0].ego_poses);

    // And the compression is recorded rather than smoothed away — how a dataset was stored is a
    // fact about it.
    assert!(
        packed
            .metadata
            .iter()
            .any(|(k, v)| k == "rosbag2_compression" && v == "zstd"),
        "{:?}",
        packed.metadata
    );
}

#[test]
fn a_bare_compressed_shard_is_named_as_the_recording_not_as_the_file() {
    // `shard_0.db3.zstd`'s file stem is `shard_0.db3`. Taking that as the dataset id would name the
    // same recording differently depending on whether it happened to be compressed, and the id is
    // bound into the content hash — so a certificate issued over the uncompressed bag would not
    // verify against the compressed one.
    let packed = ingest("compressed_rig/compressed_rig_0.db3.zstd").dataset;
    assert_eq!(packed.id, "compressed_rig_0");
}

#[test]
fn per_message_compression_is_refused_by_name_rather_than_read_wrong() {
    // The tables of a MESSAGE-mode bag are plain, so it would read — and every frame's content hash
    // would fingerprint a zstd frame instead of the message, and no AV header would decode, so a
    // full rig would come back with no point fields, no calibration and no ego trajectory. That is a
    // wrong answer, which is worse than a refusal.
    match ingest_with("message_compressed", &IngestOptions::default()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("MESSAGE"), "{message}");
            assert!(
                message.contains("FILE"),
                "names what it does read: {message}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_zstd_bomb_is_stopped_by_the_budget_rather_than_billed_for_afterwards() {
    // `zstd_bomb/` is a few kilobytes on disk that unpack to 96 MiB. It is not a database and never
    // reaches the reader: the decompressor is handed a cap of what the budget has left, so the
    // unpacking stops at the bound instead of completing and then being charged — which is the
    // difference between a refusal and an out-of-memory kill inside someone's CI gate.
    match ingest_with("zstd_bomb", &IngestOptions::default()) {
        Err(IngestError::DecompressionBudgetExceeded {
            format_id,
            limit,
            requested,
        }) => {
            assert_eq!(format_id, "rosbag2");
            // The proof that the read stopped rather than finished: `requested` is what was actually
            // unpacked, and it is one byte past the budget — not the 96 MiB the shard holds. An
            // implementation that decompressed first and charged afterwards would report
            // 100,663,296 here, having already spent the memory it is being refused for.
            assert_eq!(
                requested,
                limit + 1,
                "the decompressor was capped at the budget, not run to completion"
            );
            assert!(requested < 96 * 1024 * 1024);
        }
        other => panic!("expected a decompression-budget error, got {other:?}"),
    }
}

#[test]
fn an_aggressive_ratio_does_not_squeeze_an_honest_small_bag() {
    // The budget scales with the source but keeps a floor, so a small bag stays readable under a
    // ratio a user set for a different reason. Pinned because the bomb test above would also pass
    // if the floor were dropped, and dropping it would refuse ordinary recordings.
    let options = IngestOptions {
        max_decompression_ratio: Some(1),
        ..IngestOptions::default()
    };
    assert!(ingest_with("compressed_rig", &options).is_ok());
}

#[test]
fn a_recording_short_of_its_manifest_is_a_coverage_hole_not_a_clean_read() {
    // `interrupted/` is the clean bag with its last 40 messages missing, and a `metadata.yaml` that
    // still closes with the full count — a recorder killed mid-flush. Reading it as a complete bag
    // is the exact failure this tool exists to prevent.
    let out = ingest("interrupted");
    let ingested: usize = out.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.frames.len())
        .sum();
    assert_eq!(ingested, 361);
    let unread = &out.report.unread_sources;
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0].source_path, "metadata.yaml message_count");
    assert!(
        unread[0].note.contains("401") && unread[0].note.contains("361"),
        "the disclosure names both numbers: {}",
        unread[0].note
    );

    // The manifest total is *not* mapped to `declared_frame_count`: it counts every topic's
    // messages, while that field is what each of an episode's streams should hold. Mapping it there
    // would fail a sound bag on `STRUCTURAL.EPISODE_BOUNDARY`.
    assert_eq!(out.dataset.episodes[0].declared_frame_count, None);
    let clean = ingest("clean_rig");
    assert!(
        clean.report.unread_sources.is_empty(),
        "a whole bag discloses nothing unread: {:?}",
        clean.report.unread_sources
    );
}

#[test]
fn a_latched_topic_is_read_from_its_qos_and_stops_deflating_the_score() {
    // Every ROS 2 stack publishes `/tf_static` latched: once at startup, retained for late
    // subscribers. Graded as a sampled stream it drew `STRUCTURAL.SINGLE_FRAME_STREAM` ("carries no
    // temporal signal") and `TEMPORAL.END_OFFSET` ("ends 1990 ms before /imu/data") — both true as
    // stated, neither describing a fault, and the score deducts per finding. A flawless bag scored
    // `data 92` for a topic behaving exactly as designed, and a bag with three latched topics
    // proportionally worse.
    let dataset = ingest("clean_rig").dataset;
    let by_name = |n: &str| {
        dataset.episodes[0]
            .streams
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("stream {n}"))
    };
    // Read from the topic's recorded QoS durability, not inferred from its one frame.
    assert_eq!(by_name("/tf_static").latched, Some(true));
    assert_eq!(by_name("/lidar/points").latched, Some(false));

    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&dataset);
    let verdict = engine.run(&dataset, hash, &veridex_core::RunConfig::default());
    let codes: Vec<&str> = verdict.findings.iter().map(|f| f.code.as_str()).collect();
    assert!(
        !codes.contains(&"STRUCTURAL.SINGLE_FRAME_STREAM"),
        "a latched topic's single frame is what it is for: {codes:?}"
    );
    assert!(
        !codes.contains(&"TEMPORAL.END_OFFSET"),
        "a latched topic does not cover the recording's window: {codes:?}"
    );
    // The only thing left to say about this bag is that it declares no license.
    assert_eq!(
        codes
            .iter()
            .filter(|c| !c.starts_with("PROVENANCE.") && !c.starts_with("STATISTICAL."))
            .count(),
        0,
        "{codes:?}"
    );
}

#[test]
fn a_stream_that_declares_nothing_about_delivery_is_still_graded() {
    // The suppression is driven by a recorded declaration, never by the shape of the frames. A
    // source that says nothing keeps every check it had — otherwise a sensor that fired once and
    // stopped, which is what these checks exist to catch, would go quiet along with the transform
    // trees.
    let dataset = ingest("bare.db3").dataset;
    let tf = dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/tf_static")
        .expect("the tf stream");
    // `bare.db3` carries the same QoS column, so this is the latched case; the point below is the
    // *engine* behavior when the flag is absent.
    assert_eq!(tf.latched, Some(true));

    let mut stripped = dataset.clone();
    for s in &mut stripped.episodes[0].streams {
        s.latched = None;
    }
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let verdict = engine.run(
        &stripped,
        veridex_core::content_hash(&stripped),
        &veridex_core::RunConfig::default(),
    );
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.SINGLE_FRAME_STREAM"),
        "with no declaration, a one-frame stream is still reported"
    );
}

#[test]
fn a_bag_that_is_still_recording_is_read_rather_than_refused() {
    // rosbag2 writes `metadata.yaml` when the recorder *closes*. A bag mid-recording is a directory
    // with a growing `.db3` and nothing else — and requiring the manifest refused it as an
    // unrecognized format, which broke `veridex watch` on exactly the case it exists for: catching a
    // clock skew while the robot is still driving, when it is worth the most.
    let out = ingest("recording");
    assert_eq!(out.report.format_id, "rosbag2");
    assert_eq!(out.dataset.episodes[0].streams.len(), 6);
    // No manifest, so no version — the honest answer rather than a guessed default.
    assert_eq!(out.report.source_version, None);
    assert!(
        out.report
            .omitted_fields
            .iter()
            .any(|f| f.starts_with("metadata.yaml (the bag has not written one")),
        "what the manifest would have supplied is reported missing, not assumed: {:?}",
        out.report.omitted_fields
    );
    // And exactly one line about it, not one per key it would have carried.
    assert_eq!(
        out.report
            .omitted_fields
            .iter()
            .filter(|f| f.starts_with("metadata.yaml"))
            .count(),
        1
    );
}

#[test]
fn a_live_write_ahead_log_is_data_this_run_did_not_read() {
    // `recording/` carries a real, uncheckpointed WAL holding 50 committed messages that the `.db3`
    // itself does not. This reader walks the shard's own pages and does not replay a write-ahead log,
    // so those messages exist, are committed, and were not read — and a report that stayed quiet
    // about them would speak for a recording it only partly saw.
    let out = ingest("recording");
    let frames: usize = out.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.frames.len())
        .sum();
    assert_eq!(
        frames, 401,
        "the 50 messages committed into the WAL are genuinely not in this CDM"
    );
    let unread = &out.report.unread_sources;
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0].source_path, "recording_0.db3-wal");
    assert!(
        unread[0].note.contains("write-ahead log"),
        "{}",
        unread[0].note
    );

    // A closed bag has no sidecar and discloses nothing.
    assert!(ingest("clean_rig").report.unread_sources.is_empty());
}

#[test]
fn a_message_on_an_undeclared_topic_is_reported_unread() {
    let out = ingest("orphan_topic.db3");
    // The one declared topic still maps; the orphan is not invented into a stream.
    assert_eq!(out.dataset.episodes[0].streams.len(), 1);
    let unread = &out.report.unread_sources;
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0].source_path, "messages.topic_id=99");
}

#[test]
fn a_bare_db3_is_read_and_says_what_it_therefore_cannot_know() {
    let out = ingest("bare.db3");
    assert_eq!(out.dataset.id, "bare");
    assert_eq!(out.dataset.episodes[0].streams.len(), 6);
    // No manifest, so no version to report — `None`, not a guessed default.
    assert_eq!(out.report.source_version, None);
    assert!(
        out.report
            .omitted_fields
            .iter()
            .any(|f| f.starts_with("metadata.yaml (")),
        "{:?}",
        out.report.omitted_fields
    );
    // And no recorder was invented for it.
    assert!(!out.dataset.provenance[0]
        .elements
        .iter()
        .any(|e| e.key == "recorder"));
}

#[test]
fn the_manifests_recorder_is_recorded_as_provenance() {
    let out = ingest("clean_rig");
    let recorder = out.dataset.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "recorder")
        .expect("a recorder element");
    assert_eq!(recorder.value.as_deref(), Some("rosbag2 (humble)"));
}

#[test]
fn a_sqlite_database_that_is_not_a_bag_is_refused_by_name() {
    match ingest_with("not_a_bag.db3", &IngestOptions::default()) {
        Err(IngestError::Parse { format_id, message }) => {
            assert_eq!(format_id, "rosbag2");
            assert!(message.contains("topics"), "{message}");
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[test]
fn detection_claims_a_bag_directory_and_a_bare_db3_and_nothing_else() {
    let reg = default_registry();
    // Autodetection resolves both without ambiguity — no other adapter claims them.
    assert!(reg
        .ingest(
            &Source::Local(fixtures().join("clean_rig")),
            &IngestOptions::default()
        )
        .is_ok());

    let adapter = veridex_core::adapter::rosbag2::Rosbag2Adapter;
    assert_eq!(
        adapter.detect(&Source::Local(fixtures().join("clean_rig"))),
        Detection::Yes {
            version: Some("5".into())
        }
    );
    assert_eq!(
        adapter.detect(&Source::Local(fixtures().join("bare.db3"))),
        Detection::Yes { version: None }
    );
    // A bag still recording has written no manifest yet, so it is claimed with no version rather
    // than refused — `veridex watch` depends on this.
    assert_eq!(
        adapter.detect(&Source::Local(fixtures().join("recording"))),
        Detection::Yes { version: None }
    );
    // A directory with no shard in it is not a bag, manifest or not.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        adapter.detect(&Source::Local(empty.path().to_path_buf())),
        Detection::No
    );
    std::fs::write(
        empty.path().join("metadata.yaml"),
        "rosbag2_bagfile_information:\n",
    )
    .unwrap();
    assert_eq!(
        adapter.detect(&Source::Local(empty.path().to_path_buf())),
        Detection::No,
        "a metadata.yaml with no .db3 beside it belongs to some other tool"
    );
}

#[test]
fn sampling_a_bag_is_refused_rather_than_silently_ignored() {
    let options = IngestOptions {
        sample: Sample::FirstEpisodes(1),
        ..IngestOptions::default()
    };
    match ingest_with("clean_rig", &options) {
        Err(IngestError::SamplingUnsupported { format_id, .. }) => {
            assert_eq!(format_id, "rosbag2")
        }
        other => panic!("expected a sampling refusal, got {other:?}"),
    }
}

fn metadata_only() -> IngestOptions {
    IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    }
}

#[test]
fn a_bag_can_be_checked_from_its_manifest_without_opening_a_shard() {
    // The point of this path is a bag too large to read: the topic inventory, the ROS types, the
    // recorder identity and the storage, from `metadata.yaml` alone.
    let out = ingest_with("clean_rig", &metadata_only()).expect("a manifest-only ingest");
    assert_eq!(
        out.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: 1
        }
    );
    let ep = &out.dataset.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "/camera/front/camera_info",
            "/camera/front/image_raw",
            "/imu/data",
            "/lidar/points",
            "/odom",
            "/tf_static",
        ],
        "every topic the manifest declares"
    );
    // The ROS type still selects the modality, because the manifest records it.
    assert_eq!(
        ep.streams
            .iter()
            .find(|s| s.name == "/lidar/points")
            .unwrap()
            .modality,
        Modality::PointCloud
    );
    // Zero frames by request, not by defect — and nothing is claimed that a shard would have
    // answered.
    assert!(ep.streams.iter().all(|s| s.frames.is_empty()));
    assert_eq!(ep.start_ts, None, "no frames, so no measured span to state");
    assert_eq!(out.dataset.calibration, None);
    assert_eq!(ep.ego_poses, None);
    assert!(ep.streams.iter().all(|s| s.latched.is_none()));
    // The recorder is in the manifest, so it survives.
    assert!(out.dataset.provenance[0]
        .elements
        .iter()
        .any(|e| e.key == "recorder"));
}

#[test]
fn a_metadata_only_run_does_not_accuse_a_rig_of_what_it_declined_to_read() {
    // A rig log's transform tree and camera intrinsics are decoded from message *bodies*, which a
    // metadata-only run does not open. `autonomy.calibration-completeness` concludes from their
    // absence, so it read the absence the run had created and reported a fully calibrated bag as
    // having "no transform (TF) tree" — twice, at warning severity. A check that fires on what a run
    // declined to look at is measuring the request, not the data.
    let out = ingest_with("clean_rig", &metadata_only()).expect("a manifest-only ingest");
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let verdict = engine.run_over(
        &out.dataset,
        veridex_core::content_hash(&out.dataset),
        &veridex_core::RunConfig::default(),
        veridex_core::engine::CoverageNote::MetadataOnly {
            episodes_declared: 1,
        },
    );
    let codes: Vec<&str> = verdict.findings.iter().map(|f| f.code.as_str()).collect();
    assert!(
        !codes.contains(&"AUTONOMY.CALIBRATION_INCOMPLETE"),
        "{codes:?}"
    );
    // Nor does it say twice what the coverage disclosure already says: with no frames read, no two
    // streams can be compared, which is the run's shape rather than the dataset's.
    assert!(!codes.contains(&"TEMPORAL.UNCOMPARED_STREAMS"), "{codes:?}");

    // The same bag read in full still reports its calibration as complete — the check is silenced by
    // the *request*, not disabled.
    let full = ingest("clean_rig").dataset;
    let full_verdict = engine.run(
        &full,
        veridex_core::content_hash(&full),
        &veridex_core::RunConfig::default(),
    );
    assert!(full.calibration.is_some());
    assert!(!full_verdict
        .findings
        .iter()
        .any(|f| f.code == "AUTONOMY.CALIBRATION_INCOMPLETE"));
}

#[test]
fn a_manifest_whose_inventory_does_not_add_up_is_refused() {
    // `partial_inventory/` lists two of its six topics while still declaring the full total.
    // Presenting two topics as the bag's contents is invisible to the caller and is the exact shape
    // of failure this tool exists to prevent — so the run is refused, naming both numbers.
    match ingest_with("partial_inventory", &metadata_only()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("401"), "{message}");
            assert!(message.contains("60"), "{message}");
            assert!(message.contains("--metadata-only"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // Reading the shards is unaffected: the manifest is wrong about its inventory, not the data.
    let full = ingest("partial_inventory");
    assert_eq!(full.dataset.episodes[0].streams.len(), 6);
}

#[test]
fn metadata_only_needs_a_manifest_and_says_so_when_there_is_none() {
    // A bare `.db3` has no manifest at all, so there is nothing to read but the shard the caller
    // asked not to open.
    match ingest_with("bare.db3", &metadata_only()) {
        Err(IngestError::NotImplemented { what, hint }) => {
            assert!(what.contains("bare .db3"), "{what}");
            assert!(hint.contains("metadata.yaml"), "{hint}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    // And a bag still recording has not written one yet.
    match ingest_with("recording", &metadata_only()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("topics_with_message_count"), "{message}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_metadata_only_run_over_a_bag_cannot_be_certified() {
    // The guard that makes the whole path safe to offer: a verdict that read no data must not become
    // a signed claim about the data. Enforced by the pipeline for every format; asserted here so the
    // new adapter is covered by it.
    let out = ingest_with("clean_rig", &metadata_only()).expect("a manifest-only ingest");
    assert!(
        matches!(out.report.coverage, Coverage::MetadataOnly { .. }),
        "the coverage is what `certify` refuses on"
    );
}

#[test]
fn the_frame_budget_bounds_a_bag() {
    let options = IngestOptions {
        max_frames: Some(10),
        ..IngestOptions::default()
    };
    match ingest_with("clean_rig", &options) {
        Err(IngestError::FrameBudgetExceeded {
            format_id, limit, ..
        }) => {
            assert_eq!(format_id, "rosbag2");
            assert_eq!(limit, 10);
        }
        // The budget must surface as a budget error, not as the parse error the scan's visitor
        // returns internally to stop the walk.
        other => panic!("expected a frame-budget error, got {other:?}"),
    }
}

#[test]
fn the_same_bag_read_twice_produces_the_same_content_hash() {
    let a = veridex_core::canonical::content_hash(&ingest("clean_rig").dataset);
    let b = veridex_core::canonical::content_hash(&ingest("clean_rig").dataset);
    assert_eq!(a, b);
    // And the bare `.db3` inside the bag is not the same dataset as the bag: it is identified by a
    // different name and carries none of the manifest's provenance.
    let bare = veridex_core::canonical::content_hash(&ingest("bare.db3").dataset);
    assert_ne!(a, bare);
}

// ---- cross-format neutrality ----

/// (stream name, modality, frame timestamps).
type StreamSig = (String, Modality, Vec<i64>);
/// Per-episode structural signature: (episode index, its streams).
type EpisodeSig = (u64, Vec<StreamSig>);

/// (stream name, modality, frame timestamps) — the structural signature the ingestion spec's
/// equivalence is defined over. Format-specific fields (clock id, declared rate, provenance) are
/// deliberately excluded: a rosbag2 log clock is `rosbag2-log` and an MCAP one is `mcap-log`, and
/// they are not meant to be the same string.
fn signature(d: &veridex_core::cdm::Dataset) -> Vec<EpisodeSig> {
    let mut eps: Vec<EpisodeSig> = d
        .episodes
        .iter()
        .map(|ep| {
            let mut streams: Vec<StreamSig> = ep
                .streams
                .iter()
                .map(|s| {
                    (
                        s.name.clone(),
                        s.modality,
                        s.frames.iter().map(|f| f.ts).collect(),
                    )
                })
                .collect();
            streams.sort_by(|a, b| a.0.cmp(&b.0));
            (ep.index, streams)
        })
        .collect();
    eps.sort_by_key(|(i, _)| *i);
    eps
}

/// One channel to write: its schema (ROS type), its topic, and the times of its messages.
type Channel = (String, String, Vec<u64>);

/// Write an MCAP carrying the given channels.
fn write_mcap(path: &std::path::Path, channels: &[Channel]) {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("an mcap writer");
        for (schema, topic, times) in channels {
            let sid = w.add_schema(schema, "ros2msg", b"").unwrap();
            let cid = w.add_channel(sid, topic, "cdr", &BTreeMap::new()).unwrap();
            for (seq, &t) in times.iter().enumerate() {
                w.write_to_known_channel(
                    &mcap::records::MessageHeader {
                        channel_id: cid,
                        sequence: seq as u32,
                        log_time: t,
                        publish_time: t,
                    },
                    b"x",
                )
                .unwrap();
            }
        }
        w.finish().unwrap();
    }
    std::fs::write(path, &out).unwrap();
}

#[test]
fn the_same_recording_as_a_bag_and_as_mcap_yields_equivalent_cdms() {
    // rosbag2 and MCAP are the two storage plugins of one recorder, and the neutrality claim is that
    // which one a team chose does not change what Veridex sees. This is the gate for that on the new
    // adapter: take the bag fixture's own topics, ROS types and message times, replay them into an
    // MCAP, and require the two CDMs to carry the same episodes, streams, modalities and timestamps.
    //
    // Worth having because the two paths reach the same place by different routes — one reads a
    // SQLite `topics` table, the other MCAP channel and schema records — and a divergence would show
    // up as a dataset that changes shape when a team switches storage, which is precisely the thing
    // a cross-format verifier must not do.
    let bag = ingest("clean_rig").dataset;

    // The ROS type per topic, as `clean_rig` records them.
    let types: &[(&str, &str)] = &[
        ("/camera/front/camera_info", "sensor_msgs/msg/CameraInfo"),
        ("/camera/front/image_raw", "sensor_msgs/msg/Image"),
        ("/imu/data", "sensor_msgs/msg/Imu"),
        ("/lidar/points", "sensor_msgs/msg/PointCloud2"),
        ("/odom", "nav_msgs/msg/Odometry"),
        ("/tf_static", "tf2_msgs/msg/TFMessage"),
    ];
    let channels: Vec<Channel> = types
        .iter()
        .map(|(topic, ty)| {
            let times = bag.episodes[0]
                .streams
                .iter()
                .find(|s| s.name == *topic)
                .unwrap_or_else(|| panic!("the bag has {topic}"))
                .frames
                .iter()
                .map(|f| f.ts as u64)
                .collect();
            (ty.to_string(), topic.to_string(), times)
        })
        .collect();

    let dir = tempfile::tempdir().unwrap();
    let mcap_path = dir.path().join("equiv.mcap");
    write_mcap(&mcap_path, &channels);
    let via_mcap = veridex_core::adapter::mcap::McapAdapter
        .ingest(&Source::Local(mcap_path), &IngestOptions::default())
        .expect("mcap ingest")
        .dataset;

    assert_eq!(
        signature(&bag),
        signature(&via_mcap),
        "one recording stored two ways must produce equivalent CDMs"
    );
}

// ---------------------------------------------------------------------------
// The MCAP storage plugin: what `ros2 bag record` writes by default from Jazzy on.
// ---------------------------------------------------------------------------

/// A bag directory holding `shards` (name, channels) and the manifest that describes them.
fn write_mcap_bag(
    dir: &std::path::Path,
    shards: &[(&str, Vec<Channel>)],
    storage_identifier: &str,
    message_count: u64,
) {
    let mut listed = String::new();
    let mut topics: BTreeMap<String, (String, u64)> = BTreeMap::new();
    for (name, channels) in shards {
        write_mcap(&dir.join(name), channels);
        listed.push_str(&format!("    - {name}\n"));
        for (ty, topic, times) in channels {
            let entry = topics.entry(topic.clone()).or_insert((ty.clone(), 0));
            entry.1 += times.len() as u64;
        }
    }
    let mut inventory = String::new();
    for (topic, (ty, count)) in &topics {
        inventory.push_str(&format!(
            "    - topic_metadata:\n        name: {topic}\n        type: {ty}\n        \
             serialization_format: cdr\n        offered_qos_profiles: \"\"\n      \
             message_count: {count}\n"
        ));
    }
    std::fs::write(
        dir.join("metadata.yaml"),
        format!(
            "rosbag2_bagfile_information:\n  version: 9\n  storage_identifier: \
             {storage_identifier}\n  relative_file_paths:\n{listed}  message_count: \
             {message_count}\n  topics_with_message_count:\n{inventory}  compression_format: \"\"\n  \
             compression_mode: \"\"\n  ros_distro: jazzy\n"
        ),
    )
    .unwrap();
}

/// One rig's worth of channels, as `clean_rig` records them.
fn rig_channels(offset: u64, per_topic: usize) -> Vec<Channel> {
    [
        ("sensor_msgs/msg/PointCloud2", "/lidar/points"),
        ("sensor_msgs/msg/Imu", "/imu/data"),
        ("nav_msgs/msg/Odometry", "/odom"),
    ]
    .iter()
    .map(|(ty, topic)| {
        let times = (0..per_topic)
            .map(|i| offset + i as u64 * 10_000_000)
            .collect();
        (ty.to_string(), topic.to_string(), times)
    })
    .collect()
}

#[test]
fn a_bag_recorded_through_the_mcap_storage_plugin_is_read_as_a_bag() {
    let dir = tempfile::tempdir().unwrap();
    // Two shards, as a split recording writes: the second continues the first's timeline.
    write_mcap_bag(
        dir.path(),
        &[
            ("rec_0.mcap", rig_channels(1_000_000_000, 5)),
            ("rec_1.mcap", rig_channels(1_100_000_000, 5)),
        ],
        "mcap",
        30,
    );

    let adapter = veridex_core::adapter::rosbag2::Rosbag2Adapter;
    assert_eq!(
        adapter.detect(&Source::Local(dir.path().to_path_buf())),
        Detection::Yes {
            version: Some("9".into())
        },
        "a directory of .mcap shards with a manifest is a bag, and the manifest names its version"
    );

    let ingested = default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("the bag ingests");
    assert_eq!(ingested.report.format_id, "rosbag2");
    assert_eq!(ingested.report.coverage, Coverage::Full);

    let ep = &ingested.dataset.episodes[0];
    let lidar = ep
        .streams
        .iter()
        .find(|s| s.name == "/lidar/points")
        .expect("the lidar topic is a stream");
    assert_eq!(
        lidar.frames.len(),
        10,
        "both shards' messages land in one stream"
    );
    assert!(
        lidar.frames.windows(2).all(|w| w[0].ts <= w[1].ts),
        "the shards are read in the order the manifest lists them, so the timeline is in order"
    );
    assert_eq!(lidar.modality, Modality::PointCloud);

    // The bag's own facts, which reading the bare `.mcap` would not have supplied.
    let meta = |key: &str| {
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(meta("rosbag2_storage").as_deref(), Some("mcap"));
    assert_eq!(meta("ros_distro").as_deref(), Some("jazzy"));
    assert_eq!(meta("serialization_format").as_deref(), Some("cdr"));

    // The report describes the container that was actually read, not SQLite tables the bag has none
    // of.
    assert!(
        ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f == "channel.topic -> stream.name"),
        "{:?}",
        ingested.report.mapped_fields
    );
    assert!(
        !ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f.starts_with("topics.")),
        "an MCAP-backed bag has no `topics` table: {:?}",
        ingested.report.mapped_fields
    );
}

#[test]
fn which_storage_plugin_recorded_a_bag_does_not_change_what_veridex_sees() {
    // The neutrality claim, one level down: the same recording through the two storage plugins of
    // the same recorder must yield the same streams, modalities and timestamps.
    let dir = tempfile::tempdir().unwrap();
    let channels = rig_channels(1_000_000_000, 6);
    write_mcap_bag(dir.path(), &[("rec_0.mcap", channels.clone())], "mcap", 18);

    let as_bag = default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("the bag ingests")
        .dataset;

    let bare = dir.path().join("bare.mcap");
    write_mcap(&bare, &channels);
    let as_mcap = veridex_core::adapter::mcap::McapAdapter
        .ingest(&Source::Local(bare), &IngestOptions::default())
        .expect("the bare recording ingests")
        .dataset;

    assert_eq!(signature(&as_bag), signature(&as_mcap));
}

#[test]
fn an_mcap_shard_the_manifest_lists_but_the_bag_does_not_hold_is_unread() {
    let dir = tempfile::tempdir().unwrap();
    write_mcap_bag(
        dir.path(),
        &[("rec_0.mcap", rig_channels(1_000_000_000, 4))],
        "mcap",
        24, // the manifest counts a second shard's messages too
    );
    // Name a shard that is not there, as a manifest written before a shard was lost does.
    let manifest = std::fs::read_to_string(dir.path().join("metadata.yaml")).unwrap();
    std::fs::write(
        dir.path().join("metadata.yaml"),
        manifest.replace("    - rec_0.mcap\n", "    - rec_0.mcap\n    - rec_1.mcap\n"),
    )
    .unwrap();

    let ingested = default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("the bag still ingests over what it does hold");
    let unread: Vec<String> = ingested
        .report
        .unread_sources
        .iter()
        .map(|u| format!("{} :: {}", u.source_path, u.note))
        .collect();
    assert!(
        unread
            .iter()
            .any(|u| u.contains("rec_1.mcap") && u.contains("not in the bag directory")),
        "the missing shard is a coverage hole: {unread:#?}"
    );
    assert!(
        unread
            .iter()
            .any(|u| u.contains("message_count") && u.contains(".mcap file(s)")),
        "the shortfall against the manifest's total names the storage that was read: {unread:#?}"
    );
}

#[test]
fn a_manifest_that_disagrees_with_the_shards_it_has_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_mcap_bag(
        dir.path(),
        &[("rec_0.mcap", rig_channels(1_000_000_000, 3))],
        "sqlite3", // …over .mcap shards
        9,
    );
    let err = default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a manifest that describes a different container is refused");
    let text = err.to_string();
    assert!(
        text.contains("declares storage `sqlite3`") && text.contains(".mcap"),
        "the refusal names both sides of the disagreement: {text}"
    );
}

#[test]
fn a_directory_of_mcap_files_with_no_manifest_is_not_claimed_as_a_bag() {
    // A folder someone dropped three unrelated recordings into is not one bag, and reading it as one
    // would concatenate three timelines into a single episode and report the seams as defects.
    let dir = tempfile::tempdir().unwrap();
    write_mcap(&dir.path().join("a.mcap"), &rig_channels(1_000_000_000, 2));
    write_mcap(&dir.path().join("b.mcap"), &rig_channels(9_000_000_000, 2));
    assert_eq!(
        veridex_core::adapter::rosbag2::Rosbag2Adapter
            .detect(&Source::Local(dir.path().to_path_buf())),
        Detection::No
    );
}

#[test]
fn a_metadata_only_run_over_an_mcap_bag_names_the_shards_it_did_not_open() {
    let dir = tempfile::tempdir().unwrap();
    write_mcap_bag(
        dir.path(),
        &[("rec_0.mcap", rig_channels(1_000_000_000, 4))],
        "mcap",
        12,
    );
    let ingested = default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions {
                metadata_only: true,
                ..IngestOptions::default()
            },
        )
        .expect("a metadata-only run reads the manifest");
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.source_path == "*.mcap"),
        "the unopened shards are named by the extension they actually have: {:?}",
        ingested.report.unmapped_fields
    );
}

/// A recorder that died before writing a message leaves a shard with channels and no data. The bag
/// still ingests — refusing it would say nothing about what is wrong — but it must not come back
/// clean: an episode with no frames at all is the shape "silence reads as a pass" takes here.
#[test]
fn a_bag_whose_shard_holds_no_messages_fails_rather_than_passing_empty() {
    let dir = tempfile::tempdir().unwrap();
    write_mcap_bag(dir.path(), &[("rec_0.mcap", vec![])], "mcap", 0);
    let outcome = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(dir.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the bag ingests");
    assert_eq!(outcome.verdict.status, veridex_core::engine::Status::Fail);
    assert!(
        outcome
            .verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EMPTY_EPISODE"),
        "{:?}",
        outcome
            .verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
}
