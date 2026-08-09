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
fn gaps_are_detected_against_declared_rate() {
    // 10 Hz declared (100 ms expected); a 500 ms jump between frame 2 and 3 is a gap.
    let ts = [0i64, 100_000_000, 200_000_000, 700_000_000, 800_000_000];
    let d = dataset(vec![episode(0, vec![stream("s", "c", Some(10.0), &ts)])]);
    let f = temporal::Gaps::default().run(&d);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "TEMPORAL.GAP");
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
    // A stream with no declared rate exercises the median-interval path (also saturating).
    let no_rate = dataset(vec![episode(
        0,
        vec![stream("s", "c", None, &[i64::MIN, 0, i64::MAX])],
    )]);
    let _ = temporal::Gaps::default().run(&no_rate);
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
    assert_eq!(verdict.executed_checks.len(), 14);
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
