//! Video and media checks end-to-end: the container's headers against the data they are paired with.
//!
//! The MP4s here are built by hand from the *minimum* set of boxes the ISO base media format
//! requires to describe a video track — deliberately fewer than the demo generator writes, so these
//! tests prove the probe reads the structure it is supposed to read rather than relying on anything
//! extra a particular writer happens to emit.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float32Array, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::{Adapter, IngestOptions, Source};
use veridex_core::cdm::{Dataset, MediaStatus};
use veridex_core::check::{Finding, Severity};
use veridex_core::checks::default_engine;

const FEATURE: &str = "observation.images.top";
const FPS: f64 = 30.0;

// ---- container construction ---------------------------------------------------------------------

/// One ISO base media box: big-endian 32-bit size, four-character type, payload.
fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A minimal MP4 describing `frames` samples of `width`x`height` in `codec` at `fps`.
fn build_mp4(frames: u32, width: u16, height: u16, codec: &[u8; 4], fps: u32) -> Vec<u8> {
    build_mp4_shaped(frames, width, height, codec, fps, Shape::default())
}

/// Container shapes a real encoder produces that are not the plain progressive one.
#[derive(Clone, Copy, Default)]
struct Shape {
    /// Declare `mvex` and put the samples in a `moof` fragment, leaving `stsz` at zero — what
    /// `ffmpeg -movflags frag_keyframe+empty_moov`, DASH/CMAF, and most hardware recorders write.
    fragmented: bool,
    /// Use the compact sample-size table (`stz2`) instead of `stsz`.
    compact_sample_table: bool,
    /// Write the all-ones `mdhd` duration the spec reserves for "unknown".
    unknown_duration: bool,
    /// Put a `trak` with no `mdia` ahead of the real video track.
    leading_bare_trak: bool,
}

fn build_mp4_shaped(
    frames: u32,
    width: u16,
    height: u16,
    codec: &[u8; 4],
    fps: u32,
    shape: Shape,
) -> Vec<u8> {
    let timescale: u32 = 30_000;
    let delta = timescale / fps.max(1);
    let duration = if shape.unknown_duration {
        u32::MAX
    } else {
        delta * frames
    };
    let table_frames = if shape.fragmented { 0 } else { frames };

    let mut mdhd = vec![0u8; 12]; // version + flags, creation, modification
    mdhd.extend_from_slice(&timescale.to_be_bytes());
    mdhd.extend_from_slice(&duration.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 4]); // language + pre_defined

    let mut hdlr = vec![0u8; 8]; // version + flags, pre_defined
    hdlr.extend_from_slice(b"vide");
    hdlr.extend_from_slice(&[0u8; 13]); // reserved + empty name

    let mut entry = vec![0u8; 6]; // reserved
    entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    entry.extend_from_slice(&[0u8; 16]); // pre_defined + reserved
    entry.extend_from_slice(&width.to_be_bytes());
    entry.extend_from_slice(&height.to_be_bytes());
    entry.extend_from_slice(&[0u8; 50]); // the rest of the VisualSampleEntry
    let entry = bx(codec, &entry);

    let mut stsd = vec![0u8; 4]; // version + flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd.extend_from_slice(&entry);

    let mut sizes = vec![0u8; 4]; // version + flags
    sizes.extend_from_slice(&1u32.to_be_bytes()); // uniform sample size / field size
    sizes.extend_from_slice(&table_frames.to_be_bytes()); // sample_count

    let size_box = if shape.compact_sample_table {
        bx(b"stz2", &sizes)
    } else {
        bx(b"stsz", &sizes)
    };
    let stbl = [bx(b"stsd", &stsd), size_box].concat();
    let minf = bx(b"stbl", &stbl);
    let mdia = [bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), bx(b"minf", &minf)].concat();
    let mut moov = Vec::new();
    if shape.leading_bare_trak {
        // A track with no `mdia` at all — a file can carry one and still hold a good video track.
        moov.extend_from_slice(&bx(b"trak", &bx(b"tkhd", &[0u8; 84])));
    }
    moov.extend_from_slice(&bx(b"trak", &mdia_trak(&mdia)));
    if shape.fragmented {
        // `mvex` is what marks the sample tables as living in fragments rather than in `moov`.
        moov.extend_from_slice(&bx(b"mvex", &bx(b"trex", &[0u8; 24])));
    }
    let mut out = [bx(b"ftyp", b"isom\0\0\0\0isom"), bx(b"moov", &moov)].concat();
    if shape.fragmented {
        out.extend_from_slice(&bx(b"moof", &bx(b"mfhd", &[0u8; 8])));
    }
    out
}

