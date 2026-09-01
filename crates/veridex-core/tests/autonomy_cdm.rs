//! Tests for the autonomy sensor-rig CDM extensions (`autonomy-sensor-data` A0): point-cloud
//! streams, the transform (TF) tree, timestamped camera intrinsics, and the ego-pose trajectory.
//!
//! The two properties A0 must hold: the new content-bearing fields are **bound into the content hash**
//! (so a corrupted calibration or trajectory can never share a hash with a clean one), and every
//! collection that is logically a *set* is **order-independent** (so the same rig recorded in a
//! different order hashes identically), mirroring the guarantees the manipulation CDM already has.

use veridex_core::canonical::content_hash;
use veridex_core::cdm::{
    Calibration, CameraIntrinsics, ClockKind, Dataset, EgoPose, Episode, Frame, Modality,
    PointField, Pose, Stream, Transform, ValueRef,
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
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        dim_names: None,
        frames: vec![frame(0), frame(100_000_000)],
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        observed_point_counts: None,
        media: None,
        frame_id: None,
        latched: None,
        declared_range: None,
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

/// A plain scalar stream with the given name and timestamps.
fn stream(name: &str, modality: Modality, ts: &[i64]) -> Stream {
    let mut s = cloud_stream(&["x"]);
    s.name = name.into();
    s.modality = modality;
    s.point_fields = None;
    s.frames = ts.iter().map(|t| frame(*t)).collect();
    s
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
        distortion_model: None,
        width: None,
        height: None,
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
            ego_frame: None,
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

/// Permuting every collection the encoder canonicalizes must leave **both** the content hash and the
/// verdict unchanged after `canonicalize_order`.
///
/// The hash treating a collection as a set while a check reads it as a sequence — or by "first match"
/// — is the dangerous shape: two datasets share a content hash but disagree on the verdict, so a
/// certificate attests a hash that also matches a dataset that fails. This one property covers
/// ego-pose order (read as a trajectory), metadata order (read by first match), and provenance
/// record/element order (read by first match, and emitted).
#[test]
fn permuting_every_canonicalized_collection_changes_neither_the_hash_nor_the_verdict() {
    use veridex_core::cdm::{
        Dataset, EgoPose, Episode, Frame, Modality, Pose, Provenance, ProvenanceClass,
        ProvenanceElement, ProvenanceScope, Stream, ValueRef,
    };

    let frames: Vec<Frame> = (0..10)
        .map(|i| Frame {
            ts: i * 100_000_000,
            value_ref: ValueRef {
                uri: "u".into(),
                byte_offset: None,
                byte_len: None,
                content_hash: None,
            },
        })
        .collect();
    let stream = |name: &str, modality: Modality| Stream {
        name: name.into(),
        modality,
        declared_rate_hz: None,
        clock_id: "rig".into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
        dim_names: None,
        frames: frames.clone(),
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
        media: None,
        frame_id: None,
    };
    // A trajectory that is continuous in ts order and full of teleports in any other order.
    let poses: Vec<EgoPose> = (0..6)
        .map(|i| EgoPose {
            ts: i * 100_000_000,
            pose: Pose {
                translation: [i as f64 * 1.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        })
        .collect();
    let element = |key: &str, value: &str| ProvenanceElement {
        key: key.into(),
        value: Some(value.into()),
        class: ProvenanceClass::Known,
    };

    let base = Dataset {
        id: "perm".into(),
        metadata: vec![
            ("declared_total_frames".into(), "10".into()),
            ("source_format".into(), "test".into()),
        ],
        provenance: vec![
            Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![element("license", "MIT"), element("sensor", "cam")],
            },
            Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![element("annotator", "a"), element("recorder", "r")],
            },
        ],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(900_000_000),
            streams: vec![
                stream("lidar", Modality::PointCloud),
                stream("imu", Modality::Imu),
                stream("gnss", Modality::Gnss),
            ],
            task: None,
            labels: vec![],
            ego_poses: Some(poses.clone()),
            ego_frame: None,
            declared_frame_count: None,
        }],
        calibration: None,
    };

    // A permutation of every collection at once.
    let mut permuted = base.clone();
    permuted.metadata.reverse();
    permuted.provenance.reverse();
    for r in &mut permuted.provenance {
        r.elements.reverse();
    }
    permuted.episodes[0].streams.reverse();
    if let Some(p) = &mut permuted.episodes[0].ego_poses {
        p.reverse();
    }

    let run = |mut d: Dataset| {
        d.canonicalize_order();
        let hash = veridex_core::content_hash(&d);
        let engine = veridex_core::checks::default_engine().expect("engine");
        let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
        (hash, verdict.result_content_hash.clone(), verdict.status)
    };

    let (hash_a, result_a, status_a) = run(base);
    let (hash_b, result_b, status_b) = run(permuted);
    assert_eq!(
        hash_a, hash_b,
        "content hash must be permutation-independent"
    );
    assert_eq!(
        result_a, result_b,
        "the verdict must be permutation-independent too — a hash-identical twin cannot disagree"
    );
    assert_eq!(status_a, status_b);
}

