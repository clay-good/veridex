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
    let timescale: u32 = 30_000;
    let delta = timescale / fps.max(1);
    let duration = delta * frames;

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

    let mut stsz = vec![0u8; 4]; // version + flags
    stsz.extend_from_slice(&1u32.to_be_bytes()); // uniform sample size
    stsz.extend_from_slice(&frames.to_be_bytes()); // sample_count

    let stbl = [bx(b"stsd", &stsd), bx(b"stsz", &stsz)].concat();
    let minf = bx(b"stbl", &stbl);
    let mdia = [bx(b"mdhd", &mdhd), bx(b"hdlr", &hdlr), bx(b"minf", &minf)].concat();
    let moov = bx(b"trak", &bx(b"mdia", &mdia));
    [bx(b"ftyp", b"isom\0\0\0\0isom"), bx(b"moov", &moov)].concat()
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

    // No media is attributed to any episode — attributing a shared file's frames to one episode
    // would invent the very number the checks compare.
    assert!(ingested
        .dataset
        .episodes
        .iter()
        .flat_map(|e| &e.streams)
        .all(|s| s.media.is_none()));
    assert!(video_findings(&ingested.dataset).is_empty());
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
fn a_dataset_with_no_video_tree_is_not_accused_of_losing_one() {
    let (dataset, findings) = run(
        VideoPlan {
            no_videos: true,
            ..VideoPlan::default()
        },
        10,
    );
    assert!(findings.is_empty(), "{findings:#?}");
    assert!(dataset
        .episodes
        .iter()
        .flat_map(|e| &e.streams)
        .all(|s| s.media.is_none()));
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
