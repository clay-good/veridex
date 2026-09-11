//! Video and media checks end-to-end: the container's headers against the data they are paired with.
//!
//! The MP4s here are built by hand from the *minimum* set of boxes the ISO base media format
//! requires to describe a video track — deliberately fewer than the demo generator writes, so these
//! tests prove the probe reads the structure it is supposed to read rather than relying on anything
//! extra a particular writer happens to emit. The Matroska files are built the same way, and to the
//! same standard: a Matroska carries no sample table, so its frame count exists only as the blocks
//! in its clusters, and the fixtures state that count in the one way the format does.

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

// ---- Matroska construction ----------------------------------------------------------------------

/// One EBML element: the id bytes verbatim, then the payload length as a variable-length integer in
/// its shortest form (what a real writer emits), then the payload.
fn el(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend_from_slice(&size_vint(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// A length as an EBML vint, in the shortest width that holds it without colliding with the
/// all-ones value the format reserves for "unknown".
fn size_vint(n: u64) -> Vec<u8> {
    for width in 1..=8u32 {
        let bits = 7 * width;
        // The all-ones value is reserved for "unknown", so a length that would encode as it needs
        // the next width up.
        if n < (1u64 << bits) - 1 {
            let mut bytes = n.to_be_bytes()[8 - width as usize..].to_vec();
            bytes[0] |= 0x80u8 >> (width - 1);
            return bytes;
        }
    }
    unreachable!("a length this large is not written by these fixtures")
}

/// An EBML unsigned integer, big-endian, in its shortest whole-byte form.
fn uint_el(id: &[u8], v: u64) -> Vec<u8> {
    let mut bytes = v.to_be_bytes().to_vec();
    while bytes.len() > 1 && bytes[0] == 0 {
        bytes.remove(0);
    }
    el(id, &bytes)
}

/// How a Matroska fixture should differ from the plain one-frame-per-block file.
#[derive(Clone, Copy, Default, PartialEq)]
struct Mkv {
    /// Put `lace` frames in each block using EBML lacing, instead of one.
    lace: Option<u8>,
    /// Declare the cluster's size as the all-ones "unknown" vint, as a live muxer does.
    unknown_cluster_size: bool,
    /// Write `DocType` `webm` instead of `matroska`.
    webm: bool,
    /// Declare no `DefaultDuration`, as a variable-rate file does — leaving the block timestamps as
    /// the only record of how fast the recording actually ran.
    no_default_duration: bool,
}

/// A minimal Matroska describing `frames` frames of `width`x`height` in `codec` at `fps`.
fn build_mkv(frames: u32, width: u16, height: u16, codec: &str, fps: u32, shape: Mkv) -> Vec<u8> {
    let doc_type = if shape.webm { "webm" } else { "matroska" };
    let mut ebml = Vec::new();
    ebml.extend(uint_el(&[0x42, 0x86], 1)); // EBMLVersion
    ebml.extend(el(&[0x42, 0x82], doc_type.as_bytes())); // DocType
    let header = el(&[0x1A, 0x45, 0xDF, 0xA3], &ebml);

    let mut video = Vec::new();
    video.extend(uint_el(&[0xB0], width as u64)); // PixelWidth
    video.extend(uint_el(&[0xBA], height as u64)); // PixelHeight
    let mut entry = Vec::new();
    entry.extend(uint_el(&[0xD7], 1)); // TrackNumber
    entry.extend(uint_el(&[0x83], 1)); // TrackType: video
    entry.extend(el(&[0x86], codec.as_bytes())); // CodecID
    if !shape.no_default_duration {
        // DefaultDuration: what the file *declares* a frame lasts, in nanoseconds.
        entry.extend(uint_el(
            &[0x23, 0xE3, 0x83],
            1_000_000_000 / fps.max(1) as u64,
        ));
    }
    entry.extend(el(&[0xE0], &video));
    let tracks = el(&[0x16, 0x54, 0xAE, 0x6B], &el(&[0xAE], &entry));

    // One cluster holding every frame, as `SimpleBlock`s on track 1. The payload is a single byte:
    // the probe must never look at it, and a fixture that carried real pixels could not prove that.
    let per_block = shape.lace.map_or(1, |n| n as u32);
    let mut cluster = uint_el(&[0xE7], 0); // Timestamp
    let mut written = 0u32;
    while written < frames {
        let mut block = vec![0x81]; // track number 1, as a one-byte vint
                                    // The block's own offset from the cluster, in the segment's ticks — one millisecond each by
                                    // default, so a frame at `fps` sits `1000 / fps` ticks after the one before it.
        let tick = (written as i64 * 1000 / fps.max(1) as i64) as i16;
        block.extend_from_slice(&tick.to_be_bytes());
        match shape.lace {
            // Flags with fixed lacing set, then the lace count minus one, then one byte per frame.
            Some(n) => {
                block.push(0x84);
                block.push(n - 1);
                block.extend(std::iter::repeat(0u8).take(n as usize));
            }
            None => {
                block.push(0x80);
                block.push(0);
            }
        }
        cluster.extend(el(&[0xA3], &block));
        written += per_block;
    }
    let cluster = if shape.unknown_cluster_size {
        let mut out = vec![0x1F, 0x43, 0xB6, 0x75, 0xFF];
        out.extend_from_slice(&cluster);
        out
    } else {
        el(&[0x1F, 0x43, 0xB6, 0x75], &cluster)
    };

    let mut segment = tracks;
    segment.extend(cluster);
    let mut out = header;
    out.extend(el(&[0x18, 0x53, 0x80, 0x67], &segment));
    out
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
    /// The container the videos are written in. The manifest says the same thing either way: a
    /// LeRobot manifest names a codec and a rate, never a container.
    container: Container,
}

/// Which container a variant's videos are written in.
#[derive(Clone, Copy, Default)]
enum Container {
    #[default]
    Mp4,
    Matroska(Mkv),
}

impl Container {
    fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Matroska(_) => "mkv",
        }
    }
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
        let extension = plan.container.extension();
        let name = if plan.aggregated {
            format!("file-{episode:03}.{extension}")
        } else {
            format!("episode_{episode:06}.{extension}")
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
        let bytes = match plan.container {
            Container::Mp4 => build_mp4(frames, width, height, &codec, FPS as u32),
            Container::Matroska(shape) => build_mkv(
                frames,
                width,
                height,
                &matroska_codec_id(&codec),
                FPS as u32,
                shape,
            ),
        };
        fs::write(&path, bytes).unwrap();
    }
}

/// The Matroska `CodecID` for the encoding an MP4 names by this fourcc — the same encoder, spelled
/// the way each container spells it.
fn matroska_codec_id(fourcc: &[u8; 4]) -> String {
    match fourcc {
        b"avc1" => "V_MPEG4/ISO/AVC".to_string(),
        b"hvc1" => "V_MPEGH/ISO/HEVC".to_string(),
        b"av01" => "V_AV1".to_string(),
        b"vp09" => "V_VP9".to_string(),
        other => String::from_utf8_lossy(other).to_string(),
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
    // Nothing is *accused*: no error, no warning. What the family does say is that it could not
    // measure this stream's frame count, which is a different statement from silence.
    assert!(
        findings
            .iter()
            .all(|f| f.severity == Severity::Info && f.code == "VIDEO.FRAME_COUNT_UNMEASURED"),
        "{findings:#?}"
    );
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

// --- The container sweep -------------------------------------------------------------------------

#[test]
fn no_damaged_container_takes_the_probe_down() {
    // `probe_mp4` walks a tree of boxes whose every size comes out of the file: a 32-bit size, the
    // 64-bit extension a size of 1 introduces, and the size of 0 that means "I run to the end". It is
    // careful today — each length is validated against the bytes actually remaining, so the walk
    // always advances by at least a header and can neither stall nor read past the end — and that is
    // exactly the property worth holding as the code changes. It was the last untrusted binary
    // parser in the workspace with no sweep.
    //
    // Every shape the builder can produce, because each reaches different box handlers: a fragmented
    // file has a `moof` and an empty `stsz`, a compact one a `stz2`, and the leading bare `trak`
    // exercises the walk that has to skip a track carrying no media.
    let shapes: Vec<(&str, Vec<u8>)> = vec![
        ("progressive", build_mp4(30, 640, 480, b"avc1", 30)),
        (
            "fragmented",
            build_mp4_shaped(
                30,
                640,
                480,
                b"avc1",
                30,
                Shape {
                    fragmented: true,
                    ..Shape::default()
                },
            ),
        ),
        (
            "compact-sample-table",
            build_mp4_shaped(
                30,
                640,
                480,
                b"avc1",
                30,
                Shape {
                    compact_sample_table: true,
                    ..Shape::default()
                },
            ),
        ),
        (
            "leading-bare-trak",
            build_mp4_shaped(
                30,
                640,
                480,
                b"avc1",
                30,
                Shape {
                    leading_bare_trak: true,
                    ..Shape::default()
                },
            ),
        ),
    ];

    let tmp = tempfile::tempdir().expect("temp dir");
    let path = tmp.path().join("sweep.mp4");
    for (shape, clean) in &shapes {
        let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
        // Strided single-byte damage across the whole container. The stride keeps the corpus small
        // while still landing inside every box header, which is where the sizes live.
        for at in (0..clean.len()).step_by(5) {
            for (tag, byte) in [("zero", 0x00u8), ("max", 0xFF), ("flip", clean[at] ^ 0xFF)] {
                let mut m = clean.clone();
                m[at] = byte;
                cases.push((format!("{tag}@{at}"), m));
            }
        }
        // And the sizes that mean something special, written where the first box's size lives: 0
        // ("to the end of the file"), 1 (a 64-bit size follows), and a 64-bit size of u64::MAX.
        for (tag, size) in [("size-0", 0u32), ("size-1", 1), ("size-7", 7)] {
            let mut m = clean.clone();
            m[..4].copy_from_slice(&size.to_be_bytes());
            cases.push((tag.to_string(), m));
        }
        let mut huge = clean.clone();
        huge[..4].copy_from_slice(&1u32.to_be_bytes());
        huge.splice(8..8, u64::MAX.to_be_bytes());
        cases.push(("size-1-then-u64-max".into(), huge));
        cases.push(("truncated-header".into(), clean[..4].to_vec()));
        cases.push(("empty".into(), Vec::new()));

        for (what, bytes) in cases {
            std::fs::write(&path, &bytes).unwrap();
            // The invariant: a verdict either way, never a panic, a hang, or an unbounded read.
            // `probe_mp4` returning `Err` is a perfectly good outcome — most of these are not
            // containers any more.
            let _ = std::panic::catch_unwind(|| veridex_core::media::probe_mp4(&path))
                .unwrap_or_else(|_| panic!("`{shape}` / `{what}` panicked the probe"));
        }
    }
}

/// The camera stream's media in episode 0 — the one the manifest declares as video.
fn camera_media(dataset: &Dataset) -> &veridex_core::cdm::Media {
    dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == FEATURE)
        .and_then(|s| s.media.as_ref())
        .expect("the camera stream carries its media file")
}

// ---- Matroska ------------------------------------------------------------------------------------

/// The `.mkv` a dataset ships is *its* video, not a missing one.
///
/// Before the EBML walk existed, a container Veridex could not read was not even looked for: the
/// video index only collected ISO base media extensions, so every episode of an honest dataset
/// reported `VIDEO.MEDIA_ABSENT` — "no video files present at all", remedy `git lfs pull` — about
/// files sitting on the caller's disk. A wrong diagnosis of a real dataset is worse than an
/// abstention, because it sends someone to fix what is not broken.
#[test]
fn a_matroska_dataset_is_read_rather_than_reported_as_having_no_video() {
    let (dataset, findings) = run(
        VideoPlan {
            container: Container::Matroska(Mkv::default()),
            ..VideoPlan::default()
        },
        10,
    );
    assert!(findings.is_empty(), "{findings:#?}");
    let media = camera_media(&dataset);
    assert_eq!(media.status, MediaStatus::Read);
    // Every one of the four facts the video checks need, measured from the container rather than
    // copied from the manifest that is being checked against it.
    assert_eq!(media.frame_count, Some(10));
    assert_eq!(media.observed.width, Some(640));
    assert_eq!(media.observed.height, Some(480));
    // A Matroska states a frame *duration* in whole nanoseconds, so 30 fps is stored as 33333333 ns
    // and reads back a hair over 30 — the value the file actually holds, not a rounded one.
    assert!(
        (media.observed.fps.unwrap() - 30.0).abs() < 1e-5,
        "{:?}",
        media.observed.fps
    );
    assert_eq!(media.observed.codec.as_deref(), Some("V_MPEG4/ISO/AVC"));
}

/// The count comes from the blocks, so a short Matroska is caught the same way a short MP4 is —
/// which is the whole point of reading the container rather than trusting the manifest.
#[test]
fn a_matroska_shorter_than_its_episode_is_caught() {
    let (_, findings) = run(
        VideoPlan {
            container: Container::Matroska(Mkv::default()),
            frames_override: Some(7),
            episode: 1,
            ..VideoPlan::default()
        },
        10,
    );
    assert_eq!(findings.len(), 1, "{findings:#?}");
    assert_eq!(findings[0].code, "VIDEO.FRAME_COUNT_MISMATCH");
    assert!(findings[0].message.contains("episode 1"), "{findings:#?}");
}

/// WebM is Matroska with a different `DocType`, and a dataset written in it is not a different
/// dataset.
#[test]
fn a_webm_dataset_reads_the_same_as_a_matroska_one() {
    let (_, findings) = run(
        VideoPlan {
            container: Container::Matroska(Mkv {
                webm: true,
                ..Mkv::default()
            }),
            ..VideoPlan::default()
        },
        10,
    );
    assert!(findings.is_empty(), "{findings:#?}");
}

/// A laced block carries several frames under one block header. Counting blocks instead of frames
/// would report every such file as holding a fraction of its own footage — a frame-count mismatch
/// on a perfectly good dataset, which is the same false alarm in a different costume.
#[test]
fn a_laced_block_counts_the_frames_it_laces_not_one() {
    let (dataset, findings) = run(
        VideoPlan {
            container: Container::Matroska(Mkv {
                lace: Some(5),
                ..Mkv::default()
            }),
            ..VideoPlan::default()
        },
        10,
    );
    assert!(findings.is_empty(), "{findings:#?}");
    // Ten frames in two blocks of five, not two.
    let media = camera_media(&dataset);
    assert_eq!(media.frame_count, Some(10));
}

/// A live muxer writes clusters of unknown size, and their end cannot be found without guessing.
/// The count is then *absent*, never zero: a zero would be reported as a frame-count mismatch
/// against every episode, which is a claim about the recording made out of a limit of the reader.
#[test]
fn a_cluster_of_unknown_size_yields_no_count_rather_than_a_wrong_one() {
    let (dataset, findings) = run(
        VideoPlan {
            container: Container::Matroska(Mkv {
                unknown_cluster_size: true,
                ..Mkv::default()
            }),
            ..VideoPlan::default()
        },
        10,
    );
    // The count is absent, so the family discloses that and accuses the file of nothing.
    assert!(
        findings
            .iter()
            .all(|f| f.severity == Severity::Info && f.code == "VIDEO.FRAME_COUNT_UNMEASURED"),
        "{findings:#?}"
    );
    let media = camera_media(&dataset);
    assert_eq!(media.status, MediaStatus::Read);
    assert_eq!(media.frame_count, None);
    // What the Tracks element stated is still reported: an abstention on the count is not an
    // abstention on the file.
    assert_eq!(media.observed.width, Some(640));
    assert!(media.observed.fps.is_some());
}

/// The container is decided by the file's bytes, not its name. A pipeline that muxed Matroska into
/// a `.mp4` — `ffmpeg` does exactly this when told `-f matroska` with an `.mp4` output — ships a
/// dataset whose videos are readable, and calling them corrupt would be a claim about the file made
/// out of its file name.
#[test]
fn a_matroska_named_mp4_is_read_as_what_its_bytes_say_it_is() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    for episode in 0..2u64 {
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mkv(10, 640, 480, "V_MPEG4/ISO/AVC", FPS as u32, Mkv::default()),
        )
        .unwrap();
    }
    let dataset = ingest(dir.path());
    assert!(video_findings(&dataset).is_empty());
    assert_eq!(camera_media(&dataset).frame_count, Some(10));
}

