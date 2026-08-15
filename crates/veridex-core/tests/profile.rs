//! Tests for the `world-model-ready` policy profile and its per-criterion readiness report (A4).

use veridex_core::cdm::{
    Calibration, Dataset, Episode, Frame, Modality, Pose, Stream, Transform, ValueRef,
};
use veridex_core::certificate::ReadinessReport;
use veridex_core::{content_hash, profile, RunConfig};

fn frames(ts: &[i64]) -> Vec<Frame> {
    ts.iter()
        .map(|&t| Frame {
            ts: t,
            value_ref: ValueRef {
                uri: "s".into(),
                byte_offset: None,
                byte_len: None,
                content_hash: None,
            },
        })
        .collect()
}

fn sensor(name: &str, modality: Modality, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality,
        declared_rate_hz: None,
        clock_id: "rig".into(),
        dtype: None,
        shape: None,
        frames: frames(ts),
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
    }
}

fn xf(parent: &str, child: &str) -> Transform {
    Transform {
        parent_frame: parent.into(),
        child_frame: child.into(),
        pose: Pose {
            translation: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
        valid_from: None,
        valid_to: None,
    }
}

/// A well-formed rig: three AV-native sensors + a camera, all spanning the same 20 steady 100 ms
/// ticks, with a connected TF tree and camera intrinsics.
fn healthy_rig() -> Dataset {
    let ticks: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let cam = veridex_core::cdm::CameraIntrinsics {
        stream: "cam".into(),
        fx: 600.0,
        fy: 600.0,
        cx: 320.0,
        cy: 240.0,
        distortion: vec![],
        valid_from: None,
        valid_to: None,
    };
    Dataset {
        id: "rig".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(1_900_000_000),
            streams: vec![
                sensor("lidar", Modality::PointCloud, &ticks),
                sensor("gnss", Modality::Gnss, &ticks),
                sensor("imu", Modality::Imu, &ticks),
                sensor("cam", Modality::Video, &ticks),
            ],
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
        }],
        calibration: Some(Calibration {
            transforms: vec![xf("base_link", "lidar"), xf("base_link", "cam")],
            intrinsics: vec![cam],
        }),
    }
}

fn verdict_for(d: &Dataset, p: &profile::Profile) -> veridex_core::Verdict {
    let mut d = d.clone();
    d.canonicalize_order();
    let hash = content_hash(&d);
    let rc = RunConfig {
        tolerances: p.tolerances,
        ..RunConfig::default()
    };
    let engine =
        veridex_core::checks::default_engine_with(&p.tolerances).expect("unique check ids");
    engine.run(&d, hash, &rc)
}

#[test]
fn a_healthy_rig_is_world_model_ready() {
    let p = profile::world_model_ready();
    let d = healthy_rig();
    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(r.applicable, "a rig is applicable");
    assert!(r.ready, "a healthy rig should be ready: {:?}", r.criteria);
    assert_eq!(r.criteria.len(), 4);
    assert!(r.criteria.iter().all(|c| c.passed));
}

#[test]
fn a_desynced_rig_is_not_ready() {
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    // Cut the IMU span in half so the rig-wide sync spread blows past the 20 ms profile tolerance.
    let short: Vec<i64> = (0..10).map(|i| i * 100_000_000).collect();
    d.episodes[0].streams[2] = sensor("imu", Modality::Imu, &short);
    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(r.applicable);
    assert!(!r.ready, "a desynced rig is not ready");
    let sync = r
        .criteria
        .iter()
        .find(|c| c.check_id == "autonomy.rig-sync")
        .unwrap();
    assert!(!sync.passed);
    assert!(sync.findings >= 1);
}

#[test]
fn a_manipulation_dataset_is_not_applicable() {
    let p = profile::world_model_ready();
    // No AV-native rig sensors → not a rig.
    let ticks: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let d = Dataset {
        id: "manip".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(1_900_000_000),
            streams: vec![
                sensor("cam", Modality::Video, &ticks),
                sensor("state", Modality::ScalarState, &ticks),
            ],
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
        }],
        calibration: None,
    };
    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(!r.applicable, "a manipulation dataset is not a rig");
    assert!(!r.ready, "not applicable is never ready (no vacuous pass)");
}
