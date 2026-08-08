//! Behavior tests for the MVP checks catalog.

use veridex_core::cdm::{
    Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};
use veridex_core::check::{Check, Severity};
use veridex_core::checks::{provenance, statistical, structural, temporal};

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
    assert_eq!(verdict.executed_checks.len(), 8);
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