/// Every prefix of a Matroska is a file some interrupted transfer left behind, and every one of
/// them must produce a verdict or a reason — never a panic, and never a frame count assembled out
/// of whatever the truncation happened to leave.
#[test]
fn a_truncated_matroska_is_refused_rather_than_read_past_its_end() {
    let whole = build_mkv(10, 640, 480, "V_AV1", FPS as u32, Mkv::default());
    let dir = tempfile::tempdir().unwrap();
    for cut in 1..whole.len() {
        let path = dir.path().join("cut.mkv");
        fs::write(&path, &whole[..cut]).unwrap();
        match veridex_core::media::probe(&path) {
            // A prefix that happens to hold the whole Tracks element is legitimately readable; what
            // it must never do is report more frames than the bytes it kept can hold.
            Ok(probe) => assert!(
                probe.frame_count.unwrap_or(0) <= 10,
                "cut at {cut} reported {:?} frames",
                probe.frame_count
            ),
            Err(reason) => assert!(!reason.is_empty(), "cut at {cut}"),
        }
    }
}

/// A byte flipped anywhere in a container is the shape a corrupted download takes, and the walk is
/// pointed at attacker-controlled files: the only two acceptable answers are a probe and a reason.
#[test]
fn a_bit_flipped_matroska_never_panics() {
    let whole = build_mkv(6, 320, 240, "V_VP9", FPS as u32, Mkv::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flip.mkv");
    for byte in 0..whole.len() {
        for bit in [0x01u8, 0x40, 0x80] {
            let mut bytes = whole.clone();
            bytes[byte] ^= bit;
            fs::write(&path, &bytes).unwrap();
            let _ = veridex_core::media::probe(&path);
        }
    }
}

/// A Matroska need not declare a frame rate at all: `DefaultDuration` is optional, and a
/// variable-rate file carries none. Reading only that field reported no rate for those files, so
/// `video.media-conformance` compared nothing — and a container running at half the rate its
/// manifest declares passed in silence, which is the shape of a video/data desync that worsens
/// through every episode.
///
/// The rate is measured from the block timestamps instead, which is the same quantity the MP4 path
/// reports: frames over the media time they span.
#[test]
fn a_matroska_that_declares_no_rate_has_one_measured_from_its_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vfr.mkv");
    let vfr = Mkv {
        no_default_duration: true,
        ..Mkv::default()
    };
    fs::write(
        &path,
        build_mkv(31, 640, 480, "V_MPEG4/ISO/AVC", FPS as u32, vfr),
    )
    .unwrap();
    let probe = veridex_core::media::probe(&path).expect("a readable container");
    // Thirty-one frames one millisecond-tick group apart: thirty intervals over one second.
    let fps = probe.params.fps.expect("a rate measured from the blocks");
    assert!((fps - 30.0).abs() < 0.5, "{fps}");
}

