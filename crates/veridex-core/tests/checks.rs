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
        observed_header_stamps: None,
        observed_sequence: None,
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
        ego_frame: None,
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
fn an_episode_that_starts_and_ends_at_once_is_short_not_inverted() {
    // The rule reports an *inverted* boundary — `start > end` — and a single-frame episode has
    // `start == end`, which is short, not backwards. Unpinned: a mutation sweep flipping `>` to
    // `>=` left the suite green, and the rule would then have called every one-frame episode a
    // corrupt boundary. Reachable on any real dataset with a one-frame episode.
    let mut ep = episode(0, vec![stream("s", "c", None, &[5])]);
    ep.start_ts = Some(5);
    ep.end_ts = Some(5);
    assert!(structural::EpisodeBoundary
        .run(&dataset(vec![ep]))
        .is_empty());

    // One nanosecond the wrong way round is inverted.
    let mut bad = episode(0, vec![stream("s", "c", None, &[5])]);
    bad.start_ts = Some(6);
    bad.end_ts = Some(5);
    assert_eq!(
        structural::EpisodeBoundary.run(&dataset(vec![bad])).len(),
        1
    );
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
        observed_header_stamps: None,
        observed_sequence: None,
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
        observed_header_stamps: None,
        observed_sequence: None,
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
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        observed_header_stamps: None,
        observed_sequence: None,
        media: None,
        frame_id: None,
        dim_names: None,
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
fn an_abstention_does_not_swallow_the_near_duplicates_already_found() {
    // The failure this pins: the abstention *replaced* the result. One boilerplate-only episode
    // among a thousand, or one pair past the tracking ceiling, and every near-duplicate the check
    // had already found was thrown away behind a note saying some episodes were not examined. A
    // reader saw an info line about coverage and no warning at all — the copy was not absent from
    // the report, it was deleted from it.
    //
    // The dataset: 600 episodes sharing one boilerplate frame content (past the per-hash ceiling,
    // so those are skipped), plus two episodes that are genuine near-duplicates of each other on
    // their own content.
    let boilerplate: Vec<u8> = (0..12).collect();
    let original: Vec<u8> = (100..112).collect();
    let trimmed: Vec<u8> = (100..110).collect();
    let mut episodes: Vec<_> = (0..600u64)
        .map(|i| {
            episode(
                i,
                vec![near_stream("s", i as i64 * 1_000_000_000, &boilerplate)],
            )
        })
        .collect();
    episodes.push(episode(600, vec![near_stream("s", 0, &original)]));
    episodes.push(episode(
        601,
        vec![near_stream("s", 9_000_000_000, &trimmed)],
    ));

    let f = near_duplicate().run(&dataset(episodes));
    let codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    assert!(
        codes.contains(&"STRUCTURAL.NEAR_DUPLICATE_EPISODE"),
        "the pair the check did find must survive the abstention: {codes:?}"
    );
    assert!(
        codes.contains(&"STRUCTURAL.NEAR_DUPLICATE_UNCHECKED"),
        "and the abstention must still be said: {codes:?}"
    );
    let pair = f
        .iter()
        .find(|x| x.code == "STRUCTURAL.NEAR_DUPLICATE_EPISODE")
        .expect("the pair");
    assert!(
        pair.message.contains("600") && pair.message.contains("601"),
        "{}",
        pair.message
    );
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
fn the_duration_factor_bounds_are_inclusive_at_both_ends() {
    // The rule is `d < median / factor || d > median * factor`, so an episode exactly `factor` times
    // shorter or longer than the median is *at* the configured limit and not past it. Unpinned: the
    // sweep's `<` → `<=` mutation survived, and a user's `episode_duration_factor` could have been
    // silently tightened to flag an episode that sits on the boundary they chose. Reachable
    // exactly, because a median of 1 s over a factor of 10 is 100 ms in whole nanoseconds.
    let four_seconds_of_median = || {
        vec![
            episode_lasting(0, 1_000_000_000),
            episode_lasting(1, 1_000_000_000),
            episode_lasting(2, 1_000_000_000),
            episode_lasting(3, 1_000_000_000),
        ]
    };
    let judged = |dur: i64| {
        let mut eps = four_seconds_of_median();
        eps.push(episode_lasting(4, dur));
        temporal::EpisodeDuration { factor: 10.0 }
            .run(&dataset(eps))
            .len()
    };
    assert_eq!(
        judged(100_000_000),
        0,
        "exactly 10x shorter is at the limit"
    );
    assert_eq!(judged(10_000_000_000), 0, "and exactly 10x longer");
    assert_eq!(judged(99_000_000), 1, "past it in the short direction");
    assert_eq!(judged(10_100_000_000), 1, "and in the long one");
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
fn the_clock_skew_allowance_is_a_boundary_the_config_promises() {
    // The headline check, and its threshold was unpinned: a mutation sweep flipping `>` to `>=`
    // left the suite green, so `clock_skew_ns` could have been silently tightened to fail a pair of
    // streams that sit exactly on the tolerance a user configured.
    //
    // The allowance is the configured tolerance widened by the larger sampling period, because two
    // synchronized streams sampling one window differ in span by up to that quantum. Both streams
    // here run at 100 Hz, so the quantum is exactly 10 ms and the allowance is exactly 60 ms at the
    // 50 ms default. Spans are whole 10 ms ticks, so 60 ms is landed on rather than approached.
    let dense = |span_ns: i64| -> Vec<i64> {
        (0..=(span_ns / 10_000_000))
            .map(|i| i * 10_000_000)
            .collect()
    };
    let pair = |robot_span: i64| {
        dataset(vec![episode(
            0,
            vec![
                stream("cam", "camera", None, &dense(1_000_000_000)),
                stream("robot", "robot", None, &dense(robot_span)),
            ],
        )])
    };
    assert!(
        temporal::ClockSkew::default()
            .run(&pair(1_060_000_000))
            .is_empty(),
        "a 60 ms drift is within a 60 ms allowance"
    );
    assert_eq!(
        temporal::ClockSkew::default()
            .run(&pair(1_070_000_000))
            .len(),
        1,
        "one tick past it, the clocks have drifted"
    );
}

#[test]
fn the_gap_threshold_is_a_multiple_of_the_cadence_not_a_number_near_it() {
    // `TEMPORAL.GAP` fires when an interval exceeds `expected × gap_factor`, so an interval of
    // exactly that product is the largest one the configured factor permits. Unpinned: the sweep's
    // `>` → `>=` mutation survived, and a user's `gap_factor` could have been silently tightened by
    // one cadence tick. Reachable exactly, because both the cadence and the interval are whole
    // nanoseconds.
    //
    // Ten frames at 10 ms, then one interval of exactly 3 × 10 ms at the default factor of 3.
    let with_gap = |gap_ns: i64| {
        let mut ts: Vec<i64> = (0..10).map(|i| i * 10_000_000).collect();
        let last = *ts.last().unwrap();
        ts.push(last + gap_ns);
        for k in 1..6 {
            ts.push(last + gap_ns + k * 10_000_000);
        }
        dataset(vec![episode(0, vec![stream("s", "wall", None, &ts)])])
    };
    let gaps = |gap_ns: i64| {
        temporal::Gaps { gap_factor: 3.0 }
            .run(&with_gap(gap_ns))
            .len()
    };
    assert_eq!(gaps(30_000_000), 0, "exactly 3x the cadence is not a gap");
    assert_eq!(gaps(40_000_000), 1, "4x is");
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

/// `provenance.completeness` over a run that read frame payloads. `upstream` and `calibration` are
/// payload-derived, so the bare `run` deliberately abstains on them; the partiality of a per-episode
/// lineage is only reachable on a run that actually opened the payloads it lives in.
fn provenance_findings_after_full_read(d: &Dataset) -> Vec<veridex_core::check::Finding> {
    use veridex_core::check::{Check, CheckContext};
    provenance::ProvenanceCompleteness.run_in(
        d,
        &CheckContext {
            frames_read: true,
            attested_keys: Vec::new(),
        },
    )
}

/// A dataset of `episodes` episodes, with `upstream` recorded at episode scope for the first
/// `covered` of them.
fn dataset_with_episode_scoped_upstream(episodes: u64, covered: u64) -> Dataset {
    let mut d = dataset(
        (0..episodes)
            .map(|i| episode(i, vec![stream("s", "c", None, &[0, 1])]))
            .collect(),
    );
    d.provenance = (0..covered)
        .map(|i| Provenance {
            scope: ProvenanceScope::Episode(i),
            elements: vec![el(
                "upstream",
                Some(&format!("raw/session-{i}.bag")),
                ProvenanceClass::Known,
            )],
        })
        .collect();
    d
}

#[test]
fn lineage_on_one_episode_is_not_lineage_for_a_thousand() {
    // What an Open X-Embodiment conversion looks like when only part of the shards carried
    // `episode_metadata/file_path`. The element is *present*, so `PROVENANCE.MISSING_UPSTREAM` is
    // correctly silent — and the coverage percentage counts the strongest class found anywhere, so
    // the certificate reads `upstream: known` over 999 episodes with no origin at all. Nothing said
    // which was which.
    let d = dataset_with_episode_scoped_upstream(1000, 1);
    let f = provenance_findings_after_full_read(&d);
    let partial = f
        .iter()
        .find(|x| x.code == "PROVENANCE.PARTIAL")
        .unwrap_or_else(|| panic!("{f:?}"));
    assert_eq!(partial.severity, Severity::Info);
    assert!(
        partial.message.contains("`upstream`") && partial.message.contains("1 of 1000"),
        "{}",
        partial.message
    );
    assert!(
        f.iter().all(|x| x.code != "PROVENANCE.MISSING_UPSTREAM"),
        "it is present, so it is not missing — the two must never both fire: {f:?}"
    );
}

#[test]
fn an_attested_element_is_not_also_reported_partial() {
    // The precedent is exact: this report already said an element was both attested and missing,
    // and the remedy it printed was the one the reader had already followed. Signing for an element
    // is a claim about the *whole dataset* — that is what makes the trust score count it as covered
    // — so it settles the scope question the way a dataset-scoped record does. Reporting it partial
    // beside the attestation would have the same report say both again.
    use veridex_core::check::{Check, CheckContext};
    let d = dataset_with_episode_scoped_upstream(1000, 1);
    let context = CheckContext {
        frames_read: true,
        attested_keys: vec!["upstream".to_string()],
    };
    let f = provenance::ProvenanceCompleteness.run_in(&d, &context);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PARTIAL"), "{f:?}");
    // And without the attestation it is still reported — the silence is the signature's doing, not
    // the rule going quiet.
    assert!(provenance_findings_after_full_read(&d)
        .iter()
        .any(|x| x.code == "PROVENANCE.PARTIAL"));
}

#[test]
fn an_element_every_episode_records_is_not_partial() {
    let d = dataset_with_episode_scoped_upstream(4, 4);
    let f = provenance_findings_after_full_read(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PARTIAL"), "{f:?}");
}

#[test]
fn a_dataset_scoped_element_covers_every_episode() {
    // A record at dataset scope speaks for the whole dataset by construction. Demanding a per-episode
    // record too would report every honest manifest-derived license as partial.
    let mut d = dataset_with_episode_scoped_upstream(1000, 0);
    d.provenance.push(Provenance {
        scope: ProvenanceScope::Dataset,
        elements: vec![el("license", Some("apache-2.0"), ProvenanceClass::Known)],
    });
    let f = provenance_findings_after_full_read(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PARTIAL"), "{f:?}");
    assert!(f.iter().all(|x| x.code != "PROVENANCE.MISSING_LICENSE"));
}

#[test]
fn an_element_no_episode_records_is_missing_not_partial() {
    // Absent everywhere is the existing `MISSING_*` case, and must stay exactly one finding.
    let d = dataset_with_episode_scoped_upstream(4, 0);
    let f = provenance_findings_after_full_read(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PARTIAL"), "{f:?}");
    assert!(f.iter().any(|x| x.code == "PROVENANCE.MISSING_UPSTREAM"));
}

#[test]
fn a_placeholder_on_one_episode_does_not_make_the_element_partially_present() {
    // A value of "unknown" is present in form and empty in substance, so it must not count as an
    // episode that carries the element — which would turn an absent element into a partial one and
    // silence `PROVENANCE.MISSING_UPSTREAM`.
    let mut d = dataset_with_episode_scoped_upstream(4, 1);
    d.provenance[0].elements[0].value = Some("n/a".into());
    let f = provenance_findings_after_full_read(&d);
    assert!(f.iter().all(|x| x.code != "PROVENANCE.PARTIAL"), "{f:?}");
    assert!(f.iter().any(|x| x.code == "PROVENANCE.MISSING_UPSTREAM"));
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
    assert_eq!(verdict.executed_checks.len(), 45);
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
fn the_saturation_knobs_are_boundaries_a_user_configured() {
    // Both `saturation_fraction` and `saturation_min_samples` were unpinned: a mutation sweep
    // flipping `<` to `<=` at each gate left the suite green, so a stream sitting exactly on the
    // fraction a user configured, or carrying exactly the minimum sample count they set, could have
    // been silently dropped from the check they turned it on for. Both land exactly — one is a count
    // of whole samples, the other a ratio of two of them.
    let check = statistical::Saturation {
        min_fraction: 0.5,
        min_samples: 20,
    };
    let judged = |sample_count: u64, at_max: u64| {
        check
            .run(&dataset(vec![episode(
                0,
                vec![stream_with_saturation(
                    "state",
                    sample_count,
                    0,
                    at_max,
                    -1.0,
                    1.0,
                )],
            )]))
            .len()
    };

    // Exactly the minimum sample count is enough to judge, and exactly the fraction is saturated.
    assert_eq!(judged(20, 10), 1, "20 samples, exactly half at the rail");
    // One sample short of the minimum is not judged at all, however pinned it looks.
    assert_eq!(
        judged(19, 19),
        0,
        "below the sample floor, nothing is claimed"
    );
    // ...and just under the fraction is not saturation.
    assert_eq!(judged(100, 49), 0, "49% is under a 50% threshold");
    assert_eq!(judged(100, 50), 1, "50% is not");
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

/// The README's own picture of the engine must name every check family the engine runs.
///
/// It named three of seven. The flowchart read "structural · temporal · provenance checks" and the
/// sequence diagram said the same, so the front page of the project understated its own catalog by
/// four families — including `autonomy`, which is most of what a rig log is checked for. Prose that
/// summarizes a list drifts the moment the list grows, and nothing was watching: the `docs/checks.md`
/// guards below cover the catalog page and not the page most readers see first.
#[test]
fn the_readme_names_every_check_family_the_engine_runs() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
        .expect("README.md is readable");
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let families: std::collections::BTreeSet<&str> =
        engine.catalog().iter().map(|c| c.category.tag()).collect();
    assert!(!families.is_empty(), "the catalog has categories");

    // The flowchart node, which is the picture a reader forms of what the engine does.
    let engine_node = readme
        .lines()
        .find(|l| l.contains("Validation engine"))
        .expect("the README's flowchart names the validation engine");
    for family in &families {
        assert!(
            engine_node.to_ascii_lowercase().contains(*family),
            "the README's engine node does not name the `{family}` family:\n  {engine_node}"
        );
    }

    // ...and the count the status table commits to. The sensor-rig row used to enumerate finding
    // codes and had silently fallen four rules behind the catalog; it is a summary now, and a
    // summary that counts is only useful while the count is right. Words rather than digits,
    // because that is how the row reads.
    let autonomy = engine
        .catalog()
        .iter()
        .filter(|c| c.category == veridex_core::Category::Autonomy)
        .count();
    let spelled = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
        "eleven", "twelve",
    ];
    let claim = format!(
        "{} checks over an autonomy rig",
        spelled.get(autonomy).copied().unwrap_or("?")
    );
    let row = readme
        .lines()
        .find(|l| l.contains("**Sensor-rig checks**"))
        .expect("the README's status table has a sensor-rig row");
    assert!(
        row.to_ascii_lowercase().contains(&claim),
        "the README's sensor-rig row should read `{claim}`:\n  {row}"
    );
}

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
        // A producer attestation applied to the run, and any value in it that contradicts the data.
        // Emitted by the engine for the same reason coverage is: whether someone signed for a
        // provenance element is a property of the run's inputs, not of the CDM.
        "PROVENANCE.ATTESTED",
        "PROVENANCE.ATTESTATION_CONFLICT",
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

    // Every *other* page too. The guard covered `docs/checks.md` alone, and it is not the only page
    // that names finding codes: the README's headline list, the format walkthroughs, the quickstart
    // and the profile reference all quote them, all as the thing a reader should expect to see. A
    // renamed code would leave those pages pointing at something that can never fire, and only the
    // catalog page would have said so.
    //
    // Here a code is looked for unquoted as well, because these pages paste real terminal output,
    // where a code appears bare: `  [error] AUTONOMY.RIG_SYNC  episode 0`.
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let mut pages: Vec<std::path::PathBuf> = vec![root.join("README.md")];
    for entry in std::fs::read_dir(root.join("docs")).expect("docs/ is readable") {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_some_and(|e| e == "md") {
            pages.push(path);
        }
    }
    pages.sort();
    let mut checked = 0usize;
    for page in &pages {
        let text = std::fs::read_to_string(page).expect("the page is readable");
        let name = page.file_name().unwrap().to_string_lossy();
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')) {
            if !is_finding_code(token) {
                continue;
            }
            checked += 1;
            assert!(
                registered.contains(token) || engine_emitted.contains(token),
                "{name} names `{token}`, which no registered check emits"
            );
        }
    }
    assert!(
        checked > 100,
        "the sweep must actually find codes to check: {checked}"
    );
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

// ---- temporal: the cross-stream checks had nothing to compare ----

#[test]
fn a_dataset_where_nothing_could_be_compared_says_so() {
    // `CLOCK_SKEW`, `START_OFFSET` and `END_OFFSET` are the three checks that answer whether a
    // dataset's sensors are aligned, and all three need two streams on one clock. Given fewer they
    // report nothing — which is the same silence a perfectly synchronized dataset produces, and it
    // reaches the certificate's list of executed checks looking exactly like that.
    //
    // The sharp case is a ROS bag holding only latched topics: a transform tree and a robot
    // description, each published once. No sensor data at all, and it scored `data 100` with not one
    // temporal finding.
    let mut tf = stream("/tf_static", "bag", None, &[0]);
    tf.latched = Some(true);
    let mut desc = stream("/robot_description", "bag", None, &[0]);
    desc.latched = Some(true);
    let f = temporal::ClockMeasurability.run(&dataset(vec![episode(0, vec![tf, desc])]));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "TEMPORAL.UNCOMPARED_STREAMS");
    assert_eq!(f[0].severity, Severity::Info);
    assert!(f[0].message.contains("1 of 1 episode"), "{}", f[0].message);
}

#[test]
fn two_comparable_streams_are_enough_to_stay_quiet() {
    let ep = episode(
        0,
        vec![
            stream("a", "bag", None, &[0, 1_000_000, 2_000_000]),
            stream("b", "bag", None, &[0, 1_000_000, 2_000_000]),
        ],
    );
    assert!(temporal::ClockMeasurability
        .run(&dataset(vec![ep]))
        .is_empty());
}

#[test]
fn a_step_index_dataset_is_not_told_twice() {
    // An episode with no measured time at all is already covered, in full, by
    // `TEMPORAL.UNMEASURED_CLOCK`. Saying it twice is noise on every RLDS dataset — and the
    // suppression is deliberately narrower than that finding's precondition, so nothing goes
    // unreported: `UNMEASURED_CLOCK` fires for *any* step-index stream, so an episode with none
    // measured always reaches it.
    let mut a = stream("a", "step", None, &[0, 1, 2]);
    a.clock_kind = ClockKind::StepIndex;
    let f = temporal::ClockMeasurability.run(&dataset(vec![episode(0, vec![a])]));
    let codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    assert_eq!(codes, vec!["TEMPORAL.UNMEASURED_CLOCK"], "{f:?}");
}

#[test]
fn an_episode_mixing_measured_and_step_index_streams_gets_both_disclosures() {
    // The boundary the suppression above must not overshoot: one measured stream (too few to
    // compare) beside a step-index one. Both facts are true and both are reported.
    let measured = stream("measured", "wall", None, &[0, 1_000_000, 2_000_000]);
    let mut indexed = stream("indexed", "step", None, &[0, 1, 2]);
    indexed.clock_kind = ClockKind::StepIndex;
    let f = temporal::ClockMeasurability.run(&dataset(vec![episode(0, vec![measured, indexed])]));
    let mut codes: Vec<&str> = f.iter().map(|x| x.code.as_str()).collect();
    codes.sort_unstable();
    assert_eq!(
        codes,
        vec!["TEMPORAL.UNCOMPARED_STREAMS", "TEMPORAL.UNMEASURED_CLOCK"],
        "{f:?}"
    );
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
fn the_rig_sync_allowance_is_a_boundary_the_profile_promises() {
    // `world-model-ready` attests "rig sensors within a 20 ms cross-sensor span drift", and the rule
    // is `spread > allowance` — so a spread of exactly the allowance is *within* it and passes.
    // Nothing pinned that: a mutation sweep flipping `>` to `>=` left the suite green, and the
    // tolerance could have been silently tightened to fail a rig that meets the criterion the
    // certificate names.
    //
    // The allowance is the configured tolerance widened by the slowest sensor's sampling period,
    // because a rig is multi-rate by construction and each span quantizes to its own period. Every
    // sensor here runs at 100 Hz, so the quantum is exactly 10 ms and the allowance is exactly
    // 60 ms at the default 50 ms tolerance. Spans are whole 10 ms ticks, so 60 ms is reachable
    // exactly rather than approached.
    let rig = |imu_span: i64| {
        dataset(vec![episode(
            0,
            vec![
                rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
                rig_stream("gnss", Modality::Gnss, 1_000_000_000),
                rig_stream("imu", Modality::Imu, imu_span),
            ],
        )])
    };
    assert!(
        autonomy::RigSync::default()
            .run(&rig(940_000_000))
            .is_empty(),
        "a 60 ms spread is within a 60 ms allowance"
    );
    assert_eq!(
        autonomy::RigSync::default().run(&rig(930_000_000)).len(),
        1,
        "one tick past it, the rig is out of sync"
    );
}

/// A rig sensor shifted whole by a constant latency: the same span, starting and ending later.
fn rig_stream_shifted(name: &str, modality: Modality, span_ns: i64, shift_ns: i64) -> Stream {
    const STEP_NS: i64 = 10_000_000; // 100 Hz
    let ts: Vec<i64> = (0..=(span_ns / STEP_NS))
        .map(|i| i * STEP_NS + shift_ns)
        .collect();
    let mut s = stream(name, "rig", None, &ts);
    s.modality = modality;
    s
}

#[test]
fn a_constant_sensor_latency_is_a_start_and_end_offset_not_a_sync_spread() {
    // A rig whose LiDAR is triggered a constant 200 ms after the rest is a known, ordinary rig
    // characteristic — and it is worth pinning which check says so, because the two answer different
    // questions and the docs described this case as the wrong one.
    //
    // `AUTONOMY.RIG_SYNC` compares *durations*, and a whole-stream shift does not change a duration,
    // so it is silent — correctly: nothing drifted. What is true is that the LiDAR starts and ends
    // later than its peers on the same clock, which is exactly `TEMPORAL.START_OFFSET` and its mirror
    // `TEMPORAL.END_OFFSET`. Anyone reading a rig report needs that boundary to hold.
    let ep = episode(
        0,
        vec![
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
            rig_stream_shifted("lidar", Modality::PointCloud, 1_000_000_000, 200_000_000),
        ],
    );
    let d = dataset(vec![ep]);

    assert!(
        autonomy::RigSync::default().run(&d).is_empty(),
        "a constant latency shifts a sensor, it does not drift it: every span is still 1.0 s"
    );
    let starts = temporal::StartOffset::default().run(&d);
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].code, "TEMPORAL.START_OFFSET");
    assert!(starts[0].message.contains("lidar"), "{}", starts[0].message);
    let ends = temporal::EndOffset::default().run(&d);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].code, "TEMPORAL.END_OFFSET");
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
fn node_chatter_beside_a_rig_is_not_a_rig_sensor() {
    // Every `ros2 bag record -a` captures /rosout, /parameter_events and /diagnostics next to the
    // sensors. None of them observes the world, none keeps a sensor's cadence, and all three are
    // routinely short of the recording's window — a log line at 0.2 s and 0.35 s in a 1 s drive.
    //
    // Graded as rig sensors they made a perfectly synchronized rig FAIL: "rig sensors are out of
    // sync — `/rosout` spans 150.0 ms but `/imu/data` spans 1990.0 ms, a 1840.0 ms drift across 7
    // sensors". Error severity, on sound data, with a remedy (re-synchronize the rig) that sends
    // the reader after nothing. The check's own risk statement is about a sensor's observations
    // being mis-aligned from the other sensors; a log topic has no observations.
    let mut chatter = stream("/rosout", "rig", None, &[200_000_000, 350_000_000]);
    chatter.modality = Modality::ScalarState;
    let mut params = stream("/parameter_events", "rig", None, &[5_000_000, 900_000_000]);
    params.modality = Modality::Action;
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
            chatter,
            params,
        ],
    );
    assert!(
        autonomy::RigSync::default()
            .run(&dataset(vec![ep]))
            .is_empty(),
        "the rig is synchronized; the topics that are not sensors do not make it otherwise"
    );
}

#[test]
fn a_drifting_sensor_is_still_caught_beside_that_chatter() {
    // The guard above must narrow *who is compared*, not soften the comparison: the same rig with a
    // genuinely short IMU still fails, and still names the IMU.
    let mut chatter = stream("/rosout", "rig", None, &[200_000_000, 350_000_000]);
    chatter.modality = Modality::ScalarState;
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 700_000_000),
            chatter,
        ],
    );
    let f = autonomy::RigSync::default().run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("imu"), "{}", f[0].message);
    assert!(
        !f[0].message.contains("/rosout"),
        "the log topic is not one of the sensors compared: {}",
        f[0].message
    );
}

