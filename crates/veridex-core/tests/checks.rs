//! Behavior tests for the MVP checks catalog.

use veridex_core::cdm::{
    ClockKind, Dataset, Episode, Frame, Label, Modality, Provenance, ProvenanceClass,
    ProvenanceElement, ProvenanceScope, Stream, ValueRef,
};
use veridex_core::check::{Check, Severity};
use veridex_core::checks::{autonomy, provenance, semantic, statistical, structural, temporal};

fn vref() -> ValueRef {
    ValueRef {
        uri: "x".into(),
        byte_offset: None,
        byte_len: None,
        content_hash: None,
    }
}

fn frames_at(ts: &[i64]) -> Vec<Frame> {
    ts.iter()
        .map(|t| Frame {
            ts: *t,
            value_ref: vref(),
        })
        .collect()
}

fn stream(name: &str, clock: &str, rate: Option<f64>, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: rate,
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: None,
        frames: frames_at(ts),
    }
}

fn episode(index: u64, streams: Vec<Stream>) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams,
        task: None,
        labels: vec![],
        ego_poses: None,
        declared_frame_count: None,
    }
}

fn dataset(episodes: Vec<Episode>) -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes,
    }
}

// ---- structural ----

#[test]
fn duplicate_episode_index_is_a_boundary_error() {
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    let f = structural::EpisodeBoundary.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EPISODE_BOUNDARY");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(!f[0].risk.is_empty(), "check must document a risk");
    assert!(!f[0].remedy.is_empty(), "check must document a remedy");
}

#[test]
fn inverted_episode_bounds_are_flagged() {
    let mut ep = episode(0, vec![stream("s", "c", None, &[0, 1])]);
    ep.start_ts = Some(100);
    ep.end_ts = Some(50);
    let f = structural::EpisodeBoundary.run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("inverted"));
}

#[test]
fn declared_episode_length_mismatch_is_a_boundary_error() {
    // The lerobot#4143 class: the manifest declares 7 frames for the episode but 10 were ingested.
    let mut ep = episode(
        1,
        vec![stream("s", "c", None, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9])],
    );
    ep.declared_frame_count = Some(7);
    let f = structural::EpisodeBoundary.run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EPISODE_BOUNDARY");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("declares 7 frames but 10"));
    assert!(!f[0].risk.is_empty(), "check must document a risk");
    assert!(!f[0].remedy.is_empty(), "check must document a remedy");
}

#[test]
fn declared_episode_length_matching_or_absent_is_clean() {
    // Declared length equals the frames ingested → no boundary finding.
    let mut ep = episode(0, vec![stream("s", "c", None, &[0, 1, 2])]);
    ep.declared_frame_count = Some(3);
    assert!(structural::EpisodeBoundary
        .run(&dataset(vec![ep]))
        .is_empty());
    // No declared length at all → the comparison is skipped.
    let plain = episode(0, vec![stream("s", "c", None, &[0, 1, 2])]);
    assert!(structural::EpisodeBoundary
        .run(&dataset(vec![plain]))
        .is_empty());
}

#[test]
fn clean_episodes_produce_no_structural_findings() {
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1, 2])]),
        episode(1, vec![stream("s", "c", None, &[0, 1, 2])]),
    ]);
    assert!(structural::EpisodeBoundary.run(&d).is_empty());
    assert!(structural::DegenerateEpisode.run(&d).is_empty());
}

#[test]
fn empty_and_single_frame_streams_are_degenerate() {
    let d = dataset(vec![episode(
        0,
        vec![
            stream("empty", "c", None, &[]),
            stream("single", "c", None, &[5]),
        ],
    )]);
    let f = structural::DegenerateEpisode.run(&d);
    let codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    assert!(codes.contains(&"STRUCTURAL.EMPTY_STREAM"));
    assert!(codes.contains(&"STRUCTURAL.SINGLE_FRAME_STREAM"));
    let empty = f
        .iter()
        .find(|x| x.code == "STRUCTURAL.EMPTY_STREAM")
        .unwrap();
    assert_eq!(empty.severity, Severity::Error);
    let single = f
        .iter()
        .find(|x| x.code == "STRUCTURAL.SINGLE_FRAME_STREAM")
        .unwrap();
    assert_eq!(single.severity, Severity::Warning);
}

#[test]
fn episode_with_no_streams_is_empty() {
    let f = structural::DegenerateEpisode.run(&dataset(vec![episode(0, vec![])]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EMPTY_EPISODE");
}

#[test]
fn dataset_with_no_episodes_is_flagged() {
    let f = structural::DegenerateEpisode.run(&dataset(vec![]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EMPTY_DATASET");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn declared_episode_count_mismatch_is_flagged() {
    // Manifest says 3 episodes; only 2 were ingested → truncated export.
    let mut d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    d.metadata
        .push((veridex_core::cdm::META_DECLARED_EPISODES.into(), "3".into()));
    let f = structural::DeclaredEpisodeCount.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EPISODE_COUNT_MISMATCH");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn declared_episode_count_matching_or_absent_is_clean() {
    let base = vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
    ];
    // Matching declared count → no finding.
    let mut matches = dataset(base.clone());
    matches
        .metadata
        .push((veridex_core::cdm::META_DECLARED_EPISODES.into(), "2".into()));
    assert!(structural::DeclaredEpisodeCount.run(&matches).is_empty());
    // No declared count at all → check is skipped.
    assert!(structural::DeclaredEpisodeCount
        .run(&dataset(base))
        .is_empty());
}

#[test]
fn declared_frame_count_mismatch_is_flagged() {
    // Manifest says 5 frames; episodes hold 2 + 2 = 4 → truncated (episodes present but short).
    let mut d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    d.metadata
        .push((veridex_core::cdm::META_DECLARED_FRAMES.into(), "5".into()));
    let f = structural::DeclaredFrameCount.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.FRAME_COUNT_MISMATCH");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn declared_frame_count_matching_or_absent_is_clean() {
    // 2 + 2 = 4 actual frames; the longest stream per episode defines its length.
    let mut d = dataset(vec![
        episode(
            0,
            vec![
                stream("a", "c", None, &[0, 1]),
                stream("b", "c", None, &[0, 1]),
            ],
        ),
        episode(1, vec![stream("a", "c", None, &[0, 1])]),
    ]);
    d.metadata
        .push((veridex_core::cdm::META_DECLARED_FRAMES.into(), "4".into()));
    assert!(structural::DeclaredFrameCount.run(&d).is_empty());
    // No declared frame count → skipped.
    let plain = dataset(vec![episode(0, vec![stream("s", "c", None, &[0, 1])])]);
    assert!(structural::DeclaredFrameCount.run(&plain).is_empty());
}

#[test]
fn missing_episode_index_is_a_continuity_gap() {
    // Episodes 0, 1, 3 → episode 2 was dropped.
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
        episode(3, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    let f = structural::EpisodeContinuity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EPISODE_INDEX_GAP");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains('2'));
}

#[test]
fn contiguous_episode_indices_have_no_gap() {
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
        episode(2, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    assert!(structural::EpisodeContinuity.run(&d).is_empty());
    // A single episode (or none) can't have a gap.
    let one = dataset(vec![episode(7, vec![stream("s", "c", None, &[0, 1])])]);
    assert!(structural::EpisodeContinuity.run(&one).is_empty());
}

/// A stream whose frames carry content hashes: frame `i` gets `content_hash = [contents[i]; 32]`, so
/// two streams with equal `contents` have provably-identical content and differing `contents` do not.
/// A duplicate claim is only sound when frame content is known, so the duplicate tests use this.
fn stream_hashed(name: &str, clock: &str, ts: &[i64], contents: &[u8]) -> Stream {
    assert_eq!(ts.len(), contents.len());
    let frames = ts
        .iter()
        .zip(contents)
        .map(|(t, c)| Frame {
            ts: *t,
            value_ref: ValueRef {
                uri: "x".into(),
                byte_offset: None,
                byte_len: None,
                content_hash: Some([*c; 32]),
            },
        })
        .collect();
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: Some(10.0),
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: None,
        frames,
    }
}

#[test]
fn exact_duplicate_episodes_are_grouped() {
    // Episodes 0, 1, 2 where 0 and 2 have identical frame content (a re-upload). The check groups
    // them and reports both indices; the distinct episode 1 (same timing, different content) is not.
    let dup = || vec![stream_hashed("cam", "wall", &[0, 100, 200], &[1, 2, 3])];
    let distinct = vec![stream_hashed("cam", "wall", &[0, 100, 200], &[9, 9, 9])];
    let d = dataset(vec![
        episode(0, dup()),
        episode(1, distinct),
        episode(2, dup()),
    ]);
    let f = structural::DuplicateEpisode.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.DUPLICATE_EPISODE");
    assert_eq!(f[0].severity, Severity::Warning);
    // Both duplicate indices are named; the distinct episode is not.
    assert!(f[0].message.contains('0') && f[0].message.contains('2'));
    assert!(!f[0].message.contains('1'));
}

#[test]
fn same_timing_but_different_content_is_not_a_duplicate() {
    // The false-positive guard: two episodes with identical timestamps, schema, and stored stats but
    // DIFFERENT frame content are NOT duplicates. Without this, every same-length LeRobot episode
    // (shared relative time base + dataset-global stats) would be mis-flagged.
    let stats = Some(veridex_core::cdm::StreamStats {
        min: 0.0,
        max: 1.0,
        mean: 0.5,
        std: 0.1,
    });
    let mut a = stream_hashed("cam", "wall", &[0, 100, 200], &[1, 2, 3]);
    a.stats = stats;
    let mut b = stream_hashed("cam", "wall", &[0, 100, 200], &[4, 5, 6]); // same timing, other content
    b.stats = stats;
    let d = dataset(vec![episode(0, vec![a]), episode(1, vec![b])]);
    assert!(structural::DuplicateEpisode.run(&d).is_empty());
}

#[test]
fn episodes_without_content_hashes_are_never_flagged_as_duplicates() {
    // The `stream` helper builds hashless frames (content_hash: None). Two structurally-identical
    // such episodes must NOT be flagged, because duplication can't be proven without frame content —
    // this is exactly the shape-only coincidence that would false-positive on real datasets.
    let ep_streams = || vec![stream("cam", "wall", Some(10.0), &[0, 100, 200])];
    let d = dataset(vec![episode(0, ep_streams()), episode(1, ep_streams())]);
    assert!(structural::DuplicateEpisode.run(&d).is_empty());
}

/// A stream with a given modality whose frames carry the given per-frame content bytes (`content_hash
/// = [byte; 32]`). Used to exercise the frozen-sensor check on real content.
fn stream_with_content(name: &str, modality: Modality, contents: &[u8]) -> Stream {
    let frames = contents
        .iter()
        .enumerate()
        .map(|(i, c)| Frame {
            ts: i as i64 * 1_000_000,
            value_ref: ValueRef {
                uri: name.into(),
                byte_offset: None,
                byte_len: None,
                content_hash: Some([*c; 32]),
            },
        })
        .collect();
    Stream {
        name: name.into(),
        modality,
        declared_rate_hz: Some(30.0),
        clock_id: "c".into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: None,
        frames,
    }
}

#[test]
fn a_frozen_video_stream_is_flagged_as_stuck() {
    // Eight byte-identical camera frames while timestamps advance — a frozen feed (run 8 ≥ 5).
    let cam = stream_with_content("camera", Modality::Video, &[7; 8]);
    let d = dataset(vec![episode(0, vec![cam])]);
    let f = structural::StuckStream.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.STUCK_STREAM");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("frozen") || f[0].message.contains("stuck"));
}

#[test]
fn a_short_repeat_and_a_varying_video_stream_are_clean() {
    // A brief 3-frame repeat (an encoder hiccup, under the run-of-5 threshold) is not a freeze.
    let hiccup = stream_with_content("camera", Modality::Video, &[1, 1, 1, 2, 3, 4, 5, 6]);
    assert!(structural::StuckStream
        .run(&dataset(vec![episode(0, vec![hiccup])]))
        .is_empty());
    // A normal camera (every frame distinct) is clean.
    let varying = stream_with_content("camera", Modality::Video, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(structural::StuckStream
        .run(&dataset(vec![episode(0, vec![varying])]))
        .is_empty());
}

#[test]
fn a_constant_scalar_stream_is_not_a_stuck_video() {
    // A scalar stream held constant (an arm at rest) is legitimate — DEGENERATE's concern, not this.
    // The frozen-sensor check is scoped to Video, so an identical-content ScalarState is ignored.
    let rest = stream_with_content("state", Modality::ScalarState, &[9; 8]);
    assert!(structural::StuckStream
        .run(&dataset(vec![episode(0, vec![rest])]))
        .is_empty());
}

#[test]
fn a_hashless_video_stream_is_not_flagged_as_stuck() {
    // Without content hashes the check can't prove frames repeat, so it abstains (no false positive
    // on LeRobot video features, which live outside the Parquet and are unhashed).
    let cam = stream(
        "camera_hashless",
        "c",
        Some(30.0),
        &[0, 1, 2, 3, 4, 5, 6, 7],
    );
    let mut cam = cam;
    cam.modality = Modality::Video;
    assert!(structural::StuckStream
        .run(&dataset(vec![episode(0, vec![cam])]))
        .is_empty());
}

fn shaped(name: &str, dtype: Option<&str>, shape: Option<Vec<u64>>, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: "c".into(),
        clock_kind: ClockKind::Measured,
        dtype: dtype.map(Into::into),
        shape,
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: None,
        frames: frames_at(ts),
    }
}

#[test]
fn shape_mismatch_across_episodes_is_flagged() {
    let d = dataset(vec![
        episode(
            0,
            vec![shaped(
                "observation.state",
                Some("float32"),
                Some(vec![6]),
                &[0],
            )],
        ),
        episode(
            1,
            vec![shaped(
                "observation.state",
                Some("float32"),
                Some(vec![7]),
                &[0],
            )],
        ),
    ]);
    let f = structural::ShapeConsistency.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.SHAPE_MISMATCH");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn dtype_mismatch_across_episodes_is_flagged() {
    let d = dataset(vec![
        episode(0, vec![shaped("action", Some("float32"), None, &[0])]),
        episode(1, vec![shaped("action", Some("int64"), None, &[0])]),
    ]);
    let f = structural::ShapeConsistency.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.SHAPE_MISMATCH");
}

#[test]
fn consistent_shapes_produce_no_finding_and_drift_reports_once() {
    // Consistent across episodes → clean.
    let clean = dataset(vec![
        episode(0, vec![shaped("s", Some("float32"), Some(vec![4]), &[0])]),
        episode(1, vec![shaped("s", Some("float32"), Some(vec![4]), &[0])]),
    ]);
    assert!(structural::ShapeConsistency.run(&clean).is_empty());

    // A stream that never declares a schema is skipped (Veridex never infers).
    let undeclared = dataset(vec![
        episode(0, vec![shaped("s", None, None, &[0])]),
        episode(1, vec![shaped("s", None, None, &[0])]),
    ]);
    assert!(structural::ShapeConsistency.run(&undeclared).is_empty());

    // Drift spanning three episodes yields exactly one finding, not two.
    let drift = dataset(vec![
        episode(0, vec![shaped("s", Some("float32"), Some(vec![4]), &[0])]),
        episode(1, vec![shaped("s", Some("float32"), Some(vec![5]), &[0])]),
        episode(2, vec![shaped("s", Some("float32"), Some(vec![6]), &[0])]),
    ]);
    assert_eq!(structural::ShapeConsistency.run(&drift).len(), 1);
}

#[test]
fn stream_missing_from_some_episodes_is_flagged() {
    // `wrist` is present in episode 0 but absent from episode 1.
    let d = dataset(vec![
        episode(
            0,
            vec![
                stream("base", "c", None, &[0, 1]),
                stream("wrist", "c", None, &[0, 1]),
            ],
        ),
        episode(1, vec![stream("base", "c", None, &[0, 1])]),
    ]);
    let findings = structural::StreamPresence.run(&d);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].code, "STRUCTURAL.STREAM_PRESENCE_INCONSISTENT");
    assert_eq!(findings[0].severity, Severity::Warning);
    assert!(findings[0].message.contains("wrist"));
    assert!(findings[0].message.contains("1 of 2"));
    assert!(findings[0].message.contains("missing from 1"));
}

#[test]
fn stream_present_in_every_episode_is_clean() {
    let d = dataset(vec![
        episode(0, vec![stream("base", "c", None, &[0, 1])]),
        episode(1, vec![stream("base", "c", None, &[0, 1])]),
    ]);
    assert!(structural::StreamPresence.run(&d).is_empty());
}

#[test]
fn stream_presence_needs_two_populated_episodes_and_ignores_empty_ones() {
    // A single populated episode has nothing to compare against.
    let single = dataset(vec![episode(0, vec![stream("base", "c", None, &[0, 1])])]);
    assert!(structural::StreamPresence.run(&single).is_empty());

    // An empty episode is DegenerateEpisode's concern; it must not make `base` look inconsistent,
    // and with only one populated episode remaining there is nothing to compare.
    let with_empty = dataset(vec![
        episode(0, vec![stream("base", "c", None, &[0, 1])]),
        episode(1, vec![]),
    ]);
    assert!(structural::StreamPresence.run(&with_empty).is_empty());
}

#[test]
fn duplicate_episode_indices_do_not_produce_a_malformed_presence_finding() {
    // Two episodes share index 0 (a boundary corruption EpisodeBoundary flags). `base` is present in
    // both, so it is in every *distinct* episode — the presence check must stay silent rather than
    // emit "present in 1 of 2 episodes; missing from " with an empty list.
    let d = dataset(vec![
        episode(0, vec![stream("base", "c", None, &[0, 1])]),
        episode(0, vec![stream("base", "c", None, &[0, 1])]),
    ]);
    assert!(structural::StreamPresence.run(&d).is_empty());
}

// ---- near-duplicate episodes ----

/// A ten-frame stream whose contents are the given bytes, at 100 ms spacing from `start`.
fn near_stream(name: &str, start: i64, contents: &[u8]) -> veridex_core::cdm::Stream {
    let ts: Vec<i64> = (0..contents.len() as i64)
        .map(|i| start + i * 100_000_000)
        .collect();
    stream_hashed(name, "c", &ts, contents)
}

/// The check under test, at its default threshold.
fn near_duplicate() -> structural::NearDuplicateEpisode {
    structural::NearDuplicateEpisode { min_overlap: 0.8 }
}

#[test]
fn an_episode_re_uploaded_with_its_tail_trimmed_is_a_near_duplicate() {
    // The case the exact check cannot see: same recording, fewer frames, different timestamps.
    let original: Vec<u8> = (0..12).collect();
    let trimmed: Vec<u8> = (0..10).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &original)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &trimmed)]),
    ]);
    // The exact check is silent, which is why this one has to speak.
    assert!(structural::DuplicateEpisode.run(&d).is_empty());

    let f = near_duplicate().run(&d);
    assert_eq!(f.len(), 1, "one root cause, one finding: {f:?}");
    assert_eq!(f[0].code, "STRUCTURAL.NEAR_DUPLICATE_EPISODE");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("0, 1"), "{}", f[0].message);
    assert!(!f[0].risk.is_empty() && !f[0].remedy.is_empty());
}

