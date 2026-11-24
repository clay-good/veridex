//! KITTI raw-data adapter tests.
//!
//! The fixtures are written by `veridex_demo::kitti`, which builds the layout from the KITTI raw dev
//! kit's own published shape rather than from this reader — the rule every generator in that crate
//! follows, so a fixture cannot agree with a reader that is wrong about the format.
//!
//! The format is a directory of text files and raw `float32` blobs, so there is no reference writer
//! to pin bytes against the way the HDF5 and Zarr fixtures do. What is pinned instead is the shape
//! the dev kit documents: `<sensor>/timestamps.txt` beside `<sensor>/data/*`, the calibration in the
//! date directory above the drive, thirty whitespace-separated OXTS fields with latitude, longitude
//! and altitude first, and sixteen bytes per Velodyne point.

use std::path::{Path, PathBuf};

use veridex_core::adapter::{Adapter, Coverage, IngestOptions, Ingested, Source};
use veridex_core::cdm::{ClockKind, Modality};

/// Write a demo drive into a fresh temporary directory and return the drive path.
fn drive(variant: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    veridex_demo::kitti::write(dir.path(), variant).expect("write the demo drive");
    let path = veridex_demo::kitti::drive_path(dir.path());
    (dir, path)
}

fn ingest_at(path: &Path, options: IngestOptions) -> Ingested {
    veridex_core::adapter::default_registry()
        .ingest(&Source::Local(path.to_path_buf()), &options)
        .expect("the demo drive ingests")
}

fn ingest(variant: &str) -> (tempfile::TempDir, Ingested) {
    let (dir, path) = drive(variant);
    let got = ingest_at(&path, IngestOptions::default());
    (dir, got)
}

#[test]
fn a_drive_is_detected_and_a_date_directory_says_what_it_is() {
    let (dir, path) = drive("rig");
    // The registry picks this reader for a drive directory: an ingest that succeeds under no
    // explicit `--format` is the detection working.
    let got = ingest_at(&path, IngestOptions::default());
    assert_eq!(got.report.format_id, "kitti");
    assert_eq!(got.report.source_version.as_deref(), Some("raw-sync"));
    // The date directory holds the calibration and the drives; it is not a dataset, and saying
    // "unsupported format" about it would be untrue and unhelpful.
    let date_dir = path.parent().expect("date directory").to_path_buf();
    let hint = veridex_core::adapter::kitti::KittiAdapter
        .incomplete_hint(&Source::Local(date_dir))
        .expect("a date directory is recognized for what it is");
    assert!(hint.contains("date directory"), "{hint}");
    assert!(
        hint.contains("2011_09_26_drive_0005_sync"),
        "it names the drive to point at: {hint}"
    );
    drop(dir);
}

#[test]
fn every_sensor_becomes_a_stream_on_one_clock_with_its_own_timestamps() {
    let (_dir, got) = ingest("rig");
    let episode = &got.dataset.episodes[0];
    // Sorted here because the pipeline canonicalizes stream order before anything reads it; what
    // this pins is the set, which is every sensor directory the drive holds.
    let mut names: Vec<&str> = episode.streams.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "image_00",
            "image_01",
            "image_02",
            "image_03",
            "oxts",
            "velodyne_points"
        ]
    );
    let modality = |name: &str| {
        episode
            .streams
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.modality)
            .expect(name)
    };
    assert_eq!(modality("image_00"), Modality::Video);
    assert_eq!(modality("velodyne_points"), Modality::PointCloud);
    assert_eq!(modality("oxts"), Modality::Gnss);

    // One time base for the whole rig — which is what makes the cross-sensor checks answerable.
    assert!(episode.streams.iter().all(|s| s.clock_id == "kitti-rig"));
    assert!(episode
        .streams
        .iter()
        .all(|s| s.clock_kind == ClockKind::Measured));
    // Each stream carries the times its own `timestamps.txt` states, as nanoseconds since the
    // epoch: 2011-09-26 13:02:25.0 and twenty samples at 100 ms.
    let camera = episode
        .streams
        .iter()
        .find(|s| s.name == "image_00")
        .expect("the camera");
    assert_eq!(camera.frames.len(), 20);
    assert_eq!(camera.frames[0].ts, 1_317_042_145_000_000_000);
    assert_eq!(
        camera.frames[1].ts - camera.frames[0].ts,
        100_000_000,
        "the interval is the one the file states"
    );
    // A frame points at the sample file it came from, relative to the drive.
    assert_eq!(
        camera.frames[0].value_ref.uri,
        "image_00/data/0000000000.png"
    );
    assert!(
        camera.frames[0].value_ref.content_hash.is_some(),
        "a full read fingerprints each sample"
    );
    // Every sensor's directory is the coordinate frame the calibration relates it through.
    assert_eq!(camera.frame_id.as_deref(), Some("image_00"));
}

