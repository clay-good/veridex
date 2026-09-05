//! Behavior tests for the v1 trust-scoring rubric and provenance coverage.

use veridex_core::cdm::{
    ClockKind, Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};
use veridex_core::certificate::{score, Grade, ProvenanceCoverage, RUBRIC_VERSION};
use veridex_core::{content_hash, RunConfig};

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
        observed_body_decodes: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
        media: None,
        frame_id: None,
        frames: ts
            .iter()
            .map(|t| Frame {
                ts: *t,
                value_ref: ValueRef {
                    uri: "x".into(),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: None,
                },
            })
            .collect(),
    }
}

fn dataset(streams: Vec<Stream>, provenance: Vec<ProvenanceElement>) -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: if provenance.is_empty() {
            vec![]
        } else {
            vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements: provenance,
            }]
        },
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams,
            task: None,
            labels: vec![],
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
    }
}

fn el(key: &str, class: ProvenanceClass) -> ProvenanceElement {
    ProvenanceElement {
        key: key.into(),
        value: Some("v".into()),
        class,
    }
}

fn all_provenance() -> Vec<ProvenanceElement> {
    [
        "license",
        "sensor",
        "clock",
        "calibration",
        "annotator",
        "upstream",
    ]
    .iter()
    .map(|k| el(k, ProvenanceClass::Known))
    .collect()
}

fn run(d: &Dataset) -> veridex_core::Verdict {
    let engine = veridex_core::checks::default_engine().unwrap();
    engine.run(d, content_hash(d), &RunConfig::default())
}

#[test]
fn clean_data_with_full_provenance_scores_a() {
    let clean = stream("s", "c", Some(10.0), &[0, 100_000_000, 200_000_000]);
    let d = dataset(vec![clean], all_provenance());
    let verdict = run(&d);
    let coverage = ProvenanceCoverage::of(&d);
    let ts = score(&verdict, &coverage);
    assert_eq!(ts.provenance_pct, 100);
    assert_eq!(ts.data_score, 100);
    assert_eq!(ts.score, 100);
    assert_eq!(ts.grade, Grade::A);
    assert_eq!(ts.rubric_version, RUBRIC_VERSION);
}

#[test]
fn clean_data_with_no_provenance_is_capped_at_seventy() {
    // No provenance: coverage 0% => overall capped at data*0.7 = 70. A C, never an A.
    let clean = stream("s", "c", Some(10.0), &[0, 100_000_000, 200_000_000]);
    let d = dataset(vec![clean], vec![]);
    let verdict = run(&d);
    let coverage = ProvenanceCoverage::of(&d);
    let ts = score(&verdict, &coverage);
    assert_eq!(ts.data_score, 100);
    assert_eq!(ts.provenance_pct, 0);
    assert_eq!(ts.score, 70);
    assert_eq!(ts.grade, Grade::C);
}

#[test]
fn a_clock_skew_error_drags_the_data_score_down() {
    // Two 100 Hz streams drifting 500 ms => one CLOCK_SKEW error (-15 data). Full provenance isolates
    // the effect to the data axis. The cadence is load-bearing: a span comparison allows for each
    // stream's own sampling period, so a two-frame stream could not evidence the drift.
    let dense = |span_ns: i64| -> Vec<i64> {
        (0..=(span_ns / 10_000_000))
            .map(|i| i * 10_000_000)
            .collect()
    };
    let cam = stream("cam", "camera", None, &dense(1_000_000_000));
    let robot = stream("robot", "robot", None, &dense(1_500_000_000));
    let d = dataset(vec![cam, robot], all_provenance());
    let verdict = run(&d);
    let ts = score(&verdict, &ProvenanceCoverage::of(&d));
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
    assert_eq!(ts.data_score, 85, "one error costs 15 points");
    // overall = (85*7 + 100*3)/10 = 89 => B
    assert_eq!(ts.score, 89);
    assert_eq!(ts.grade, Grade::B);
}

#[test]
fn coverage_counts_known_and_asserted_separately() {
    let d = dataset(
        vec![stream("s", "c", None, &[0, 1])],
        vec![
            el("license", ProvenanceClass::Known),
            el("sensor", ProvenanceClass::Asserted),
        ],
    );
    let cov = ProvenanceCoverage::of(&d);
    assert_eq!(cov.known, 1);
    assert_eq!(cov.asserted, 1);
    assert_eq!(cov.unknown, 4); // clock/calibration/annotator/upstream absent
    assert_eq!(cov.total(), 6);
    assert_eq!(cov.covered_pct(), 33); // 2/6 => 33%
}

#[test]
fn placeholder_value_does_not_count_toward_coverage() {
    // A license "known" as the literal string "unknown" is present in form but empty in substance;
    // it must not inflate coverage, or fake provenance would mask a missing origin in the score.
    let placeholder = ProvenanceElement {
        key: "license".into(),
        value: Some("unknown".into()),
        class: ProvenanceClass::Known,
    };
    let d = dataset(vec![stream("s", "c", None, &[0, 1])], vec![placeholder]);
    let cov = ProvenanceCoverage::of(&d);
    assert_eq!(cov.known, 0, "a placeholder license is not real coverage");
    assert_eq!(
        cov.unknown, 6,
        "all six expected keys are effectively unknown"
    );
    assert_eq!(cov.covered_pct(), 0);
}

#[test]
fn scoring_is_reproducible() {
    let cam = stream("cam", "camera", None, &[0, 1_500_000_000]);
    let robot = stream("robot", "robot", None, &[0, 1_000_000_000]);
    let d = dataset(vec![cam, robot], all_provenance());
    let a = score(&run(&d), &ProvenanceCoverage::of(&d));
    let b = score(&run(&d), &ProvenanceCoverage::of(&d));
    assert_eq!(a, b);
}

#[test]
fn grade_bands_are_correct() {
    assert_eq!(Grade::from_score(100), Grade::A);
    assert_eq!(Grade::from_score(90), Grade::A);
    assert_eq!(Grade::from_score(89), Grade::B);
    assert_eq!(Grade::from_score(70), Grade::C);
    assert_eq!(Grade::from_score(60), Grade::D);
    assert_eq!(Grade::from_score(59), Grade::F);
    assert_eq!(Grade::A.letter(), 'A');
}