#[test]
fn two_genuinely_different_episodes_are_not_near_duplicates() {
    let a: Vec<u8> = (0..12).collect();
    let b: Vec<u8> = (100..112).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &a)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &b)]),
    ]);
    assert!(near_duplicate().run(&d).is_empty());

    // And a pair that overlaps, but not enough: 6 of 12 frames is half, under the 0.8 default.
    let half: Vec<u8> = (0..6).chain(200..206).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &a)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &half)]),
    ]);
    assert!(
        near_duplicate().run(&d).is_empty(),
        "half an episode is not a copy of it"
    );
}

#[test]
fn a_stream_that_repeats_itself_is_not_evidence_of_anything() {
    // The false-positive class this check would otherwise fall into: an arm at rest, a locked
    // joint, a quantized channel. Every episode of the dataset shares those values, and that is a
    // fact about the sensor, not about duplication.
    let resting = [7u8; 12];
    let d = dataset(vec![
        episode(0, vec![near_stream("joint", 0, &resting)]),
        episode(1, vec![near_stream("joint", 5_000_000_000, &resting)]),
    ]);
    assert!(
        near_duplicate().run(&d).is_empty(),
        "a stream with one distinct value cannot evidence duplication"
    );

    // A short stream is not evidence either: two three-frame episodes coinciding is a coincidence.
    let short: Vec<u8> = (0..4).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &short)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &short)]),
    ]);
    assert!(near_duplicate().run(&d).is_empty());
}

#[test]
fn one_agreeing_stream_cannot_outvote_a_disagreeing_one() {
    // Two episodes whose proprioception happens to repeat, and whose camera does not. The camera is
    // the evidence that these are different recordings, and it must win.
    let same: Vec<u8> = (0..12).collect();
    let different: Vec<u8> = (100..112).collect();
    let d = dataset(vec![
        episode(
            0,
            vec![near_stream("state", 0, &same), near_stream("cam", 0, &same)],
        ),
        episode(
            1,
            vec![
                near_stream("state", 5_000_000_000, &same),
                near_stream("cam", 5_000_000_000, &different),
            ],
        ),
    ]);
    assert!(
        near_duplicate().run(&d).is_empty(),
        "the weakest shared stream decides"
    );
}

#[test]
fn an_exact_duplicate_is_reported_once_by_the_check_that_proves_it() {
    // Both checks look at the same pair. The exact one proves more, so it speaks; this one must not
    // double the deduction — and the suppression is computed from the exact check's own signature,
    // so it can never be wider than what that check actually reports.
    let contents: Vec<u8> = (0..12).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &contents)]),
        episode(1, vec![near_stream("s", 0, &contents)]),
    ]);
    assert_eq!(structural::DuplicateEpisode.run(&d).len(), 1);
    assert!(
        near_duplicate().run(&d).is_empty(),
        "the exact check speaks for this pair"
    );

    // Same frames, *different* timestamps: the exact check is silent (its signature includes the
    // time base), so this one must not be silent too — that is the direction that loses a defect.
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &contents)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &contents)]),
    ]);
    assert!(structural::DuplicateEpisode.run(&d).is_empty());
    assert_eq!(near_duplicate().run(&d).len(), 1);
}

#[test]
fn three_copies_of_one_recording_are_one_finding() {
    // The score deducts per finding, so a group of near-identical episodes must not be charged once
    // per pair.
    let contents: Vec<u8> = (0..12).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &contents)]),
        episode(1, vec![near_stream("s", 1_000_000_000, &contents[..11])]),
        episode(2, vec![near_stream("s", 2_000_000_000, &contents[..10])]),
    ]);
    let f = near_duplicate().run(&d);
    assert_eq!(f.len(), 1, "three pairs, one group, one finding: {f:?}");
    assert!(f[0].message.contains("0, 1, 2"), "{}", f[0].message);
}

#[test]
fn an_unhashed_stream_is_not_read_as_agreement() {
    // Without hashes there is no evidence either way, and "no evidence" must not read as "no
    // overlap" *or* as overlap. The dataset-level abstention is disclosed by
    // `structural.content-measurability`.
    let contents: Vec<u8> = (0..12).collect();
    let ts: Vec<i64> = (0..12).map(|i| i * 100_000_000).collect();
    let d = dataset(vec![
        episode(
            0,
            vec![
                near_stream("s", 0, &contents),
                stream("video", "c", None, &ts),
            ],
        ),
        episode(
            1,
            vec![
                near_stream("s", 5_000_000_000, &contents),
                stream("video", "c", None, &ts),
            ],
        ),
    ]);
    // The hashed stream still carries the claim; the unhashed one neither helps nor blocks it.
    assert_eq!(near_duplicate().run(&d).len(), 1);
}

#[test]
fn the_threshold_is_the_one_the_run_was_configured_with() {
    let a: Vec<u8> = (0..12).collect();
    let half: Vec<u8> = (0..6).chain(200..206).collect();
    let d = dataset(vec![
        episode(0, vec![near_stream("s", 0, &a)]),
        episode(1, vec![near_stream("s", 5_000_000_000, &half)]),
    ]);
    assert!(near_duplicate().run(&d).is_empty());
    let sensitive = structural::NearDuplicateEpisode { min_overlap: 0.5 };
    assert_eq!(sensitive.run(&d).len(), 1, "half clears a half threshold");
}

#[test]
fn a_recording_uploaded_forty_times_is_still_a_near_duplicate() {
    // The cap on how many episodes may share one frame hash exists to stop the pair counting going
    // quadratic on boilerplate — a home position, a calibration frame. Set too low it defeats the
    // very case the check is for: a recording ingested forty times shares *every* frame with
    // thirty-nine others, so every one of its hashes is over a cap of 32 and the whole group goes
    // unreported. The global pair ceiling is what bounds the pathological case, and it abstains
    // loudly rather than silently.
    let contents: Vec<u8> = (0..12).collect();
    let episodes: Vec<_> = (0..40u64)
        .map(|i| {
            // Different time base each upload, so the exact check is silent and this one must speak.
            episode(
                i,
                vec![near_stream("s", i as i64 * 1_000_000_000, &contents)],
            )
        })
        .collect();
    let d = dataset(episodes);
    assert!(
        structural::DuplicateEpisode.run(&d).is_empty(),
        "different time bases: the exact check does not speak for these"
    );

    let f = near_duplicate().run(&d);
    assert_eq!(f.len(), 1, "forty copies, one group, one finding: {f:?}");
    assert!(f[0].message.contains("39"), "{}", f[0].message);
}

#[test]
fn episodes_the_boilerplate_rule_skipped_are_reported_not_passed_over() {
    // Past the boilerplate ceiling the check stops comparing — which is the right call for a home
    // position shared by every episode, and the wrong thing to do *silently* when it means an
    // episode was never examined at all. A silent skip and a clean result look identical.
    let contents: Vec<u8> = (0..12).collect();
    let episodes: Vec<_> = (0..600u64)
        .map(|i| {
            episode(
                i,
                vec![near_stream("s", i as i64 * 1_000_000_000, &contents)],
            )
        })
        .collect();
    let f = near_duplicate().run(&dataset(episodes));

    assert_eq!(f.len(), 1, "one abstention, not silence: {f:?}");
    assert_eq!(f[0].code, "STRUCTURAL.NEAR_DUPLICATE_UNCHECKED");
    assert_eq!(f[0].severity, Severity::Info);
    assert!(
        f[0].message.contains("600 episode(s)") && f[0].message.contains("boilerplate"),
        "{}",
        f[0].message
    );
}

// ---- temporal ----

#[test]
fn non_monotonic_timestamps_are_caught_with_frame_indices() {
    // decreases at frame 2
    let d = dataset(vec![episode(
        0,
        vec![stream("s", "c", None, &[0, 10, 5, 20])],
    )]);
    let f = temporal::Monotonicity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.NON_MONOTONIC");
    assert!(f[0].message.contains("frame 2"));
}

#[test]
fn repeated_timestamp_is_non_monotonic() {
    let d = dataset(vec![episode(0, vec![stream("s", "c", None, &[0, 10, 10])])]);
    assert_eq!(temporal::Monotonicity.run(&d).len(), 1);
}

#[test]
fn rate_conformance_flags_wrong_declared_rate() {
    // 11 frames over 1s => 10 Hz observed, but declared 50 Hz.
    let ts: Vec<i64> = (0..11).map(|i| i * 100_000_000).collect();
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(50.0), &ts)])]);
    let f = temporal::RateConformance::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.RATE");

    // Declaring the correct 10 Hz produces no finding.
    let d_ok = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    assert!(temporal::RateConformance::default().run(&d_ok).is_empty());
}

#[test]
fn rate_validity_flags_a_corrupt_declared_rate() {
    // Zero, negative, and non-finite declared rates are each corrupt metadata. Frames are provided
    // so this isn't confused with a degenerate stream; the fault is the declared rate itself.
    let ts = [0i64, 100_000_000, 200_000_000];
    for bad in [0.0, -30.0, f64::NAN, f64::INFINITY] {
        let d = dataset(vec![episode(0, vec![stream("s", "c", Some(bad), &ts)])]);
        let f = temporal::RateValidity.run(&d);
        assert_eq!(f.len(), 1, "declared rate {bad} should be flagged");
        assert_eq!(f[0].code, "TEMPORAL.INVALID_RATE");
        assert_eq!(f[0].severity, Severity::Error);
        // The rate/gap checks skip a corrupt rate silently — this is the check that catches it.
        assert!(temporal::RateConformance::default().run(&d).is_empty());
    }
}

#[test]
fn rate_validity_ignores_valid_and_absent_rates() {
    let ts = [0i64, 100_000_000, 200_000_000];
    // A positive, finite declared rate is fine.
    let good = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    assert!(temporal::RateValidity.run(&good).is_empty());
    // No declared rate at all is fine — the check only judges a rate the source actually states.
    let absent = dataset(vec![episode(0, vec![stream("s", "c", None, &ts)])]);
    assert!(temporal::RateValidity.run(&absent).is_empty());
}

#[test]
fn rate_consistency_flags_a_stream_whose_rate_changes_between_episodes() {
    // `cam` is 30 Hz in episode 0 but 10 Hz in episode 2 — differently-configured sources pooled.
    let ts = [0i64, 100_000_000];
    let d = dataset(vec![
        episode(0, vec![stream("cam", "c", Some(30.0), &ts)]),
        episode(1, vec![stream("cam", "c", Some(30.0), &ts)]),
        episode(2, vec![stream("cam", "c", Some(10.0), &ts)]),
    ]);
    let f = temporal::RateConsistency.run(&d);
    assert_eq!(
        f.len(),
        1,
        "one finding per drifting stream, not per episode"
    );
    assert_eq!(f[0].code, "TEMPORAL.RATE_INCONSISTENT");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("30.000") && f[0].message.contains("10.000"));
}

#[test]
fn rate_consistency_ignores_uniform_noise_and_absent_rates() {
    let ts = [0i64, 100_000_000];
    // Same rate across episodes → clean.
    let uniform = dataset(vec![
        episode(0, vec![stream("cam", "c", Some(30.0), &ts)]),
        episode(1, vec![stream("cam", "c", Some(30.0), &ts)]),
    ]);
    assert!(temporal::RateConsistency.run(&uniform).is_empty());
    // A sub-1% difference is floating-point noise, not a real rate change → clean.
    let noisy = dataset(vec![
        episode(0, vec![stream("cam", "c", Some(30.0), &ts)]),
        episode(1, vec![stream("cam", "c", Some(30.05), &ts)]),
    ]);
    assert!(temporal::RateConsistency.run(&noisy).is_empty());
    // Episodes that declare no rate can't be inconsistent.
    let absent = dataset(vec![
        episode(0, vec![stream("cam", "c", None, &ts)]),
        episode(1, vec![stream("cam", "c", None, &ts)]),
    ]);
    assert!(temporal::RateConsistency.run(&absent).is_empty());
}

/// An episode whose single stream spans `dur_ns` (frames at 0 and `dur_ns`), so
/// `episode_duration_ns` measures exactly `dur_ns`.
fn episode_lasting(index: u64, dur_ns: i64) -> Episode {
    episode(index, vec![stream("s", "c", None, &[0, dur_ns])])
}

#[test]
fn episode_duration_flags_a_truncated_episode() {
    // Four ~1 s episodes and one 10 ms fragment: median 1 s, so 10 ms is 100x shorter (> 10x).
    let d = dataset(vec![
        episode_lasting(0, 1_000_000_000),
        episode_lasting(1, 1_000_000_000),
        episode_lasting(2, 1_000_000_000),
        episode_lasting(3, 1_000_000_000),
        episode_lasting(4, 10_000_000),
    ]);
    let f = temporal::EpisodeDuration::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.EPISODE_DURATION_OUTLIER");
    assert!(matches!(
        f[0].location,
        veridex_core::check::Location::Episode { episode: 4 }
    ));
}

#[test]
fn episode_duration_flags_a_stuck_recorder() {
    // One 20 s episode against four 1 s episodes: median 1 s, so 20 s is 20x longer (> 10x).
    let d = dataset(vec![
        episode_lasting(0, 1_000_000_000),
        episode_lasting(1, 1_000_000_000),
        episode_lasting(2, 1_000_000_000),
        episode_lasting(3, 1_000_000_000),
        episode_lasting(4, 20_000_000_000),
    ]);
    let f = temporal::EpisodeDuration::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.EPISODE_DURATION_OUTLIER");
}

#[test]
fn episode_duration_abstains_below_the_minimum_episode_count() {
    // Only three episodes — too few for a stable median — so even a wild outlier is not flagged.
    let d = dataset(vec![
        episode_lasting(0, 1_000_000_000),
        episode_lasting(1, 1_000_000_000),
        episode_lasting(2, 1_000_000),
    ]);
    assert!(temporal::EpisodeDuration::default().run(&d).is_empty());
}

#[test]
fn episode_duration_ignores_natural_variation_within_factor() {
    // Durations 1–5 s all sit within 10x of the ~2.5 s median: normal task-length variation.
    let d = dataset(vec![
        episode_lasting(0, 1_000_000_000),
        episode_lasting(1, 2_000_000_000),
        episode_lasting(2, 3_000_000_000),
        episode_lasting(3, 5_000_000_000),
    ]);
    assert!(temporal::EpisodeDuration::default().run(&d).is_empty());
}

#[test]
fn gaps_are_detected_against_declared_rate() {
    // 10 Hz declared (100 ms expected); a 500 ms jump between frame 2 and 3 is a gap.
    let ts = [0i64, 100_000_000, 200_000_000, 700_000_000, 800_000_000];
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    let f = temporal::Gaps::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.GAP");
}