#[test]
fn a_camera_is_a_rig_sensor_for_sync_even_though_it_does_not_mark_a_rig() {
    // `Modality::is_rig_sensor` deliberately excludes Video, because a camera alone does not make a
    // dataset a rig — manipulation datasets have cameras. But once the episode *is* a rig, a camera
    // that dropped out early is exactly what this check is for, so the sync comparison must include
    // it.
    let ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
            rig_stream("camera", Modality::Video, 600_000_000),
        ],
    );
    let f = autonomy::RigSync::default().run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1);
    assert!(f[0].message.contains("camera"), "{}", f[0].message);
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
fn the_drop_fraction_threshold_is_a_boundary_the_wording_promises() {
    // `world-model-ready` attests "no rig sensor dropping more than 5% of its frames", and the rule
    // is `drop_fraction > max_drop_fraction` — so exactly 5% passes, which is what "more than" means.
    // Nothing pinned that: a mutation sweep flipping `>` to `>=` left the suite green, and the
    // threshold could have been silently tightened to reject a sensor that meets the criterion the
    // certificate names. Reachable exactly, because the fraction is a ratio of whole frames.
    //
    // 19 frames present of 20 nominal ticks: `missing` is 1, `expected` 20, so the fraction is
    // exactly 0.05.
    let full: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let dropped_one: Vec<i64> = full
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 7)
        .map(|(_, t)| *t)
        .collect();
    let rig = |imu: &[i64]| {
        episode(
            0,
            vec![
                rig_stream_ts("lidar", Modality::PointCloud, &full),
                rig_stream_ts("gnss", Modality::Gnss, &full),
                rig_stream_ts("imu", Modality::Imu, imu),
            ],
        )
    };
    let judged = |imu: &[i64]| {
        autonomy::SequenceComplete::default()
            .run(&dataset(vec![rig(imu)]))
            .len()
    };
    assert_eq!(judged(&dropped_one), 0, "exactly 5% is not more than 5%");

    // Two of twenty is 10%, and past the threshold.
    let dropped_two: Vec<i64> = full
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 7 && *i != 13)
        .map(|(_, t)| *t)
        .collect();
    assert_eq!(judged(&dropped_two), 1, "past it, the sensor is flagged");
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
fn the_ego_speed_ceiling_is_a_ceiling_not_a_limit_to_reach() {
    // `world-model-ready` attests "no step above 100 m/s implied speed", and the rule is
    // `speed > max_speed_mps` — so exactly 100 m/s passes, which is what "above" means. Nothing
    // pinned that: a mutation sweep flipping `>` to `>=` left the suite green, and the ceiling
    // could have been silently tightened to fail a trajectory that meets the criterion the
    // certificate names.
    //
    // 10 m in 100 ms is exactly 100 m/s, and both are exact in binary floating point, so the
    // comparison really does land on the boundary rather than near it.
    let at_ceiling = vec![ego(0, 0.0, 0.0), ego(100_000_000, 10.0, 0.0)];
    assert!(
        autonomy::EgoPoseContinuity::default()
            .run(&dataset(vec![rig_episode_with_ego(at_ceiling)]))
            .is_empty(),
        "exactly 100 m/s is not above 100 m/s"
    );

    // 10.5 m in the same 100 ms is 105 m/s, and past it.
    let past_ceiling = vec![ego(0, 0.0, 0.0), ego(100_000_000, 10.5, 0.0)];
    assert_eq!(
        autonomy::EgoPoseContinuity::default()
            .run(&dataset(vec![rig_episode_with_ego(past_ceiling)]))
            .len(),
        1,
        "past it, the step is a teleport"
    );
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
        distortion_model: None,
        width: None,
        height: None,
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
                dim_names: None,
                frames: vec![],
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
                observed_header_stamps: None,
                observed_sequence: None,
                media: None,
                frame_id: None,
            })
            .collect(),
        task: None,
        labels: vec![],
        ego_poses: None,
        ego_frame: None,
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
        dim_names: None,
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
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        observed_header_stamps: None,
        observed_sequence: None,
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
            ego_frame: None,
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
fn a_rig_of_ten_thousand_sensors_still_resolves_its_frames() {
    // Nothing caps how many channels a bag may declare, so the number of cameras and the number of
    // point-cloud sensors on one episode are both counts the input file chooses. The check tested
    // each spatial sensor's frame against a `Vec` of camera frames — their product — and built the
    // reachable set by walking the whole transform tree once per camera. A log of 5,000 image topics
    // beside 5,000 LiDAR topics is legal and made both quadratic.
    let mut gnss = rig_stream("gnss", Modality::Gnss, 1_000_000_000);
    gnss.frame_id = Some("gnss_link".to_string());
    let mut imu = rig_stream("imu", Modality::Imu, 1_000_000_000);
    imu.frame_id = Some("imu_link".to_string());
    let mut streams = vec![gnss, imu];
    let mut transforms = vec![
        xf("base_link", "lidar_mount"),
        xf("base_link", "gnss_link"),
        xf("base_link", "imu_link"),
    ];
    for i in 0..5_000u32 {
        let mut cam = rig_stream(&format!("cam{i}"), Modality::Video, 1_000_000_000);
        cam.frame_id = Some(format!("cam{i}_link"));
        streams.push(cam);
        transforms.push(xf("base_link", &format!("cam{i}_link")));
        // The LiDARs hang off a mount that *is* joined to `base_link`, so every one of them
        // resolves — the expensive path, where no early exit hides the cost.
        let mut lidar = rig_stream(&format!("lidar{i}"), Modality::PointCloud, 1_000_000_000);
        lidar.frame_id = Some("lidar_mount".to_string());
        streams.push(lidar);
    }
    let mut d = dataset(vec![episode(0, streams)]);
    d.calibration = Some(veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam0")],
    });

    let started = std::time::Instant::now();
    let f = autonomy::SensorFrameResolution.run(&d);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "neither the frame test nor the reachability walk may scale with the product of two counts \
         the file chooses"
    );
    assert!(
        f.is_empty(),
        "and the rig is correctly wired, so nothing is reported: {:?}",
        f.iter().take(3).collect::<Vec<_>>()
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
    // The failure this guards: a container whose payload is fingerprinted without being interpreted
    // leaves every statistical check at its `let Some(..) else { continue }`, producing nothing — and a CAN log with a wheel speed pinned at its rail for 70% of the
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
    // The one-episode dataset also draws the check's other disclosure, so select by code.
    let all = structural::ContentMeasurability.run(&d);
    let f = all
        .iter()
        .find(|f| f.code == "STRUCTURAL.UNFINGERPRINTED_CONTENT")
        .unwrap_or_else(|| panic!("{all:?}"));
    assert_eq!(f.severity, Severity::Info);
    assert!(
        f.message.contains("no episode was fully fingerprinted"),
        "the dataset-wide consequence must be stated, not inferred: {}",
        f.message
    );

    // A fully fingerprinted dataset is not accused of anything on this count. (It carries the
    // check's other disclosure, because one episode is too few to compare episodes — a different
    // fact about a different absence.)
    let hashed = |name: &str| {
        let mut s = stream(name, "c", None, &[0, 1]);
        for (i, f) in s.frames.iter_mut().enumerate() {
            f.value_ref.content_hash = Some([i as u8; 32]);
        }
        s
    };
    let d = dataset((0..4u64).map(|i| episode(i, vec![hashed("a")])).collect());
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

// ---- boundaries a mutation sweep found unpinned ----
//
// Each test below kills a relational mutant: flipping one `>=` to `>` (or `<=` to `<`) in a check
// left the whole suite green, which means the boundary itself — the run of exactly the threshold
// length, the z-score exactly at it, the interval of exactly zero — was never exercised. A threshold
// no test stands on is a threshold that can move by one without anyone noticing.

/// A run of exactly `STUCK_RUN` frames is a freeze. The constant is the *minimum* run that counts,
/// and one frame under it (tested above) is a hiccup; both sides of that line matter.
#[test]
fn a_stuck_run_of_exactly_the_threshold_is_flagged() {
    let exactly = structural::StuckStream::STUCK_RUN;
    let mut contents: Vec<u8> = vec![9; exactly];
    contents.extend([1, 2, 3]); // distinct frames after, so the run is exactly the threshold
    let cam = stream_with_content("camera", Modality::Video, &contents);
    let f = structural::StuckStream.run(&dataset(vec![episode(0, vec![cam])]));
    assert_eq!(f.len(), 1, "a run of exactly {exactly} is a freeze");
    assert!(f[0].message.contains(&exactly.to_string()));
}

/// A stream present in *every* episode is consistent, and must produce nothing. The check counts
/// episodes it appears in against the total, and "appears in all of them" is exactly the boundary.
#[test]
fn a_stream_in_every_episode_is_not_reported_as_missing() {
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", None, &[0, 1])]),
        episode(1, vec![stream("s", "c", None, &[0, 1])]),
        episode(2, vec![stream("s", "c", None, &[0, 1])]),
    ]);
    assert!(
        structural::StreamPresence.run(&d).is_empty(),
        "a stream in all three episodes is missing from none of them"
    );
}