#[test]
fn a_sweeps_point_count_comes_from_its_length_and_the_oxts_position_from_its_row() {
    let (_dir, got) = ingest("rig");
    let episode = &got.dataset.episodes[0];
    let lidar = episode
        .streams
        .iter()
        .find(|s| s.name == "velodyne_points")
        .expect("the LiDAR");
    let counts = lidar
        .observed_point_counts
        .expect("a sweep's length is its point count");
    assert_eq!(counts.message_count, 20);
    assert_eq!(counts.max, 64, "sixteen bytes per point, read from `stat`");
    assert_eq!(counts.empty, 0);

    // The three OXTS fields the plausibility rules grade, named by what they are.
    let oxts = episode
        .streams
        .iter()
        .find(|s| s.name == "oxts")
        .expect("the GPS/IMU");
    assert_eq!(
        oxts.dim_names.as_deref(),
        Some(
            [
                "latitude".to_string(),
                "longitude".to_string(),
                "altitude".to_string()
            ]
            .as_slice()
        )
    );
    let stats = oxts.observed_stats.expect("the positions are summarized");
    assert!(
        stats.min > 8.0 && stats.max < 113.0,
        "the summary is over the recorded values: {stats:?}"
    );
    let dims = oxts
        .observed_dim_stats
        .as_ref()
        .expect("per-dimension, so a check can name the field");
    assert_eq!(dims.len(), 3);
    assert!(
        (dims[0].stats.min - 49.015).abs() < 1e-6,
        "latitude: {:?}",
        dims[0].stats
    );
}

#[test]
fn the_calibration_beside_the_drive_becomes_a_connected_transform_tree() {
    let (_dir, got) = ingest("rig");
    let calib = got
        .dataset
        .calibration
        .as_ref()
        .expect("the date directory's calibration files are read");
    // One set of intrinsics per camera, with the image size the calibration was computed for —
    // which is what makes the principal point checkable at all.
    assert_eq!(calib.intrinsics.len(), 4);
    let cam0 = calib
        .intrinsics
        .iter()
        .find(|c| c.stream == "image_00")
        .expect("camera 0");
    assert!((cam0.fx - 984.2439).abs() < 1e-3, "{cam0:?}");
    assert!((cam0.cx - 690.0).abs() < 1e-6, "{cam0:?}");
    assert_eq!(cam0.width, Some(1392));
    assert_eq!(cam0.height, Some(512));
    assert_eq!(cam0.distortion.len(), 5);
    assert_eq!(cam0.distortion_model.as_deref(), Some("plumb_bob"));

    // `velo -> cam`, `imu -> velo`, and the three cameras off camera 0: every sensor in one tree.
    let edges: Vec<(&str, &str)> = calib
        .transforms
        .iter()
        .map(|t| (t.parent_frame.as_str(), t.child_frame.as_str()))
        .collect();
    assert!(
        edges.contains(&("image_00", "velodyne_points")),
        "{edges:?}"
    );
    assert!(edges.contains(&("velodyne_points", "oxts")), "{edges:?}");
    assert!(edges.contains(&("image_00", "image_02")), "{edges:?}");
    // A rotation matrix is carried as a unit quaternion, so the calibration rules can check it.
    let velo = calib
        .transforms
        .iter()
        .find(|t| t.child_frame == "velodyne_points")
        .expect("velo -> cam");
    let norm: f64 = velo.pose.rotation.iter().map(|v| v * v).sum::<f64>().sqrt();
    assert!((norm - 1.0).abs() < 1e-9, "{:?}", velo.pose.rotation);

    // And the calibration's own date, which is the one provenance fact these files state.
    let calibration = got
        .dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .find(|e| e.key == "calibration")
        .expect("calibration provenance");
    assert!(
        calibration
            .value
            .as_deref()
            .is_some_and(|v| v.contains("09-Jan-2012")),
        "{calibration:?}"
    );
}