#[test]
fn a_grossly_overstated_rate_does_not_flood_the_gap_report() {
    // A stream declares 1 kHz (1 ms expected) but is actually sampled at ~10 Hz (100 ms intervals).
    // Trusting the declared rate would flag every one of the real intervals as a gap; the check must
    // fall back to the observed median and stay quiet on an otherwise-regular timeline. The wrong rate
    // itself is RateConformance's to report.
    let ts: Vec<i64> = (0..12).map(|i| i * 100_000_000).collect();
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(1000.0), &ts)])]);
    let gaps = temporal::Gaps::default().run(&d);
    assert!(
        gaps.is_empty(),
        "an overstated declared rate must not turn every interval into a gap: {} findings",
        gaps.len()
    );
    // A genuine gap on the same overstated-rate stream is still caught (observed-median baseline).
    let mut ts2: Vec<i64> = (0..12).map(|i| i * 100_000_000).collect();
    ts2.push(ts2.last().unwrap() + 1_000_000_000); // a 1 s hole ~10x the median
    let d2 = dataset(vec![episode(0, vec![stream("s", "c", Some(1000.0), &ts2)])]);
    let gaps2 = temporal::Gaps::default().run(&d2);
    assert_eq!(gaps2.len(), 1, "a real gap is still detected");
    assert_eq!(gaps2[0].code, "TEMPORAL.GAP");
}

#[test]
fn jitter_flags_an_irregular_timeline() {
    // Intervals alternate 40 ms / 160 ms: mean 100 ms, std 60 ms → cv 0.6, above the 0.5 default.
    // The mean rate is a clean 10 Hz, so RATE would not fire — jitter is the distinct signal.
    let ts = [
        0i64,
        40_000_000,
        200_000_000,
        240_000_000,
        400_000_000,
        440_000_000,
        600_000_000,
        640_000_000,
        800_000_000,
        840_000_000,
        1_000_000_000,
    ];
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    // The mean rate is within tolerance, so rate-conformance stays quiet.
    assert!(temporal::RateConformance::default().run(&d).is_empty());
    let f = temporal::Jitter::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.JITTER");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("cv"));
}

#[test]
fn regular_timeline_has_no_jitter() {
    // 11 evenly-spaced frames at 100 ms → cv 0 → clean.
    let ts: Vec<i64> = (0..11).map(|i| i * 100_000_000).collect();
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    assert!(temporal::Jitter::default().run(&d).is_empty());
}

#[test]
fn jitter_needs_enough_intervals_to_be_meaningful() {
    // Only 5 frames (4 intervals), even wildly irregular, is too small a sample to judge → skipped.
    let ts = [0i64, 10_000_000, 500_000_000, 510_000_000, 1_000_000_000];
    let d = dataset(vec![episode(0, vec![stream("s", "c", None, &ts)])]);
    assert!(temporal::Jitter::default().run(&d).is_empty());
}

#[test]
fn clock_skew_flags_streams_that_drift_apart() {
    // Both sample at 100 Hz: the camera spans 1000 ms, the robot 1200 ms => 200 ms drift, well beyond
    // the 50 ms default plus the 10 ms sampling quantum.
    let dense = |span_ns: i64| -> Vec<i64> {
        (0..=(span_ns / 10_000_000))
            .map(|i| i * 10_000_000)
            .collect()
    };
    let cam = stream("cam", "camera", None, &dense(1_000_000_000));
    let robot = stream("robot", "robot", None, &dense(1_200_000_000));
    let d = dataset(vec![episode(0, vec![cam, robot])]);
    let f = temporal::ClockSkew::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.CLOCK_SKEW");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("drift"));
    assert!(f[0].risk.contains("observations") || f[0].risk.contains("action"));
}

#[test]
fn clock_skew_within_tolerance_is_clean() {
    // spans differ by 20 ms, under the 50 ms default tolerance.
    let cam = stream("cam", "camera", None, &[0, 1_000_000_000]);
    let robot = stream("robot", "robot", None, &[0, 1_020_000_000]);
    let d = dataset(vec![episode(0, vec![cam, robot])]);
    assert!(temporal::ClockSkew::default().run(&d).is_empty());
}

#[test]
fn start_offset_flags_a_late_starting_stream_on_the_same_clock() {
    // Both on clock `wall`; the arm starts 200 ms after the camera — beyond the 50 ms default.
    let cam = stream("cam", "wall", None, &[0, 1_000_000_000]);
    let arm = stream("arm", "wall", None, &[200_000_000, 1_200_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    let f = temporal::StartOffset::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.START_OFFSET");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("arm"));
}

#[test]
fn start_offset_ignores_streams_on_different_clocks() {
    // Same 200 ms start gap, but on different clocks → absolute times aren't comparable, so no
    // finding. (Cross-clock drift is CLOCK_SKEW's job, not this check's.)
    let cam = stream("cam", "cam_clock", None, &[0, 1_000_000_000]);
    let arm = stream("arm", "arm_clock", None, &[200_000_000, 1_200_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    assert!(temporal::StartOffset::default().run(&d).is_empty());
}

#[test]
fn start_offset_within_tolerance_is_clean() {
    // 20 ms start gap on a shared clock, under the 50 ms default tolerance.
    let cam = stream("cam", "wall", None, &[0, 1_000_000_000]);
    let arm = stream("arm", "wall", None, &[20_000_000, 1_020_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    assert!(temporal::StartOffset::default().run(&d).is_empty());
}

#[test]
fn end_offset_flags_an_early_ending_stream_on_the_same_clock() {
    // Both on clock `wall`, same start; the arm stops 200 ms before the camera — beyond the 50 ms
    // default. Same-start means CLOCK_SKEW would also catch it, but the check that owns a tail
    // misalignment is END_OFFSET.
    let cam = stream("cam", "wall", None, &[0, 1_200_000_000]);
    let arm = stream("arm", "wall", None, &[0, 1_000_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    let f = temporal::EndOffset::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.END_OFFSET");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("arm"));
}

#[test]
fn end_offset_catches_a_tail_misalignment_start_and_skew_both_miss() {
    // The gap END_OFFSET exists to close: because end = start + duration, a stream can pass both
    // START_OFFSET (|Δstart| ≤ tol) and CLOCK_SKEW (|Δduration| ≤ tol) yet be misaligned at the tail
    // by up to 2·tol. cam spans [0, 1000ms]; arm spans [40ms, 1040ms]: Δstart = 40 ms (< 50),
    // Δduration = 0 (< 50), but Δend = 40 ms... push it past tolerance with a 60 ms tail gap while
    // keeping start and duration within tolerance.
    let cam = stream("cam", "wall", None, &[0, 1_000_000_000]);
    // arm starts 40 ms late (under tol) and runs 20 ms longer (duration drift 20 ms, under tol), so
    // it ends 60 ms after cam — over the 50 ms tolerance.
    let arm = stream("arm", "wall", None, &[40_000_000, 1_060_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    assert!(
        temporal::StartOffset::default().run(&d).is_empty(),
        "start offset (40 ms) is within tolerance"
    );
    assert!(
        temporal::ClockSkew::default().run(&d).is_empty(),
        "duration drift (20 ms) is within tolerance"
    );
    let f = temporal::EndOffset::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.END_OFFSET");
}

#[test]
fn end_offset_ignores_streams_on_different_clocks() {
    // Same 200 ms end gap, but on different clocks → absolute times aren't comparable, so no finding.
    let cam = stream("cam", "cam_clock", None, &[0, 1_200_000_000]);
    let arm = stream("arm", "arm_clock", None, &[0, 1_000_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    assert!(temporal::EndOffset::default().run(&d).is_empty());
}

#[test]
fn end_offset_within_tolerance_is_clean() {
    // 20 ms end gap on a shared clock, under the 50 ms default tolerance.
    let cam = stream("cam", "wall", None, &[0, 1_020_000_000]);
    let arm = stream("arm", "wall", None, &[0, 1_000_000_000]);
    let d = dataset(vec![episode(0, vec![cam, arm])]);
    assert!(temporal::EndOffset::default().run(&d).is_empty());
}

#[test]
fn temporal_checks_do_not_overflow_on_extreme_timestamps() {
    // Corrupt timestamps spanning the full i64 range must not overflow the interval/span math
    // (which would panic in debug builds). Veridex's whole job is surviving bad data. The
    // subtractions saturate, so the checks simply run and report rather than crashing.
    let cam = stream("cam", "camera", Some(30.0), &[i64::MIN, i64::MAX]);
    let robot = stream("robot", "robot", Some(30.0), &[i64::MIN, 0]);
    let d = dataset(vec![episode(0, vec![cam, robot])]);

    // None of these should panic; each returns a (possibly empty) finding list.
    let _ = temporal::RateConformance::default().run(&d);
    let _ = temporal::Gaps::default().run(&d);
    let _ = temporal::ClockSkew::default().run(&d);
    let _ = temporal::StartOffset::default().run(&d);
    let _ = temporal::EndOffset::default().run(&d);
    // A stream with no declared rate exercises the median-interval path (also saturating).
    let no_rate = dataset(vec![episode(
        0,
        vec![stream("s", "c", None, &[i64::MIN, 0, i64::MAX])],
    )]);
    let _ = temporal::Gaps::default().run(&no_rate);

    // The duration-outlier check spans episodes: give four episodes declared boundaries covering the
    // full i64 range so `Episode::duration_ns` takes the `end - start` path on extreme values. It
    // must saturate, not panic.
    let mut extreme = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
        episode(2, vec![stream("s", "c", None, &[0, 1])]),
        episode(3, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    for (i, ep) in extreme.episodes.iter_mut().enumerate() {
        ep.start_ts = Some(i64::MIN);
        ep.end_ts = Some(if i == 0 { 0 } else { i64::MAX });
    }
    let _ = temporal::EpisodeDuration::default().run(&extreme);
}

// ---- provenance completeness ----

fn dataset_with_provenance(elements: Vec<ProvenanceElement>) -> Dataset {
    let mut d = dataset(vec![episode(0, vec![stream("s", "c", None, &[0, 1])])]);
    d.provenance = vec![Provenance {
        scope: ProvenanceScope::Dataset,
        elements,
    }];
    d
}

fn el(key: &str, value: Option<&str>, class: ProvenanceClass) -> ProvenanceElement {
    ProvenanceElement {
        key: key.into(),
        value: value.map(|v| v.into()),
        class,
    }
}

#[test]
fn missing_license_and_sensor_are_surfaced() {
    // No provenance at all: every expected element is missing.
    let d = dataset(vec![episode(0, vec![stream("s", "c", None, &[0, 1])])]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    let codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    assert!(codes.contains(&"PROVENANCE.MISSING_LICENSE"));
    assert!(codes.contains(&"PROVENANCE.MISSING_SENSOR"));
    // License absence is a warning; sensor absence is info.
    let lic = f
        .iter()
        .find(|x| x.code == "PROVENANCE.MISSING_LICENSE")
        .unwrap();
    assert_eq!(lic.severity, Severity::Warning);
}

#[test]
fn present_known_element_is_not_reported_missing() {
    let d = dataset_with_provenance(vec![el(
        "license",
        Some("apache-2.0"),
        ProvenanceClass::Known,
    )]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.MISSING_LICENSE"));
}

#[test]
fn unknown_class_still_counts_as_missing() {
    let d = dataset_with_provenance(vec![el("license", None, ProvenanceClass::Unknown)]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    assert!(f.iter().any(|x| x.code == "PROVENANCE.MISSING_LICENSE"));
}

#[test]
fn placeholder_value_is_flagged_and_not_counted_as_present() {
    // A license "known" as the string "unknown" is present in form but empty in substance.
    let d = dataset_with_provenance(vec![el("license", Some("unknown"), ProvenanceClass::Known)]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    let codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    // The placeholder is called out, and the element does not satisfy the presence check.
    assert!(codes.contains(&"PROVENANCE.PLACEHOLDER_VALUE"));
    assert!(codes.contains(&"PROVENANCE.MISSING_LICENSE"));
    let ph = f
        .iter()
        .find(|x| x.code == "PROVENANCE.PLACEHOLDER_VALUE")
        .unwrap();
    assert_eq!(ph.severity, Severity::Info);
}

#[test]
fn real_value_is_not_flagged_as_placeholder() {
    let d = dataset_with_provenance(vec![el(
        "license",
        Some("apache-2.0"),
        ProvenanceClass::Known,
    )]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PLACEHOLDER_VALUE"));
}

#[test]
fn internally_inconsistent_element_is_flagged() {
    // known but no value.
    let d = dataset_with_provenance(vec![el("license", None, ProvenanceClass::Known)]);
    let f = provenance::ProvenanceCompleteness.run(&d);
    assert!(f.iter().any(|x| x.code == "PROVENANCE.INCONSISTENT"));
}

#[test]
fn default_engine_runs_all_families_end_to_end() {
    // A dataset with a clock-skew problem should fail via the standard engine. Both streams sample at
    // 100 Hz, so the 500 ms drift is far larger than the sampling quantum a span comparison allows for.
    let dense = |span_ns: i64| -> Vec<i64> {
        (0..=(span_ns / 10_000_000))
            .map(|i| i * 10_000_000)
            .collect()
    };
    let cam = stream("cam", "camera", None, &dense(1_000_000_000));
    let robot = stream("robot", "robot", None, &dense(1_500_000_000));
    let d = dataset(vec![episode(0, vec![cam, robot])]);
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert_eq!(verdict.status, veridex_core::Status::Fail);
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
    assert_eq!(verdict.executed_checks.len(), 39);
}

#[test]
fn dataset_level_stat_checks_fire_once_regardless_of_episode_count() {
    // Regression guard for the dataset-level-stats duplication class: several checks read
    // dataset-level data (stored/recomputed stats) attached to every episode's copy of a stream, and
    // must report per stream, not per episode. The bug this guards produced findings at *different*
    // episode locations (ep0/s, ep1/s, …), so a (code, location) check would miss it — the real
    // invariant is that these codes' finding counts don't scale with episode count. (RangeSanity once
    // emitted DEGENERATE once per episode.)
    let mk = || {
        // A degenerate stored stat (DEGENERATE) plus a stored range the recompute escapes
        // (STATS_STALE), both dataset-level and repeated across every episode.
        let mut s = stream_with_stats("s", stats(0.0, 0.0, 0.0, 0.0));
        s.observed_stats = Some(stats(-1.0, 1.0, 0.0, 0.5));
        s
    };
    let run = |episode_count: u64| {
        let episodes: Vec<_> = (0..episode_count).map(|i| episode(i, vec![mk()])).collect();
        let d = dataset(episodes);
        let engine =
            veridex_core::checks::default_engine().expect("standard checks have unique ids");
        let hash = veridex_core::content_hash(&d);
        engine.run(&d, hash, &veridex_core::RunConfig::default())
    };

    // The dataset-level statistical codes must each fire exactly the same number of times whether the
    // dataset has 1 episode or 5 — their count is per stream, not per episode.
    let dataset_level_codes = [
        "STATISTICAL.DEGENERATE",
        "STATISTICAL.STATS_STALE",
        "STATISTICAL.NON_FINITE",
        "STATISTICAL.RANGE_INVERTED",
        "STATISTICAL.SATURATED",
        "STATISTICAL.OUTLIER",
        "STATISTICAL.NON_FINITE_OBSERVED",
    ];
    let count = |v: &veridex_core::Verdict, code: &str| {
        v.findings.iter().filter(|f| f.code == code).count()
    };
    let one = run(1);
    let five = run(5);
    for code in dataset_level_codes {
        assert_eq!(
            count(&one, code),
            count(&five, code),
            "`{code}` count must not scale with episode count (dataset-level stat)"
        );
    }
    // Sanity: at least one dataset-level check actually fired (else the guard proves nothing).
    assert!(count(&five, "STATISTICAL.DEGENERATE") >= 1);
}

#[test]
fn catalog_lists_every_standard_check_with_metadata() {
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let catalog = engine.catalog();
    // One entry per registered check, in registration order.
    assert_eq!(catalog.len(), engine.check_ids().len());
    assert_eq!(
        catalog.iter().map(|c| c.id).collect::<Vec<_>>(),
        engine.check_ids()
    );
    // The new shape-consistency check is present with the expected metadata.
    let shape = catalog
        .iter()
        .find(|c| c.id == "structural.shape-consistency")
        .expect("shape-consistency is in the catalog");
    assert_eq!(shape.category, veridex_core::Category::Structural);
    assert_eq!(shape.default_severity, Severity::Error);
    assert!(!shape.title.is_empty());
}

// ---- statistical ----

fn stream_with_stats(name: &str, stats: veridex_core::cdm::StreamStats) -> Stream {
    let mut s = stream(name, "c", None, &[0, 1]);
    s.stats = Some(stats);
    s
}

fn stats(min: f64, max: f64, mean: f64, std: f64) -> veridex_core::cdm::StreamStats {
    veridex_core::cdm::StreamStats {
        min,
        max,
        mean,
        std,
    }
}

#[test]
fn inverted_stat_range_is_an_error() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(5.0, 1.0, 3.0, 1.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.RANGE_INVERTED");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn non_finite_stats_are_an_error() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, f64::NAN, 0.0, 1.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.NON_FINITE");
}

#[test]
fn mean_outside_range_is_an_error() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, 10.0, 12.0, 1.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.MEAN_OUT_OF_RANGE");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn mean_one_ulp_outside_the_range_is_tolerated() {
    // A source's independently-rounded mean can land a hair past a bound on a near-constant stream.
    // That's honest rounding, not corrupt stats — the check must not raise a hard error on it.
    let m = 0.1_f64;
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(m, m, m + f64::EPSILON, 0.0))],
    )]);
    let mean_findings: Vec<_> = statistical::RangeSanity
        .run(&d)
        .into_iter()
        .filter(|f| f.code == "STATISTICAL.MEAN_OUT_OF_RANGE")
        .collect();
    assert!(
        mean_findings.is_empty(),
        "a one-ULP rounding overshoot must not trip MEAN_OUT_OF_RANGE"
    );
}

#[test]
fn std_exceeding_popoviciu_bound_is_an_error() {
    // For values in [0, 10] the std cannot exceed (10-0)/2 = 5; a stored std of 8 is impossible.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, 10.0, 5.0, 8.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.STD_IMPLAUSIBLE");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn std_at_the_popoviciu_bound_is_accepted() {
    // std exactly (max-min)/2 is the extremal two-point distribution — valid, no finding.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, 10.0, 5.0, 5.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert!(f.is_empty());
}

#[test]
fn constant_stream_is_a_degenerate_warning() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(2.0, 2.0, 2.0, 0.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.DEGENERATE");
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn range_sanity_reports_dataset_level_stats_once_not_per_episode() {
    // Stored stats are dataset-level, attached to every episode's copy of the stream. A corrupt or
    // degenerate stored stat must produce one finding, not one per episode (the same dedup the other
    // stored-stats checks apply).
    let mk = || stream_with_stats("s", stats(2.0, 2.0, 2.0, 0.0)); // constant → DEGENERATE
    let d = dataset(vec![
        episode(0, vec![mk()]),
        episode(1, vec![mk()]),
        episode(2, vec![mk()]),
    ]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(
        f.len(),
        1,
        "one finding for the dataset-level stat, not one per episode"
    );
    assert_eq!(f[0].code, "STATISTICAL.DEGENERATE");
}

#[test]
fn healthy_stats_produce_no_findings() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(-1.0, 1.0, 0.0, 0.5))],
    )]);
    assert!(statistical::RangeSanity.run(&d).is_empty());
}

#[test]
fn stats_outside_declared_dtype_range_is_an_error() {
    // A uint8 stream cannot hold 300; stored max 300 means the dtype or the stats are wrong.
    let mut s = stream_with_stats("img", stats(0.0, 300.0, 128.0, 40.0));
    s.dtype = Some("uint8".into());
    let f = statistical::RangeSanity.run(&dataset(vec![episode(0, vec![s])]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.DTYPE_RANGE");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn stats_within_declared_dtype_range_are_clean() {
    // uint8 stats within [0, 255] are fine.
    let mut s = stream_with_stats("img", stats(0.0, 255.0, 128.0, 40.0));
    s.dtype = Some("uint8".into());
    assert!(statistical::RangeSanity
        .run(&dataset(vec![episode(0, vec![s])]))
        .is_empty());

    // A float dtype has no integer bound to exceed, so nothing fires even for large values.
    let mut f32s = stream_with_stats("state", stats(-1000.0, 1000.0, 0.0, 100.0));
    f32s.dtype = Some("float32".into());
    assert!(statistical::RangeSanity
        .run(&dataset(vec![episode(0, vec![f32s])]))
        .is_empty());
}

/// A stream carrying a recomputed [`Saturation`] summary (as the LeRobot adapter would populate it).
fn stream_with_saturation(
    name: &str,
    sample_count: u64,
    at_min: u64,
    at_max: u64,
    min: f64,
    max: f64,
) -> Stream {
    let mut s = stream(name, "c", None, &[0, 1]);
    s.observed_saturation = Some(veridex_core::cdm::Saturation {
        sample_count,
        at_min,
        at_max,
        min,
        max,
        dim: 0,
    });
    s
}

#[test]
fn stream_pinned_at_its_max_is_saturated() {
    // 70 of 100 samples sit exactly at the max rail → saturated.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("gripper", 100, 2, 70, 0.0, 1.0)],
    )]);
    let f = statistical::Saturation::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.SATURATED");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("maximum"));
}

#[test]
fn saturation_names_a_multi_dimension_but_not_a_scalar() {
    // dim 0 (scalar): no "dimension" qualifier, to avoid noise.
    let d0 = dataset(vec![episode(
        0,
        vec![stream_with_saturation("gripper", 100, 2, 70, 0.0, 1.0)],
    )]);
    let f0 = statistical::Saturation::default().run(&d0);
    assert_eq!(f0.len(), 1);
    assert!(!f0[0].message.contains("dimension"));

    // dim 6 (a joint of a multi-DoF feature): the finding names it.
    let mut s = stream_with_saturation("action", 100, 2, 70, 0.0, 1.0);
    if let Some(sat) = s.observed_saturation.as_mut() {
        sat.dim = 6;
    }
    let d6 = dataset(vec![episode(0, vec![s])]);
    let f6 = statistical::Saturation::default().run(&d6);
    assert_eq!(f6.len(), 1);
    assert!(f6[0].message.contains("dimension 6"));
}

#[test]
fn stream_pinned_at_its_min_reports_the_minimum() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("actuator", 100, 80, 1, -1.0, 1.0)],
    )]);
    let f = statistical::Saturation::default().run(&d);
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("minimum"));
}