/// And the rate reaches the check: a stream whose manifest declares 30 fps against a container that
/// really ran at 15 is a finding, not a silence.
#[test]
fn a_declared_rate_a_rateless_matroska_does_not_match_is_still_caught() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    let vfr = Mkv {
        no_default_duration: true,
        ..Mkv::default()
    };
    for episode in 0..2u64 {
        // Ten frames at 15 fps, against a manifest that says 30.
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mkv(10, 640, 480, "V_MPEG4/ISO/AVC", 15, vfr),
        )
        .unwrap();
    }
    let dataset = ingest(dir.path());
    let findings = video_findings(&dataset);
    assert!(
        findings.iter().any(|f| f.code == "VIDEO.FPS_MISMATCH"),
        "{findings:#?}"
    );
}

/// A single frame spans no time, so nothing about a rate can be measured from it. Reporting one
/// anyway — an infinity, or a division by zero — would be a number invented out of one timestamp.
#[test]
fn a_single_frame_matroska_measures_no_rate_rather_than_dividing_by_zero() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.mkv");
    let vfr = Mkv {
        no_default_duration: true,
        ..Mkv::default()
    };
    fs::write(
        &path,
        build_mkv(1, 640, 480, "V_MPEG4/ISO/AVC", FPS as u32, vfr),
    )
    .unwrap();
    let probe = veridex_core::media::probe(&path).expect("a readable container");
    assert_eq!(probe.params.fps, None);
    assert_eq!(probe.frame_count, Some(1));
}

