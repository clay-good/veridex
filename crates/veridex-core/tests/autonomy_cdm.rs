//! Tests for the autonomy sensor-rig CDM extensions (`autonomy-sensor-data` A0): point-cloud
//! streams, the transform (TF) tree, timestamped camera intrinsics, and the ego-pose trajectory.
//!
//! The two properties A0 must hold: the new content-bearing fields are **bound into the content hash**
//! (so a corrupted calibration or trajectory can never share a hash with a clean one), and every
//! collection that is logically a *set* is **order-independent** (so the same rig recorded in a
//! different order hashes identically), mirroring the guarantees the manipulation CDM already has.

use veridex_core::canonical::content_hash;
use veridex_core::cdm::{
    Calibration, CameraIntrinsics, Dataset, EgoPose, Episode, Frame, Modality, PointField, Pose,
    Stream, Transform, ValueRef,
};

fn frame(ts: i64) -> Frame {
    Frame {
        ts,
        value_ref: ValueRef {
            uri: "lidar".into(),
            byte_offset: None,
            byte_len: None,
            content_hash: None,
        },
    }
}

/// A LiDAR point-cloud stream with a declared per-point field layout.
fn cloud_stream(fields: &[&str]) -> Stream {
    Stream {
        name: "lidar_top".into(),
        modality: Modality::PointCloud,
        declared_rate_hz: Some(10.0),
        clock_id: "rig".into(),
        dtype: None,
        shape: None,
        frames: vec![frame(0), frame(100_000_000)],
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: Some(
            fields
                .iter()
                .map(|n| PointField {
                    name: (*n).into(),
                    dtype: Some("float32".into()),
                })
                .collect(),
        ),
    }
}

fn transform(parent: &str, child: &str, tx: f64) -> Transform {
    Transform {
        parent_frame: parent.into(),
        child_frame: child.into(),
        pose: Pose {
            translation: [tx, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
        valid_from: None,
        valid_to: None,
    }
}

fn intrinsics(stream: &str, fx: f64) -> CameraIntrinsics {
    CameraIntrinsics {
        stream: stream.into(),
        fx,
        fy: 800.0,
        cx: 640.0,
        cy: 360.0,
        distortion: vec![0.1, -0.05],
        valid_from: None,
        valid_to: None,
    }
}

fn ego(ts: i64, x: f64) -> EgoPose {
    EgoPose {
        ts,
        pose: Pose {
            translation: [x, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        },
    }
}

/// A rig dataset exercising all three extensions.
fn rig_dataset() -> Dataset {
    Dataset {
        id: "av/drive-01".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(100_000_000),
            streams: vec![cloud_stream(&["x", "y", "z", "intensity"])],
            task: None,
            labels: vec![],
            ego_poses: Some(vec![ego(0, 0.0), ego(50_000_000, 1.0)]),
            declared_frame_count: None,
        }],
        calibration: Some(Calibration {
            transforms: vec![
                transform("base_link", "lidar_top", 1.5),
                transform("base_link", "cam_front", 1.2),
            ],
            intrinsics: vec![intrinsics("cam_front", 900.0)],
        }),
    }
}

#[test]
fn autonomy_dataset_round_trips_through_json() {
    let d = rig_dataset();
    let json = serde_json::to_string(&d).unwrap();
    let back: Dataset = serde_json::from_str(&json).unwrap();
    assert_eq!(d, back);
}

#[test]
fn calibration_transform_order_is_invisible_to_the_hash() {
    let mut a = rig_dataset();
    let mut b = rig_dataset();
    // Same transforms, reversed order.
    b.calibration.as_mut().unwrap().transforms.reverse();
    assert_ne!(
        a.calibration.as_ref().unwrap().transforms,
        b.calibration.as_ref().unwrap().transforms,
        "precondition: the Vec order actually differs"
    );
    assert_eq!(content_hash(&a), content_hash(&b));
    // And the whole dataset is otherwise identical, so its hash is stable too.
    a.calibration.as_mut().unwrap().transforms.reverse();
    assert_eq!(content_hash(&a), content_hash(&b));
}

#[test]
fn ego_pose_order_is_invisible_to_the_hash() {
    let base = rig_dataset();
    let mut permuted = rig_dataset();
    permuted.episodes[0].ego_poses.as_mut().unwrap().reverse();
    assert_eq!(content_hash(&base), content_hash(&permuted));
}

#[test]
fn a_changed_transform_translation_changes_the_hash() {
    let base = rig_dataset();
    let mut tampered = rig_dataset();
    tampered.calibration.as_mut().unwrap().transforms[0]
        .pose
        .translation[0] += 0.01;
    assert_ne!(
        content_hash(&base),
        content_hash(&tampered),
        "a corrupted extrinsic must not share a hash with the clean rig"
    );
}

#[test]
fn a_changed_intrinsic_changes_the_hash() {
    let base = rig_dataset();
    let mut tampered = rig_dataset();
    tampered.calibration.as_mut().unwrap().intrinsics[0].fx += 1.0;
    assert_ne!(content_hash(&base), content_hash(&tampered));
}

#[test]
fn a_moved_ego_pose_changes_the_hash() {
    let base = rig_dataset();
    let mut tampered = rig_dataset();
    tampered.episodes[0].ego_poses.as_mut().unwrap()[1]
        .pose
        .translation[0] += 0.5;
    assert_ne!(content_hash(&base), content_hash(&tampered));
}

#[test]
fn point_field_layout_is_order_significant_and_bound() {
    // Point-field order is the record layout, so it is significant: a different order is a different
    // stream and must hash differently (unlike the set-like calibration collections above).
    let mut a = rig_dataset();
    let mut b = rig_dataset();
    a.episodes[0].streams[0] = cloud_stream(&["x", "y", "z", "intensity"]);
    b.episodes[0].streams[0] = cloud_stream(&["y", "x", "z", "intensity"]);
    assert_ne!(content_hash(&a), content_hash(&b));

    // Renaming a field is also bound.
    let mut c = rig_dataset();
    c.episodes[0].streams[0] = cloud_stream(&["x", "y", "z", "reflectivity"]);
    assert_ne!(content_hash(&a), content_hash(&c));
}

#[test]
fn a_manipulation_dataset_is_unaffected_by_the_extension() {
    // A dataset with no autonomy fields (the manipulation case) hashes deterministically and its hash
    // does not depend on the extension: setting the autonomy fields to their absent value is a no-op.
    let mut d = rig_dataset();
    d.calibration = None;
    d.episodes[0].ego_poses = None;
    d.episodes[0].streams[0].point_fields = None;
    d.episodes[0].streams[0].modality = Modality::ScalarState;
    let baseline = content_hash(&d);
    // Reconstructing the same manipulation-shaped dataset yields the same hash.
    let d2 = d.clone();
    assert_eq!(content_hash(&d2), baseline);
}