#[test]
fn lightly_touched_limit_is_not_saturated() {
    // Only 10% of samples at the rail — normal contact, not saturation.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("state", 100, 3, 10, 0.0, 5.0)],
    )]);
    assert!(statistical::Saturation::default().run(&d).is_empty());
}

#[test]
fn constant_stream_is_left_to_degenerate() {
    // min == max: every sample pins both ends, but that's DEGENERATE's job, not saturation.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("const", 100, 100, 100, 2.0, 2.0)],
    )]);
    assert!(statistical::Saturation::default().run(&d).is_empty());
}

#[test]
fn too_few_samples_to_judge_saturation() {
    // Below min_samples the fraction says little, so the check abstains even at 100% pinned.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("short", 5, 0, 5, 0.0, 1.0)],
    )]);
    assert!(statistical::Saturation::default().run(&d).is_empty());
}

#[test]
fn saturation_is_reported_once_per_stream_not_per_episode() {
    // The adapter attaches the same dataset-level summary to every episode's copy of the stream;
    // the check must report it once, not once per episode.
    let d = dataset(vec![
        episode(
            0,
            vec![stream_with_saturation("gripper", 100, 0, 90, 0.0, 1.0)],
        ),
        episode(
            1,
            vec![stream_with_saturation("gripper", 100, 0, 90, 0.0, 1.0)],
        ),
    ]);
    let f = statistical::Saturation::default().run(&d);
    assert_eq!(f.len(), 1);
}

/// A stream carrying a recomputed non-finite count (as the LeRobot adapter would populate it).
fn stream_with_non_finite(name: &str, count: u64) -> Stream {
    let mut s = stream(name, "c", None, &[0, 1]);
    s.observed_non_finite = Some(count);
    s
}

#[test]
fn non_finite_values_in_the_data_are_an_error() {
    let d = dataset(vec![episode(0, vec![stream_with_non_finite("state", 3)])]);
    let f = statistical::NonFiniteObserved.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.NON_FINITE_OBSERVED");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains('3'));
}

#[test]
fn clean_data_and_unread_values_do_not_flag_non_finite() {
    // Some(0): values were read and all finite. None: values were never read (e.g. MCAP).
    let clean = stream_with_non_finite("a", 0);
    let mut unread = stream("b", "c", None, &[0, 1]);
    unread.observed_non_finite = None;
    let d = dataset(vec![episode(0, vec![clean, unread])]);
    assert!(statistical::NonFiniteObserved.run(&d).is_empty());
}

#[test]
fn non_finite_is_reported_once_per_stream_not_per_episode() {
    // The count is dataset-level, attached to every episode's copy of the stream.
    let d = dataset(vec![
        episode(0, vec![stream_with_non_finite("state", 2)]),
        episode(1, vec![stream_with_non_finite("state", 2)]),
    ]);
    let f = statistical::NonFiniteObserved.run(&d);
    assert_eq!(f.len(), 1);
}

#[test]
fn stale_stats_on_dataset_level_stats_are_reported_once_not_per_episode() {
    // Stored + recomputed stats are dataset-level, attached to every episode's copy of the stream.
    // A stored range that doesn't bound the data must produce one STATS_STALE, not one per episode.
    let mk = || {
        let mut s = stream_with_stats("state", stats(0.0, 3.0, 1.5, 1.0));
        s.observed_stats = Some(stats(0.0, 5.0, 2.0, 1.0)); // real max 5 exceeds stored 3
        s
    };
    let d = dataset(vec![
        episode(0, vec![mk()]),
        episode(1, vec![mk()]),
        episode(2, vec![mk()]),
    ]);
    let f = statistical::StoredVsObserved.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.STATS_STALE");
}

#[test]
fn a_lone_extreme_far_from_the_mean_is_an_outlier() {
    // Bulk near 0 (tiny std), one spike at 100 → max is 100σ from mean → OUTLIER.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(0.0, 100.0, 0.0, 1.0))],
    )]);
    let f = statistical::ExtremeOutlier::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.OUTLIER");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("maximum"));
}

#[test]
fn outlier_on_dataset_level_stats_is_reported_once_not_per_episode() {
    // Stored/recomputed stats are dataset-level, attached to every episode's copy of the stream.
    // The check must report the outlier once, not once per episode.
    let d = dataset(vec![
        episode(
            0,
            vec![stream_with_stats("state", stats(0.0, 100.0, 0.0, 1.0))],
        ),
        episode(
            1,
            vec![stream_with_stats("state", stats(0.0, 100.0, 0.0, 1.0))],
        ),
        episode(
            2,
            vec![stream_with_stats("state", stats(0.0, 100.0, 0.0, 1.0))],
        ),
    ]);
    let f = statistical::ExtremeOutlier::default().run(&d);
    assert_eq!(f.len(), 1);
}

#[test]
fn a_low_spike_reports_the_minimum() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(-100.0, 1.0, 0.0, 1.0))],
    )]);
    let f = statistical::ExtremeOutlier::default().run(&d);
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("minimum"));
}

#[test]
fn a_wide_but_normal_distribution_is_not_an_outlier() {
    // Extremes only ~2σ out — a broad distribution, not a spike.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(-2.0, 2.0, 0.0, 1.0))],
    )]);
    assert!(statistical::ExtremeOutlier::default().run(&d).is_empty());
}

#[test]
fn extreme_outlier_leaves_corrupt_and_degenerate_stats_to_range_sanity() {
    // std == 0 (degenerate): no z-scale, so this check abstains (DEGENERATE owns it).
    let degenerate = dataset(vec![episode(
        0,
        vec![stream_with_stats("c", stats(5.0, 5.0, 5.0, 0.0))],
    )]);
    assert!(statistical::ExtremeOutlier::default()
        .run(&degenerate)
        .is_empty());
    // Non-finite stats: RangeSanity's error, skipped here.
    let corrupt = dataset(vec![episode(
        0,
        vec![stream_with_stats("c", stats(0.0, f64::NAN, 0.0, 1.0))],
    )]);
    assert!(statistical::ExtremeOutlier::default()
        .run(&corrupt)
        .is_empty());
}

// ---- semantic ----

/// An episode carrying a specific task string.
fn episode_with_task(index: u64, task: Option<&str>) -> Episode {
    let mut ep = episode(index, vec![stream("s", "c", None, &[0, 1])]);
    ep.task = task.map(Into::into);
    ep
}

/// An episode over frames ts 0..10 carrying the given language labels.
fn episode_with_labels(index: u64, labels: Vec<Label>) -> Episode {
    let mut ep = episode(index, vec![stream("s", "c", None, &[0, 5, 10])]);
    ep.labels = labels;
    ep
}

fn lang(value: &str, ts: Option<i64>) -> Label {
    Label {
        key: "language".into(),
        value: value.into(),
        ts,
    }
}

#[test]
fn a_language_annotation_outside_the_episode_span_is_unaligned() {
    // Frames span [0, 10]; an annotation at ts 50 references a moment the episode never recorded.
    let d = dataset(vec![episode_with_labels(0, vec![lang("push", Some(50))])]);
    let f = semantic::AnnotationIntegrity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.ANNOTATION_UNALIGNED");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn an_annotation_on_a_recorded_frame_outside_narrow_declared_bounds_is_aligned() {
    // The episode declares a window [0, 5] that is narrower than its recorded frames (ts 0..10).
    // An annotation at ts 8 lands on a genuinely recorded moment, so it must NOT be flagged
    // unaligned just because the declared bounds under-report the episode's real extent.
    let mut ep = episode_with_labels(0, vec![lang("grasp", Some(8))]);
    ep.start_ts = Some(0);
    ep.end_ts = Some(5);
    let d = dataset(vec![ep]);
    assert!(semantic::AnnotationIntegrity.run(&d).is_empty());
}

#[test]
fn an_aligned_annotation_is_clean() {
    let d = dataset(vec![episode_with_labels(
        0,
        vec![lang("push the block", Some(5))],
    )]);
    assert!(semantic::AnnotationIntegrity.run(&d).is_empty());
}

#[test]
fn conflicting_annotations_at_one_timestamp_are_flagged() {
    let d = dataset(vec![episode_with_labels(
        0,
        vec![lang("push", Some(5)), lang("pull", Some(5))],
    )]);
    let f = semantic::AnnotationIntegrity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.ANNOTATION_CONFLICT");
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn identical_annotations_at_one_timestamp_do_not_conflict() {
    let d = dataset(vec![episode_with_labels(
        0,
        vec![lang("push", Some(5)), lang("push", Some(5))],
    )]);
    assert!(semantic::AnnotationIntegrity.run(&d).is_empty());
}

#[test]
fn an_empty_language_annotation_is_flagged() {
    let d = dataset(vec![episode_with_labels(0, vec![lang("   ", Some(5))])]);
    let f = semantic::AnnotationIntegrity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.EMPTY_ANNOTATION");
}

#[test]
fn non_language_labels_are_ignored_by_the_annotation_check() {
    // A `success` label out of span is not a language annotation → not this check's concern.
    let d = dataset(vec![episode_with_labels(
        0,
        vec![Label {
            key: "success".into(),
            value: "true".into(),
            ts: Some(999),
        }],
    )]);
    assert!(semantic::AnnotationIntegrity.run(&d).is_empty());
}

#[test]
fn empty_task_string_is_flagged() {
    let d = dataset(vec![episode_with_task(0, Some("   "))]);
    let f = semantic::TaskQuality.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.EMPTY_TASK");
    assert_eq!(f[0].severity, Severity::Warning);
}

#[test]
fn placeholder_task_is_low_information() {
    let d = dataset(vec![episode_with_task(0, Some("Hold"))]);
    let f = semantic::TaskQuality.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.PLACEHOLDER_TASK");
    assert_eq!(f[0].severity, Severity::Info);
}

#[test]
fn stream_keys_colliding_by_case_are_ambiguous() {
    let d = dataset(vec![episode(
        0,
        vec![
            stream("observation.images.top", "c", None, &[0, 1]),
            stream("observation.images.Top", "c", None, &[0, 1]),
        ],
    )]);
    let f = semantic::StreamKeyClarity.run(&d);
    // Both members of the colliding group are reported.
    assert_eq!(f.len(), 2);
    assert!(f.iter().all(|x| x.code == "SEMANTIC.AMBIGUOUS_STREAM_KEY"));
    assert!(f.iter().all(|x| x.severity == Severity::Warning));
}

#[test]
fn distinct_stream_keys_are_not_flagged() {
    let d = dataset(vec![episode(
        0,
        vec![
            stream("observation.images.top", "c", None, &[0, 1]),
            stream("observation.images.wrist", "c", None, &[0, 1]),
        ],
    )]);
    assert!(semantic::StreamKeyClarity.run(&d).is_empty());
}

#[test]
fn exact_duplicate_stream_key_is_an_error_not_a_broken_ambiguity() {
    // Two streams with the identical name in one episode violate the CDM's uniqueness invariant.
    // This must be a single, well-formed DUPLICATE_STREAM_KEY error — not the malformed
    // "ambiguous with " (empty list) that the case/whitespace path would otherwise produce.
    let d = dataset(vec![episode(
        0,
        vec![
            stream("cam", "c", None, &[0, 1]),
            stream("cam", "c", None, &[0, 1]),
        ],
    )]);
    let f = semantic::StreamKeyClarity.run(&d);
    assert_eq!(
        f.len(),
        1,
        "one finding per duplicated name, not per occurrence"
    );
    assert_eq!(f[0].code, "SEMANTIC.DUPLICATE_STREAM_KEY");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("appears 2 times"));
    // The ambiguity path must not fire for an exact duplicate (no empty "ambiguous with" clause).
    assert!(f.iter().all(|x| x.code != "SEMANTIC.AMBIGUOUS_STREAM_KEY"));
}