/// The z-score threshold is inclusive: a value sitting exactly on it is an outlier. Documented as
/// "z >= threshold", and a run of the catalog with the comparison loosened by one boundary reported
/// nothing different.
#[test]
fn a_z_score_exactly_at_the_threshold_is_an_outlier() {
    let check = statistical::ExtremeOutlier::default();
    let z = check.z_threshold;
    // mean 0, std 1 → the maximum's z is its own value.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(-1.0, z, 0.0, 1.0))],
    )]);
    let f = check.run(&d);
    assert_eq!(f.len(), 1, "z exactly at the threshold is an outlier");
    assert_eq!(f[0].code, "STATISTICAL.OUTLIER");
    // And one hair under it is not.
    let under = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(-1.0, z - 0.001, 0.0, 1.0))],
    )]);
    assert!(check.run(&under).is_empty());
}

/// A repeated timestamp is an interval of exactly zero, which makes a coefficient of variation
/// meaningless — the stream is non-monotonic, which is `TEMPORAL.NON_MONOTONIC`'s finding, not
/// jitter's. Without the boundary the same frames report irregular *timing* on top of it, which
/// sends a reader after a clock problem that is really a duplicated frame.
#[test]
fn a_repeated_timestamp_makes_jitter_abstain_rather_than_report_irregular_timing() {
    // Six 100 ns intervals and two of zero: enough spread that jitter would fire if zeros counted.
    let ts = [0, 100, 200, 300, 400, 500, 600, 600, 600];
    let d = dataset(vec![episode(0, vec![stream("s", "c", None, &ts)])]);
    assert!(
        temporal::Jitter::default().run(&d).is_empty(),
        "a zero interval is a monotonicity defect, not a jitter measurement"
    );
    // The defect itself is still reported, by the check that owns it.
    assert!(!temporal::Monotonicity.run(&d).is_empty());
}

/// A declared rate of exactly zero is a corrupt declaration (`TEMPORAL.INVALID_RATE`'s finding), not
/// a baseline for other episodes to be compared against. Treated as one, every honest episode after
/// it is reported as disagreeing with a rate nobody declared.
#[test]
fn a_declared_rate_of_zero_is_not_a_baseline_for_the_other_episodes() {
    let d = dataset(vec![
        episode(0, vec![stream("s", "c", Some(0.0), &[0, 1])]),
        episode(1, vec![stream("s", "c", Some(10.0), &[0, 1])]),
        episode(2, vec![stream("s", "c", Some(10.0), &[0, 1])]),
    ]);
    assert!(
        temporal::RateConsistency.run(&d).is_empty(),
        "the two episodes that agree with each other must not be flagged against a zero"
    );
}

/// An episode with no measurable span contributes no duration at all, so it neither forms the
/// baseline nor is compared against it. Three empty episodes beside two real ones leave too few
/// durations for a baseline, and the real ones are not reported as the anomaly.
#[test]
fn an_episode_with_no_span_contributes_no_duration() {
    let zero = |index: u64| {
        let mut ep = episode(index, vec![stream("s", "c", None, &[0, 0])]);
        ep.start_ts = Some(0);
        ep.end_ts = Some(0);
        ep
    };
    let real = |index: u64| {
        let mut ep = episode(index, vec![stream("s", "c", None, &[0, 1])]);
        ep.start_ts = Some(0);
        ep.end_ts = Some(1_000_000_000);
        ep
    };
    let d = dataset(vec![zero(0), zero(1), zero(2), real(3), real(4)]);
    assert!(
        d.episodes[0].duration_ns().is_none(),
        "a zero span is not a duration of zero; it is no measurement"
    );
    assert!(
        temporal::EpisodeDuration::default().run(&d).is_empty(),
        "two durations are not a baseline, and the empty episodes are not one either"
    );
}

/// Two ego poses on the same timestamp are a monotonicity fault, not a teleport: the distance
/// between them divided by a zero interval is infinite speed, and an infinite speed clears any
/// tolerance. Without the guard a duplicated pose message — which a recorder replaying a queue
/// writes — reports the vehicle jumping at the speed of light.
#[test]
fn two_ego_poses_on_one_timestamp_are_not_an_infinite_speed() {
    let poses = vec![
        ego(0, 0.0, 0.0),
        ego(100_000_000, 0.1, 0.0),
        // The same instant, two metres apart: dt is exactly zero.
        ego(100_000_000, 2.1, 0.0),
        ego(200_000_000, 2.2, 0.0),
    ];
    let f = autonomy::EgoPoseContinuity::default().run(&dataset(vec![rig_episode_with_ego(poses)]));
    assert!(
        f.is_empty(),
        "a zero interval is not a measurement of speed: {f:#?}"
    );
}

/// A constant stream has a standard deviation of zero, and every z-score over it is infinite. That
/// is `STATISTICAL.DEGENERATE_STREAM`'s finding — a stuck sensor — and reporting it as an *outlier*
/// as well points the reader at the wrong defect, on a stream where nothing is an outlier at all.
#[test]
fn a_stream_with_no_spread_has_no_outliers() {
    let d = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(3.0, 3.0, 3.0, 0.0))],
    )]);
    assert!(
        statistical::ExtremeOutlier::default().run(&d).is_empty(),
        "with a zero standard deviation there is no z-scale to be extreme on"
    );

    // And the corrupt version of the same shape — a source that stored a spread of zero beside a
    // min and max that disagree with it — divides by that zero into an infinite z, which would make
    // every value an extreme outlier over statistics that contradict themselves.
    let inconsistent = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(0.0, 10.0, 5.0, 0.0))],
    )]);
    assert!(
        statistical::ExtremeOutlier::default()
            .run(&inconsistent)
            .is_empty(),
        "a zero spread under a non-zero range is corrupt, not an outlier"
    );
    // And the check that owns corrupt stored statistics says so, rather than everyone stepping
    // aside for someone else.
    let f = statistical::RangeSanity.run(&inconsistent);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.STD_IMPLAUSIBLE");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("zero over range"), "{}", f[0].message);
}

