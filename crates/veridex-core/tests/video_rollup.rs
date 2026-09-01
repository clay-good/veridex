//! When the video length defect is charged once for the whole export rather than per episode.
//!
//! The roll-up says "every episode's video is N frames shorter than its rows". It may only say that
//! when every episode's video was actually measured — an episode whose file is missing, unreadable,
//! or whose container declares no sample count contributed no length at all, and a stream where one
//! episode was measured and the rest were not has no pattern to be systematic about.

use veridex_core::cdm::{
    ClockKind, Dataset, Episode, Frame, Media, MediaParams, MediaStatus, Modality, Stream, ValueRef,
};
use veridex_core::check::{Check, Finding};
use veridex_core::checks::video::MediaConformance;

const STREAM: &str = "observation.images.top";

fn frames(n: usize) -> Vec<Frame> {
    (0..n)
        .map(|i| Frame {
            ts: i as i64 * 33_333_333,
            value_ref: ValueRef {
                uri: "x".into(),
                byte_offset: None,
                byte_len: None,
                content_hash: None,
            },
        })
        .collect()
}

fn video_stream(rows: usize, media: Media) -> Stream {
    Stream {
        name: STREAM.into(),
        modality: Modality::Video,
        declared_rate_hz: Some(30.0),
        clock_id: "cam".into(),
        clock_kind: ClockKind::Measured,
        dtype: Some("video".into()),
        shape: None,
        dim_names: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        media: Some(media),
        frame_id: None,
        frames: frames(rows),
    }
}

fn read(uri: &str, container_frames: u64) -> Media {
    Media {
        uri: uri.into(),
        declared: MediaParams::default(),
        status: MediaStatus::Read,
        observed: MediaParams::default(),
        frame_count: Some(container_frames),
    }
}

fn missing(uri: &str) -> Media {
    Media {
        uri: uri.into(),
        declared: MediaParams::default(),
        status: MediaStatus::Missing,
        observed: MediaParams::default(),
        frame_count: None,
    }
}

/// A `Read` container that declares no sample count — a fragmented MP4 with an empty `stsz`, which
/// the probe reports as readable with `frame_count: None`.
fn read_without_count(uri: &str) -> Media {
    Media {
        uri: uri.into(),
        declared: MediaParams::default(),
        status: MediaStatus::Read,
        observed: MediaParams::default(),
        frame_count: None,
    }
}

fn episode(index: u64, stream: Stream) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams: vec![stream],
        task: None,
        labels: vec![],
        ego_poses: None,
        ego_frame: None,
        declared_frame_count: None,
    }
}

fn dataset(episodes: Vec<Episode>) -> Dataset {
    Dataset {
        id: "rollup".into(),
        metadata: vec![],
        provenance: vec![],
        episodes,
        calibration: None,
    }
}

fn run(dataset: &Dataset) -> Vec<Finding> {
    MediaConformance {
        fps_tolerance: 0.05,
    }
    .run(dataset)
}

fn length_findings(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|f| f.code == "VIDEO.FRAME_COUNT_MISMATCH")
        .collect()
}

/// One episode measured short, one episode with no file at all. The measured episode is the only
/// evidence there is, so it must be reported as itself — naming the file and both counts — not
/// rolled up into a claim about "every episode".
#[test]
fn one_measured_episode_beside_a_missing_file_is_not_a_systematic_defect() {
    let ds = dataset(vec![
        episode(0, video_stream(10, read("videos/ep_0.mp4", 9))),
        episode(1, video_stream(10, missing("videos/ep_1.mp4"))),
    ]);

    let findings = run(&ds);
    let lengths = length_findings(&findings);
    assert_eq!(lengths.len(), 1, "one measured episode, one length finding");
    let message = &lengths[0].message;
    assert!(
        !message.contains("every episode"),
        "only episode 0 had a container to measure, so the report must not claim every episode's \
         video is short: {message}"
    );
    assert!(
        message.contains("videos/ep_0.mp4") && message.contains('9') && message.contains("10"),
        "the per-episode report names the file and both counts: {message}"
    );
}