#[test]
fn the_reader_claims_no_provenance_the_files_do_not_state() {
    // The rig is famous and the files say nothing about it. A reader that filled in "Velodyne
    // HDL-64E" from what its author knows would be inventing provenance, which is the one thing
    // this tool must never do.
    let (_dir, got) = ingest("rig");
    let keys: Vec<&str> = got
        .dataset
        .provenance
        .iter()
        .flat_map(|p| p.elements.iter())
        .map(|e| e.key.as_str())
        .collect();
    assert!(!keys.contains(&"sensor"), "{keys:?}");
    assert!(!keys.contains(&"license"), "{keys:?}");
    assert!(
        got.report
            .omitted_fields
            .iter()
            .any(|f| f.contains("provenance.sensor")),
        "and it says so: {:?}",
        got.report.omitted_fields
    );
}

#[test]
fn a_metadata_only_run_reads_the_manifest_and_opens_no_sample() {
    // `timestamps.txt` is this format's manifest: the sensor inventory, each one's modality, and the
    // sample count it states — which is the episode's declared length here. No sample is opened, and
    // like every other format the run carries no frames, so the temporal family abstains.
    //
    // The sample *times* are in that same manifest and are deliberately not materialized: a run
    // that emitted them would answer the timing questions while its coverage note said it had read
    // nothing, and the checks that stand down when no payload was read would stand down over frames
    // that were there.
    let (dir, path) = drive("rig");
    let got = ingest_at(
        &path,
        IngestOptions {
            metadata_only: true,
            ..IngestOptions::default()
        },
    );
    assert!(matches!(got.report.coverage, Coverage::MetadataOnly { .. }));
    let episode = &got.dataset.episodes[0];
    assert_eq!(episode.streams.len(), 6);
    assert!(
        episode.streams.iter().all(|s| s.frames.is_empty()),
        "no payload was read, so no stream carries frames"
    );
    // What the manifest *does* state reaches the CDM: the episode's declared length, which a full
    // read then has something to disagree with.
    assert_eq!(episode.declared_frame_count, Some(20));
    assert!(episode
        .streams
        .iter()
        .all(|s| s.observed_point_counts.is_none() && s.observed_stats.is_none()));
    assert!(
        got.report
            .omitted_fields
            .iter()
            .any(|f| f.contains("metadata-only")),
        "{:?}",
        got.report.omitted_fields
    );
    drop(dir);
}

#[test]
fn a_part_uploaded_drive_is_disclosed_rather_than_read_as_a_shorter_one() {
    let (_dir, got) = ingest("missing-samples");
    let camera = got.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "image_00")
        .expect("the camera");
    assert_eq!(
        camera.frames.len(),
        15,
        "the frames are the samples the manifest and the directory agree on"
    );
    let note = got
        .report
        .unread_sources
        .iter()
        .find(|f| f.source_path.ends_with("image_00"))
        .expect("the difference is disclosed as unread source data");
    assert!(note.note.contains("states 20"), "{note:?}");
    assert!(note.note.contains("holds 15"), "{note:?}");
}