/// A container can be perfectly readable and still not say how many frames it holds.
///
/// A **fragmented** MP4 keeps its samples in `moof` fragments and leaves the sample table in `moov`
/// empty — what `ffmpeg -movflags frag_keyframe+empty_moov`, DASH/CMAF and most hardware recorders
/// write. The frame-count comparison, which is what the video family exists for, then never runs;
/// and until it was disclosed, a stream the family never compared read exactly like one it compared
/// and found sound.
#[test]
fn a_stream_whose_containers_state_no_frame_count_says_so() {
    let fragmented = Shape {
        fragmented: true,
        ..Shape::default()
    };
    let findings = run_shaped(fragmented);
    let abstention = findings
        .iter()
        .find(|f| f.code == "VIDEO.FRAME_COUNT_UNMEASURED")
        .unwrap_or_else(|| {
            panic!("the family must disclose what it could not measure: {findings:#?}")
        });
    assert_eq!(abstention.severity, Severity::Info);
    assert!(
        abstention.message.contains(FEATURE),
        "{}",
        abstention.message
    );
    // It says what it is about, not merely that something was skipped.
    assert!(
        abstention.message.contains("how many frames"),
        "{}",
        abstention.message
    );
}

/// And the direction that keeps it honest: a stream that *was* measured says nothing. An abstention
/// that fires beside a real measurement is noise on every sound dataset.
#[test]
fn a_measured_stream_raises_no_abstention() {
    let (_, findings) = run(VideoPlan::default(), 10);
    assert!(
        !findings
            .iter()
            .any(|f| f.code == "VIDEO.FRAME_COUNT_UNMEASURED"),
        "{findings:#?}"
    );
}