/// The rule is the *lower* bound of the same inequality, and only its exact point is impossible: a
/// distribution can sit almost entirely at its mean, so a small std over a wide range is ordinary
/// data, not corruption. And a genuinely constant stream keeps its own finding.
#[test]
fn a_small_std_over_a_wide_range_is_ordinary_and_a_constant_stream_is_degenerate() {
    let tiny_spread = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(0.0, 10.0, 5.0, 0.02))],
    )]);
    assert!(
        statistical::RangeSanity.run(&tiny_spread).is_empty(),
        "a narrow distribution between two rare extremes is data, not a contradiction"
    );

    let constant = dataset(vec![episode(
        0,
        vec![stream_with_stats("state", stats(3.0, 3.0, 3.0, 0.0))],
    )]);
    let f = statistical::RangeSanity.run(&constant);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].code, "STATISTICAL.DEGENERATE",
        "a stuck sensor is degenerate, not impossible"
    );
}

/// When a stream is pinned equally at both rails, the finding names *one* of them, and which one is
/// not arbitrary: the maximum is reported on a tie, so the same data always produces the same
/// finding text — and the same content hash over the report that carries it.
#[test]
fn saturation_at_both_rails_equally_names_the_maximum() {
    // 60 of 100 at each rail (a signal that only ever sits at one end or the other), so both
    // fractions clear the threshold and the tie is what decides which is named.
    let d = dataset(vec![episode(
        0,
        vec![stream_with_saturation("gripper", 100, 60, 60, 0.0, 1.0)],
    )]);
    let f = statistical::Saturation::default().run(&d);
    assert_eq!(f.len(), 1);
    assert!(
        f[0].message.contains("maximum"),
        "a tie is broken toward the maximum, deterministically: {}",
        f[0].message
    );
}

/// An episode that declares a single instant — `start_ts == end_ts`, which is what a one-frame or
/// zero-length recording writes — still bounds its annotations. Read as "no span", an annotation
/// anywhere at all becomes unjudgeable, and the check that exists to catch a label attached to a
/// moment the episode never recorded goes quiet on the episode with the least room to be right.
#[test]
fn an_episode_declaring_one_instant_still_bounds_its_annotations() {
    let mut ep = episode(0, vec![stream("s", "c", None, &[])]);
    ep.start_ts = Some(1_000);
    ep.end_ts = Some(1_000);
    ep.labels = vec![Label {
        key: "language".into(),
        value: "pick up the block".into(),
        ts: Some(5_000),
    }];
    let f = semantic::AnnotationIntegrity.run(&dataset(vec![ep]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "SEMANTIC.ANNOTATION_UNALIGNED");

    // And one on the instant itself is aligned.
    let mut ok = episode(0, vec![stream("s", "c", None, &[])]);
    ok.start_ts = Some(1_000);
    ok.end_ts = Some(1_000);
    ok.labels = vec![Label {
        key: "language".into(),
        value: "pick up the block".into(),
        ts: Some(1_000),
    }];
    assert!(semantic::AnnotationIntegrity
        .run(&dataset(vec![ok]))
        .is_empty());
}

/// A declared rate sitting exactly on the gap factor still counts as agreeing with the observed
/// cadence, so the declared period is what gaps are measured against. On the other side of that
/// line the check falls back to the observed median, and the same recording reports gaps it does
/// not have.
#[test]
fn a_declared_rate_exactly_at_the_gap_factor_is_still_trusted() {
    // Observed median interval 100 ns; declared period 300 ns — exactly 3x, the default factor.
    let mut ts: Vec<i64> = (0..12).map(|i| i * 100).collect();
    // One interval of 500 ns: over 3x the observed median, under 3x the declared period.
    ts.push(ts.last().unwrap() + 500);
    let rate = 1_000_000_000.0 / 300.0;
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(rate), &ts)])]);
    assert!(
        temporal::Gaps::default().run(&d).is_empty(),
        "the declared cadence is trusted at exactly the factor, and 500 ns is inside 3x300"
    );
}

/// A declaration that is not a range describes nothing to conform to: a maximum below its own
/// minimum would put every value outside it, turning one corrupt line into a finding about the data.
#[test]
fn an_inverted_declared_range_is_not_a_breach_of_itself() {
    let mut stream = stream_with_stats("signal", stats(0.0, 10.0, 5.0, 2.0));
    stream.observed_stats = stream.stats;
    stream.stats = None;
    stream.declared_range = Some(veridex_core::cdm::DeclaredRange {
        min: 100.0,
        max: -100.0,
    });
    let d = dataset(vec![episode(0, vec![stream.clone()])]);
    assert!(
        statistical::DeclaredRangeConformance.run(&d).is_empty(),
        "an inverted declaration is corrupt, not a range the data failed"
    );

    // The same values against a real declaration they do exceed are reported.
    let mut sound = stream;
    sound.declared_range = Some(veridex_core::cdm::DeclaredRange { min: 0.0, max: 1.0 });
    let f = statistical::DeclaredRangeConformance.run(&dataset(vec![episode(0, vec![sound])]));
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.OUT_OF_DECLARED_RANGE");
}

// ---- structural.step-alignment ----

/// A step-indexed stream of `n` steps: what an HDF5 `demo_0` array or a Zarr array becomes.
fn step_stream(name: &str, clock: &str, n: i64) -> Stream {
    let mut s = stream(name, clock, None, &(0..n).collect::<Vec<i64>>());
    s.clock_kind = ClockKind::StepIndex;
    s
}

#[test]
fn one_row_apart_is_the_terminal_observation_convention_two_is_a_mismatch() {
    // A dataset that stores one more observation than actions is following the terminal-observation
    // convention — the last observation has no action after it — so a difference of one is expected
    // and silent. Unpinned: the sweep's `<` → `<=` mutation left the suite green, and the rule would
    // then have flagged that convention as a corrupt episode on every dataset that uses it. Whole
    // rows, so the boundary is landed on exactly.
    let pair = |a: i64, b: i64| {
        dataset(vec![episode(
            0,
            vec![
                step_stream("action", "hdf5-step-index", a),
                step_stream("observation.state", "hdf5-step-index", b),
            ],
        )])
    };
    assert!(
        structural::StepAlignment.run(&pair(50, 51)).is_empty(),
        "one more observation than actions is the convention, not a fault"
    );
    assert_eq!(
        structural::StepAlignment.run(&pair(50, 52)).len(),
        1,
        "two apart is a mismatch"
    );
}

#[test]
fn step_indexed_streams_that_disagree_on_the_episodes_length_are_flagged() {
    // The gap this closes: the temporal family abstains on a step index (correctly — an index is
    // flawlessly monotonic), and nothing else compared the arrays. An episode holding 100 actions
    // beside 50 observations came back clean, with every pair past row 50 built from the wrong
    // observation.
    let d = dataset(vec![episode(
        0,
        vec![
            step_stream("action", "hdf5-step-index", 100),
            step_stream("observation.state", "hdf5-step-index", 50),
        ],
    )]);
    let f = structural::StepAlignment.run(&d);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STRUCTURAL.STEP_COUNT_MISMATCH");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(
        f[0].message.contains("`action` holds 100")
            && f[0].message.contains("`observation.state` holds 50"),
        "{}",
        f[0].message
    );
}

#[test]
fn a_terminal_observation_one_row_longer_than_the_actions_is_not_a_defect() {
    // Several collectors store the observation a trajectory *ends* in, giving `observation` one row
    // more than `action`. That is a deliberate convention, and a check that fired on it would fail
    // sound robomimic data — the fastest way for a real user to conclude the tool is wrong.
    let d = dataset(vec![episode(
        0,
        vec![
            step_stream("action", "hdf5-step-index", 100),
            step_stream("observation.state", "hdf5-step-index", 101),
        ],
    )]);
    assert!(structural::StepAlignment.run(&d).is_empty());
}

#[test]
fn measured_time_and_separate_step_counters_are_left_to_the_checks_that_grade_them() {
    // Streams on *measured* time are the temporal family's business — a length difference there is
    // CLOCK_SKEW, and reporting it twice under two names helps nobody.
    let measured = dataset(vec![episode(
        0,
        vec![
            stream("action", "c", None, &[0, 1, 2, 3]),
            stream("observation.state", "c", None, &[0]),
        ],
    )]);
    assert!(structural::StepAlignment.run(&measured).is_empty());

    // Two *different* step counters are two independent indexings; comparing across them compares
    // nothing, so an RLDS episode beside an HDF5 one in the same CDM is not accused.
    let two_clocks = dataset(vec![episode(
        0,
        vec![
            step_stream("action", "hdf5-step-index", 100),
            step_stream("frames", "rlds-step-index", 10),
        ],
    )]);
    assert!(structural::StepAlignment.run(&two_clocks).is_empty());

    // An empty stream is `STRUCTURAL.EMPTY_STREAM`'s finding; counting it here would report the
    // same defect twice under a name that misdescribes it.
    let with_empty = dataset(vec![episode(
        0,
        vec![
            step_stream("action", "hdf5-step-index", 100),
            step_stream("observation.state", "hdf5-step-index", 0),
        ],
    )]);
    assert!(structural::StepAlignment.run(&with_empty).is_empty());
}

// ---- structural.frozen-episode ----

/// An `action` vector stream whose frames carry the given fingerprints, one per frame.
fn action_stream(name: &str, hashes: &[u8]) -> Stream {
    let mut s = stream(
        name,
        "c",
        Some(10.0),
        &(0..hashes.len() as i64).collect::<Vec<i64>>(),
    );
    s.modality = Modality::Action;
    s.shape = Some(vec![7]);
    for (frame, h) in s.frames.iter_mut().zip(hashes) {
        frame.value_ref.content_hash = Some([*h; 32]);
    }
    s
}

/// `n` episodes of `action`, with the ones named in `frozen` holding a single repeated frame.
fn dataset_with_frozen(n: u64, frozen: &[u64]) -> veridex_core::cdm::Dataset {
    dataset(
        (0..n)
            .map(|i| {
                let hashes: Vec<u8> = if frozen.contains(&i) {
                    vec![7; 20]
                } else {
                    (0..20)
                        .map(|k| (i as u8).wrapping_mul(31).wrapping_add(k))
                        .collect()
                };
                episode(i, vec![action_stream("action", &hashes)])
            })
            .collect(),
    )
}

#[test]
fn an_episode_where_nothing_moved_is_flagged() {
    // The gap this closes: `stuck-stream` looks only at video, and `DEGENERATE` reads statistics
    // that LeRobot computes dataset-wide — so one dead episode among five scored like five good
    // ones, and the policy learned that holding still is sometimes correct.
    let f = structural::FrozenEpisode.run(&dataset_with_frozen(5, &[3]));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "STRUCTURAL.FROZEN_EPISODE");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(
        matches!(&f[0].location, veridex_core::check::Location::Stream { episode: 3, stream } if stream == "action"),
        "{:?}",
        f[0].location
    );
    assert!(f[0].message.contains("4 of the 5"), "{}", f[0].message);
}

#[test]
fn frozen_in_exactly_half_the_episodes_is_the_dataset_not_a_fault() {
    // "A minority, strictly": a stream frozen in half its episodes or more is the dataset's shape.
    // Both halves of that sentence were unpinned — a mutation sweep flipping `>=` to `>` at the
    // minority rule, and `<` to `<=` at the episode floor, both left the suite green. So the rule
    // could have started reporting a dataset half of which is deliberately still, or stopped
    // judging a dataset at exactly the episode count it needs to judge one.
    //
    // Two of four is exactly half and is not reported; one of four is a minority and is.
    assert!(structural::FrozenEpisode
        .run(&dataset_with_frozen(4, &[0, 2]))
        .is_empty());
    assert_eq!(
        structural::FrozenEpisode
            .run(&dataset_with_frozen(4, &[2]))
            .len(),
        1
    );

    // And the floor: three episodes is the minimum that can be judged, two is not.
    assert_eq!(
        structural::FrozenEpisode
            .run(&dataset_with_frozen(3, &[1]))
            .len(),
        1
    );
    assert!(structural::FrozenEpisode
        .run(&dataset_with_frozen(2, &[1]))
        .is_empty());
}

