//! Behavior tests for the MVP checks catalog.

use veridex_core::cdm::{
    Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};
use veridex_core::check::{Check, Severity};
use veridex_core::checks::{provenance, semantic, statistical, structural, temporal};

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
        dtype: None,
        shape: None,
        stats: None,
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
    }
}

fn dataset(episodes: Vec<Episode>) -> Dataset {
    Dataset {
        id: "t".into(),
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
        dtype: None,
        shape: None,
        stats: None,
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

fn shaped(name: &str, dtype: Option<&str>, shape: Option<Vec<u64>>, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: "c".into(),
        dtype: dtype.map(Into::into),
        shape,
        stats: None,
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
    // camera spans 1000 ms, robot spans 1200 ms => 200 ms drift, beyond the 50 ms default.
    let cam = stream("cam", "camera", None, &[0, 500_000_000, 1_000_000_000]);
    let robot = stream("robot", "robot", None, &[0, 600_000_000, 1_200_000_000]);
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
    // A dataset with a clock-skew problem should fail via the standard engine.
    let cam = stream("cam", "camera", None, &[0, 1_000_000_000]);
    let robot = stream("robot", "robot", None, &[0, 1_500_000_000]);
    let d = dataset(vec![episode(0, vec![cam, robot])]);
    let engine = veridex_core::checks::default_engine().expect("standard checks have unique ids");
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    assert_eq!(verdict.status, veridex_core::Status::Fail);
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
    assert_eq!(verdict.executed_checks.len(), 22);
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

// ---- semantic ----

/// An episode carrying a specific task string.
fn episode_with_task(index: u64, task: Option<&str>) -> Episode {
    let mut ep = episode(index, vec![stream("s", "c", None, &[0, 1])]);
    ep.task = task.map(Into::into);
    ep
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

    // Backtick-delimited spans are the odd-indexed pieces when splitting on '`'.
    for token in doc.split('`').skip(1).step_by(2) {
        if is_finding_code(token) {
            assert!(
                registered.contains(token),
                "docs/checks.md lists `{token}`, which no registered check emits"
            );
        }
    }
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
