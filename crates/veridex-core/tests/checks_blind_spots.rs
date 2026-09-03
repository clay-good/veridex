//! Three places a check looked at part of its input and reported on all of it.
//!
//! Two are false negatives — a defect the check could not see, so it came back clean and that pass
//! was signed into a certificate. One is a false positive — honest data graded as broken, which is
//! how a user learns to stop reading the report.

use veridex_core::cdm::{
    ClockKind, Dataset, DimStats, Episode, Frame, Modality, Stream, StreamStats, ValueRef,
};
use veridex_core::check::{Check, Severity};
use veridex_core::checks::{statistical, structural, temporal};

fn vref() -> ValueRef {
    ValueRef {
        uri: "x".into(),
        byte_offset: None,
        byte_len: None,
        content_hash: None,
    }
}

fn stream(name: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: None,
        clock_id: "clk".into(),
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
        observed_fix_availability: None,
        media: None,
        frame_id: None,
        frames: ts
            .iter()
            .map(|t| Frame {
                ts: *t,
                value_ref: vref(),
            })
            .collect(),
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
        id: "blind".into(),
        metadata: vec![],
        provenance: vec![],
        episodes,
        calibration: None,
    }
}

fn sane(min: f64, max: f64) -> StreamStats {
    StreamStats {
        min,
        max,
        mean: (min + max) / 2.0,
        std: (max - min).abs() / 4.0,
    }
}

// ---- false negative: the stored stats of every joint but the first went unread -------------------

/// A LeRobot `meta/stats.json` for a 7-DoF `action` stores min/max/mean/std as *arrays*. The sanity
/// check read element 0 and nothing else, so an inverted range on dimension 1 — the stored summary
/// declaring a joint's minimum above its maximum — passed clean, and `value-measurability` counted
/// `dim_stats` as "stats present" so nothing abstained either.
#[test]
fn an_inverted_range_on_a_dimension_above_zero_is_caught() {
    let mut s = stream("action", &[0, 1_000_000]);
    s.stats = Some(sane(0.0, 1.0));
    s.dim_stats = Some(vec![
        DimStats {
            dim: 0,
            stats: sane(0.0, 1.0),
        },
        DimStats {
            dim: 1,
            // min above max: the summary cannot describe any real data.
            stats: StreamStats {
                min: 5.0,
                max: -5.0,
                mean: 0.0,
                std: 1.0,
            },
        },
    ]);
    let ds = dataset(vec![episode(0, vec![s])]);

    let findings = statistical::RangeSanity.run(&ds);
    assert_eq!(findings.len(), 1, "the corrupt dimension must be reported");
    assert!(
        findings[0].message.contains("dim 1"),
        "the finding must name which joint, not just the feature: {}",
        findings[0].message
    );
    assert_eq!(findings[0].severity, Severity::Error);
}

/// The same hole for a non-finite value — a NaN buried in one axis of a stored summary.
#[test]
fn a_non_finite_stat_on_a_dimension_above_zero_is_caught() {
    let mut s = stream("action", &[0, 1_000_000]);
    s.stats = Some(sane(0.0, 1.0));
    s.dim_stats = Some(vec![
        DimStats {
            dim: 0,
            stats: sane(0.0, 1.0),
        },
        DimStats {
            dim: 2,
            stats: StreamStats {
                min: f64::NAN,
                max: f64::INFINITY,
                mean: f64::NAN,
                std: -3.0,
            },
        },
    ]);
    let ds = dataset(vec![episode(0, vec![s])]);

    let findings = statistical::RangeSanity.run(&ds);
    assert_eq!(findings.len(), 1);
    assert!(
        findings[0].message.contains("dim 2"),
        "{}",
        findings[0].message
    );
}

/// And a feature whose every dimension is sound is still clean — the walk must not manufacture
/// findings out of healthy per-dimension stats.
#[test]
fn sound_per_dimension_stats_stay_clean() {
    let mut s = stream("action", &[0, 1_000_000]);
    s.stats = Some(sane(0.0, 1.0));
    s.dim_stats = Some(vec![
        DimStats {
            dim: 0,
            stats: sane(0.0, 1.0),
        },
        DimStats {
            dim: 1,
            stats: sane(-2.0, 2.0),
        },
    ]);
    let ds = dataset(vec![episode(0, vec![s])]);

    assert!(statistical::RangeSanity.run(&ds).is_empty());
}

// ---- false negative: a shape baseline that could never be filled in ------------------------------