#[test]
fn a_string_has_no_minimum_and_that_is_not_a_gap_in_the_run() {
    // Two silences the statistical family used to report as one. A numeric stream this run did not
    // summarize is *unmeasured*: read it again where the values are read. A text feature has no
    // minimum and imagery is pixels Veridex never decodes — those are *unmeasurable*, here and in
    // every other format, so the remedy that says "check them in a format whose values Veridex
    // reads" sends their reader after a summary that does not exist.
    //
    // Live misdirection, found by running the CLI on the RLDS demo: its `language_instruction` was
    // named as unread beside a remedy listing RLDS among the formats whose values Veridex reads.
    let mut text = stream("language_instruction", "c", None, &[0, 1]);
    text.dtype = Some("string".into());
    let mut image = stream("observation/image", "c", None, &[0, 1]);
    image.modality = Modality::Video;
    image.dtype = Some("uint8".into());
    let numeric = stream("observation/state", "c", None, &[0, 1]);

    let f =
        statistical::ValueMeasurability.run(&dataset(vec![episode(0, vec![text, image, numeric])]));
    let by_code = |code: &str| -> Option<String> {
        f.iter().find(|x| x.code == code).map(|x| x.message.clone())
    };

    let unmeasurable = by_code("STATISTICAL.UNMEASURABLE_VALUES").expect("{f:?}");
    assert!(
        unmeasurable.contains("language_instruction"),
        "{unmeasurable}"
    );
    assert!(
        !unmeasurable.contains("observation/state") && !unmeasurable.contains("observation/image"),
        "only a stream no source could summarize belongs here: {unmeasurable}"
    );

    // ...and the two whose values a different source *could* summarize stay unmeasured, with the
    // remedy that fits them.
    let unmeasured = by_code("STATISTICAL.UNMEASURED_VALUES").expect("{f:?}");
    assert!(unmeasured.contains("observation/state"), "{unmeasured}");
    assert!(unmeasured.contains("observation/image"), "{unmeasured}");
    assert!(
        !unmeasured.contains("language_instruction"),
        "the two causes must not be mixed: {unmeasured}"
    );
}

#[test]
fn a_stream_frozen_in_most_episodes_is_how_the_dataset_is_built() {
    // Frozen in three of five is not an anomaly in the dataset, it is the dataset. The same
    // reasoning as comparing an episode's duration against the dataset's own median.
    assert!(structural::FrozenEpisode
        .run(&dataset_with_frozen(5, &[0, 1, 3]))
        .is_empty());
    // And with nothing to compare against, nothing is claimed.
    assert!(structural::FrozenEpisode
        .run(&dataset_with_frozen(2, &[1]))
        .is_empty());
}

#[test]
fn a_single_column_flag_is_not_an_actuator_that_stopped() {
    // A `reward` or `done` column is legitimately constant through a demonstration that did not
    // succeed. Only a stream carrying more than one scalar per frame is an actuator or a sensor,
    // and the source's own declared shape is what says which this is — no guess at what the column
    // means.
    let d = dataset(
        (0..5u64)
            .map(|i| {
                let hashes: Vec<u8> = if i == 3 {
                    vec![7; 20]
                } else {
                    (0..20)
                        .map(|k| (i as u8).wrapping_mul(31).wrapping_add(k))
                        .collect()
                };
                let mut s = action_stream("next.reward", &hashes);
                s.modality = Modality::ScalarState;
                s.shape = Some(vec![1]);
                episode(i, vec![s])
            })
            .collect(),
    );
    assert!(structural::FrozenEpisode.run(&d).is_empty());
}

#[test]
fn an_unfingerprinted_episode_is_not_evidence_either_way() {
    // A stream Veridex could not fingerprint proves nothing about whether it moved, and must not be
    // counted as an episode where it did — that would inflate the denominator the minority rule
    // divides by and let a real frozen episode through.
    let mut d = dataset_with_frozen(5, &[3]);
    for ep in d.episodes.iter_mut().filter(|e| e.index != 3) {
        ep.streams[0].frames[0].value_ref.content_hash = None;
    }
    // Four episodes are now unreadable, leaving one examined episode: too few to judge.
    assert!(structural::FrozenEpisode.run(&d).is_empty());
}

// ---- AUTONOMY.CALIBRATION_IMPLAUSIBLE ----

#[test]
fn a_camera_with_no_focal_length_is_not_a_calibrated_camera() {
    // What an uncalibrated ROS camera driver publishes: a `CameraInfo` of all zeros. It satisfies
    // every presence test — intrinsics are *there* — so the rig scored a clean pass and the
    // `world-model-ready` calibration criterion reported green, over a camera that can project
    // nothing. Present is not usable.
    let mut zeroed = intr("cam");
    zeroed.fx = 0.0;
    zeroed.fy = 0.0;
    zeroed.cx = 0.0;
    zeroed.cy = 0.0;
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![zeroed],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_IMPLAUSIBLE");
    assert_eq!(
        f[0].severity,
        Severity::Error,
        "a focal length of zero is arithmetic with no answer, not a judgment call"
    );
    assert!(f[0].message.contains("focal length"), "{}", f[0].message);
}

#[test]
fn an_uninitialized_transform_is_not_a_pose() {
    // An all-zero quaternion is what an unset transform holds; it is not a rotation, so the
    // transform places nothing — while a presence check counts it as a calibrated edge.
    let mut dead = xf("base_link", "lidar");
    dead.pose.rotation = [0.0, 0.0, 0.0, 0.0];
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![dead, xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_IMPLAUSIBLE");
    assert!(f[0].message.contains("zero rotation"), "{}", f[0].message);
}

#[test]
fn only_impossible_calibration_is_flagged_never_merely_unusual() {
    // A long lens, an off-centre principal point, a strong distortion coefficient and a quaternion
    // off unit by the rounding an honest producer does are all legitimate. Judging plausibility
    // would need the image dimensions the CDM does not carry, so it is not attempted — a wrong
    // accusation about a working rig is worse than the silence it replaces. The quaternion here is
    // 0.2% off unit, well inside the 1% tolerance that separates rounding from a scale error.
    let mut unusual = intr("cam");
    unusual.fx = 12_000.0;
    unusual.fy = 0.5;
    unusual.cx = 0.0;
    unusual.cy = 4000.0;
    unusual.distortion = vec![-3.0, 9.5, 0.0, 0.0, 0.0];
    let mut off_norm = xf("base_link", "lidar");
    off_norm.pose.rotation = [0.0, 0.0, 0.0, 0.998];
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![off_norm, xf("base_link", "cam")],
        intrinsics: vec![unusual],
    };
    assert!(autonomy::CalibrationCompleteness
        .run(&rig_with_calibration(Some(cal)))
        .is_empty());
}

#[test]
fn a_quaternion_that_is_not_unit_is_a_rotation_with_a_scale_in_it() {
    // What a producer that writes only the vector part of a quaternion leaves behind: a 90° yaw
    // recorded as [0.707, 0, 0, 0] instead of [0.707, 0, 0, 0.707]. Every presence check passes —
    // the edge is there, the numbers are finite, and it is nowhere near the all-zero uninitialized
    // value the norm floor catches. But a quaternion is a rotation only when it is a *unit*
    // quaternion, and the standard conversion to a matrix does not renormalize: norm 0.707 composes
    // a uniform scale of 0.5 into the transform, so every LiDAR point it places lands at half its
    // real distance from the rig, and the fused scene is quietly wrong rather than visibly broken.
    let mut half = xf("base_link", "lidar");
    half.pose.rotation = [0.707, 0.0, 0.0, 0.0];
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![half, xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_IMPLAUSIBLE");
    assert_eq!(
        f[0].severity,
        Severity::Error,
        "a quaternion that is not a rotation is arithmetic with no answer, not a judgment call"
    );
    // The reader needs both numbers: the norm to recognize which producer bug this is, and the
    // scale to know how far the placement is off.
    assert!(f[0].message.contains("norm 0.7070"), "{}", f[0].message);
    assert!(f[0].message.contains("0.4998"), "{}", f[0].message);
    // Named, so a reader knows which edge to re-publish.
    assert!(
        f[0].message.contains("`base_link` → `lidar`"),
        "{}",
        f[0].message
    );
}

#[test]
fn the_rotation_norm_boundary_holds_in_both_directions() {
    // The tolerance has to sit far from honest rounding and far from the defect, and a rule pinned
    // on only one side of its threshold passes just as well with the threshold moved. Both sides,
    // above and below 1, so the rule is not accidentally one-tailed: a quaternion read with a
    // too-large scale factor overshoots exactly as a truncated one undershoots.
    let flagged = |q: [f64; 4]| {
        let mut t = xf("base_link", "lidar");
        t.pose.rotation = q;
        let cal = veridex_core::cdm::Calibration {
            transforms: vec![t, xf("base_link", "cam")],
            intrinsics: vec![intr("cam")],
        };
        !autonomy::CalibrationCompleteness
            .run(&rig_with_calibration(Some(cal)))
            .is_empty()
    };
    // Inside 1%: rounding an honest producer does, on both sides. Silent.
    assert!(!flagged([0.0, 0.0, 0.0, 0.995]));
    assert!(!flagged([0.0, 0.0, 0.0, 1.005]));
    // Outside 1%: a scale in the transform, on both sides. Flagged.
    assert!(flagged([0.0, 0.0, 0.0, 0.98]));
    assert!(flagged([0.0, 0.0, 0.0, 1.02]));
}

#[test]
fn a_systematically_broken_calibration_is_a_bounded_report_not_one_finding_per_edge() {
    // The defects this rule finds are the systematic kind: a producer that drops the `w` component
    // drops it on every edge it writes. Nothing caps how many transforms a file may declare, so one
    // finding per bad edge is a report size the *input file* chooses — and every finding reaches
    // the terminal, the JSON, the SARIF and the signed certificate. Bounded to eight, with the
    // remainder counted rather than dropped: a bound that stops a check mid-judgement has to reach
    // the verdict, or a reader cannot tell a capped report from a complete one.
    let mut transforms: Vec<_> = (0..50)
        .map(|i| {
            let mut t = xf("base_link", &format!("lidar_{i}"));
            t.pose.rotation = [0.707, 0.0, 0.0, 0.0];
            t
        })
        .collect();
    transforms.push(xf("base_link", "cam"));
    let cal = veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    let unusable: Vec<_> = f
        .iter()
        .filter(|x| x.code == "AUTONOMY.CALIBRATION_IMPLAUSIBLE")
        .collect();
    assert_eq!(unusable.len(), 9, "eight named, one summarizing the rest");
    let summary = unusable.last().unwrap();
    assert!(
        summary
            .message
            .contains("42 further calibration element(s)"),
        "the skipped elements must be counted, not silently dropped: {}",
        summary.message
    );
}

#[test]
fn a_principal_point_outside_the_image_is_a_calibration_for_a_different_camera() {
    // Intrinsics calibrated at 1920×1080 and applied to a stream recorded at 640×480: `cx` of 960
    // is the centre of the image it was computed for and off the right-hand edge of the one that
    // was recorded. Every presence check passes, the focal length is positive and finite, and the
    // undistortion silently rectifies about a point outside the sensor. The `CameraInfo` states the
    // image size in the same message as the matrix, so this is arithmetic, not a judgement.
    let mut mismatched = intr("cam");
    mismatched.cx = 960.0;
    mismatched.cy = 540.0;
    mismatched.width = Some(640);
    mismatched.height = Some(480);
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![mismatched],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_IMPLAUSIBLE");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(f[0].message.contains("640×480"), "{}", f[0].message);
    assert!(f[0].message.contains("principal point"), "{}", f[0].message);
}

#[test]
fn a_camera_that_never_said_how_big_its_image_is_is_not_judged_against_one() {
    // The rule reads only what the calibration itself declares. A source that carries no dimensions
    // (an MF4 rig, an HDF5 collector, a driver publishing `width: 0`) says nothing about where the
    // principal point should fall, and assuming an image would flag every honest calibration whose
    // source is quieter than ROS. Abstention, not a guess.
    let mut undeclared = intr("cam");
    undeclared.cx = 100_000.0;
    undeclared.cy = 100_000.0;
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![undeclared],
    };
    assert!(autonomy::CalibrationCompleteness
        .run(&rig_with_calibration(Some(cal)))
        .is_empty());
}

#[test]
fn the_image_boundary_is_the_last_pixel_not_the_width() {
    // A 640-wide image's rightmost pixel is 639, so a principal point at 639.9 is inside it and one
    // at 640.0 is not. Pinned in both directions: an off-centre principal point is legitimate and a
    // rule that flagged it would accuse a working wide-angle rig, while a rule that only fired well
    // past the edge would miss the transposed-matrix case by a pixel and pass just the same.
    let judged = |cx: f64, cy: f64| {
        let mut k = intr("cam");
        k.cx = cx;
        k.cy = cy;
        k.width = Some(640);
        k.height = Some(480);
        let cal = veridex_core::cdm::Calibration {
            transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
            intrinsics: vec![k],
        };
        !autonomy::CalibrationCompleteness
            .run(&rig_with_calibration(Some(cal)))
            .is_empty()
    };
    assert!(!judged(639.9, 479.9), "the far edge is still in the image");
    assert!(judged(640.0, 240.0), "one past the last column is outside");
    assert!(judged(320.0, 480.0), "one past the last row is outside");
}

#[test]
fn coefficients_that_do_not_fit_their_own_distortion_model_cannot_be_applied() {
    // What a calibration copied between two models leaves behind: five `plumb_bob` coefficients
    // still declared under `rational_polynomial`, which takes eight. The numbers are finite, the
    // focal length is positive, the principal point is inside the image — every presence test and
    // every other impossibility rule passes — and undistortion has no defined result, because three
    // of the eight terms the model needs were never written. The coefficients themselves are still
    // not interpreted; only how many of them there are, which is not a guess.
    let mut truncated = intr("cam");
    truncated.distortion_model = Some("rational_polynomial".into());
    truncated.distortion = vec![0.1, -0.2, 0.0, 0.0, 0.0];
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![truncated],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_IMPLAUSIBLE");
    assert_eq!(f[0].severity, Severity::Error);
    assert!(
        f[0].message.contains("rational_polynomial"),
        "{}",
        f[0].message
    );
    assert!(
        f[0].message.contains("8 coefficient(s)"),
        "{}",
        f[0].message
    );
    assert!(f[0].message.contains("records 5"), "{}", f[0].message);
}