fn mdia_trak(mdia: &[u8]) -> Vec<u8> {
    bx(b"mdia", mdia)
}

// ---- dataset construction -----------------------------------------------------------------------

/// How a variant's video files should differ from what the manifest declares.
#[derive(Clone, Copy, Default)]
struct VideoPlan {
    /// Frames the container holds, when it should differ from the episode's row count.
    frames_override: Option<u32>,
    /// The episode `frames_override` (or `skip_episode`/`corrupt_episode`) applies to.
    episode: u64,
    /// Write no file at all for `episode`.
    skip_episode: bool,
    /// Write bytes that are not a container for `episode`.
    corrupt_episode: bool,
    /// The resolution the containers are actually encoded at.
    encoded: Option<(u16, u16)>,
    /// The codec fourcc the containers actually carry.
    codec: Option<[u8; 4]>,
    /// Write the files under a name that names no episode (the aggregated layout).
    aggregated: bool,
    /// Write no `videos/` tree at all.
    no_videos: bool,
}

/// Write a two-episode LeRobot dataset of `rows_per_episode` frames with one camera feature, its
/// videos laid out per `plan`. The manifest always declares 640x480 h264 at 30 fps.
fn write_dataset(dir: &Path, rows_per_episode: u64, plan: VideoPlan) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": FPS,
        "robot_type": "so100",
        "total_episodes": 2,
        "total_frames": rows_per_episode * 2,
        "features": {
            "action": { "dtype": "float32", "shape": [1] },
            FEATURE: {
                "dtype": "video",
                "shape": [480, 640, 3],
                "info": {
                    "video.codec": "h264",
                    "video.fps": FPS,
                    "video.height": 480,
                    "video.width": 640,
                },
            },
        },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();

    let rows: Vec<(i64, f64, f32)> = (0..2i64)
        .flat_map(|ep| {
            (0..rows_per_episode as i64).map(move |f| (ep, f as f64 / FPS, (ep * 100 + f) as f32))
        })
        .collect();
    write_parquet(&dir.join("data/file-000.parquet"), &rows);

    if plan.no_videos {
        return;
    }
    let dest = dir.join("videos").join(FEATURE);
    fs::create_dir_all(&dest).unwrap();
    let (width, height) = plan.encoded.unwrap_or((640, 480));
    let codec = plan.codec.unwrap_or(*b"avc1");
    for episode in 0..2u64 {
        if plan.skip_episode && episode == plan.episode {
            continue;
        }
        let name = if plan.aggregated {
            format!("file-{episode:03}.mp4")
        } else {
            format!("episode_{episode:06}.mp4")
        };
        let path = dest.join(name);
        if plan.corrupt_episode && episode == plan.episode {
            fs::write(&path, b"this is not a container at all").unwrap();
            continue;
        }
        let frames = match plan.frames_override {
            Some(n) if episode == plan.episode => n,
            _ => rows_per_episode as u32,
        };
        fs::write(&path, build_mp4(frames, width, height, &codec, FPS as u32)).unwrap();
    }
}

fn write_parquet(path: &Path, rows: &[(i64, f64, f32)]) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
        Field::new("action", DataType::Float32, false),
    ]));
    let episodes: Vec<i64> = rows.iter().map(|(e, _, _)| *e).collect();
    let frame_index: Vec<i64> = (0..rows.len() as i64).collect();
    let timestamps: Vec<f64> = rows.iter().map(|(_, t, _)| *t).collect();
    let values: Vec<f32> = rows.iter().map(|(_, _, v)| *v).collect();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(episodes)) as ArrayRef,
            Arc::new(Int64Array::from(frame_index)),
            Arc::new(Float64Array::from(timestamps)),
            Arc::new(Float32Array::from(values)),
        ],
    )
    .unwrap();
    let file = fs::File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn ingest(dir: &Path) -> Dataset {
    LeRobotAdapter
        .ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
        .expect("ingest")
        .dataset
}