#[test]
fn a_drive_whose_lidar_recorded_nothing_is_caught_end_to_end() {
    // The fault this format's point counts exist for: every sweep is a well-formed file of zero
    // points, on time, in the right frame — so the structural and temporal families pass, and the
    // length of the file is the only thing that says the sensor recorded nothing.
    let (_dir, got) = ingest("dead-lidar");
    let counts = got.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "velodyne_points")
        .and_then(|s| s.observed_point_counts)
        .expect("the counts were read");
    assert_eq!(counts.message_count, 20);
    assert_eq!(counts.empty, 20);

    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&got.dataset);
    let codes: Vec<String> = engine
        .run(&got.dataset, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        codes.contains(&"AUTONOMY.POINT_CLOUD_EMPTY".to_string()),
        "{codes:?}"
    );
}

#[test]
fn a_calibration_that_states_a_zero_image_size_states_no_size() {
    // `S_0N: 0.0 0.0` is what an unconfigured or hand-trimmed `calib_cam_to_cam.txt` carries. Read
    // literally it is a zero-pixel image, and `autonomy.calibration-plausibility` judges a
    // principal point against the size the calibration declares — any point at all is outside an
    // image that small, so the camera would be accused of intrinsics from another resolution on
    // the strength of a field that said nothing. The ROS path already maps a `CameraInfo` of
    // `width: 0` to "not declared" (`cdr::camera_info`), and the same silence has to read the same
    // way whichever container carries it.
    let (dir, path) = drive("rig");
    let date_dir = path.parent().expect("date directory");
    let calib = date_dir.join("calib_cam_to_cam.txt");
    let text = std::fs::read_to_string(&calib).expect("the demo calibration");
    let zeroed = text
        .lines()
        .map(|line| {
            if line.starts_with("S_") {
                format!("{}: 0.000000e+00 0.000000e+00", &line[..4])
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(zeroed.contains("S_00: 0.000000e+00"), "{zeroed}");
    std::fs::write(&calib, zeroed + "\n").expect("rewrite the calibration");

    let got = ingest_at(&path, IngestOptions::default());
    let calib = got
        .dataset
        .calibration
        .as_ref()
        .expect("the calibration is still read");
    for cam in &calib.intrinsics {
        assert_eq!(cam.width, None, "{cam:?}");
        assert_eq!(cam.height, None, "{cam:?}");
        // The intrinsics themselves are untouched: only the size went unstated.
        assert!((cam.cx - 690.0).abs() < 1e-6, "{cam:?}");
    }

    drop(dir);
}

#[test]
fn a_receiver_that_never_acquired_a_fix_is_caught_end_to_end() {
    let (_dir, got) = ingest("null-island");
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&got.dataset);
    let codes: Vec<String> = engine
        .run(&got.dataset, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        codes.contains(&"AUTONOMY.GNSS_UNSET".to_string()),
        "{codes:?}"
    );
}

#[test]
fn what_the_receiver_said_about_its_own_sky_is_read_from_numsats() {
    // The one field in an OXTS row that says whether the position beside it was measured from
    // satellites at all. Without it this format abstained on fix availability — and the fault that
    // check exists for is invisible to every other one, because a row with no satellites still
    // arrives on time with a plausible-looking position in it.
    let (_dir, got) = ingest("rig");
    let fix = got.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "oxts")
        .and_then(|s| s.observed_fix_availability)
        .expect("the satellite count is read");
    assert_eq!(fix.message_count, 20);
    assert_eq!(fix.unfixed, 0);

    // A drive that lost the sky for four fifths of itself says so in `numsats`, and the position is
    // still recorded: unlike a `NavSatFix` whose `STATUS_NO_FIX` disclaims its own coordinates, an
    // OXTS row reports a blended inertial position and disclaims nothing.
    let (_dir2, lost) = ingest("no-fix");
    let oxts = lost.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "oxts")
        .expect("the GPS/IMU");
    let fix = oxts.observed_fix_availability.expect("counted");
    assert_eq!(fix.message_count, 20);
    assert_eq!(fix.unfixed, 16);
    assert!(
        oxts.observed_stats.is_some(),
        "the positions are still summarized: the row does not disclaim them"
    );

    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&lost.dataset);
    let codes: Vec<String> = engine
        .run(&lost.dataset, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        codes.contains(&"AUTONOMY.GNSS_NO_FIX".to_string()),
        "{codes:?}"
    );
    // And the clean drive no longer abstains on the question, because the count is there to read.
    let (_dir3, clean) = ingest("rig");
    let clean_codes: Vec<String> = engine
        .run(
            &clean.dataset,
            veridex_core::content_hash(&clean.dataset),
            &veridex_core::RunConfig::default(),
        )
        .findings
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        !clean_codes.contains(&"AUTONOMY.GNSS_STATUS_UNREAD".to_string()),
        "{clean_codes:?}"
    );
}