#[test]
fn meaningful_and_absent_tasks_are_not_flagged() {
    // A real instruction is clean.
    let good = dataset(vec![episode_with_task(0, Some("pick up the red cube"))]);
    assert!(semantic::TaskQuality.run(&good).is_empty());
    // An unresolved (None) task is deliberately not flagged — it means "unknown", not "empty".
    let absent = dataset(vec![episode_with_task(0, None)]);
    assert!(semantic::TaskQuality.run(&absent).is_empty());
}

// ---- documentation drift guard ----

#[test]
fn every_registered_check_is_documented_in_docs_checks_md() {
    // docs/checks.md is the user-facing catalog reference; guard it against silently drifting when a
    // check is added. Path is relative to this crate's manifest (repo-root/docs/checks.md).
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/checks.md"))
        .expect("docs/checks.md is readable");
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    for id in engine.check_ids() {
        assert!(
            doc.contains(id),
            "check `{id}` is registered but missing from docs/checks.md"
        );
    }
    // Every finding code a check declares must also appear in the catalog page — so a newly emitted
    // code can't ship undocumented.
    for c in engine.catalog() {
        for code in c.finding_codes {
            assert!(
                doc.contains(code),
                "finding code `{code}` (check `{}`) is missing from docs/checks.md",
                c.id
            );
        }
    }
}

#[test]
fn docs_checks_md_lists_no_unknown_finding_codes() {
    // The reverse guard: every `FAMILY.CODE`-shaped token in docs/checks.md must be a code some
    // registered check actually emits, so a stale row for a renamed/removed code can't linger.
    let doc = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/checks.md"))
        .expect("docs/checks.md is readable");
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let registered: std::collections::HashSet<&str> = engine
        .catalog()
        .into_iter()
        .flat_map(|c| c.finding_codes.iter().copied())
        .collect();

    // A finding code looks like FAMILY.CODE — uppercase family, '.', then uppercase/underscore.
    let is_finding_code = |s: &str| -> bool {
        match s.split_once('.') {
            Some((family, code)) => {
                !family.is_empty()
                    && family.chars().all(|c| c.is_ascii_uppercase())
                    && !code.is_empty()
                    && code.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            }
            None => false,
        }
    };

    // The engine's own coverage disclosure. Not emitted by a registered check — coverage is a
    // property of the ingest, which no check can read from the CDM — so it is documented without
    // appearing in the catalog. Named explicitly here rather than pattern-matched away, so a typo in
    // either the docs or the engine is still caught.
    let engine_emitted: std::collections::BTreeSet<&str> = [
        "COVERAGE.SAMPLE",
        "COVERAGE.METADATA_ONLY",
        // Source the adapter declined to read. Also a property of the ingest, and the one the
        // `coverage` field cannot express: a `Coverage::Full` ingest read everything it was
        // *willing* to read, which is not everything the dataset declared.
        "COVERAGE.SOURCE_UNREAD",
        // The scope disclosure, emitted by the engine for the same reason: whether the catalog was
        // narrowed is a property of the run's configuration, which no check can read from the CDM.
        "SCOPE.NARROWED",
        // A check that crashed. Emitted as a SARIF rule id by the reporter, so no registered check
        // declares it either.
        "VERIDEX.CHECK_ERRORED",
        // A profile's readiness verdict, synthesized as a SARIF result for the same reason a
        // crashed check is: results are the only channel a code-scanning system reads.
        "VERIDEX.PROFILE_NOT_READY",
        // The redaction disclosure. Attached at render time by `--redact`, not by a check: what a
        // report may quote is a property of who will read it, which nothing in the CDM knows.
        "REPORT.REDACTED",
    ]
    .into();

    // Forward direction, which did not exist: this list was consulted *only* to excuse a code the
    // docs mention that no check emits. Nothing required an engine-emitted code to be documented at
    // all, so `COVERAGE.*` and `SCOPE.NARROWED` were documented by luck and `VERIDEX.CHECK_ERRORED`
    // — which SARIF hands to code scanning with a `helpUri` pointing at this very page — was not
    // documented and nothing noticed.
    for code in &engine_emitted {
        assert!(
            doc.contains(&format!("`{code}`")),
            "the engine can emit `{code}`, but docs/checks.md does not document it"
        );
    }

    // Backtick-delimited spans are the odd-indexed pieces when splitting on '`'.
    for token in doc.split('`').skip(1).step_by(2) {
        if is_finding_code(token) {
            assert!(
                registered.contains(token) || engine_emitted.contains(token),
                "docs/checks.md lists `{token}`, which no registered check emits"
            );
        }
    }
}

#[test]
fn the_engines_coverage_codes_are_the_ones_it_actually_emits() {
    // The pair above is hand-maintained, so it is pinned to the engine's real output: a renamed
    // code would otherwise be waved through the catalog gate by a stale exemption.
    let d = dataset(vec![episode(0, vec![stream("a", "c", None, &[0, 1])])]);
    let hash = veridex_core::content_hash(&d);
    let engine = veridex_core::checks::default_engine().unwrap();
    let config = veridex_core::RunConfig::default();

    for (coverage, expected) in [
        (
            veridex_core::CoverageNote::Sample {
                request: "first 1 episode(s) by index".into(),
                episodes_ingested: 1,
            },
            "COVERAGE.SAMPLE",
        ),
        (
            veridex_core::CoverageNote::MetadataOnly {
                episodes_declared: 1,
            },
            "COVERAGE.METADATA_ONLY",
        ),
    ] {
        let v = engine.run_over(&d, hash, &config, coverage);
        assert!(
            v.findings.iter().any(|f| f.code == expected),
            "expected {expected}, got {:?}",
            v.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
        );
    }
    // And a full run says nothing about coverage at all.
    let full = engine.run(&d, hash, &config);
    assert!(!full
        .findings
        .iter()
        .any(|f| f.code.starts_with("COVERAGE.")));
}

#[test]
fn every_check_declares_at_least_one_unique_finding_code() {
    // Each check must own a non-empty, globally-unique set of finding codes; a code belongs to
    // exactly one check so findings trace back unambiguously.
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let mut seen = std::collections::HashSet::new();
    for c in engine.catalog() {
        assert!(
            !c.finding_codes.is_empty(),
            "check `{}` declares no finding codes",
            c.id
        );
        for code in c.finding_codes {
            assert!(
                seen.insert(*code),
                "finding code `{code}` is declared by more than one check"
            );
        }
    }
}

#[test]
fn canonicalizing_order_makes_the_verdict_order_independent() {
    // The content hash is order-independent (episodes sort by index, streams by name). After
    // `canonicalize_order`, the verdict and its `result_content_hash` must be too: two datasets that
    // differ only in episode/stream `Vec` order produce byte-identical verdicts. Uses a shape
    // mismatch across episodes — an order-sensitive check (its "baseline" was first-seen) — as the probe.
    let build = |episodes: Vec<Episode>| {
        let mut d = dataset(episodes);
        d.canonicalize_order();
        d
    };
    let ep0 = episode(
        0,
        vec![
            shaped("action", Some("float32"), Some(vec![6]), &[0]),
            shaped("observation.state", Some("float32"), Some(vec![6]), &[0]),
        ],
    );
    let ep1 = episode(
        1,
        vec![
            shaped("observation.state", Some("float32"), Some(vec![7]), &[0]),
            shaped("action", Some("float32"), Some(vec![6]), &[0]),
        ],
    );
    // Same content, opposite episode order and opposite stream order within an episode.
    let a = build(vec![ep0.clone(), ep1.clone()]);
    let b = build(vec![ep1, ep0]);

    let engine = veridex_core::checks::default_engine().unwrap();
    let cfg = veridex_core::RunConfig::default();
    let va = engine.run(&a, veridex_core::content_hash(&a), &cfg);
    let vb = engine.run(&b, veridex_core::content_hash(&b), &cfg);

    assert_eq!(
        va.result_content_hash, vb.result_content_hash,
        "canonicalized order must yield identical verdict hashes"
    );
    assert_eq!(
        veridex_core::render_json(&va, None),
        veridex_core::render_json(&vb, None),
        "canonicalized order must yield byte-identical report JSON"
    );
}

#[test]
fn non_finite_tolerances_are_sanitized_to_defaults() {
    // A direct library caller can build a Tolerances with a non-finite field; that would serialize to
    // JSON `null` (breaking certificate round-trip) and silently disable the guarding check. The
    // sanitizer replaces it with the finite default.
    let dirty = veridex_core::engine::Tolerances {
        rate_deviation: f64::NAN,
        gap_factor: f64::INFINITY,
        ..veridex_core::engine::Tolerances::default()
    };
    let clean = dirty.finite_or_default();
    let d = veridex_core::engine::Tolerances::default();
    assert_eq!(clean.rate_deviation, d.rate_deviation);
    assert_eq!(clean.gap_factor, d.gap_factor);
    assert!(clean.rate_deviation.is_finite() && clean.gap_factor.is_finite());
}

// ---- autonomy: rig-wide sync ----

/// A rig-sensor stream of a given modality spanning `span_ns` from t=0.
/// A rig sensor sampling at 100 Hz across `span_ns`. The cadence matters: a span comparison cannot
/// resolve drift smaller than a stream's own sampling period, so a two-frame stream spanning a second
/// is a 1 Hz sensor whose span is meaningless to a 50 ms tolerance (see `temporal::sampling_period_ns`).
fn rig_stream(name: &str, modality: Modality, span_ns: i64) -> Stream {
    const STEP_NS: i64 = 10_000_000; // 100 Hz
    let ts: Vec<i64> = (0..=(span_ns / STEP_NS)).map(|i| i * STEP_NS).collect();
    let mut s = stream(name, "rig", None, &ts);
    s.modality = modality;
    s
}

#[test]
fn a_rig_with_a_drifting_sensor_is_flagged_once() {
    // Three AV-native rig sensors: LiDAR and GNSS span 1.0 s, the IMU only 0.7 s (0.3 s drift).
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 700_000_000),
        ],
    );
    let f = autonomy::RigSync::default().run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1, "one rig-wide finding, not pairwise");
    assert_eq!(f[0].code, "AUTONOMY.RIG_SYNC");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("imu"), "names the drifted sensor");
}

#[test]
fn a_synced_rig_is_clean() {
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    assert!(autonomy::RigSync::default()
        .run(&dataset(vec![ep]))
        .is_empty());
}

#[test]
fn too_few_rig_sensors_is_not_a_rig() {
    // Only two AV-native sensors — below the rig threshold, so the rig check abstains entirely
    // (the pairwise TEMPORAL.CLOCK_SKEW still covers this case).
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 700_000_000),
        ],
    );
    assert!(autonomy::RigSync::default()
        .run(&dataset(vec![ep]))
        .is_empty());
}

#[test]
fn a_manipulation_dataset_is_never_a_rig() {
    // Video + scalar-state streams with a big duration drift: RigSync must abstain (not a rig), while
    // the pairwise ClockSkew still fires — manipulation behavior is unchanged.
    let ep = episode(
        0,
        vec![
            rig_stream("cam", Modality::Video, 1_000_000_000),
            rig_stream("state", Modality::ScalarState, 1_500_000_000),
            rig_stream("action", Modality::Action, 1_000_000_000),
        ],
    );
    let d = dataset(vec![ep]);
    assert!(autonomy::RigSync::default().run(&d).is_empty());
    assert!(
        temporal::ClockSkew::default()
            .run(&d)
            .iter()
            .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"),
        "pairwise clock-skew still applies to a manipulation dataset"
    );
}

// ---- autonomy: sequence completeness ----

/// A rig-sensor stream of a given modality at explicit frame timestamps.
fn rig_stream_ts(name: &str, modality: Modality, ts: &[i64]) -> Stream {
    let mut s = stream(name, "rig", None, ts);
    s.modality = modality;
    s
}

#[test]
fn a_dropped_frame_sensor_is_flagged_incomplete() {
    // 20 nominal ticks at 100 ms (0..1900 ms). LiDAR and GNSS are complete; the IMU is missing 5 of
    // its 20 frames (25% drop), spread out so no single gap is huge and the median stays ~100 ms.
    let full: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let dropped: Vec<i64> = full
        .iter()
        .enumerate()
        .filter(|(i, _)| ![3usize, 7, 11, 15, 17].contains(i))
        .map(|(_, t)| *t)
        .collect();
    let ep = episode(
        0,
        vec![
            rig_stream_ts("lidar", Modality::PointCloud, &full),
            rig_stream_ts("gnss", Modality::Gnss, &full),
            rig_stream_ts("imu", Modality::Imu, &dropped),
        ],
    );
    let f = autonomy::SequenceComplete::default().run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1, "only the dropping sensor is flagged");
    assert_eq!(f[0].code, "AUTONOMY.SEQUENCE_COMPLETE");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("imu"), "names the incomplete sensor");
}

#[test]
fn a_complete_rig_sequence_is_clean() {
    let full: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let ep = episode(
        0,
        vec![
            rig_stream_ts("lidar", Modality::PointCloud, &full),
            rig_stream_ts("gnss", Modality::Gnss, &full),
            rig_stream_ts("imu", Modality::Imu, &full),
        ],
    );
    assert!(autonomy::SequenceComplete::default()
        .run(&dataset(vec![ep]))
        .is_empty());
}

#[test]
fn sequence_completeness_only_runs_on_rigs() {
    // A manipulation dataset with a dropping stream is not a rig, so the check abstains.
    let full: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let dropped: Vec<i64> = full.iter().step_by(2).copied().collect();
    let ep = episode(
        0,
        vec![
            rig_stream_ts("cam", Modality::Video, &full),
            rig_stream_ts("state", Modality::ScalarState, &dropped),
            rig_stream_ts("action", Modality::Action, &full),
        ],
    );
    assert!(autonomy::SequenceComplete::default()
        .run(&dataset(vec![ep]))
        .is_empty());
}

// ---- autonomy: ego-pose continuity ----