/// Every finding whose code is in the `VIDEO.` family, in canonical order.
fn video_findings(dataset: &Dataset) -> Vec<Finding> {
    let mut dataset = dataset.clone();
    dataset.canonicalize_order();
    let hash = veridex_core::content_hash(&dataset);
    default_engine()
        .unwrap()
        .run(&dataset, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .filter(|f| f.code.starts_with("VIDEO."))
        .collect()
}

fn run(plan: VideoPlan, rows_per_episode: u64) -> (Dataset, Vec<Finding>) {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), rows_per_episode, plan);
    let dataset = ingest(dir.path());
    let findings = video_findings(&dataset);
    (dataset, findings)
}

// ---- tests --------------------------------------------------------------------------------------

#[test]
fn a_video_that_matches_its_data_and_its_manifest_says_nothing() {
    let (dataset, findings) = run(VideoPlan::default(), 10);
    assert!(
        findings.is_empty(),
        "expected no findings, got {findings:#?}"
    );

    // The probe still read the container: the checks were silent because they agreed, not because
    // there was nothing to compare.
    let media = dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == FEATURE)
        .and_then(|s| s.media.as_ref())
        .expect("the camera stream carries its media file");
    assert_eq!(media.status, MediaStatus::Read);
    assert_eq!(media.frame_count, Some(10));
    assert_eq!(media.observed.width, Some(640));
    assert_eq!(media.observed.height, Some(480));
    assert_eq!(media.observed.codec.as_deref(), Some("avc1"));
    assert_eq!(media.observed.fps, Some(30.0));
}

#[test]
fn the_manifests_codec_name_and_the_containers_fourcc_are_the_same_encoder() {
    // The manifest says `h264`; the container's sample entry is `avc1`. Reporting that as a mismatch
    // would fire on essentially every real LeRobot dataset.
    let (_, findings) = run(VideoPlan::default(), 10);
    assert!(!findings.iter().any(|f| f.code == "VIDEO.CODEC_MISMATCH"));

    // A genuinely different encoder is still caught.
    let (_, findings) = run(
        VideoPlan {
            codec: Some(*b"vp09"),
            ..VideoPlan::default()
        },
        10,
    );
    let f = findings
        .iter()
        .find(|f| f.code == "VIDEO.CODEC_MISMATCH")
        .expect("a vp9 container against a declared h264 is a mismatch");
    assert_eq!(f.severity, Severity::Warning);
    assert!(f.message.contains("vp09"), "{}", f.message);
}