/// HDF5 and Zarr both write `shape: None` for a 1-D dataset and `Some([7])` for `(N,7)`. The
/// baseline was captured whole from the first episode that declared *anything*, and never enriched —
/// so a stream whose first episode declared a dtype but no shape had `shape: None` frozen in, and
/// the comparison requires both sides declared. Shape drift for that stream could never be reported
/// again, however many later episodes conflicted.
#[test]
fn shape_drift_is_caught_even_when_the_first_episode_declared_no_shape() {
    let mut a = stream("obs", &[0]);
    a.dtype = Some("float32".into());
    a.shape = None; // 1-D
    let mut b = stream("obs", &[0]);
    b.dtype = Some("float32".into());
    b.shape = Some(vec![6]);
    let mut c = stream("obs", &[0]);
    c.dtype = Some("float32".into());
    c.shape = Some(vec![7]);

    let ds = dataset(vec![
        episode(0, vec![a]),
        episode(1, vec![b]),
        episode(2, vec![c]),
    ]);

    let findings = structural::ShapeConsistency.run(&ds);
    assert_eq!(
        findings.len(),
        1,
        "episodes 1 and 2 declare incompatible shapes: {findings:?}"
    );
    let m = &findings[0].message;
    assert!(
        m.contains("[6]") && m.contains("[7]"),
        "the finding must name both shapes: {m}"
    );
}

/// The control: with no shapeless episode in front, this already worked, and must keep working.
#[test]
fn shape_drift_between_two_declaring_episodes_is_still_caught() {
    let mut b = stream("obs", &[0]);
    b.dtype = Some("float32".into());
    b.shape = Some(vec![6]);
    let mut c = stream("obs", &[0]);
    c.dtype = Some("float32".into());
    c.shape = Some(vec![7]);

    let ds = dataset(vec![episode(0, vec![b]), episode(1, vec![c])]);
    assert_eq!(structural::ShapeConsistency.run(&ds).len(), 1);
}

/// A dtype that drifts is still one finding, and a consistent stream is still silent.
#[test]
fn a_consistent_schema_stays_clean_and_a_dtype_drift_still_fires() {
    let mut a = stream("obs", &[0]);
    a.dtype = Some("float32".into());
    a.shape = Some(vec![7]);
    let b = a.clone();
    let ds = dataset(vec![episode(0, vec![a.clone()]), episode(1, vec![b])]);
    assert!(structural::ShapeConsistency.run(&ds).is_empty());

    let mut c = a.clone();
    c.dtype = Some("float64".into());
    let ds = dataset(vec![episode(0, vec![a]), episode(1, vec![c])]);
    let findings = structural::ShapeConsistency.run(&ds);
    assert_eq!(findings.len(), 1);
    assert!(findings[0].message.contains("float64"));
}

// ---- false positive: a slow sensor that caught exactly two samples -------------------------------

/// A stream observing a window of length `W` at period `T` spans `floor(W/T)*T`, so its measured
/// span understates `W` by up to one full period with a perfect clock. The skew check widens its
/// tolerance by that quantum — but the quantum was 0 below two intervals, which is exactly where a
/// slow sensor lands in a short episode. A 1 Hz LiDAR beside a 100 Hz IMU, perfectly synchronized,
/// drew a headline `TEMPORAL.CLOCK_SKEW` error for a 990 ms "drift" that is one LiDAR period — and
/// flipped to clean the moment the LiDAR caught a third sample.
#[test]
fn a_two_sample_slow_sensor_is_not_reported_as_skewed() {
    let ms = 1_000_000i64;
    let imu_ts: Vec<i64> = (0..200).map(|i| i * 10 * ms).collect();
    let mut imu = stream("imu", &imu_ts);
    imu.declared_rate_hz = Some(100.0);

    for lidar_frames in [2usize, 3, 4] {
        let lidar_ts: Vec<i64> = (0..lidar_frames as i64).map(|i| i * 1000 * ms).collect();
        let mut lidar = stream("lidar", &lidar_ts);
        lidar.declared_rate_hz = Some(1.0);

        let ds = dataset(vec![episode(0, vec![imu.clone(), lidar])]);
        let findings = temporal::ClockSkew::default().run(&ds);
        assert!(
            findings.is_empty(),
            "{lidar_frames} LiDAR samples: a one-period span difference is the sampling quantum, \
             not drift: {findings:?}"
        );
    }
}

/// The protection the old `return 0` was there for, and the reason the declared period is bounded by
/// the observed interval rather than replacing it.
///
/// This sensor declares 100 Hz — a 10 ms period — but its only two frames sit a full second apart
/// and then it stops, covering 1.0 s of an episode the IMU covered 1.9 s of. Taking the *observed*
/// interval as the cadence would hand it a 1000 ms allowance and swallow the 900 ms shortfall whole.
/// The declared period is the smaller of the two, so the allowance stays ~60 ms and the defect is
/// still reported.
#[test]
fn a_sensor_that_died_after_two_frames_cannot_widen_its_way_out_of_the_finding() {
    let ms = 1_000_000i64;
    let imu_ts: Vec<i64> = (0..191).map(|i| i * 10 * ms).collect();
    let mut imu = stream("imu", &imu_ts);
    imu.declared_rate_hz = Some(100.0);

    let mut dead = stream("dead", &[0, 1000 * ms]);
    dead.declared_rate_hz = Some(100.0);

    let ds = dataset(vec![episode(0, vec![imu, dead])]);
    let findings = temporal::ClockSkew::default().run(&ds);
    assert!(
        !findings.is_empty(),
        "a 900 ms shortfall against a declared 10 ms cadence is a real span defect; a two-frame \
         stream must not buy a 1000 ms allowance out of the one gap it happens to contain"
    );
}