fn ego(ts: i64, x: f64, y: f64) -> veridex_core::cdm::EgoPose {
    veridex_core::cdm::EgoPose {
        ts,
        pose: veridex_core::cdm::Pose {
            translation: [x, y, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
    }
}

/// A rig episode carrying the given ego trajectory (three rig sensors present just so it's a rig,
/// though EgoPoseContinuity itself only needs `ego_poses`).
fn rig_episode_with_ego(poses: Vec<veridex_core::cdm::EgoPose>) -> Episode {
    let mut ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    ep.ego_poses = Some(poses);
    ep
}

#[test]
fn a_teleporting_ego_trajectory_is_flagged() {
    // Smooth ~1 m/s for two steps, then a 500 m jump in 100 ms (5000 m/s) — a teleport.
    let poses = vec![
        ego(0, 0.0, 0.0),
        ego(100_000_000, 0.1, 0.0),
        ego(200_000_000, 0.2, 0.0),
        ego(300_000_000, 500.2, 0.0),
    ];
    let f = autonomy::EgoPoseContinuity::default().run(&dataset(vec![rig_episode_with_ego(poses)]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "AUTONOMY.EGO_POSE_CONTINUITY");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn a_smooth_ego_trajectory_is_clean() {
    // Steady 10 m/s (1 m per 100 ms) — well under the 100 m/s ceiling.
    let poses: Vec<_> = (0..10)
        .map(|i| ego(i * 100_000_000, i as f64, 0.0))
        .collect();
    assert!(autonomy::EgoPoseContinuity::default()
        .run(&dataset(vec![rig_episode_with_ego(poses)]))
        .is_empty());
}

#[test]
fn ego_pose_continuity_abstains_without_a_trajectory() {
    // A rig with no ego_poses: nothing to check.
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    assert!(autonomy::EgoPoseContinuity::default()
        .run(&dataset(vec![ep]))
        .is_empty());
}

// ---- autonomy: calibration completeness ----

fn xf(parent: &str, child: &str) -> veridex_core::cdm::Transform {
    veridex_core::cdm::Transform {
        parent_frame: parent.into(),
        child_frame: child.into(),
        pose: veridex_core::cdm::Pose {
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
        valid_from: None,
        valid_to: None,
    }
}

fn intr(stream: &str) -> veridex_core::cdm::CameraIntrinsics {
    veridex_core::cdm::CameraIntrinsics {
        stream: stream.into(),
        fx: 600.0,
        fy: 600.0,
        cx: 320.0,
        cy: 240.0,
        distortion: vec![],
        valid_from: None,
        valid_to: None,
    }
}

/// A rig (lidar+gnss+imu = 3 AV-native) plus a camera, with the given calibration.
fn rig_with_calibration(cal: Option<veridex_core::cdm::Calibration>) -> Dataset {
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
            rig_stream("cam", Modality::Video, 1_000_000_000),
        ],
    );
    let mut d = dataset(vec![ep]);
    d.calibration = cal;
    d
}

#[test]
fn a_rig_without_calibration_is_flagged() {
    let d = rig_with_calibration(None);
    let f = autonomy::CalibrationCompleteness.run(&d);
    assert!(f
        .iter()
        .any(|x| x.code == "AUTONOMY.CALIBRATION_INCOMPLETE"));
    assert!(f[0].message.contains("no transform"));
}

#[test]
fn a_fully_calibrated_rig_is_clean() {
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let d = rig_with_calibration(Some(cal));
    assert!(autonomy::CalibrationCompleteness.run(&d).is_empty());
}

#[test]
fn a_disconnected_transform_tree_is_flagged() {
    // Two components: {base_link, lidar} and {map, cam}.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("map", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let d = rig_with_calibration(Some(cal));
    let f = autonomy::CalibrationCompleteness.run(&d);
    assert!(f.iter().any(|x| x.message.contains("disconnected")));
}

#[test]
fn a_rig_camera_without_intrinsics_is_flagged() {
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![], // no CameraInfo
    };
    let d = rig_with_calibration(Some(cal));
    let f = autonomy::CalibrationCompleteness.run(&d);
    assert!(f.iter().any(|x| x.message.contains("no camera intrinsics")));
}

#[test]
fn a_bus_only_measurement_is_not_treated_as_a_sensor_rig() {
    // A CAN or MF4 log is dozens of `CanSignal` streams off one bus, not several sensors observing
    // the world from different places. Treating it as a rig made ordinary raster differences (a 1 Hz
    // group vs a 100 Hz group over one measurement) read as cross-sensor clock drift.
    use veridex_core::cdm::Modality;
    let ep = |modalities: &[Modality]| veridex_core::cdm::Episode {
        index: 0,
        start_ts: Some(0),
        end_ts: Some(1_000_000_000),
        streams: modalities
            .iter()
            .enumerate()
            .map(|(i, &m)| veridex_core::cdm::Stream {
                name: format!("s{i}"),
                modality: m,
                declared_rate_hz: None,
                clock_id: "c".into(),
                clock_kind: ClockKind::Measured,
                dtype: None,
                shape: None,
                frames: vec![],
                stats: None,
                dim_stats: None,
                observed_stats: None,
                observed_saturation: None,
                observed_non_finite: None,
                observed_dim_stats: None,
                point_fields: None,
                media: None,
                frame_id: None,
            })
            .collect(),
        task: None,
        labels: vec![],
        ego_poses: None,
        declared_frame_count: None,
    };
    let is_rig = veridex_core::checks::autonomy::is_rig_episode;

    assert!(
        !is_rig(&ep(&[
            Modality::CanSignal,
            Modality::CanSignal,
            Modality::CanSignal,
            Modality::CanSignal
        ])),
        "one bus is not a rig, however many signals it carries"
    );
    // A real rig always mixes modalities, and is unaffected.
    assert!(is_rig(&ep(&[
        Modality::PointCloud,
        Modality::Imu,
        Modality::Gnss
    ])));
    assert!(is_rig(&ep(&[
        Modality::CanSignal,
        Modality::CanSignal,
        Modality::EgoPose
    ])));
}

#[test]
fn one_shared_timeline_reports_once_and_an_event_driven_signal_is_not_called_incomplete() {
    // An MF4 channel group samples every channel on one raster, and a CAN message decodes into many
    // signals off the same frames: their timing is one fact, not N. And a change-triggered signal has
    // no cadence to fall short of, so a complete log must not read as 88% dropped.
    use veridex_core::cdm::{Dataset, Episode, Frame, Modality, Stream, ValueRef};
    use veridex_core::check::Check;

    // Bursts of 20 samples 10 ms apart, separated by 2 s idles — a normal on-change log.
    let mut ts = Vec::new();
    let mut t = 0i64;
    for _ in 0..4 {
        for _ in 0..20 {
            ts.push(t);
            t += 10_000_000;
        }
        t += 2_000_000_000;
    }
    let stream = |name: &str| Stream {
        name: name.into(),
        modality: Modality::CanSignal,
        declared_rate_hz: None,
        clock_id: "bus".into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        frames: ts
            .iter()
            .map(|&ts| Frame {
                ts,
                value_ref: ValueRef {
                    uri: "u".into(),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: None,
                },
            })
            .collect(),
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: None,
    };
    let dataset = Dataset {
        id: "bus".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(t),
            streams: (0..8).map(|i| stream(&format!("sig{i}"))).collect(),
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
        }],
        calibration: None,
    };

    // Eight streams, one timeline: each timing finding is reported once, naming the others.
    let gaps = veridex_core::checks::temporal::Gaps::default().run(&dataset);
    assert!(
        !gaps.is_empty(),
        "the idles are still real gaps worth reporting"
    );
    let per_stream: std::collections::BTreeSet<&str> = gaps
        .iter()
        .filter_map(|f| match &f.location {
            veridex_core::check::Location::TimeRange { stream, .. } => Some(stream.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        per_stream.len(),
        1,
        "one timeline must not produce one finding per stream: {gaps:#?}"
    );
    assert!(
        gaps[0]
            .message
            .contains("other stream(s) on the same timeline"),
        "{}",
        gaps[0].message
    );

    let jitter = veridex_core::checks::temporal::Jitter::default().run(&dataset);
    assert!(
        jitter.len() <= 1,
        "jitter is one fact per timeline too: {jitter:#?}"
    );

    // And the burst pattern is not a "dropped frames" claim.
    let seq = veridex_core::checks::autonomy::SequenceComplete::default().run(&dataset);
    assert!(
        seq.is_empty(),
        "an event-driven signal has no cadence to fall short of: {seq:#?}"
    );
}

#[test]
fn corrupt_stats_in_a_later_episode_are_not_masked_by_a_clean_earlier_one() {
    // Stored stats are dataset-level today, so RangeSanity reports each stream once. It must claim
    // the stream when it finds something, not when it first *sees* it: a clean episode 0 followed by
    // a corrupt episode 1 (which per-episode stats would produce) must still be reported.
    let d = dataset(vec![
        episode(0, vec![stream_with_stats("s", stats(0.0, 1.0, 0.5, 0.2))]),
        episode(1, vec![stream_with_stats("s", stats(5.0, 1.0, 3.0, 1.0))]),
    ]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1, "the corrupt episode must be reported: {f:?}");
    assert_eq!(f[0].code, "STATISTICAL.RANGE_INVERTED");
    // And it is attributed to the episode it was actually found in.
    assert!(
        format!("{:?}", f[0].location).contains('1'),
        "expected episode 1, got {:?}",
        f[0].location
    );
}

#[test]
fn the_configured_autonomy_tolerances_reach_the_checks() {
    // The autonomy thresholds are config-wired like every other family: an engine built at a looser
    // ego-speed ceiling must stop flagging a trajectory the default ceiling rejects.
    let poses = vec![
        ego(0, 0.0, 0.0),
        ego(100_000_000, 0.1, 0.0),
        ego(200_000_000, 50.1, 0.0), // 500 m/s
    ];
    let d = dataset(vec![rig_episode_with_ego(poses)]);
    let run_at = |ego_max_speed_mps: f64| {
        let tolerances = veridex_core::Tolerances {
            ego_max_speed_mps,
            ..Default::default()
        };
        let engine = veridex_core::checks::default_engine_with(&tolerances)
            .expect("standard checks have unique ids");
        let hash = veridex_core::content_hash(&d);
        let verdict = engine.run(
            &d,
            hash,
            &veridex_core::RunConfig {
                tolerances,
                ..Default::default()
            },
        );
        verdict
            .findings
            .iter()
            .filter(|f| f.code == "AUTONOMY.EGO_POSE_CONTINUITY")
            .count()
    };
    assert_eq!(run_at(100.0), 1, "500 m/s exceeds the default 100 m/s");
    assert_eq!(run_at(600.0), 0, "a 600 m/s ceiling tolerates it");
}

// ---- regression: span comparisons must allow for each stream's own sampling period ----

/// Timestamps for a sensor of period `period_ns` observing a window of `window_ns`.
fn sampled(period_ns: i64, window_ns: i64) -> Vec<i64> {
    (0..=(window_ns / period_ns))
        .map(|i| i * period_ns)
        .collect()
}

#[test]
fn a_synchronized_multi_rate_rig_is_clean() {
    // A perfectly synchronized rig with zero drift: LiDAR 10 Hz, IMU 100 Hz, GNSS 5 Hz, all observing
    // the same ~30 s window. Each sensor's span quantizes to its own period, so the raw spans differ
    // by up to 200 ms with nothing wrong — which the check used to report as a 200 ms "drift".
    let window = 30_070_000_000;
    let ep = episode(
        0,
        vec![
            rig_stream_ts("lidar", Modality::PointCloud, &sampled(100_000_000, window)),
            rig_stream_ts("imu", Modality::Imu, &sampled(10_000_000, window)),
            rig_stream_ts("gnss", Modality::Gnss, &sampled(200_000_000, window)),
        ],
    );
    let f = autonomy::RigSync::default().run(&dataset(vec![ep]));
    assert!(
        f.is_empty(),
        "a synchronized rig must not be flagged: {f:?}"
    );
}

#[test]
fn a_multi_rate_pair_is_not_reported_as_clock_skew() {
    // 30 fps camera against a 10 Hz state stream over one window — the ordinary manipulation shape.
    let window = 30_070_000_000;
    let d = dataset(vec![episode(
        0,
        vec![
            stream("cam", "c", None, &sampled(33_000_000, window)),
            stream("state", "c", None, &sampled(100_000_000, window)),
        ],
    )]);
    assert!(
        temporal::ClockSkew::default().run(&d).is_empty(),
        "differing sample rates are not clock skew"
    );
}

#[test]
fn a_slow_sensor_that_really_drifts_is_still_flagged() {
    // The allowance is one sampling period, not a blank cheque: a 10 Hz LiDAR cut half a second short
    // is still well past it.
    let ep = episode(
        0,
        vec![
            rig_stream_ts(
                "lidar",
                Modality::PointCloud,
                &sampled(100_000_000, 9_500_000_000),
            ),
            rig_stream_ts("imu", Modality::Imu, &sampled(10_000_000, 10_000_000_000)),
            rig_stream_ts(
                "gnss",
                Modality::Gnss,
                &sampled(200_000_000, 10_000_000_000),
            ),
        ],
    );
    let f = autonomy::RigSync::default().run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1, "a real 500 ms drift must still be flagged");
    assert!(f[0].message.contains("lidar"));
}

// ---- regression: one root cause, one finding ----

#[test]
fn one_stuck_clock_on_a_shared_timeline_is_one_monotonicity_finding() {
    // Eight CAN channels off one bus share a timeline; a single repeated timestamp is one defect, not
    // eight Errors (which cost 8 x 15 points and floored the data score).
    let ts = [0i64, 10_000_000, 10_000_000, 20_000_000];
    let streams: Vec<_> = (0..8)
        .map(|i| stream(&format!("sig{i}"), "can", None, &ts))
        .collect();
    let f = temporal::Monotonicity.run(&dataset(vec![episode(0, streams)]));
    assert_eq!(f.len(), 1, "one shared-timeline defect, one finding: {f:?}");
    assert!(f[0]
        .message
        .contains("other stream(s) on the same timeline"));
}

#[test]
fn an_ambiguous_stream_key_is_reported_once_for_the_dataset() {
    // One naming mistake repeated across every episode is one mistake. Counts must not scale with
    // episode count (50 episodes once produced 100 warnings).
    let count_for = |episodes: u64| {
        let eps: Vec<_> = (0..episodes)
            .map(|i| {
                episode(
                    i,
                    vec![
                        stream("Gripper", "c", None, &[0, 1_000_000]),
                        stream("gripper", "c", None, &[0, 1_000_000]),
                    ],
                )
            })
            .collect();
        semantic::StreamKeyClarity
            .run(&dataset(eps))
            .iter()
            .filter(|f| f.code == "SEMANTIC.AMBIGUOUS_STREAM_KEY")
            .count()
    };
    assert_eq!(
        count_for(1),
        count_for(50),
        "findings must not scale with episodes"
    );
}

// ---- regression: sequence completeness counts real holes, not idle time ----

#[test]
fn an_event_driven_stream_with_every_event_present_is_not_called_incomplete() {
    // A change-triggered CAN channel: 40 intervals of 80 ms and 10 of 200 ms, every event present.
    // Its interval CV (0.46) slips under the uniformity guard, and dividing the span by the median
    // charged the idle stretches as ~23% dropped frames. 200 ms is no multiple of 80 ms, so nothing
    // was swallowed.
    let mut ts = vec![0i64];
    for i in 0..50 {
        let step = if i % 5 == 4 { 200_000_000 } else { 80_000_000 };
        ts.push(ts[ts.len() - 1] + step);
    }
    let ep = episode(
        0,
        vec![
            rig_stream_ts("can", Modality::CanSignal, &ts),
            rig_stream_ts("imu", Modality::Imu, &sampled(10_000_000, 5_000_000_000)),
            rig_stream_ts("gnss", Modality::Gnss, &sampled(200_000_000, 5_000_000_000)),
        ],
    );
    let f = autonomy::SequenceComplete::default().run(&dataset(vec![ep]));
    assert!(
        !f.iter().any(|f| f.message.contains("`can`")),
        "a complete event-driven stream is not incomplete: {f:?}"
    );
}

#[test]
fn a_steady_sensor_dropping_frames_is_still_flagged() {
    // 100 ms cadence with 5 frames missing out of 20: each hole is exactly two periods wide, so the
    // multiples estimator sees them.
    let full: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let dropped: Vec<i64> = full
        .iter()
        .enumerate()
        .filter(|(i, _)| ![3usize, 7, 11, 15, 17].contains(i))
        .map(|(_, t)| *t)
        .collect();
    let ep = episode(
        0,
        vec![
            rig_stream_ts("imu", Modality::Imu, &dropped),
            rig_stream_ts("lidar", Modality::PointCloud, &full),
            rig_stream_ts("gnss", Modality::Gnss, &full),
        ],
    );
    let f = autonomy::SequenceComplete::default().run(&dataset(vec![ep]));
    assert_eq!(
        f.len(),
        1,
        "the dropping sensor must still be flagged: {f:?}"
    );
    assert!(f[0].message.contains("imu"));
}

// ---- regression: float-noise statistics ----

#[test]
fn a_constant_stream_whose_std_is_float_noise_is_degenerate_not_impossible() {
    // An exporter computing std as E[x²] - E[x]² on values near 0.7 loses ~1e-8 to cancellation. That
    // is a constant stream reported honestly, not a mathematically impossible standard deviation.
    for std in [0.0, 1e-12, 1e-8] {
        let d = dataset(vec![episode(
            0,
            vec![stream_with_stats("s", stats(0.7, 0.7, 0.7, std))],
        )]);
        let f = statistical::RangeSanity.run(&d);
        assert_eq!(
            f.len(),
            1,
            "std {std}: expected exactly one finding, got {f:?}"
        );
        assert_eq!(f[0].code, "STATISTICAL.DEGENERATE", "std {std}");
        assert_eq!(f[0].severity, Severity::Warning, "std {std}");
    }
}

#[test]
fn a_near_constant_channel_at_a_large_magnitude_is_not_impossible() {
    // min 300.0, max 300.0002 — a Popoviciu bound of 1e-4, against an f32-computed std of 3e-4. The
    // tolerance has to scale with the magnitude of the values, not the width of their range.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats(
            "s",
            stats(300.0, 300.0002, 300.0001, 3e-4),
        )],
    )]);
    assert!(
        statistical::RangeSanity.run(&d).is_empty(),
        "honest float noise at magnitude 300 is not an impossible std"
    );
}

#[test]
fn a_genuinely_impossible_std_is_still_an_error() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, 1.0, 0.5, 5.0))],
    )]);
    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.STD_IMPLAUSIBLE");
}

#[test]
fn a_saturated_later_episode_is_not_masked_by_a_clean_earlier_one() {
    // Saturation reports each stream once, but it must claim the stream when it finds something, not
    // when it first sees it — otherwise the finding depends on episode order.
    let sat = |at_max: u64| {
        let mut s = stream("gripper", "c", None, &[0, 1_000_000]);
        s.observed_saturation = Some(veridex_core::cdm::Saturation {
            min: 0.0,
            max: 1.0,
            at_min: 0,
            at_max,
            sample_count: 100,
            dim: 0,
        });
        s
    };
    let clean_first = dataset(vec![episode(0, vec![sat(1)]), episode(1, vec![sat(95)])]);
    let saturated_first = dataset(vec![episode(0, vec![sat(95)]), episode(1, vec![sat(1)])]);
    let count = |d| statistical::Saturation::default().run(d).len();
    assert_eq!(
        count(&clean_first),
        1,
        "the saturated episode must be reported"
    );
    assert_eq!(
        count(&clean_first),
        count(&saturated_first),
        "the finding must not depend on episode order"
    );
}