// ---- regression: the content hash must separate datasets that produce different verdicts ----

#[test]
fn a_manifest_frame_count_changes_the_content_hash() {
    // `declared_frame_count` decides `structural.episode-boundary`, so two datasets differing only
    // there disagree on the verdict — one passes, one fails. If they shared a hash, the passing
    // dataset's certificate would verify against the failing one.
    let mk = |declared: u64| {
        let mut d = veridex_core::cdm::Dataset {
            id: "t".into(),
            metadata: vec![],
            provenance: vec![],
            calibration: None,
            episodes: vec![veridex_core::cdm::Episode {
                index: 0,
                start_ts: None,
                end_ts: None,
                streams: vec![],
                task: None,
                labels: vec![],
                ego_poses: None,
                ego_frame: None,
                declared_frame_count: Some(declared),
            }],
        };
        d.canonicalize_order();
        veridex_core::content_hash(&d)
    };
    assert_ne!(
        mk(4),
        mk(9999),
        "a manifest frame count a check reads must be bound into the hash"
    );
}

/// A dataset of `episodes`, canonicalized, and its content hash.
fn hash_of(mut d: veridex_core::cdm::Dataset) -> veridex_core::ContentHash {
    d.canonicalize_order();
    veridex_core::content_hash(&d)
}

#[test]
fn duplicate_episode_indices_still_hash_order_independently() {
    // Two episodes sharing an index is a fault Veridex reports — so the ordering must not assume it
    // away. Sorting by index alone is stable, meaning the input `Vec` order survived into the hash.
    let ep = |stream_name: &str| veridex_core::cdm::Episode {
        index: 0,
        start_ts: None,
        end_ts: None,
        streams: vec![stream(stream_name, Modality::ScalarState, &[0, 1])],
        task: None,
        labels: vec![],
        ego_poses: None,
        ego_frame: None,
        declared_frame_count: None,
    };
    let ds = |a: &str, b: &str| veridex_core::cdm::Dataset {
        id: "t".into(),
        metadata: vec![],
        provenance: vec![],
        calibration: None,
        episodes: vec![ep(a), ep(b)],
    };
    assert_eq!(
        hash_of(ds("a", "b")),
        hash_of(ds("b", "a")),
        "duplicate-index episodes must hash independently of Vec order"
    );
}

#[test]
fn duplicate_stream_names_still_hash_order_independently() {
    // Stream-name uniqueness within an episode is a CDM invariant nothing enforces — `semantic.
    // stream-key-clarity` exists to report violations — so name alone is not a total order either.
    let ds = |first_ts: &[i64], second_ts: &[i64]| veridex_core::cdm::Dataset {
        id: "t".into(),
        metadata: vec![],
        provenance: vec![],
        calibration: None,
        episodes: vec![veridex_core::cdm::Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams: vec![
                stream("dup", Modality::ScalarState, first_ts),
                stream("dup", Modality::ScalarState, second_ts),
            ],
            task: None,
            labels: vec![],
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
    };
    assert_eq!(
        hash_of(ds(&[0, 1], &[5, 6])),
        hash_of(ds(&[5, 6], &[0, 1])),
        "same-named streams must hash independently of Vec order"
    );
}