#[test]
fn a_distortion_model_is_an_open_namespace_and_an_unknown_one_is_not_a_disagreement() {
    // Three ways this rule must stay silent, and each is a working camera it would otherwise
    // accuse. A model name Veridex has not heard of says nothing about how many coefficients it
    // takes — the same reasoning `canonical_codec` follows, because a closed table judging an open
    // namespace flags honest data. An empty `d` is what `CameraInfo` specifies for a camera with no
    // distortion, so a rectified stream legitimately names a model and writes no coefficients. And
    // the right count under a known model is, of course, fine.
    let case = |model: Option<&str>, d: Vec<f64>| {
        let mut k = intr("cam");
        k.distortion_model = model.map(str::to_string);
        k.distortion = d;
        let cal = veridex_core::cdm::Calibration {
            transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
            intrinsics: vec![k],
        };
        autonomy::CalibrationCompleteness
            .run(&rig_with_calibration(Some(cal)))
            .is_empty()
    };
    assert!(case(Some("some_new_fisheye_model"), vec![0.1, 0.2, 0.3]));
    assert!(case(Some("plumb_bob"), vec![]));
    assert!(case(Some("plumb_bob"), vec![0.1, -0.2, 0.0, 0.0, 0.0]));
    assert!(case(Some("equidistant"), vec![0.1, 0.2, 0.3, 0.4]));
    assert!(case(None, vec![0.1, 0.2, 0.3]));
    // And the count is what is judged, not the name's spelling: the same list under a model that
    // takes a different number of terms is the defect.
    assert!(!case(Some("plumb_bob"), vec![0.1, 0.2, 0.3, 0.4]));
}

// ---- AUTONOMY.EGO_FRAME_UNKNOWN ----

/// The `rig_with_calibration` rig, with an ego trajectory recorded for `frame`.
fn rig_with_ego_frame(frame: Option<&str>) -> Dataset {
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let mut d = rig_with_calibration(Some(cal));
    d.episodes[0].ego_frame = frame.map(str::to_string);
    d
}

#[test]
fn a_trajectory_for_a_frame_the_tree_never_names_tracks_a_different_vehicle() {
    // A rig publishing odometry for `base_footprint` while its transform tree roots at `base_link`.
    // Every other frame check passes: the tree is well-formed, every sensor declares a frame the
    // tree knows, and each reaches the camera. The ego-pose stream is deliberately outside the
    // per-sensor rule, because its *reference* frame (`odom`, `map`) is joined to the body
    // dynamically — but the body frame the trajectory is *of* is exactly the static question, and
    // nothing was asking it. Every sensor's extrinsics hang off that frame, so a trajectory
    // expressed for one outside the tree cannot place a single observation along the drive.
    let f = autonomy::SensorFrameResolution.run(&rig_with_ego_frame(Some("base_footprint")));
    let ego: Vec<_> = f
        .iter()
        .filter(|x| x.code == "AUTONOMY.EGO_FRAME_UNKNOWN")
        .collect();
    assert_eq!(ego.len(), 1, "{f:?}");
    assert_eq!(ego[0].severity, Severity::Error);
    assert!(
        ego[0].message.contains("base_footprint"),
        "{}",
        ego[0].message
    );
}

#[test]
fn a_trajectory_for_a_frame_the_tree_does_name_is_the_rig_it_claims_to_be() {
    // The body frame in the tree is the whole point of the rule, so it must stay silent there —
    // and silent too where the source names no body frame at all: a trajectory that does not say
    // what it is of is not a trajectory of the wrong thing.
    let ego_findings = |frame: Option<&str>| {
        autonomy::SensorFrameResolution
            .run(&rig_with_ego_frame(frame))
            .into_iter()
            .filter(|x| x.code == "AUTONOMY.EGO_FRAME_UNKNOWN")
            .count()
    };
    assert_eq!(ego_findings(Some("base_link")), 0);
    assert_eq!(ego_findings(None), 0);
}

// ---- AUTONOMY.SEQUENCE_DROPPED / _RENUMBERED ----

/// A rig whose LiDAR carries the given publisher-numbering summary.
fn rig_with_numbering(q: Option<veridex_core::cdm::SequenceNumbers>) -> Dataset {
    let mut ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    ep.streams[0].observed_sequence = q;
    dataset(vec![ep])
}

#[test]
fn a_hole_in_the_publishers_numbering_is_a_count_not_an_estimate() {
    let f = autonomy::SequenceComplete::default().run(&rig_with_numbering(Some(
        veridex_core::cdm::SequenceNumbers {
            message_count: 900,
            missing: 100,
            non_increasing: 0,
        },
    )));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SEQUENCE_DROPPED");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(
        f[0].message.contains("numbered 1000") && f[0].message.contains("holds 900"),
        "{}",
        f[0].message
    );
}

#[test]
fn the_measured_drop_rule_is_pinned_at_its_boundary() {
    // Exactly at the configured fraction is not a departure from it; one message past is. Pinned
    // because the threshold is what the check promises, and `>=` and `>` differ by one message.
    let at = |missing: u64, kept: u64| {
        autonomy::SequenceComplete::default()
            .run(&rig_with_numbering(Some(
                veridex_core::cdm::SequenceNumbers {
                    message_count: kept,
                    missing,
                    non_increasing: 0,
                },
            )))
            .len()
    };
    assert_eq!(at(50, 950), 0, "5% of 1000 is exactly the default");
    assert_eq!(at(51, 949), 1);
}

#[test]
fn a_counter_that_restarts_makes_every_hole_below_it_unreadable() {
    // A gap after a restart is the distance between two unrelated counts. Reporting a drop fraction
    // from it would be arithmetic on numbers that are not comparable, so the restart is reported
    // instead — and completeness for the stream is left explicitly unverified.
    let f = autonomy::SequenceComplete::default().run(&rig_with_numbering(Some(
        veridex_core::cdm::SequenceNumbers {
            message_count: 900,
            missing: 100,
            non_increasing: 1,
        },
    )));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SEQUENCE_RENUMBERED");
    assert!(f[0].message.contains("1 time(s)"), "{}", f[0].message);
}

#[test]
fn a_publisher_that_lost_nothing_says_nothing() {
    assert!(autonomy::SequenceComplete::default()
        .run(&rig_with_numbering(Some(
            veridex_core::cdm::SequenceNumbers {
                message_count: 1000,
                missing: 0,
                non_increasing: 0,
            },
        )))
        .is_empty());
}

// ---- AUTONOMY.SENSOR_CLOCK_UNSET / _REGRESSION / _OFFSET / _UNREAD ----

/// A rig whose LiDAR carries the given capture-stamp summary. The two sensors beside it stamped
/// their data cleanly, which is what makes the finding about one stream rather than the rig.
fn rig_with_stamps(stamps: Option<veridex_core::cdm::HeaderStamps>) -> Dataset {
    let healthy = veridex_core::cdm::HeaderStamps {
        message_count: 100,
        unset: 0,
        min_offset_ns: 5_000_000,
        max_offset_ns: 6_000_000,
        regressions: 0,
    };
    let mut ep = episode(
        0,
        vec![
            rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
            rig_stream("gnss", Modality::Gnss, 1_000_000_000),
            rig_stream("imu", Modality::Imu, 1_000_000_000),
        ],
    );
    ep.streams[0].observed_header_stamps = stamps;
    ep.streams[1].observed_header_stamps = Some(healthy);
    ep.streams[2].observed_header_stamps = Some(healthy);
    dataset(vec![ep])
}

fn sensor_clock() -> autonomy::SensorClock {
    autonomy::SensorClock {
        max_offset_ns: 1_000_000_000,
    }
}

#[test]
fn a_sensor_that_never_stamped_its_data_has_no_clock_of_its_own() {
    // A bag carries two clocks and only one of them reaches a frame timestamp: the recorder's. When
    // the sensor's own `header.stamp` was never set, there is nothing for the recorder's clock to be
    // standing in for — so the rig's sync result is about the recording host's scheduler, and every
    // timing check still passes because they all read the same recorder's clock.
    let f = sensor_clock().run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
        message_count: 600,
        unset: 600,
        min_offset_ns: 0,
        max_offset_ns: 0,
        regressions: 0,
    })));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_CLOCK_UNSET");
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(
        f[0].location,
        veridex_core::check::Location::Stream {
            episode: 0,
            stream: "lidar".into()
        }
    );
    assert!(f[0].message.contains("600"), "{}", f[0].message);
}

#[test]
fn a_driver_that_stopped_stamping_partway_is_a_warning_not_a_dead_clock() {
    // Some messages stamped, not all. Distinguished from the never-stamped case the same way a
    // dropped sweep is distinguished from a dead LiDAR: the recording holds real capture times on
    // one side of it, so the segment may be usable once the affected span is cut.
    let f = sensor_clock().run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
        message_count: 600,
        unset: 37,
        min_offset_ns: 5_000_000,
        max_offset_ns: 6_000_000,
        regressions: 0,
    })));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_CLOCK_UNSET");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("37 of"), "{}", f[0].message);
}

#[test]
fn a_sensor_clock_that_steps_backwards_puts_two_capture_times_on_one_instant() {
    let f = sensor_clock().run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
        message_count: 600,
        unset: 0,
        min_offset_ns: 5_000_000,
        max_offset_ns: 6_000_000,
        regressions: 1,
    })));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_CLOCK_REGRESSION");
    assert_eq!(f[0].severity, Severity::Error);
}

#[test]
fn a_constant_pipeline_latency_is_not_a_clock_disagreement() {
    // The offset rule reads both bounds, not the spread between them. 80 ms of camera latency, and
    // 300 ms of jitter on top of it, is one clock with a slow pipeline — the temporal family already
    // grades the jitter from the frame timestamps. Only the *closest* the two clocks came all
    // recording says they are different clocks.
    assert!(sensor_clock()
        .run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
            message_count: 600,
            unset: 0,
            min_offset_ns: 80_000_000,
            max_offset_ns: 380_000_000,
            regressions: 0,
        })))
        .is_empty());
}

#[test]
fn a_sensor_host_that_never_disciplined_its_clock_is_caught_in_both_directions() {
    // An hour behind: every message arrived an hour after the sensor says it sampled.
    let behind = sensor_clock().run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
        message_count: 600,
        unset: 0,
        min_offset_ns: 3_600_000_000_000,
        max_offset_ns: 3_600_100_000_000,
        regressions: 0,
    })));
    assert_eq!(behind.len(), 1, "{behind:?}");
    assert_eq!(behind[0].code, "AUTONOMY.SENSOR_CLOCK_OFFSET");
    assert_eq!(behind[0].severity, Severity::Warning);
    assert!(
        behind[0].message.contains("behind"),
        "{}",
        behind[0].message
    );

    // And ahead: a sensor clock running in front of the recorder's, which is not a latency at all.
    let ahead = sensor_clock().run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
        message_count: 600,
        unset: 0,
        min_offset_ns: -3_600_100_000_000,
        max_offset_ns: -3_600_000_000_000,
        regressions: 0,
    })));
    assert_eq!(ahead.len(), 1, "{ahead:?}");
    assert_eq!(ahead[0].code, "AUTONOMY.SENSOR_CLOCK_OFFSET");
    assert!(
        ahead[0].message.contains("ahead of"),
        "{}",
        ahead[0].message
    );
}

#[test]
fn the_offset_rule_is_pinned_at_its_boundary() {
    // Exactly at the tolerance is not a departure from it; one nanosecond past is.
    let at = |off: i64| {
        sensor_clock()
            .run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
                message_count: 10,
                unset: 0,
                min_offset_ns: off,
                max_offset_ns: off,
                regressions: 0,
            })))
            .len()
    };
    assert_eq!(at(1_000_000_000), 0);
    assert_eq!(at(1_000_000_001), 1);
    assert_eq!(at(-1_000_000_000), 0);
    assert_eq!(at(-1_000_000_001), 1);
}

#[test]
fn a_stream_whose_capture_time_was_never_read_is_not_a_stream_found_synchronized() {
    // A format that records one clock per file rather than one stamp per sample leaves `None`, and
    // reporting that as an unstamped sensor would be measuring the request rather than the data.
    // Silence is not the answer either: a clean sync result then means one clock agreeing with
    // itself, not two clocks agreeing with each other. So it abstains out loud — info, never error.
    let f = sensor_clock().run(&rig_with_stamps(None));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.SENSOR_CLOCK_UNREAD");
    assert_eq!(f[0].severity, Severity::Info);
    assert_eq!(f[0].location, veridex_core::check::Location::Dataset);
    assert!(f[0].message.contains("lidar"), "{}", f[0].message);
}

#[test]
fn a_rig_whose_sensors_all_stamped_their_data_says_nothing() {
    assert!(sensor_clock()
        .run(&rig_with_stamps(Some(veridex_core::cdm::HeaderStamps {
            message_count: 600,
            unset: 0,
            min_offset_ns: 5_000_000,
            max_offset_ns: 6_000_000,
            regressions: 0,
        })))
        .is_empty());
}

// ---- AUTONOMY.POINT_CLOUD_EMPTY / POINT_CLOUD_DROPPED ----

fn cloud_with_counts(counts: Option<veridex_core::cdm::PointCounts>) -> Dataset {
    let mut ep = episode(
        0,
        vec![rig_stream("lidar", Modality::PointCloud, 1_000_000_000)],
    );
    ep.streams[0].observed_point_counts = counts;
    dataset(vec![ep])
}

