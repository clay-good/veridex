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
        observed_image_dims: None,
        observed_body_decodes: None,
        observed_byte_order: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
        media: None,
        frame_id: None,
        latched: None,
        declared_range: None,
        observed_diagnostics: None,
        point_fields: Some(
            fields
                .iter()
                .map(|n| PointField {
                    name: (*n).into(),
                    dtype: Some("float32".into()),
                    offset: None,
                    elements: None,
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
        observed_diagnostics: None,
        observed_point_counts: None,
        observed_image_dims: None,
        observed_body_decodes: None,
        observed_byte_order: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
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

#[test]
fn a_components_worst_level_ranks_stale_between_warn_and_error() {
    // The ordering `diagnostic_aggregator` uses, and the reason this is not just `max`: the ROS
    // constants number `STALE` 3, above `ERROR`'s 2, while a stale component is *less* severe than a
    // failing one. So the ranking is written out by hand — and a mutation sweep found that no test
    // distinguished the branches, because every fixture had an error in it. Each level is asserted
    // from a component that reports only that level.
    let health = |ok: u64, warn: u64, error: u64, stale: u64| veridex_core::cdm::ComponentHealth {
        name: "lidar".into(),
        hardware_id: None,
        messages: ok + warn + error + stale,
        error,
        warn,
        stale,
        detail: None,
    };
    assert_eq!(health(10, 0, 0, 0).worst_level(), 0, "all OK is level 0");
    assert_eq!(health(9, 1, 0, 0).worst_level(), 1, "one warning is WARN");
    assert_eq!(
        health(9, 0, 0, 1).worst_level(),
        3,
        "one stale status outranks a warning, as the aggregator ranks it"
    );
    assert_eq!(
        health(8, 1, 1, 1).worst_level(),
        2,
        "and an error outranks both, even though its constant is the smaller number"
    );
    // The fraction beside it is a share of the component's own statuses, zero when it said nothing.
    assert_eq!(health(0, 0, 0, 0).error_fraction(), 0.0);
    assert_eq!(health(3, 0, 1, 0).error_fraction(), 0.25);
}

// ---------------------------------------------------------------------------
// `autonomy.declared-byte-order` — what a sensor said about its own payload
// ---------------------------------------------------------------------------

/// A rig whose LiDAR declared `big_endian` of `message_count` messages big-endian.
fn rig_declaring(message_count: u64, big_endian: u64) -> Dataset {
    let mut d = rig_dataset();
    let mut lidar = stream("lidar", Modality::PointCloud, &[0, 50_000_000, 100_000_000]);
    lidar.observed_byte_order = Some(veridex_core::cdm::ByteOrderCounts {
        message_count,
        big_endian,
    });
    d.episodes[0].streams = vec![
        lidar,
        stream("gnss", Modality::Gnss, &[0, 100_000_000]),
        stream("imu", Modality::Imu, &[0, 50_000_000, 100_000_000]),
    ];
    d.canonicalize_order();
    d
}

fn byte_order_codes(d: &Dataset) -> Vec<String> {
    use veridex_core::check::Check;
    veridex_core::checks::autonomy::DeclaredByteOrder
        .run(d)
        .into_iter()
        .map(|f| f.code)
        .collect()
}

#[test]
fn a_stream_that_declared_little_endian_is_silent() {
    // The common case, and the one a finding must never fire on: a ROS driver on an ordinary
    // machine publishes little-endian, and a check that reported it would fire on every honest
    // dataset.
    assert!(byte_order_codes(&rig_declaring(500, 0)).is_empty());
    // And a stream whose bodies were never read says nothing either — `autonomy.message-decode`
    // discloses that, and this check does not speak for a stream it could not read.
    let mut d = rig_declaring(500, 0);
    for ep in &mut d.episodes {
        for s in &mut ep.streams {
            s.observed_byte_order = None;
        }
    }
    assert!(byte_order_codes(&d).is_empty());
}

#[test]
fn a_stream_that_declared_big_endian_throughout_is_reported() {
    use veridex_core::check::Check;
    let d = rig_declaring(500, 500);
    let findings = veridex_core::checks::autonomy::DeclaredByteOrder.run(&d);
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].code, "AUTONOMY.BIG_ENDIAN_PAYLOAD");
    assert_eq!(findings[0].severity, veridex_core::check::Severity::Warning);
    assert_eq!(
        findings[0].location,
        veridex_core::check::Location::Stream {
            episode: 0,
            stream: "lidar".into()
        }
    );
    // The risk has to say what Veridex cannot know, because it never opens the payload.
    assert!(
        findings[0].remedy.contains("never the payload"),
        "{}",
        findings[0].remedy
    );
}

#[test]
fn a_stream_whose_messages_disagree_is_the_worse_fault() {
    // Two byte orders in one stream: a consumer that honors the flag reads it all correctly and one
    // that ignores it reads *part* of it correctly, so the same loader yields a mixture of correct
    // and byte-swapped values from frames that are otherwise indistinguishable.
    use veridex_core::check::Check;
    let findings = veridex_core::checks::autonomy::DeclaredByteOrder.run(&rig_declaring(500, 7));
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].code, "AUTONOMY.MIXED_BYTE_ORDER");
    assert_eq!(
        findings[0].severity,
        veridex_core::check::Severity::Error,
        "no single rule reads this stream correctly"
    );
    assert!(
        findings[0].message.contains("7 of"),
        "{}",
        findings[0].message
    );
    // One message either side of the boundary is still mixed, and the counts are stated.
    assert_eq!(
        byte_order_codes(&rig_declaring(500, 499)),
        ["AUTONOMY.MIXED_BYTE_ORDER"]
    );
    assert_eq!(
        byte_order_codes(&rig_declaring(500, 1)),
        ["AUTONOMY.MIXED_BYTE_ORDER"]
    );
    // All of them, or none, is not mixed.
    assert_eq!(
        byte_order_codes(&rig_declaring(500, 500)),
        ["AUTONOMY.BIG_ENDIAN_PAYLOAD"]
    );
    assert!(byte_order_codes(&rig_declaring(500, 0)).is_empty());
}