#[test]
fn a_video_shorter_than_its_episode_is_caught_and_names_the_episode() {
    let (_, findings) = run(
        VideoPlan {
            frames_override: Some(7),
            episode: 1,
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let f = &findings[0];
    assert_eq!(f.code, "VIDEO.FRAME_COUNT_MISMATCH");
    assert_eq!(f.severity, Severity::Error);
    assert!(f.message.contains("episode 1"), "{}", f.message);
    assert!(
        f.message.contains("7 frames") && f.message.contains("records 10"),
        "the finding states both counts: {}",
        f.message
    );
}

#[test]
fn a_missing_video_file_is_an_error_not_a_silent_pass() {
    let (_, findings) = run(
        VideoPlan {
            skip_episode: true,
            episode: 1,
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.MEDIA_MISSING");
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(findings[0].message.contains("episode 1"));
}

#[test]
fn a_file_that_is_not_a_container_is_reported_with_the_reason() {
    let (_, findings) = run(
        VideoPlan {
            corrupt_episode: true,
            episode: 0,
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.MEDIA_UNREADABLE");
    // The reason names the structure that was wrong, so the message teaches rather than shrugs.
    assert!(
        findings[0].message.contains("box") || findings[0].message.contains("boxes"),
        "{}",
        findings[0].message
    );
}

#[test]
fn an_export_wide_resolution_mismatch_is_charged_once_not_once_per_episode() {
    // Both episodes were re-encoded at 320x240. That is one export defect, not two.
    let (_, findings) = run(
        VideoPlan {
            encoded: Some((320, 240)),
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    let f = &findings[0];
    assert_eq!(f.code, "VIDEO.RESOLUTION_MISMATCH");
    assert!(
        f.message.contains("640x480") && f.message.contains("320x240"),
        "{}",
        f.message
    );
    assert!(
        f.message.contains("2 episodes"),
        "the single finding says how many episodes it covers: {}",
        f.message
    );
}

#[test]
fn a_layout_that_names_no_episode_is_reported_as_unmapped_rather_than_guessed_at() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(
        dir.path(),
        10,
        VideoPlan {
            aggregated: true,
            ..VideoPlan::default()
        },
    );
    let ingested = LeRobotAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    // Nothing is *observed* for any episode — attributing a shared file's frames to one episode
    // would invent the very number the checks compare. But the abstention is recorded on the stream
    // rather than left as an absent `media`, which is what a non-video feature carries: with nothing
    // attached, the whole video family iterated past these streams and emitted nothing at all.
    let statuses: Vec<&veridex_core::cdm::MediaStatus> = ingested
        .dataset
        .episodes
        .iter()
        .flat_map(|e| &e.streams)
        .filter_map(|s| s.media.as_ref().map(|m| &m.status))
        .collect();
    assert!(
        !statuses.is_empty()
            && statuses
                .iter()
                .all(|st| matches!(st, veridex_core::cdm::MediaStatus::Unattributable { .. })),
        "{statuses:?}"
    );
    assert!(ingested
        .dataset
        .episodes
        .iter()
        .flat_map(|e| &e.streams)
        .filter_map(|s| s.media.as_ref())
        .all(
            |m| m.frame_count.is_none() && m.observed == veridex_core::cdm::MediaParams::default()
        ));
    // Nothing is *accused*; the one finding is the disclosure that nothing was checked.
    let found = video_findings(&ingested.dataset);
    let codes: Vec<&str> = found.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(codes, vec!["VIDEO.MEDIA_UNATTRIBUTED"], "{codes:?}");
    // And the limit is disclosed rather than passed over in silence.
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.source_path.contains(FEATURE)),
        "{:#?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_video_tree_that_never_arrived_is_reported_once_not_passed_over() {
    // The single most common real breakage: the manifest declares `dtype: "video"`, the rows are all
    // there, and `videos/` holds nothing — an un-pulled LFS pointer or an interrupted download.
    // Reading that as "nothing to check" would score a dataset with no imagery at all as sound.
    let (_, findings) = run(
        VideoPlan {
            no_videos: true,
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(
        findings.len(),
        1,
        "one gap, not one per episode: {findings:#?}"
    );
    assert_eq!(findings[0].code, "VIDEO.MEDIA_ABSENT");
    assert_eq!(findings[0].severity, Severity::Error);
    assert!(
        findings[0].message.contains("no episode of 2"),
        "{}",
        findings[0].message
    );
}

#[test]
fn a_feature_whose_pixels_are_not_in_video_files_is_not_asked_for_any() {
    // Only `dtype: "video"` means "the pixels live in video files". A feature merely *named*
    // `...images...`, or one with `dtype: "image"` (individual files) or a numeric array (inline in
    // the Parquet), has no video to find — demanding one would accuse a sound dataset.
    for dtype in ["image", "uint8"] {
        let dir = tempfile::tempdir().unwrap();
        write_dataset(
            dir.path(),
            10,
            VideoPlan {
                no_videos: true,
                ..VideoPlan::default()
            },
        );
        // Rewrite the manifest so the camera feature is no longer declared as video.
        let info = fs::read_to_string(dir.path().join("meta/info.json")).unwrap();
        let mut info: serde_json::Value = serde_json::from_str(&info).unwrap();
        info["features"][FEATURE]["dtype"] = serde_json::json!(dtype);
        fs::write(
            dir.path().join("meta/info.json"),
            serde_json::to_string(&info).unwrap(),
        )
        .unwrap();

        let dataset = ingest(dir.path());
        let findings = video_findings(&dataset);
        assert!(findings.is_empty(), "dtype {dtype}: {findings:#?}");
        assert!(dataset
            .episodes
            .iter()
            .flat_map(|e| &e.streams)
            .all(|s| s.media.is_none()));
    }
}

#[test]
fn the_media_a_stream_carries_binds_into_the_content_hash() {
    // A re-encode changes nothing else in the CDM — same rows, same timestamps, same values. If the
    // media did not bind, a certificate issued for the good export would verify against the broken
    // one.
    let good = tempfile::tempdir().unwrap();
    write_dataset(good.path(), 10, VideoPlan::default());
    let bad = tempfile::tempdir().unwrap();
    write_dataset(
        bad.path(),
        10,
        VideoPlan {
            frames_override: Some(7),
            episode: 1,
            ..VideoPlan::default()
        },
    );
    let (mut a, mut b) = (ingest(good.path()), ingest(bad.path()));
    // The dataset id is the directory name, which differs between the two temp dirs; neutralize it
    // so the hashes differ only by the media.
    a.id = "d".into();
    b.id = "d".into();
    assert_ne!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b),
        "a dataset whose video is three frames short must not hash like one whose video is whole"
    );
}

// ---- container shapes a real encoder writes ------------------------------------------------------
//
// Each of these was a false finding before it was a test: the probe read a container it did not
// model as a container that held nothing, and the checks faithfully reported the fabrication.

/// Write the two-episode dataset with every video built to `shape`, and return the video findings.
fn run_shaped(shape: Shape) -> Vec<Finding> {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    for episode in 0..2u64 {
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mp4_shaped(10, 640, 480, b"avc1", FPS as u32, shape),
        )
        .unwrap();
    }
    video_findings(&ingest(dir.path()))
}

#[test]
fn a_fragmented_container_is_not_read_as_holding_zero_frames() {
    // A fragmented MP4 keeps a complete `moov` whose sample table is empty and every sample in
    // `moof` fragments. Its `stsz` says zero, meaning "the table is not here" — not "no frames".
    // Reading it as a count fails every episode of a valid dataset with a hard error.
    let findings = run_shaped(Shape {
        fragmented: true,
        ..Shape::default()
    });
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn the_compact_sample_table_counts_frames_the_same_as_the_plain_one() {
    // `stz2` is a different encoding of the same field. Which one the encoder chose must not decide
    // whether the check runs at all.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    let compact = Shape {
        compact_sample_table: true,
        ..Shape::default()
    };
    // Episode 0 is whole; episode 1 is three frames short. Both use `stz2`.
    fs::write(
        dest.join("episode_000000.mp4"),
        build_mp4_shaped(10, 640, 480, b"avc1", FPS as u32, compact),
    )
    .unwrap();
    fs::write(
        dest.join("episode_000001.mp4"),
        build_mp4_shaped(7, 640, 480, b"avc1", FPS as u32, compact),
    )
    .unwrap();
    let findings = video_findings(&ingest(dir.path()));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.FRAME_COUNT_MISMATCH");
    assert!(findings[0].message.contains("episode 1"));
}

#[test]
fn an_unknown_duration_yields_no_rate_rather_than_a_fabricated_one() {
    // ISO/IEC 14496-12 reserves an all-ones `mdhd` duration for "unknown". Taken literally it is 49
    // days, and the rate derived from it is ~0.002 fps — a mismatch against every declared rate.
    let findings = run_shaped(Shape {
        unknown_duration: true,
        ..Shape::default()
    });
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn a_track_this_parser_cannot_walk_does_not_hide_the_video_track_behind_it() {
    // A `trak` with no `mdia` ahead of the real one must not abort the scan: the file does have a
    // video track, and reporting "no video track" about it is a wrong answer, not a cautious one.
    let findings = run_shaped(Shape {
        leading_bare_trak: true,
        ..Shape::default()
    });
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn a_box_that_runs_to_the_end_of_the_file_is_named_as_the_reason_moov_is_unreachable() {
    // A writer that emits `mdat` with a declared size of 0 makes anything after it unreachable by
    // definition. "No moov box" is true but useless; naming the box that swallowed it is not.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let path = dir
        .path()
        .join("videos")
        .join(FEATURE)
        .join("episode_000000.mp4");
    let mut bytes = bx(b"ftyp", b"isom\0\0\0\0isom");
    bytes.extend_from_slice(&[0, 0, 0, 0]); // size 0: "to the end of the file"
    bytes.extend_from_slice(b"mdat");
    bytes.extend_from_slice(&build_mp4(10, 640, 480, b"avc1", FPS as u32));
    fs::write(&path, &bytes).unwrap();

    let findings = video_findings(&ingest(dir.path()));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.MEDIA_UNREADABLE");
    assert!(
        findings[0].message.contains("mdat") && findings[0].message.contains("end of the file"),
        "{}",
        findings[0].message
    );
}

// ---- what the manifest says, and what may be inferred from it -----------------------------------

/// Rewrite the camera feature's manifest entry, then ingest and return the video findings.
fn with_feature_entry(entry: serde_json::Value, videos: bool) -> Vec<Finding> {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(
        dir.path(),
        10,
        VideoPlan {
            no_videos: !videos,
            ..VideoPlan::default()
        },
    );
    let info = fs::read_to_string(dir.path().join("meta/info.json")).unwrap();
    let mut info: serde_json::Value = serde_json::from_str(&info).unwrap();
    info["features"][FEATURE] = entry;
    fs::write(
        dir.path().join("meta/info.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();
    video_findings(&ingest(dir.path()))
}

#[test]
fn a_codec_name_veridex_does_not_recognize_produces_no_finding_either_way() {
    // Encoder names are an open namespace: `libopenh264` and `h264_videotoolbox` both write `avc1`,
    // and new encoders appear constantly. A closed table that treats "unrecognized" as "different"
    // flags honest data, which is the one thing a check must never do.
    for codec in ["libopenh264", "h264_videotoolbox", "some_future_encoder_v9"] {
        let findings = with_feature_entry(
            serde_json::json!({
                "dtype": "video",
                "shape": [480, 640, 3],
                "info": { "video.codec": codec, "video.fps": FPS, "video.height": 480, "video.width": 640 },
            }),
            true,
        );
        assert!(
            !findings.iter().any(|f| f.code == "VIDEO.CODEC_MISMATCH"),
            "{codec}: {findings:#?}"
        );
    }
}

#[test]
fn a_channel_first_shape_is_read_by_the_manifests_own_axis_names() {
    // With `video.width`/`video.height` absent, the resolution can only come from `shape` — and the
    // axis order is stated in `names`. Assuming height-first would declare this feature's height to
    // be 3 and report a resolution mismatch against a perfectly good video.
    let findings = with_feature_entry(
        serde_json::json!({
            "dtype": "video",
            "shape": [3, 480, 640],
            "names": ["channels", "height", "width"],
            "info": { "video.codec": "h264", "video.fps": FPS },
        }),
        true,
    );
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn a_shape_whose_axis_order_is_unstated_and_unguessable_yields_no_resolution() {
    // No `names`, and a leading dimension of 3 that could be channels or could be a height. Veridex
    // states nothing rather than guessing: an invented "declared 480x3" is worse than no comparison.
    let findings = with_feature_entry(
        serde_json::json!({
            "dtype": "video",
            "shape": [3, 480, 640],
            "info": { "video.codec": "h264", "video.fps": FPS },
        }),
        true,
    );
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn a_declared_rate_the_container_does_not_play_at_is_caught() {
    // The container is written at 30 fps; the manifest claims 60.
    let findings = with_feature_entry(
        serde_json::json!({
            "dtype": "video",
            "shape": [480, 640, 3],
            "info": { "video.codec": "h264", "video.fps": 60.0, "video.height": 480, "video.width": 640 },
        }),
        true,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.FPS_MISMATCH");
    assert_eq!(findings[0].severity, Severity::Warning);
    assert!(
        findings[0].message.contains("60.000") && findings[0].message.contains("30.000"),
        "{}",
        findings[0].message
    );
}

// ---- layouts a real repository actually has ------------------------------------------------------

/// Move the camera feature's videos into the LeRobot chunk layout, optionally deleting one episode's.
fn chunked_layout(delete_episode: Option<u64>) -> (Vec<Finding>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let flat = dir.path().join("videos").join(FEATURE);
    let chunked = dir.path().join("videos").join("chunk-000").join(FEATURE);
    fs::create_dir_all(&chunked).unwrap();
    for episode in 0..2u64 {
        let name = format!("episode_{episode:06}.mp4");
        if Some(episode) == delete_episode {
            fs::remove_file(flat.join(&name)).unwrap();
            continue;
        }
        fs::rename(flat.join(&name), chunked.join(&name)).unwrap();
    }
    fs::remove_dir_all(&flat).unwrap();
    let findings = video_findings(&ingest(dir.path()));
    (findings, dir)
}

#[test]
fn the_chunk_directory_layout_a_real_repository_uses_resolves() {
    let (findings, _dir) = chunked_layout(None);
    assert!(findings.is_empty(), "{findings:#?}");
}

#[test]
fn a_missing_file_names_the_path_its_siblings_actually_use() {
    // The finding is only actionable if it names the path the dataset really uses. Under the chunk
    // layout that path is not guessable, so it is copied from the sibling episode next to it.
    let (findings, _dir) = chunked_layout(Some(1));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.MEDIA_MISSING");
    assert!(
        findings[0].message.contains("chunk-000"),
        "the path names the chunk directory its sibling lives in: {}",
        findings[0].message
    );
}

#[test]
fn a_feature_with_both_a_per_episode_and_an_aggregated_file_is_not_called_incomplete() {
    // A part-converted repository: episode 0 still per-episode, episode 1's frames inside a v3
    // aggregate. The pixels are all there; calling episode 1 missing would contradict the coverage
    // note the same run prints.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    fs::rename(dest.join("episode_000001.mp4"), dest.join("file-000.mp4")).unwrap();

    let ingested = LeRobotAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    // Nothing is accused. The only finding is the informational disclosure that the layout puts
    // this stream's video beyond what the checks can pair with its rows.
    let found = video_findings(&ingested.dataset);
    let codes: Vec<&str> = found.iter().map(|f| f.code.as_str()).collect();
    assert!(
        codes.iter().all(|c| *c == "VIDEO.MEDIA_UNATTRIBUTED"),
        "{found:#?}"
    );
    assert!(ingested
        .report
        .unmapped_fields
        .iter()
        .any(|u| u.source_path.contains(FEATURE)));
}

#[test]
fn episodes_encoded_at_different_wrong_resolutions_are_two_findings_not_one() {
    // Collapsing them under whichever came first would report episode 1 as holding a resolution it
    // does not hold, and hide the more serious condition: the episodes disagree with each other.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    fs::write(
        dest.join("episode_000000.mp4"),
        build_mp4(10, 320, 240, b"avc1", FPS as u32),
    )
    .unwrap();
    fs::write(
        dest.join("episode_000001.mp4"),
        build_mp4(10, 160, 120, b"avc1", FPS as u32),
    )
    .unwrap();
    let findings = video_findings(&ingest(dir.path()));
    assert_eq!(findings.len(), 2, "{findings:#?}");
    assert!(findings
        .iter()
        .all(|f| f.code == "VIDEO.RESOLUTION_MISMATCH"));
    assert!(findings.iter().any(|f| f.message.contains("320x240")));
    assert!(findings.iter().any(|f| f.message.contains("160x120")));
}

#[test]
fn an_export_that_is_short_by_the_same_amount_everywhere_is_one_finding() {
    // Every episode's video one frame short is an encoder or converter defect, not N broken
    // episodes — charged once like the other export-wide defects, and still an error.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    for episode in 0..2u64 {
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mp4(9, 640, 480, b"avc1", FPS as u32),
        )
        .unwrap();
    }
    let findings = video_findings(&ingest(dir.path()));
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.FRAME_COUNT_MISMATCH");
    assert_eq!(
        findings[0].severity,
        Severity::Error,
        "rolling a defect up changes how often it is reported, not how serious it is"
    );
    assert!(
        findings[0].message.contains("2 episodes") && findings[0].message.contains("1 frame(s)"),
        "{}",
        findings[0].message
    );
}

#[test]
fn episodes_short_by_different_amounts_stay_per_episode() {
    // No single pattern to charge once — each episode is separately wrong, and naming them is the
    // point.
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    for (episode, frames) in [(0u64, 9u32), (1, 4)] {
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mp4(frames, 640, 480, b"avc1", FPS as u32),
        )
        .unwrap();
    }
    let findings = video_findings(&ingest(dir.path()));
    assert_eq!(findings.len(), 2, "{findings:#?}");
    assert!(findings
        .iter()
        .all(|f| f.code == "VIDEO.FRAME_COUNT_MISMATCH"));
    assert!(findings.iter().any(|f| f.message.contains("episode 0")));
    assert!(findings.iter().any(|f| f.message.contains("episode 1")));
}

#[test]
fn the_media_uri_uses_forward_slashes_whatever_the_platform() {
    // The uri binds into the content hash, so a platform path separator would make the same dataset
    // hash differently on Windows than on Linux.
    let (findings, _dir) = chunked_layout(Some(1));
    assert!(
        findings[0].message.contains("videos/chunk-000/"),
        "{}",
        findings[0].message
    );
}