#[test]
fn a_lidar_that_recorded_no_points_is_not_a_working_lidar() {
    // A driver that lost its sensor keeps publishing. The messages have the schema, the rate, the
    // coordinate frame and the monotonic timestamps of a working LiDAR — so the structural family
    // sees frames, the temporal family sees a clean 10 Hz, the frame checks place the sensor in the
    // tree, and every one of them passes on a stream carrying no data at all. The point count is in
    // the message header, ahead of the bulk blob, so reading it decodes no points.
    let f =
        autonomy::PointCloudDensity.run(&cloud_with_counts(Some(veridex_core::cdm::PointCounts {
            message_count: 600,
            min: 0,
            max: 0,
            empty: 600,
        })));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.POINT_CLOUD_EMPTY");
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(
        f[0].location,
        veridex_core::check::Location::Stream {
            episode: 0,
            stream: "lidar".into()
        }
    );
    assert!(f[0].message.contains("600"), "{}", f[0].message);
}

#[test]
fn a_sensor_that_cut_out_mid_recording_is_a_warning_not_a_dead_sensor() {
    // Some sweeps empty, not all: the sensor dropped out partway. Distinguished from the dead-sensor
    // case because a reader acts on them differently — this recording holds real data on either side
    // and may be usable once the affected span is cut. Invisible to every timing check, because the
    // empty messages keep the stream's rate and continuity intact.
    let f =
        autonomy::PointCloudDensity.run(&cloud_with_counts(Some(veridex_core::cdm::PointCounts {
            message_count: 600,
            min: 0,
            max: 24_000,
            empty: 37,
        })));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.POINT_CLOUD_DROPPED");
    assert_eq!(f[0].severity, Severity::Warning);
    assert!(f[0].message.contains("37 empty"), "{}", f[0].message);
    assert!(f[0].message.contains("24000 points"), "{}", f[0].message);
}

#[test]
fn a_stream_whose_density_was_never_measured_is_not_a_stream_found_empty() {
    // A source that carries no per-message point count leaves `None`, and reporting that as an
    // empty LiDAR would be measuring the request rather than the data. But silence is not the
    // answer either: a stream nobody asked the question about is indistinguishable in the report
    // from one that was asked and came back clean, and that difference is the whole value of the
    // result. So it abstains *out loud* — info, never an error.
    let f = autonomy::PointCloudDensity.run(&cloud_with_counts(None));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.POINT_CLOUD_UNMEASURED");
    assert_eq!(f[0].severity, Severity::Info);
    assert!(f[0].message.contains("lidar"), "{}", f[0].message);

    // A stream whose every sweep held points is simply fine, and says nothing at all.
    assert!(autonomy::PointCloudDensity
        .run(&cloud_with_counts(Some(veridex_core::cdm::PointCounts {
            message_count: 600,
            min: 19_800,
            max: 24_000,
            empty: 0,
        })))
        .is_empty());
}

#[test]
fn a_metadata_only_run_is_not_a_rig_whose_lidar_was_never_measured() {
    // The abstention's cause matters. Under `--metadata-only` no message body is opened, so *every*
    // point-cloud stream of *every* format carries no counts — and a finding saying so would blame
    // the data for a silence the request caused, beside a remedy telling the reader to go look in a
    // format that reads more. The run's own shape is already stated by `COVERAGE.METADATA_ONLY`, so
    // the check stands down entirely rather than reporting half a question.
    let d = cloud_with_counts(None);
    use veridex_core::check::CheckContext;
    let metadata_only = CheckContext {
        frames_read: false,
        ..Default::default()
    };
    assert!(autonomy::PointCloudDensity
        .run_in(&d, &metadata_only)
        .is_empty());
    let full = CheckContext {
        frames_read: true,
        ..Default::default()
    };
    assert_eq!(autonomy::PointCloudDensity.run_in(&d, &full).len(), 1);
}

// ---- AUTONOMY.CALIBRATION_AMBIGUOUS ----

#[test]
fn a_frame_with_two_parents_is_not_a_calibrated_rig() {
    // Two nodes each broadcast a transform for `lidar` — one from `base_link`, one from the
    // `velodyne_base` mount. The graph stays connected and every edge is individually valid, so
    // the completeness check counted one component and passed, and the per-sensor frame check found
    // the LiDAR reachable from the camera and passed. Both walk the graph undirected, which answers
    // whether the sensors *can* be related and nothing about whether the answer is unique. tf2
    // resolves the chain through whichever edge it latched, so the LiDAR sits at one of two poses
    // and the log does not say which.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("base_link", "lidar"),
            xf("velodyne_base", "lidar"),
            xf("base_link", "velodyne_base"),
            xf("base_link", "cam"),
        ],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_AMBIGUOUS");
    assert_eq!(
        f[0].severity,
        Severity::Error,
        "which of two chains places the sensor is not a judgment call"
    );
    assert!(f[0].message.contains("`lidar`"), "{}", f[0].message);
    assert!(
        f[0].message.contains("base_link") && f[0].message.contains("velodyne_base"),
        "the finding must name both claimants, or there is nothing to reconcile: {}",
        f[0].message
    );
}

#[test]
fn a_long_drive_log_does_not_rewalk_its_transform_tree_per_episode() {
    // The same shape as the frame-resolution fix, one check over. The transform tree is
    // dataset-level and so is everything derived from it, but the tree was read inside the episode
    // loop: `break_is_localizable` rebuilt the set of every frame the tree names, and
    // `tf_component_count` rebuilt its whole adjacency and walked it, once per episode. Both counts
    // — episodes and transforms — come from the input file, so a 2,000-episode drive log with a
    // 20,000-frame tree paid their product for an answer identical on every episode.
    //
    // The episodes carry a spatial sensor that declares no frame, which is what forces the
    // disconnected-tree branch to be reached rather than deferred.
    let mut transforms: Vec<veridex_core::cdm::Transform> = (0..10_000u32)
        .map(|i| xf(&format!("root_a{i}"), &format!("leaf_a{i}")))
        .collect();
    // A second component, so the branch actually reports rather than returning early.
    transforms.extend((0..10_000u32).map(|i| xf(&format!("root_b{i}"), &format!("leaf_b{i}"))));
    let mut d = rig_with_calibration(Some(veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam")],
    }));
    let first = d.episodes[0].clone();
    for index in 1..2_000u64 {
        let mut ep = first.clone();
        ep.index = index;
        d.episodes.push(ep);
    }

    let started = std::time::Instant::now();
    let f = autonomy::CalibrationCompleteness.run(&d);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "the tree is read once for the dataset, not once per episode"
    );
    assert!(
        f.iter()
            .any(|x| x.code == "AUTONOMY.CALIBRATION_INCOMPLETE"
                && x.message.contains("disconnected")),
        "and the disconnected tree is still reported: {:?}",
        f.iter().take(3).collect::<Vec<_>>()
    );
}

#[test]
fn one_broken_calibration_is_one_finding_however_many_episodes_the_rig_recorded() {
    // The calibration is a dataset-level document, and both rules that judge it read `dataset` —
    // yet they were emitted inside the per-episode loop, so a 200-episode drive log reported the
    // same defect 200 times, buried the one actionable line, and inflated every rollup that counts
    // findings by episode. The sibling `autonomy.sensor-frame-resolution` claims each stream once
    // for exactly this reason.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("base_link", "lidar"),
            xf("velodyne_base", "lidar"),
            xf("base_link", "velodyne_base"),
            xf("base_link", "cam"),
        ],
        // An all-zero `CameraInfo` too, so both dataset-level codes are exercised at once.
        intrinsics: vec![veridex_core::cdm::CameraIntrinsics {
            fx: 0.0,
            fy: 0.0,
            ..intr("cam")
        }],
    };
    let mut d = rig_with_calibration(Some(cal));
    let first = d.episodes[0].clone();
    for index in 1..200u64 {
        let mut ep = first.clone();
        ep.index = index;
        d.episodes.push(ep);
    }
    let f = autonomy::CalibrationCompleteness.run(&d);
    let count = |code: &str| f.iter().filter(|x| x.code == code).count();
    assert_eq!(count("AUTONOMY.CALIBRATION_AMBIGUOUS"), 1, "{f:?}");
    assert_eq!(count("AUTONOMY.CALIBRATION_IMPLAUSIBLE"), 1, "{f:?}");
    assert!(
        f.iter()
            .filter(|x| x.code != "AUTONOMY.CALIBRATION_INCOMPLETE")
            .all(|x| x.location == veridex_core::check::Location::Dataset),
        "a dataset-level fact is located at the dataset: {f:?}"
    );
}

#[test]
fn a_transform_tree_that_closes_into_a_loop_has_no_root() {
    // `base_link` → `lidar` → `radar` → `base_link`. Every frame has exactly one parent, so the
    // multiple-parent rule says nothing, and the graph is one connected component. It is still not
    // a tree: there is no frame the rig is expressed in, and composing the transforms around the
    // loop does not return the identity it must.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("base_link", "lidar"),
            xf("lidar", "radar"),
            xf("radar", "base_link"),
            xf("base_link", "cam"),
        ],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_AMBIGUOUS");
    assert!(f[0].message.contains("cycle"), "{}", f[0].message);
}

#[test]
fn a_file_naming_a_hundred_thousand_parents_for_one_frame_still_answers() {
    // Nothing caps how many transforms a log may carry, and the adapters key them by
    // `(parent, child)` — so every distinct parent a file names for one frame survives ingest. The
    // obvious pairwise form of the multiple-parent rule is quadratic in that number, which a
    // malformed rig chooses: 100k parents is 5e9 comparisons, a hang rather than a finding, reached
    // by exactly the malformed rigs the check exists for. The sweep answers the same question in
    // `O(k log k)` — and still answers it, rather than capping the input and going quiet.
    let mut transforms: Vec<veridex_core::cdm::Transform> = (0..100_000)
        .map(|i| xf(&format!("mount{i}"), "lidar"))
        .collect();
    transforms.push(xf("base_link", "cam"));
    let cal = veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam")],
    };
    let started = std::time::Instant::now();
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the sweep must not be quadratic in the edge count a file chooses"
    );
    let ambiguous = f
        .iter()
        .find(|x| x.code == "AUTONOMY.CALIBRATION_AMBIGUOUS")
        .expect("it still reports the defect");
    // The count stays exact; only the enumeration is trimmed, so the message cannot grow with a
    // number the file chooses.
    assert!(
        ambiguous.message.len() < 500,
        "{} bytes",
        ambiguous.message.len()
    );
    assert!(
        ambiguous.message.contains("100000 different parents")
            && ambiguous.message.contains("more"),
        "{}",
        ambiguous.message
    );
}

#[test]
fn a_hundred_thousand_frame_loop_still_answers() {
    // A file chooses the loop's length as freely as it chooses the parent count: a chain of mount
    // frames closing on itself is a legal input. Asking whether every edge of the loop is valid at
    // once by comparing each pair would spend 5e9 comparisons here; the intervals share a common
    // point iff `max(start) <= min(end)`, which is one pass.
    let mut transforms: Vec<veridex_core::cdm::Transform> = (0..100_000u32)
        .map(|i| xf(&format!("m{i}"), &format!("m{}", (i + 1) % 100_000)))
        .collect();
    transforms.push(xf("base_link", "cam"));
    let cal = veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam")],
    };
    let started = std::time::Instant::now();
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the loop's own length must not be a cost the file controls"
    );
    let cycle = f
        .iter()
        .find(|x| x.code == "AUTONOMY.CALIBRATION_AMBIGUOUS")
        .expect("the loop is still reported");
    // Naming all 100k frames would put a megabyte into one message, and every renderer downstream —
    // terminal, JSON, SARIF, the signed certificate — would carry it.
    assert!(
        cycle.message.len() < 500,
        "the rendering is bounded: {} bytes",
        cycle.message.len()
    );
    assert!(
        cycle.message.contains("100000 frames in the loop)"),
        "and says what it elided: {}",
        cycle.message
    );
}