#[test]
fn re_ingesting_the_same_drive_yields_the_same_content_hash() {
    let (_dir, first) = ingest("rig");
    let (_dir2, second) = ingest("rig");
    assert_eq!(
        veridex_core::content_hash(&first.dataset).to_hex(),
        veridex_core::content_hash(&second.dataset).to_hex(),
        "two writes of one drive must hash alike"
    );
    // …and a different drive does not.
    let (_dir3, dead) = ingest("dead-lidar");
    assert_ne!(
        veridex_core::content_hash(&first.dataset).to_hex(),
        veridex_core::content_hash(&dead.dataset).to_hex()
    );
}

#[test]
fn a_directory_with_no_sensor_timestamps_is_refused_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("image_00/data")).expect("mkdir");
    // No `timestamps.txt`, so this is not a drive: no reader may claim it, and the refusal says
    // the format was not recognized rather than mis-parsing an empty one.
    let err = veridex_core::adapter::default_registry()
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a directory of empty sensor folders is not a drive");
    assert!(
        matches!(
            err,
            veridex_core::adapter::IngestError::UnsupportedFormat { .. }
        ),
        "{err}"
    );
}

#[test]
fn a_sensor_directory_this_reader_has_no_modality_for_is_disclosed_not_skipped() {
    // A rig that recorded something else — a thermal camera, a radar — has data this reader did not
    // look at, which is a hole in coverage rather than a fact about the dataset.
    let (dir, path) = drive("rig");
    std::fs::create_dir_all(path.join("thermal_00/data")).expect("mkdir");
    std::fs::write(
        path.join("thermal_00/timestamps.txt"),
        "2011-09-26 13:02:25.000000000\n",
    )
    .expect("write");
    let got = ingest_at(&path, IngestOptions::default());
    assert!(
        got.report
            .unread_sources
            .iter()
            .any(|f| f.source_path == "thermal_00"),
        "{:?}",
        got.report.unread_sources
    );
    drop(dir);
}

#[test]
fn a_line_that_is_not_a_timestamp_becomes_no_frame_and_is_disclosed() {
    let (dir, path) = drive("rig");
    let stamps = path.join("oxts/timestamps.txt");
    let text = std::fs::read_to_string(&stamps).expect("read");
    std::fs::write(&stamps, format!("{text}not a timestamp\n")).expect("write");
    let got = ingest_at(&path, IngestOptions::default());
    let oxts = got.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "oxts")
        .expect("the GPS/IMU");
    assert_eq!(oxts.frames.len(), 20, "the bad line became no frame");
    assert!(
        got.report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("not a `YYYY-MM-DD")),
        "{:?}",
        got.report.unmapped_fields
    );
    drop(dir);
}

#[test]
fn the_frame_budget_refuses_a_drive_past_it() {
    let (dir, path) = drive("rig");
    let err = veridex_core::adapter::default_registry()
        .ingest(
            &Source::Local(path),
            &IngestOptions {
                max_frames: Some(10),
                ..IngestOptions::default()
            },
        )
        .expect_err("a budget below the drive's frame count refuses");
    assert!(
        format!("{err}").contains("10"),
        "the refusal names the budget: {err}"
    );
    drop(dir);
}