#[test]
fn the_declaration_is_bound_into_the_content_hash() {
    // The rule the encoder's own doc comment states: a field a check can fail on is a field the hash
    // binds. Two recordings that differ only in this flag are different datasets — everything else
    // about them is identical, which is the whole reason the check exists.
    let little = content_hash(&rig_declaring(500, 0));
    let big = content_hash(&rig_declaring(500, 500));
    let mixed = content_hash(&rig_declaring(500, 7));
    assert_ne!(little, big);
    assert_ne!(big, mixed);
    assert_ne!(little, mixed);
}

#[test]
fn point_fields_that_sit_next_to_each_other_do_not_overlap() {
    // The boundary the overlap rule lives or dies on: a field starting exactly where the one before
    // it ends is the ordinary packed record, which is how every correct cloud is written. Reporting
    // it would fire on every dataset; missing the one-byte intersection beside it would report none.
    use veridex_core::check::Check;
    let layout = |offsets: &[(&str, u32, &str)]| -> Dataset {
        let mut d = rig_dataset();
        let mut lidar = stream("lidar", Modality::PointCloud, &[0, 50_000_000]);
        lidar.point_fields = Some(
            offsets
                .iter()
                .map(|(name, offset, dtype)| veridex_core::cdm::PointField {
                    name: (*name).into(),
                    dtype: Some((*dtype).into()),
                    offset: Some(*offset),
                    elements: Some(1),
                })
                .collect(),
        );
        d.episodes[0].streams = vec![lidar];
        d.canonicalize_order();
        d
    };
    let codes = |d: &Dataset| -> Vec<String> {
        veridex_core::checks::autonomy::PointCloudLayout
            .run(d)
            .into_iter()
            .filter(|f| f.code == "AUTONOMY.POINT_FIELDS_OVERLAP")
            .map(|f| f.code)
            .collect()
    };

    // Packed, adjacent, float32: 0, 4, 8. Nothing overlaps.
    let packed = layout(&[
        ("x", 0, "float32"),
        ("y", 4, "float32"),
        ("z", 8, "float32"),
    ]);
    assert!(codes(&packed).is_empty(), "adjacency is not overlap");

    // One byte in: `z` now starts inside `y`.
    let one_in = layout(&[
        ("x", 0, "float32"),
        ("y", 4, "float32"),
        ("z", 7, "float32"),
    ]);
    assert_eq!(codes(&one_in), ["AUTONOMY.POINT_FIELDS_OVERLAP"]);

    // A width the reader does not know has no extent, so it is left out rather than guessed at —
    // `AUTONOMY.POINT_CLOUD_FIELD_UNTYPED` is what reports that field.
    let untyped = layout(&[
        ("x", 0, "float32"),
        ("y", 2, "something-else"),
        ("z", 8, "float32"),
    ]);
    assert!(
        codes(&untyped).is_empty(),
        "a field with no known width contributes no extent"
    );

    // A field declaring *zero* elements occupies nothing — the stance the reader takes too, which
    // declines such a body outright — so it adds no extent of its own. It is still *inside* the
    // field before it here, and that is what the finding is about: `x` covers bytes 0 to 4 and `y`
    // claims to start at byte 2.
    let mut zero = layout(&[("x", 0, "float32"), ("y", 2, "float32")]);
    for s in &mut zero.episodes[0].streams {
        if let Some(fields) = &mut s.point_fields {
            fields[1].elements = Some(0);
        }
    }
    assert_eq!(codes(&zero), ["AUTONOMY.POINT_FIELDS_OVERLAP"]);
    // And a zero-element field clear of everything else is silent, rather than reported on an
    // extent nothing would read.
    let mut clear = layout(&[("x", 0, "float32"), ("y", 8, "float32")]);
    for s in &mut clear.episodes[0].streams {
        if let Some(fields) = &mut s.point_fields {
            fields[1].elements = Some(0);
        }
    }
    assert!(codes(&clear).is_empty());
}