#[test]
fn two_windows_that_touch_at_one_instant_do_overlap() {
    // A range ending at `t` and one starting at `t` are both valid at `t`, so the frame does have
    // two parents — at exactly one instant, but the transform there has two answers all the same.
    // Off-by-one at the boundary is how a sweep silently becomes a rule that misses its own case.
    let mut early = xf("base_link", "lidar");
    early.valid_to = Some(1_000);
    let mut late = xf("velodyne_base", "lidar");
    late.valid_from = Some(1_000);
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![early, late, xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert!(
        f.iter().any(|x| x.code == "AUTONOMY.CALIBRATION_AMBIGUOUS"),
        "{f:?}"
    );
}

#[test]
fn a_loop_that_never_closes_at_one_instant_is_not_a_loop() {
    // `base_link` → `lidar` → `radar` → `base_link`, but the closing edge is only valid *after* the
    // others stop. At no instant does the rig have a rootless tree, so there is nothing to report —
    // the same reasoning that keeps a re-parenting from being called an ambiguity. Without the
    // simultaneity condition this is a false accusation about a rig that recorded its
    // reconfiguration honestly.
    let mut closing = xf("radar", "base_link");
    closing.valid_from = Some(2_001);
    let mut a = xf("base_link", "lidar");
    a.valid_to = Some(2_000);
    let mut b = xf("lidar", "radar");
    b.valid_to = Some(2_000);
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![a, b, closing, xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    assert!(autonomy::CalibrationCompleteness
        .run(&rig_with_calibration(Some(cal)))
        .is_empty());
}

#[test]
fn a_loop_that_closes_for_one_instant_is_a_loop() {
    // The boundary of the rule above: the closing edge opens at exactly the instant the others
    // stop. All three are valid at that instant, so the tree really is rootless there — briefly,
    // and a transform composed around it is still not the identity. `max(start) <= min(end)` has to
    // hold at equality, or the rule silently misses every loop that meets rather than overlaps.
    let mut closing = xf("radar", "base_link");
    closing.valid_from = Some(2_000);
    let mut a = xf("base_link", "lidar");
    a.valid_to = Some(2_000);
    let mut b = xf("lidar", "radar");
    b.valid_to = Some(2_000);
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![a, b, closing, xf("base_link", "cam")],
        intrinsics: vec![intr("cam")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_calibration(Some(cal)));
    assert!(
        f.iter().any(|x| x.code == "AUTONOMY.CALIBRATION_AMBIGUOUS"),
        "{f:?}"
    );
}

#[test]
fn a_recalibration_is_not_an_ambiguous_tree() {
    // The rig is re-parented partway through the log: `lidar` hangs off `base_link` for the first
    // half and off `velodyne_base` for the second. At no instant does it have two parents, so
    // nothing is ambiguous — flagging it would accuse a rig that recorded its recalibration
    // honestly, which is exactly what the validity ranges exist to express.
    let mut early = xf("base_link", "lidar");
    early.valid_to = Some(1_000);
    let mut late = xf("velodyne_base", "lidar");
    late.valid_from = Some(1_001);
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            early,
            late,
            xf("base_link", "velodyne_base"),
            xf("base_link", "cam"),
        ],
        intrinsics: vec![intr("cam")],
    };
    assert!(
        autonomy::CalibrationCompleteness
            .run(&rig_with_calibration(Some(cal)))
            .is_empty(),
        "a re-parenting across disjoint validity windows is a recalibration, not a loop"
    );
}

/// A rig whose cameras declare frames: `cams` names each camera stream's `frame_id`, so the same
/// camera can be published twice the way a bag republishes `image_raw` beside `compressed`.
fn rig_with_cameras(cams: &[(&str, &str)], cal: veridex_core::cdm::Calibration) -> Dataset {
    let mut streams = vec![
        rig_stream("lidar", Modality::PointCloud, 1_000_000_000),
        rig_stream("gnss", Modality::Gnss, 1_000_000_000),
        rig_stream("imu", Modality::Imu, 1_000_000_000),
    ];
    for (name, frame) in cams {
        let mut s = rig_stream(name, Modality::Video, 1_000_000_000);
        s.frame_id = Some((*frame).to_string());
        streams.push(s);
    }
    let mut d = dataset(vec![episode(0, streams)]);
    d.calibration = Some(cal);
    d
}

#[test]
fn one_camera_info_does_not_calibrate_six_cameras() {
    // A surround rig with one driver configured and five not. The intrinsics list is non-empty, so
    // the presence rule was satisfied and the `world-model-ready` calibration criterion reported
    // green over five cameras nothing can project into. Present for one camera is not present for
    // the rig.
    let cams: Vec<(String, String)> = (0..6)
        .map(|i| (format!("cam{i}"), format!("cam{i}_link")))
        .collect();
    let cams: Vec<(&str, &str)> = cams.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut transforms = vec![xf("base_link", "lidar")];
    for (_, frame) in &cams {
        transforms.push(xf("base_link", frame));
    }
    let cal = veridex_core::cdm::Calibration {
        transforms,
        intrinsics: vec![intr("cam0")],
    };
    let f = autonomy::CalibrationCompleteness.run(&rig_with_cameras(&cams, cal));
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, "AUTONOMY.CALIBRATION_INCOMPLETE");
    assert!(
        f[0].message.contains("6 camera(s)") && f[0].message.contains("1 set(s)"),
        "the finding must say how far short the rig is: {}",
        f[0].message
    );
}

#[test]
fn a_camera_published_twice_is_still_one_camera() {
    // A bag routinely carries `image_raw` beside a `compressed` republication of the same camera.
    // Counting topics would report this rig as short of intrinsics for publishing its camera twice
    // — a wrong accusation about a fully calibrated rig. A camera is a device, and the coordinate
    // frame is what identifies it.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam_link")],
        intrinsics: vec![intr("cam")],
    };
    assert!(autonomy::CalibrationCompleteness
        .run(&rig_with_cameras(
            &[
                ("cam/image_raw", "cam_link"),
                ("cam/compressed", "cam_link")
            ],
            cal
        ))
        .is_empty());
}

#[test]
fn a_camera_with_no_frame_is_not_counted_as_uncalibrated() {
    // With one camera declaring no frame the rig's camera count is a guess, and guessing high is
    // exactly the wrong accusation. The undeclared frame is already reported on its own, by
    // `AUTONOMY.SENSOR_FRAME_UNDECLARED`.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam_link")],
        intrinsics: vec![intr("cam_a")],
    };
    let mut d = rig_with_cameras(&[("cam_a", "cam_link"), ("cam_b", "cam_link_b")], cal);
    for s in d.episodes[0].streams.iter_mut() {
        if s.name == "cam_b" {
            s.frame_id = None;
        }
    }
    assert!(!autonomy::CalibrationCompleteness
        .run(&d)
        .iter()
        .any(|f| f.message.contains("camera(s) but only")));
}

#[test]
fn a_well_formed_tree_carries_no_ambiguity_finding() {
    // The control: one root, one parent per frame, no loop. A republished identical edge is what
    // `/tf` emits every tick and what the adapters collapse by `(parent, child)`; it must not read
    // as a second parent.
    let cal = veridex_core::cdm::Calibration {
        transforms: vec![
            xf("base_link", "lidar"),
            xf("base_link", "lidar"),
            xf("base_link", "cam"),
            xf("lidar", "radar"),
        ],
        intrinsics: vec![intr("cam")],
    };
    assert!(autonomy::CalibrationCompleteness
        .run(&rig_with_calibration(Some(cal)))
        .is_empty());
}

#[test]
fn an_attested_element_is_not_also_reported_missing() {
    // The same report said both. The trust score counts an attested element as covered — that is
    // what an attestation is for, and `PROVENANCE.ATTESTED` names the key that signed it — while
    // `provenance.completeness` ran against the raw dataset and reported it missing, with a remedy
    // ("attest this element") the reader had already followed.
    use veridex_core::check::{Check, CheckContext};
    let d = dataset(vec![episode(0, vec![])]);
    let context = CheckContext {
        frames_read: true,
        attested_keys: vec!["clock".to_string(), "license".to_string()],
    };
    let codes: Vec<String> = provenance::ProvenanceCompleteness
        .run_in(&d, &context)
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        !codes.iter().any(|c| c == "PROVENANCE.MISSING_CLOCK"),
        "an attested clock is not missing: {codes:?}"
    );
    assert!(
        !codes.iter().any(|c| c == "PROVENANCE.MISSING_LICENSE"),
        "{codes:?}"
    );
    // Everything nobody attested is still reported, so the silence is scoped to what was claimed.
    assert!(
        codes.iter().any(|c| c == "PROVENANCE.MISSING_SENSOR"),
        "{codes:?}"
    );
    assert!(
        codes.iter().any(|c| c == "PROVENANCE.MISSING_UPSTREAM"),
        "{codes:?}"
    );

    // With nothing attested, every element is reported as before.
    let codes: Vec<String> = provenance::ProvenanceCompleteness
        .run_in(&d, &CheckContext::default())
        .into_iter()
        .map(|f| f.code)
        .collect();
    assert!(
        codes.iter().any(|c| c == "PROVENANCE.MISSING_CLOCK"),
        "{codes:?}"
    );
}

#[test]
fn a_gnss_coordinate_outside_the_possible_range_is_flagged() {
    // A satellite fix is the one rig measurement whose validity has an absolute physical answer: a
    // latitude outside ±90° is not a place. When one appears the receiver, the unit conversion or
    // the field order is wrong — and every use of the trajectory is wrong with it, silently,
    // because the numbers still look like coordinates.
    let span = |min: f64, max: f64| stats(min, max, (min + max) / 2.0, 0.0);
    let gnss = |lat: (f64, f64), lon: (f64, f64)| {
        let mut s = rig_stream("/gps/fix", Modality::Gnss, 1_000_000_000);
        s.dim_names = Some(vec![
            "latitude".into(),
            "longitude".into(),
            "altitude".into(),
        ]);
        s.observed_dim_stats = Some(vec![
            veridex_core::cdm::DimStats {
                dim: 0,
                stats: span(lat.0, lat.1),
            },
            veridex_core::cdm::DimStats {
                dim: 1,
                stats: span(lon.0, lon.1),
            },
        ]);
        s
    };
    let run = |s| {
        autonomy::GnssPlausibility
            .run(&dataset(vec![episode(0, vec![s])]))
            .into_iter()
            .map(|f| (f.code, f.message))
            .collect::<Vec<_>>()
    };

    // Radians mistaken for degrees, or a scaled integer read raw: latitude past the pole.
    let out = run(gnss((37.4, 214.0), (-122.1, -122.0)));
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].0, "AUTONOMY.GNSS_IMPLAUSIBLE");
    assert!(out[0].1.contains("latitude of 214"), "{}", out[0].1);

    // And the other bound, on the other field.
    let out = run(gnss((37.4, 37.5), (-200.0, -122.0)));
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(out[0].1.contains("longitude of -200"), "{}", out[0].1);

    // An ordinary fix says nothing.
    assert!(run(gnss((37.4, 37.5), (-122.2, -122.1))).is_empty());
}

#[test]
fn the_poles_and_the_antimeridian_are_places() {
    // The bound is inclusive, and nothing pinned it: a mutation sweep flipping `<` to `<=` at both
    // GNSS comparisons left the whole suite green, so the rule could have been silently narrowed to
    // reject exactly ±90° and ±180° — a drive over the pole or across the antimeridian, which are
    // real places a receiver reports. Pinned on both sides of both bounds: the extreme itself is
    // fine, and one ulp past it is not.
    let span = |min: f64, max: f64| stats(min, max, (min + max) / 2.0, 0.0);
    let gnss = |lat: (f64, f64), lon: (f64, f64)| {
        let mut s = rig_stream("/gps/fix", Modality::Gnss, 1_000_000_000);
        s.dim_names = Some(vec!["latitude".into(), "longitude".into()]);
        s.observed_dim_stats = Some(vec![
            veridex_core::cdm::DimStats {
                dim: 0,
                stats: span(lat.0, lat.1),
            },
            veridex_core::cdm::DimStats {
                dim: 1,
                stats: span(lon.0, lon.1),
            },
        ]);
        autonomy::GnssPlausibility
            .run(&dataset(vec![episode(0, vec![s])]))
            .iter()
            .filter(|f| f.code == "AUTONOMY.GNSS_IMPLAUSIBLE")
            .count()
    };

    // Exactly at the bounds: the South Pole, the North Pole, and both ends of the antimeridian.
    assert_eq!(gnss((-90.0, 90.0), (-180.0, 180.0)), 0);
    // One representable step past any of them is not a place.
    assert_eq!(
        gnss((f64::from_bits((-90.0f64).to_bits() + 1), 0.0), (0.0, 0.0)),
        1
    );
    assert_eq!(
        gnss((0.0, f64::from_bits(90.0f64.to_bits() + 1)), (0.0, 0.0)),
        1
    );
    assert_eq!(
        gnss((0.0, 0.0), (f64::from_bits((-180.0f64).to_bits() + 1), 0.0)),
        1
    );
    assert_eq!(
        gnss((0.0, 0.0), (0.0, f64::from_bits(180.0f64.to_bits() + 1))),
        1
    );
}

#[test]
fn a_receiver_that_never_acquired_a_fix_is_flagged_but_a_real_place_is_not() {
    // Null Island is a real point in the Gulf of Guinea, so this is judged by exact equality across
    // every frame: a receiver that never got a fix reports precisely zero, and a vehicle that
    // genuinely drove there would not hold six decimal places of zero for a whole recording.
    let span = |min: f64, max: f64| stats(min, max, (min + max) / 2.0, 0.0);
    let with = |lat: (f64, f64), lon: (f64, f64)| {
        let mut s = rig_stream("/gps/fix", Modality::Gnss, 1_000_000_000);
        s.dim_names = Some(vec!["latitude".into(), "longitude".into()]);
        s.observed_dim_stats = Some(vec![
            veridex_core::cdm::DimStats {
                dim: 0,
                stats: span(lat.0, lat.1),
            },
            veridex_core::cdm::DimStats {
                dim: 1,
                stats: span(lon.0, lon.1),
            },
        ]);
        autonomy::GnssPlausibility
            .run(&dataset(vec![episode(0, vec![s])]))
            .into_iter()
            .map(|f| f.code)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        with((0.0, 0.0), (0.0, 0.0)),
        vec!["AUTONOMY.GNSS_UNSET".to_string()]
    );
    // A drive that actually passes through 0,0 moves, so it is not every frame.
    assert!(with((-0.001, 0.001), (-0.001, 0.001)).is_empty());
    // And one coordinate pinned at zero while the other moves is not the unset case.
    assert!(with((0.0, 0.0), (-122.2, -122.1)).is_empty());
}

#[test]
fn a_gnss_stream_nobody_measured_is_not_reported_plausible() {
    // A check that cannot see the values must not report them sound. The absence is
    // `STATISTICAL.UNMEASURED_VALUES`'s to report, and it does.
    let s = rig_stream("/gps/fix", Modality::Gnss, 1_000_000_000);
    assert!(autonomy::GnssPlausibility
        .run(&dataset(vec![episode(0, vec![s])]))
        .is_empty());
}