// ---------------------------------------------------------------------------------------------
// autonomy.sensor-frame-resolution — per-sensor calibration resolution (A2)
// ---------------------------------------------------------------------------------------------

/// The `rig_with_calibration` rig, with a coordinate frame attached to each named stream.
fn rig_with_frames(
    cal: Option<veridex_core::cdm::Calibration>,
    frames: &[(&str, &str)],
) -> Dataset {
    let mut d = rig_with_calibration(cal);
    for s in &mut d.episodes[0].streams {
        if let Some((_, frame)) = frames.iter().find(|(name, _)| *name == s.name) {
            s.frame_id = Some((*frame).to_string());
        }
    }
    d
}

fn wired_rig() -> veridex_core::cdm::Calibration {
    veridex_core::cdm::Calibration {
        transforms: vec![
            xf("base_link", "lidar_top"),
            xf("base_link", "camera_front"),
            xf("base_link", "imu_link"),
            xf("base_link", "gnss_link"),
        ],
        intrinsics: vec![intr("cam")],
    }
}

#[test]
fn a_sensor_whose_frame_is_absent_from_the_tree_is_named() {
    // The tree is well-formed and fully connected — it was simply recorded for a different LiDAR
    // frame name. Nothing about the tree's shape reveals this.
    let d = rig_with_frames(
        Some(wired_rig()),
        &[
            ("lidar", "lidar_top_v2"),
            ("cam", "camera_front"),
            ("imu", "imu_link"),
            ("gnss", "gnss_link"),
        ],
    );
    let f = autonomy::SensorFrameResolution.run(&d);
    assert_eq!(f.len(), 1, "one finding, naming the one stranded sensor");
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_FRAME_UNKNOWN");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("lidar_top_v2"), "{}", f[0].message);
    assert!(matches!(
        &f[0].location,
        veridex_core::check::Location::Stream { episode: 0, stream } if stream == "lidar"
    ));
}

#[test]
fn a_sensor_with_no_transform_path_to_the_camera_is_named() {
    // The LiDAR is in the tree, under a mount frame nothing joins to the camera's subtree.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("lidar_mount", "lidar_top"),
            xf("base_link", "camera_front"),
        ],
        intrinsics: vec![intr("cam")],
    };
    let d = rig_with_frames(
        Some(cal),
        &[
            ("lidar", "lidar_top"),
            ("cam", "camera_front"),
            ("imu", "base_link"),
            ("gnss", "base_link"),
        ],
    );
    let f = autonomy::SensorFrameResolution.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_FRAME_UNRELATED");
    assert!(f[0].message.contains("camera_front"), "{}", f[0].message);
}

#[test]
fn a_correctly_wired_rig_is_clean() {
    let d = rig_with_frames(
        Some(wired_rig()),
        &[
            ("lidar", "lidar_top"),
            ("cam", "camera_front"),
            ("imu", "imu_link"),
            ("gnss", "gnss_link"),
        ],
    );
    assert!(autonomy::SensorFrameResolution.run(&d).is_empty());
}

#[test]
fn a_sensor_that_declares_no_frame_is_named_not_passed_over() {
    // A rig that recorded a transform tree, beside sensors that never say which frame they are in —
    // what an unconfigured ROS driver publishing an empty `header.frame_id` produces. Skipping them
    // silently made this check find nothing, which made the `world-model-ready` criterion it backs
    // read as satisfied: a signed certificate attesting "every sensor's own frame resolves through
    // the tree to a camera" over a rig where not one sensor said where it was.
    let d = rig_with_frames(Some(wired_rig()), &[("cam", "camera_front")]);
    let f = autonomy::SensorFrameResolution.run(&d);
    let named: Vec<&str> = f
        .iter()
        .filter(|f| f.code == "AUTONOMY.SENSOR_FRAME_UNDECLARED")
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(
        named.len(),
        3,
        "lidar, imu, and gnss each declare no frame: {f:?}"
    );
    assert!(named.iter().any(|m| m.contains("`lidar`")), "{named:?}");
    // The camera did declare one, so it is not accused of anything.
    assert!(!named.iter().any(|m| m.contains("`cam`")), "{named:?}");
}

#[test]
fn a_rig_that_declares_no_frames_at_all_is_not_flagged_per_sensor() {
    // MF4 and CAN rigs record no coordinate frames *and* no transform tree. With no tree there is
    // no calibration to be missing from, so this check stays silent and
    // `autonomy.calibration-completeness` reports the one real defect once.
    let d = rig_with_frames(None, &[]);
    assert!(autonomy::SensorFrameResolution.run(&d).is_empty());
}

#[test]
fn connectivity_abstains_when_no_camera_names_a_known_frame() {
    // Without a camera frame in the tree there is no reference to measure a path against, so no
    // stream is reported as *unrelated* to a camera. The undeclared-frame half still speaks.
    let d = rig_with_frames(Some(wired_rig()), &[("lidar", "lidar_top")]);
    let f = autonomy::SensorFrameResolution.run(&d);
    assert!(
        !f.iter()
            .any(|f| f.code == "AUTONOMY.SENSOR_FRAME_UNRELATED"),
        "{f:?}"
    );
}

#[test]
fn a_rig_with_no_transform_tree_is_left_to_the_calibration_check() {
    // "No tree at all" is one defect. `autonomy.calibration-completeness` reports it; charging it
    // again, once per sensor, would bury the single actionable line under five copies.
    let d = rig_with_frames(None, &[("lidar", "lidar_top"), ("cam", "camera_front")]);
    assert!(autonomy::SensorFrameResolution.run(&d).is_empty());
    assert!(autonomy::CalibrationCompleteness
        .run(&d)
        .iter()
        .any(|x| x.code == "AUTONOMY.CALIBRATION_INCOMPLETE"));
}

#[test]
fn the_disconnected_tree_is_reported_once_at_the_finest_granularity_available() {
    // When the sensors name their frames, the per-sensor check names the stranded one and the
    // episode-level component count stays quiet; when they do not, the episode-level report is the
    // only thing that can speak, so it does.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("lidar_mount", "lidar_top"),
            xf("base_link", "camera_front"),
        ],
        intrinsics: vec![intr("cam")],
    };

    let named = rig_with_frames(
        Some(cal.clone()),
        &[
            ("lidar", "lidar_top"),
            ("cam", "camera_front"),
            ("imu", "base_link"),
            ("gnss", "base_link"),
        ],
    );
    assert!(
        !autonomy::CalibrationCompleteness
            .run(&named)
            .iter()
            .any(|x| x.message.contains("disconnected")),
        "the per-sensor check has this one"
    );

    let unnamed = rig_with_calibration(Some(cal));
    assert!(
        autonomy::CalibrationCompleteness
            .run(&unnamed)
            .iter()
            .any(|x| x.message.contains("disconnected")),
        "with no sensor frames, the episode-level report is all there is"
    );
}

/// A disconnected tree: `{lidar_mount, lidar_top}` apart from `{base_link, camera_front, imu_link,
/// gnss_link}`.
fn split_rig() -> veridex_core::cdm::Calibration {
    veridex_core::cdm::Calibration {
        transforms: vec![
            xf("lidar_mount", "lidar_top"),
            xf("base_link", "camera_front"),
            xf("base_link", "imu_link"),
            xf("base_link", "gnss_link"),
        ],
        intrinsics: vec![intr("cam")],
    }
}

/// Whether either autonomy calibration check says anything at all about `d`.
fn any_calibration_finding(d: &Dataset) -> Vec<String> {
    let mut codes: Vec<String> = autonomy::CalibrationCompleteness
        .run(d)
        .iter()
        .map(|f| f.code.clone())
        .collect();
    codes.extend(
        autonomy::SensorFrameResolution
            .run(d)
            .iter()
            .map(|f| f.code.clone()),
    );
    codes
}

#[test]
fn a_disconnected_tree_is_never_reported_by_neither_check() {
    // The supersession hazard, as four concrete rig shapes. `calibration-completeness` may only stay
    // silent about a broken tree when `sensor-frame-resolution` can actually name the stranded
    // sensors. Every shape below defeats one of the successor's preconditions, so the episode-level
    // report has to survive — a break reported by NEITHER check is the worst possible outcome, worse
    // than reporting it twice.
    let shapes: &[(&str, &[(&str, &str)])] = &[
        // (a) the stranded sensor is the one that declares no frame.
        (
            "stranded sensor declares no frame",
            &[
                ("cam", "camera_front"),
                ("imu", "imu_link"),
                ("gnss", "gnss_link"),
            ],
        ),
        // (b) no camera frame, so the connectivity half has nothing to measure against.
        (
            "no camera frame to anchor connectivity",
            &[
                ("lidar", "lidar_top"),
                ("imu", "imu_link"),
                ("gnss", "gnss_link"),
            ],
        ),
        // (c) the camera names a frame the tree does not know.
        (
            "camera frame unknown to the tree",
            &[
                ("lidar", "lidar_top"),
                ("cam", "camera_unlisted"),
                ("imu", "imu_link"),
                ("gnss", "gnss_link"),
            ],
        ),
    ];
    for (name, frames) in shapes {
        let d = rig_with_frames(Some(split_rig()), frames);
        let codes = any_calibration_finding(&d);
        assert!(
            !codes.is_empty(),
            "{name}: the disconnected tree must be reported by someone, got nothing"
        );
    }
}

#[test]
fn a_rig_with_no_camera_at_all_still_reports_a_broken_tree() {
    // (d) A LiDAR-only rig has no reprojection target, so the per-sensor check cannot speak about
    // connectivity at all — the episode-level report is the only warning there is.
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    let mut d = dataset(vec![ep]);
    d.calibration = Some(split_rig());
    for s in &mut d.episodes[0].streams {
        s.frame_id = match s.name.as_str() {
            "lidar" => Some("lidar_top".into()),
            "imu" => Some("imu_link".into()),
            "gnss" => Some("gnss_link".into()),
            _ => None,
        };
    }
    assert!(
        autonomy::CalibrationCompleteness
            .run(&d)
            .iter()
            .any(|f| f.message.contains("disconnected")),
        "a camera-less rig's broken tree is still reported"
    );
}

#[test]
fn one_mis_stamped_sensor_is_one_finding_however_many_episodes_it_spans() {
    // The calibration is dataset-level and stream names repeat per episode, so a 50-episode drive log
    // with one mis-stamped LiDAR is one defect — not fifty error-severity copies of it.
    let episodes: Vec<_> = (0..50)
        .map(|i| {
            episode(
                i,
                vec![
                    rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
                    rig_stream("gnss", Modality::Gnss, 1_000_000_000),
                    rig_stream("imu", Modality::Imu, 1_000_000_000),
                    rig_stream("cam", Modality::Video, 1_000_000_000),
                ],
            )
        })
        .collect();
    let mut d = dataset(episodes);
    d.calibration = Some(wired_rig());
    for ep in &mut d.episodes {
        for s in &mut ep.streams {
            s.frame_id = match s.name.as_str() {
                "lidar" => Some("lidar_top_v2".into()), // the one defect
                "cam" => Some("camera_front".into()),
                "imu" => Some("imu_link".into()),
                "gnss" => Some("gnss_link".into()),
                _ => None,
            };
        }
    }
    let f = autonomy::SensorFrameResolution.run(&d);
    assert_eq!(f.len(), 1, "one defect, one finding: {:?}", f.len());
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_FRAME_UNKNOWN");
}

#[test]
fn a_bus_signal_is_not_asked_to_reach_the_camera() {
    // A CAN signal is a scalar, never projected into an image, and an ego-pose frame is joined to the
    // body dynamically rather than by the static TF tree. Demanding a static chain from either would
    // flag honest rigs — a decoded DBC alone can contribute dozens of `CanSignal` streams.
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("cam", Modality::Video, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
            rig_stream("vehicle_speed", Modality::CanSignal, 1_000_000_000),
            rig_stream("odom", Modality::EgoPose, 1_000_000_000),
        ],
    );
    let mut d = dataset(vec![ep]);
    d.calibration = Some(wired_rig());
    for s in &mut d.episodes[0].streams {
        s.frame_id = match s.name.as_str() {
            "lidar" => Some("lidar_top".into()),
            "cam" => Some("camera_front".into()),
            "imu" => Some("imu_link".into()),
            "vehicle_speed" => Some("chassis".into()), // never in a TF tree
            "odom" => Some("odom".into()),             // joined dynamically, not statically
            _ => None,
        };
    }
    assert!(
        autonomy::SensorFrameResolution.run(&d).is_empty(),
        "a bus signal and an ego-pose frame are not reprojection targets"
    );
}

#[test]
fn an_enormous_episode_index_span_is_summarized_not_enumerated() {
    // Both bounds come from the file, so their span is attacker-controlled. Two episodes numbered
    // `0` and `u64::MAX` made this check walk `0..=u64::MAX` and collect the misses into a `Vec`:
    // not a slow check, a process that never returned and allocated until it was killed. Two lines
    // of a LeRobot `meta/episodes.jsonl` are enough to write it. The gap *count* is arithmetic on
    // the bounds and never needs the misses materialized.
    let d = dataset(vec![
        episode(0, vec![stream("a", "c", None, &[0, 1])]),
        episode(u64::MAX, vec![stream("a", "c", None, &[0, 1])]),
    ]);
    let started = std::time::Instant::now();
    let f = structural::EpisodeContinuity.run(&d);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the check must not walk the span"
    );
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STRUCTURAL.EPISODE_INDEX_GAP");
    // u64::MAX - 0 - 1 = 18446744073709551614 missing, reported by count.
    assert!(
        f[0].message.contains("18446744073709551614 are missing"),
        "{}",
        f[0].message
    );
    // And it still names the first few, so the message stays actionable on an ordinary gap.
    assert!(f[0].message.contains("1, 2, 3"), "{}", f[0].message);
}

#[test]
fn an_ordinary_index_gap_is_still_reported_exactly() {
    let d = dataset(vec![
        episode(0, vec![stream("a", "c", None, &[0, 1])]),
        episode(1, vec![stream("a", "c", None, &[0, 1])]),
        episode(3, vec![stream("a", "c", None, &[0, 1])]),
    ]);
    let f = structural::EpisodeContinuity.run(&d);
    assert_eq!(f.len(), 1);
    assert!(
        f[0].message.contains("1 are missing: 2"),
        "{}",
        f[0].message
    );
}

#[test]
fn an_impossible_statistic_is_still_impossible_at_a_large_magnitude() {
    // The rounding slack scales with the magnitude of the values, which is right — and uncapped it
    // grew until it swallowed the quantity it was guarding. At values around 1e9 (nanosecond stamps
    // carried as a feature, GPS in millimetres, encoder counts) it was 1000 units, so a channel
    // 100 wide got a tolerance ten times its own range and both impossibility rules went inert.
    let d = dataset(vec![episode(
        0,
        vec![
            // std 900 against a Popoviciu bound of 50 — eighteen times impossible.
            stream_with_stats("std", stats(1e9, 1e9 + 100.0, 1e9 + 50.0, 900.0)),
            // a mean sitting 800 units outside its own [min, max].
            stream_with_stats("mean", stats(1e9, 1e9 + 100.0, 1e9 + 900.0, 10.0)),
        ],
    )]);
    let found = statistical::RangeSanity.run(&d);
    let codes: Vec<&str> = found.iter().map(|f| f.code.as_str()).collect();
    assert!(
        codes.contains(&"STATISTICAL.STD_IMPLAUSIBLE"),
        "an 18x-impossible std must be caught at any scale: {codes:?}"
    );
    assert!(
        codes.contains(&"STATISTICAL.MEAN_OUT_OF_RANGE"),
        "a mean outside its own range must be caught at any scale: {codes:?}"
    );
}

#[test]
fn a_non_finite_rail_is_not_reported_as_saturation() {
    // `NaN == NaN` is false, so a NaN-railed stream slipped past the "both rails are the same"
    // guard and was reported as "90% of values sit exactly at its minimum (NaN)" — a saturation
    // claim about a value that is not a value. Non-finites belong to the non-finite check.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation(
            "s",
            1000,
            900,
            0,
            f64::NAN,
            f64::NAN,
        )],
    )]);
    assert!(statistical::Saturation::default().run(&d).is_empty());
}

#[test]
fn an_astronomical_z_score_is_not_printed_in_full() {
    // A std of `f64::MIN_POSITIVE` clears the `std <= 0` guard and gives a z around 2.2e307, which
    // `{z:.1}` expanded to 310 digits — into the message, the JSON, the SARIF, and the certificate.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats(
            "s",
            stats(0.0, 1.0, 0.5, f64::MIN_POSITIVE),
        )],
    )]);
    let f = statistical::ExtremeOutlier::default().run(&d);
    assert_eq!(f.len(), 1);
    assert!(
        f[0].message.len() < 200,
        "message is {} chars: {}",
        f[0].message.len(),
        f[0].message
    );
    assert!(f[0].message.contains("e307"), "{}", f[0].message);
}