/// The boundary the frame-count abstention promises: it fires only when *no* episode of a stream
/// could be measured, so **one** measured episode must silence it. Where some episodes could be
/// measured and others could not, the mismatch rollup already speaks for the ones that were.
#[test]
fn one_measured_episode_is_enough_to_silence_the_abstention() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    // Episode 0 keeps its ordinary container; episode 1 is fragmented and states no count.
    fs::write(
        dest.join("episode_000001.mp4"),
        build_mp4_shaped(
            10,
            640,
            480,
            b"avc1",
            FPS as u32,
            Shape {
                fragmented: true,
                ..Shape::default()
            },
        ),
    )
    .unwrap();
    let dataset = ingest(dir.path());
    let findings = video_findings(&dataset);
    assert!(
        !findings
            .iter()
            .any(|f| f.code == "VIDEO.FRAME_COUNT_UNMEASURED"),
        "one measured episode is a measurement: {findings:#?}"
    );
}

/// A live-muxed Matroska is the same case reached through the other reader: its clusters declare no
/// size, so the walk counts no blocks and states no frame count.
#[test]
fn a_matroska_with_no_countable_frames_raises_the_same_abstention() {
    let dir = tempfile::tempdir().unwrap();
    write_dataset(dir.path(), 10, VideoPlan::default());
    let dest = dir.path().join("videos").join(FEATURE);
    let live = Mkv {
        unknown_cluster_size: true,
        ..Mkv::default()
    };
    for episode in 0..2u64 {
        fs::write(
            dest.join(format!("episode_{episode:06}.mp4")),
            build_mkv(10, 640, 480, "V_MPEG4/ISO/AVC", FPS as u32, live),
        )
        .unwrap();
    }
    let dataset = ingest(dir.path());
    let findings = video_findings(&dataset);
    assert!(
        findings
            .iter()
            .any(|f| f.code == "VIDEO.FRAME_COUNT_UNMEASURED"),
        "{findings:#?}"
    );
}