/// Same shape, but the unmeasured episode has a readable container that declares no sample count.
/// It is equally not evidence of a pattern.
#[test]
fn one_measured_episode_beside_a_countless_container_is_not_a_systematic_defect() {
    let ds = dataset(vec![
        episode(0, video_stream(10, read("videos/ep_0.mp4", 9))),
        episode(1, video_stream(10, read_without_count("videos/ep_1.mp4"))),
    ]);

    let findings = run(&ds);
    let lengths = length_findings(&findings);
    assert_eq!(lengths.len(), 1);
    assert!(
        !lengths[0].message.contains("every episode"),
        "one measurable episode is not a pattern: {}",
        lengths[0].message
    );
}

/// The roll-up still applies when two episodes were both measured and both missed by the same
/// amount — that is the systematic export defect it exists to name.
#[test]
fn two_measured_episodes_off_by_the_same_amount_still_roll_up() {
    let ds = dataset(vec![
        episode(0, video_stream(10, read("videos/ep_0.mp4", 9))),
        episode(1, video_stream(20, read("videos/ep_1.mp4", 19))),
    ]);

    let findings = run(&ds);
    let lengths = length_findings(&findings);
    assert_eq!(lengths.len(), 1, "charged once for the export");
    assert!(
        lengths[0].message.contains("every episode"),
        "two measured episodes off by the same amount is the systematic defect: {}",
        lengths[0].message
    );
}

/// A third measured episode that is *fine* means the stream is not systematically off, even though
/// the two broken ones agree with each other.
#[test]
fn a_clean_measured_episode_defeats_the_roll_up() {
    let ds = dataset(vec![
        episode(0, video_stream(10, read("videos/ep_0.mp4", 9))),
        episode(1, video_stream(10, read("videos/ep_1.mp4", 9))),
        episode(2, video_stream(10, read("videos/ep_2.mp4", 10))),
    ]);

    let findings = run(&ds);
    let lengths = length_findings(&findings);
    assert_eq!(lengths.len(), 2, "each broken episode named separately");
    for f in lengths {
        assert!(!f.message.contains("every episode"), "{}", f.message);
    }
}

/// The roll-up must not switch off the rest of the check.
///
/// Once a stream's frame-count delta was recognized as systematic, the roll-up was recorded and the
/// loop `continue`d — which skipped the remainder of the per-stream body, where the resolution,
/// codec, and fps comparisons live. So a stream whose videos were *both* systematically off by a
/// constant and re-encoded at the wrong resolution reported only the length defect, and the
/// re-encode was never looked for.
///
/// That is the realistic pairing rather than a contrived one: an off-by-one converter and a
/// re-encode come out of the same bad export pass.
#[test]
fn a_systematic_length_delta_does_not_hide_a_re_encode() {
    let mut media_a = read("ep0.mp4", 99);
    let mut media_b = read("ep1.mp4", 99);
    let declared = MediaParams {
        codec: Some("h264".into()),
        width: Some(640),
        height: Some(480),
        fps: Some(30.0),
    };
    // What the container actually holds: a different codec, size, and rate.
    let observed = MediaParams {
        codec: Some("av01".into()),
        width: Some(320),
        height: Some(240),
        fps: Some(60.0),
    };
    media_a.declared = declared.clone();
    media_a.observed = observed.clone();
    media_b.declared = declared;
    media_b.observed = observed;

    // Both episodes are exactly one frame short: a systematic delta.
    let d = dataset(vec![
        episode(0, video_stream(100, media_a)),
        episode(1, video_stream(100, media_b)),
    ]);

    let f = run(&d);
    let codes: Vec<&str> = f.iter().map(|f| f.code.as_str()).collect();
    assert_eq!(
        length_findings(&f).len(),
        1,
        "the length defect is still charged once for the export: {codes:?}"
    );
    for expected in [
        "VIDEO.CODEC_MISMATCH",
        "VIDEO.RESOLUTION_MISMATCH",
        "VIDEO.FPS_MISMATCH",
    ] {
        assert!(
            codes.contains(&expected),
            "a systematic length delta must not suppress {expected}: {codes:?}"
        );
    }
}