#[test]
fn a_two_frame_stream_does_not_grant_a_ten_second_sync_allowance() {
    // The span-comparison checks widen their tolerance by the sampling period, because a stream
    // sampling every T cannot resolve the window better than T. With a single interval there is no
    // cadence to take a median of: a stream whose only two frames sit at 0 s and 10 s reported a
    // ten-second "period", and the allowance built from it (`tolerance + max(period)`) was wide
    // enough that no drift of any size could be reported — while a sensor that fired twice and died
    // is exactly what these checks exist to catch.
    let mut lidar = stream("lidar", "rig", None, &[0, 10_000_000_000]);
    lidar.modality = Modality::PointCloud;
    let ticks: Vec<i64> = (0..1000).map(|i| i * 1_000_000).collect();
    let mut imu = stream("imu", "rig", None, &ticks);
    imu.modality = Modality::Imu;
    let mut gnss = stream("gnss", "rig", None, &ticks);
    gnss.modality = Modality::Gnss;
    let d = dataset(vec![episode(0, vec![lidar, imu, gnss])]);

    let f = autonomy::RigSync::default().run(&d);
    assert_eq!(
        f.len(),
        1,
        "a sensor spanning 10 s against sensors spanning 1 s is a nine-second drift: {f:?}"
    );
    assert_eq!(f[0].code, "AUTONOMY.RIG_SYNC");
}

#[test]
fn a_non_finite_ego_pose_is_reported_rather_than_hiding_a_teleport() {
    // `dist` is NaN if any coordinate is, and `NaN > max_speed` is false — so the step was neither
    // flagged nor mentioned, and the NaN poisoned both pairs it touched. This trajectory hides a
    // genuine 10 km/s jump in its third pose.
    let mut d = dataset(vec![episode(0, vec![stream("a", "c", None, &[0, 1])])]);
    d.episodes[0].ego_poses = Some(vec![
        ego_pose(0, [0.0, 0.0, 0.0]),
        ego_pose(1_000_000_000, [f64::NAN, 0.0, 0.0]),
        ego_pose(2_000_000_000, [10_000.0, 0.0, 0.0]),
    ]);
    let f = autonomy::EgoPoseContinuity::default().run(&d);
    assert!(
        f.iter().any(|f| f.code == "AUTONOMY.EGO_POSE_NON_FINITE"),
        "the unmeasurable steps must be named, not passed over: {f:?}"
    );
}

fn ego_pose(ts: i64, translation: [f64; 3]) -> veridex_core::cdm::EgoPose {
    veridex_core::cdm::EgoPose {
        ts,
        pose: veridex_core::cdm::Pose {
            translation,
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
    }
}

#[test]
fn a_family_that_could_not_measure_anything_says_so() {
    // The failure this guards: MCAP, CAN+DBC, MF4 and RLDS fingerprint payload bytes without
    // interpreting them, so every statistical check hit its `let Some(..) else { continue }` and
    // produced nothing — and a CAN log with a wheel speed pinned at its rail for 70% of the
    // recording reported `data 100`, with the certificate listing all five statistical checks under
    // `checks_run` and no categories skipped.
    let d = dataset(vec![episode(0, vec![stream("speed", "c", None, &[0, 1])])]);
    let f = statistical::ValueMeasurability.run(&d);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STATISTICAL.UNMEASURED_VALUES");
    assert_eq!(f[0].severity, Severity::Info);
    assert!(f[0].message.contains("speed"), "{}", f[0].message);

    // A stream whose values *were* summarized is not accused of anything...
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("s", stats(0.0, 1.0, 0.5, 0.1))],
    )]);
    assert!(statistical::ValueMeasurability.run(&d).is_empty());
}

#[test]
fn recomputed_without_stored_is_reported_as_a_narrower_gap() {
    // HDF5 and Zarr recompute statistics but publish none of their own, so the two checks that
    // compare the source's summary against its data can never fire on them. The recomputed checks
    // still ran, so this is a different — and smaller — statement than "nothing was measured".
    let mut s = stream("s", "c", None, &[0, 1]);
    s.observed_stats = Some(veridex_core::cdm::StreamStats {
        min: 0.0,
        max: 1.0,
        mean: 0.5,
        std: 0.1,
    });
    let d = dataset(vec![episode(0, vec![s])]);
    let f = statistical::ValueMeasurability.run(&d);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STATISTICAL.NO_STORED_STATS");
}

#[test]
fn a_hashless_stream_disables_the_content_checks_and_the_report_says_which() {
    // A LeRobot video feature's pixels live outside the Parquet, so its frames carry no hash — and
    // `duplicate-episode` aborts the whole episode signature if any frame of any stream lacks one.
    // One video feature, the ordinary layout of a real dataset, made two byte-identical episodes
    // undetectable, and `stuck-stream` (which only looks at Video streams) never ran at all.
    let mut cam = stream("observation.images.top", "c", None, &[0, 1]);
    cam.modality = Modality::Video;
    for f in &mut cam.frames {
        f.value_ref.content_hash = None;
    }
    let d = dataset(vec![episode(0, vec![cam])]);

    // The content checks are silent, as designed — and that silence is now stated.
    assert!(structural::DuplicateEpisode.run(&d).is_empty());
    assert!(structural::StuckStream.run(&d).is_empty());
    let f = structural::ContentMeasurability.run(&d);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STRUCTURAL.UNFINGERPRINTED_CONTENT");
    assert_eq!(f[0].severity, Severity::Info);
    assert!(
        f[0].message.contains("no episode was fully fingerprinted"),
        "the dataset-wide consequence must be stated, not inferred: {}",
        f[0].message
    );

    // A fully fingerprinted dataset is not accused of anything.
    let mut hashed = stream("a", "c", None, &[0, 1]);
    for (i, f) in hashed.frames.iter_mut().enumerate() {
        f.value_ref.content_hash = Some([i as u8; 32]);
    }
    let d = dataset(vec![episode(0, vec![hashed])]);
    assert!(structural::ContentMeasurability.run(&d).is_empty());
}

#[test]
fn a_jittery_but_complete_stream_is_not_accused_of_dropping_frames() {
    // A frame is counted as dropped when an interval sits near a *multiple* of the median cadence.
    // With the window at ±0.25 and the abstention gate at CV 0.5, ordinary jitter walked into that
    // window often enough to accumulate a 6-7% "drop rate" on a stream where nothing was dropped —
    // both inside the zone the gate declared honest. Deterministic pseudo-jitter here, so the test
    // measures the check rather than a random draw.
    let mut ts = Vec::new();
    let mut t: i64 = 0;
    let (mut a, mut b) = (12_345u64, 6_789u64);
    for _ in 0..401 {
        ts.push(t);
        // Two counters beating against each other: a stable, reproducible spread around 100 ms with
        // no drops of any kind.
        a = a
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        b = b.wrapping_mul(2862933555777941757).wrapping_add(3037000493);
        let jitter = ((a >> 33) % 90_000_000) as i64 - ((b >> 33) % 90_000_000) as i64;
        t += (100_000_000 + jitter / 2).max(1_000_000);
    }

    let mut lidar = stream("lidar", "rig", None, &ts);
    lidar.modality = Modality::PointCloud;
    let mut imu = stream("imu", "rig", None, &ts);
    imu.modality = Modality::Imu;
    let mut gnss = stream("gnss", "rig", None, &ts);
    gnss.modality = Modality::Gnss;
    let d = dataset(vec![episode(0, vec![lidar, imu, gnss])]);

    let f = autonomy::SequenceComplete::default().run(&d);
    assert!(
        f.is_empty(),
        "nothing was dropped, so nothing may be reported: {f:?}"
    );
}

#[test]
fn a_stream_that_really_dropped_frames_is_still_caught() {
    // The other side of the same tuning: narrowing the window must not blind the check. A steady
    // 100 ms cadence with every fifth frame removed is a 20% drop rate.
    let ts: Vec<i64> = (0..400)
        .filter(|i| i % 5 != 0)
        .map(|i| i as i64 * 100_000_000)
        .collect();
    let mut lidar = stream("lidar", "rig", None, &ts);
    lidar.modality = Modality::PointCloud;
    let steady: Vec<i64> = (0..400).map(|i| i as i64 * 100_000_000).collect();
    let mut imu = stream("imu", "rig", None, &steady);
    imu.modality = Modality::Imu;
    let mut gnss = stream("gnss", "rig", None, &steady);
    gnss.modality = Modality::Gnss;
    let d = dataset(vec![episode(0, vec![lidar, imu, gnss])]);

    let f = autonomy::SequenceComplete::default().run(&d);
    assert!(
        f.iter()
            .any(|f| f.code == "AUTONOMY.SEQUENCE_COMPLETE" && f.message.contains("lidar")),
        "a real 20% drop rate must still be reported: {f:?}"
    );
}

/// A constant element 0 must not hide a corrupt range on a higher dimension.
///
/// `evaluate` orders its rules "the numbers are corrupt" first and "the numbers are fine but
/// degenerate" last, because later rules restate the same defect. That reasoning holds within one
/// dimension's stats — but the scan ran element 0 first and kept the *first* firing rule across the
/// whole chain. A locked DoF or a gripper at rest is constant, which is the ordinary case in robot
/// data, so its `DEGENERATE` warning fired and the per-dimension arrays were never examined.
///
/// Because max severity drives the run's status, the dataset came back pass-with-warnings instead
/// of fail, and the score deducted a warning rather than an error.
#[test]
fn a_constant_first_dimension_does_not_suppress_an_inverted_range_above_it() {
    let mut s = stream_with_stats("action", stats(0.0, 0.0, 0.0, 0.0));
    s.dim_stats = Some(vec![
        veridex_core::cdm::DimStats {
            dim: 0,
            stats: stats(0.0, 0.0, 0.0, 0.0),
        },
        veridex_core::cdm::DimStats {
            dim: 3,
            stats: stats(5.0, -5.0, 0.0, 1.0),
        },
    ]);
    let d = dataset(vec![episode(0, vec![s])]);

    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].code, "STATISTICAL.RANGE_INVERTED",
        "the inverted range on dim 3 outranks a constant element 0, got {}: {}",
        f[0].code, f[0].message
    );
    assert_eq!(f[0].severity, Severity::Error);
}

/// `extreme-outlier` promises to scan every dimension. It read only `observed_dim_stats` — Veridex's
/// own recompute — and fell back to element 0 when that was absent.
///
/// Under `--metadata-only` nothing is recomputed: only the source's stored `dim_stats` exist. So the
/// check the docs list as covering "every stored-statistics check" quietly examined one joint of
/// seven. `range-sanity` consults `dim_stats` already, so the two stored-stats checks disagreed on
/// what they read.
#[test]
fn extreme_outlier_scans_the_sources_stored_per_dimension_stats() {
    let mut s = stream_with_stats("action", stats(-1.0, 1.0, 0.0, 0.5));
    s.dim_stats = Some(vec![
        veridex_core::cdm::DimStats {
            dim: 0,
            stats: stats(-1.0, 1.0, 0.0, 0.5),
        },
        // A ~1000σ spike in the gripper, present only in the stored arrays.
        veridex_core::cdm::DimStats {
            dim: 6,
            stats: stats(0.0, 1000.0, 0.1, 1.0),
        },
    ]);
    let d = dataset(vec![episode(0, vec![s])]);

    let f = statistical::ExtremeOutlier::default().run(&d);
    assert_eq!(
        f.len(),
        1,
        "the spike lives in the stored per-dimension stats, which a metadata-only run is all that \
         exists: {f:?}"
    );
    assert_eq!(f[0].code, "STATISTICAL.OUTLIER");
    assert!(
        f[0].message.contains("dimension 6"),
        "the finding must name the outlying dimension: {}",
        f[0].message
    );
}

/// The same invariant where the primary sort key *ties*, which is the case that was untested.
///
/// `canonicalize_order`'s own comment says episodes and streams tie-break on full content "because
/// neither `index` nor `name` is guaranteed unique — duplicates of both are faults Veridex reports,
/// so the ordering cannot assume they are absent". Every existing order-independence test used
/// distinct indices and distinct names, so the primary key alone decided and the tie-break never
/// ran. A mutation audit deleted both tie-breaks and all 692 tests passed.
///
/// That matters because the encoder sorts independently, with the tie-breaks intact. So the hash
/// stays order-independent while the *checks* — which read the canonicalized sequence — see a
/// different order for the same bytes. Two datasets share a content hash and produce different
/// verdicts, which is precisely what would let a certificate attest a hash that also matches a
/// dataset that fails.
#[test]
fn canonical_order_is_total_when_the_primary_key_ties() {
    let build = |episodes: Vec<Episode>| {
        let mut d = dataset(episodes);
        d.canonicalize_order();
        d
    };
    let engine = veridex_core::checks::default_engine().unwrap();
    let run = |d: &Dataset| {
        let v = engine.run(d, veridex_core::content_hash(d), &Default::default());
        (
            v.result_content_hash.clone(),
            veridex_core::render_json(&v, None),
        )
    };

    // Two episodes sharing an index, and within one of them two streams sharing a name — both
    // faults Veridex reports, so both must still order totally.
    let dup_a = episode(
        0,
        vec![
            shaped("action", Some("float32"), Some(vec![6]), &[0]),
            shaped("action", Some("int32"), Some(vec![9]), &[0, 1]),
        ],
    );
    let dup_b = episode(
        0,
        vec![shaped("action", Some("float64"), Some(vec![7]), &[0])],
    );

    let forward = build(vec![dup_a.clone(), dup_b.clone()]);
    let reversed = build(vec![dup_b, dup_a]);

    assert_eq!(
        veridex_core::content_hash(&forward),
        veridex_core::content_hash(&reversed),
        "the encoder's order must not depend on input order when indices tie"
    );
    let (hash_f, json_f) = run(&forward);
    let (hash_r, json_r) = run(&reversed);
    assert_eq!(
        hash_f, hash_r,
        "the verdict hash must not depend on input order when indices tie"
    );
    assert_eq!(
        json_f, json_r,
        "the whole report must not depend on input order when indices tie"
    );
}

/// `STATISTICAL.NEGATIVE_STD` is declared by the check and was produced by no test.
///
/// The only fixture with a negative standard deviation also set a NaN, and the non-finite rule
/// returns first — so the rule could be deleted outright and the suite stayed green. A standard
/// deviation is a root of a sum of squares and cannot be negative; one that is means the stored
/// statistics were written by something that was not measuring the data.
#[test]
fn a_negative_standard_deviation_is_reported_on_its_own() {
    // Every number finite, so nothing earlier in the rule order can claim this first.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("action", stats(-1.0, 1.0, 0.0, -3.0))],
    )]);

    let f = statistical::RangeSanity.run(&d);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STATISTICAL.NEGATIVE_STD");
}

/// Per-episode statistics must each report, while dataset-level ones still collapse to one finding.
///
/// The five stored-statistics checks deduped on stream name across the whole dataset, on the stated
/// premise that "the adapter recomputes one summary per stream (dataset-level)". That is true for
/// LeRobot and false for HDF5 and Zarr, which build a fresh accumulator per episode group — so
/// those numbers are genuinely per-episode facts, and reporting only the first affected episode
/// dropped the rest, including episodes carrying worse defects than the one reported.
///
/// Keying the dedupe on the measured values rather than the name gets both behaviors from one rule.
#[test]
fn distinct_per_episode_statistics_each_report_while_identical_ones_collapse() {
    let sat = |at_max: u64| veridex_core::cdm::Saturation {
        sample_count: 100,
        at_min: 0,
        at_max,
        min: -1.0,
        max: 1.0,
        dim: 0,
    };
    let with_sat = |at_max: u64| {
        let mut s = stream("action", "c", None, &[0, 1]);
        s.observed_saturation = Some(sat(at_max));
        s
    };

    // Three episodes, each measured separately, each pinned at a different rate — the HDF5/Zarr
    // shape. All three are distinct defects at distinct locations.
    let per_episode = dataset(vec![
        episode(0, vec![with_sat(90)]),
        episode(1, vec![with_sat(95)]),
        episode(2, vec![with_sat(99)]),
    ]);
    let f = statistical::Saturation::default().run(&per_episode);
    assert_eq!(
        f.len(),
        3,
        "each episode's own measurement is its own defect to fix: {f:?}"
    );

    // The same stream, same dataset-level summary attached to every episode — the LeRobot shape.
    // One fact, so one finding, exactly as before.
    let dataset_level = dataset(vec![
        episode(0, vec![with_sat(90)]),
        episode(1, vec![with_sat(90)]),
        episode(2, vec![with_sat(90)]),
    ]);
    let f = statistical::Saturation::default().run(&dataset_level);
    assert_eq!(
        f.len(),
        1,
        "one dataset-level measurement is one finding, not one per episode: {f:?}"
    );
}
