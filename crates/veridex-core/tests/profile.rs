//! Tests for the `world-model-ready` policy profile and its per-criterion readiness report (A4).

use veridex_core::cdm::{
    Calibration, ClockKind, Dataset, Episode, Frame, Modality, Pose, Stream, Transform, ValueRef,
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

/// A rig sensor that declares which coordinate frame it is in — as a real one must, or its
/// calibration is unverifiable and `autonomy.sensor-frame-resolution` says so.
fn sensor(name: &str, modality: Modality, ts: &[i64]) -> Stream {
    sensor_in_frame(name, modality, ts, Some(format!("{name}_frame")))
}

fn sensor_in_frame(name: &str, modality: Modality, ts: &[i64], frame_id: Option<String>) -> Stream {
    Stream {
        name: name.into(),
        modality,
        declared_rate_hz: None,
        clock_id: "rig".into(),
        clock_kind: ClockKind::Measured,
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
        media: None,
        frame_id,
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
            // A world-model candidate carries an ego trajectory; a steady 1 m/s crawl is continuous.
            ego_poses: Some(
                ticks
                    .iter()
                    .enumerate()
                    .map(|(i, &t)| veridex_core::cdm::EgoPose {
                        ts: t,
                        pose: Pose {
                            translation: [i as f64 * 0.1, 0.0, 0.0],
                            rotation: [0.0, 0.0, 0.0, 1.0],
                        },
                    })
                    .collect(),
            ),
            declared_frame_count: None,
        }],
        calibration: Some(Calibration {
            // Every sensor's declared frame is in the tree and reaches the camera's — which is what
            // `autonomy.sensor-frame-resolution` attests, and what a rig has to actually satisfy
            // rather than satisfy by declaring nothing.
            transforms: vec![
                xf("base_link", "lidar_frame"),
                xf("base_link", "cam_frame"),
                xf("base_link", "gnss_frame"),
                xf("base_link", "imu_frame"),
            ],
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
    assert_eq!(r.criteria.len(), 5);
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

/// Certify a dataset against a profile, exactly as the CLI and Python bindings do.
fn certify_with(d: &Dataset, p: &profile::Profile) -> veridex_core::SignedCertificate {
    let mut d = d.clone();
    d.canonicalize_order();
    let v = verdict_for(&d, p);
    let trust = veridex_core::score(&v, &veridex_core::ProvenanceCoverage::of(&d));
    let keypair = veridex_core::SigningKeypair::from_secret_hex(&"01".repeat(32)).expect("key");
    let mut cert = veridex_core::Certificate::build(
        d.id.clone(),
        &v,
        trust,
        veridex_core::ProvenanceCoverage::of(&d),
        veridex_core::Issuance {
            key_id: keypair.public_hex(),
            timestamp: "1700000000".into(),
        },
    );
    cert.readiness = Some(ReadinessReport::evaluate(p, &v, &d));
    veridex_core::sign(cert, &keypair)
}

#[test]
fn a_readiness_certificate_verifies_offline_and_reports_every_criterion() {
    let p = profile::world_model_ready();
    let d = healthy_rig();
    let signed = certify_with(&d, &p);

    // Offline verification: no network, no original dataset needed for the signature itself.
    let v = veridex_core::verify(&signed, None, Some(&signed.public_key)).expect("verifies");

    let text = veridex_core::render_verified(&signed, &v, true);
    assert!(text.contains("certificate verified"), "{text}");
    assert!(text.contains("world-model-ready profile: READY"), "{text}");
    for (id, threshold) in p.criteria {
        assert!(text.contains(id), "criterion {id} must be reported: {text}");
        assert!(text.contains(threshold), "{text}");
    }

    // The machine-readable form carries the same signed readiness block.
    let doc: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&signed, &v, true)).expect("json");
    assert_eq!(doc["verified"], true);
    assert_eq!(doc["readiness"]["ready"], true);
    assert_eq!(doc["readiness"]["profile"], "world-model-ready");
    assert_eq!(doc["readiness"]["criteria"].as_array().unwrap().len(), 5);
    assert_eq!(doc["cdm_content_hash"], signed.certificate.cdm_content_hash);
}

#[test]
fn a_not_ready_certificate_says_so_and_cannot_be_upgraded_by_editing_it() {
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    let short: Vec<i64> = (0..10).map(|i| i * 100_000_000).collect();
    d.episodes[0].streams[2] = sensor("imu", Modality::Imu, &short);
    let signed = certify_with(&d, &p);

    let v = veridex_core::verify(&signed, None, None).expect("verifies");
    let text = veridex_core::render_verified(&signed, &v, true);
    assert!(
        text.contains("world-model-ready profile: NOT READY"),
        "{text}"
    );

    // Flipping the readiness verdict is exactly the attack the signature must stop: the readiness
    // block is signed like every other field, so a forged "READY" no longer verifies at all.
    let mut forged = signed.clone();
    let r = forged.certificate.readiness.as_mut().expect("readiness");
    r.ready = true;
    for c in &mut r.criteria {
        c.passed = true;
        c.findings = 0;
    }
    assert!(
        veridex_core::verify(&forged, None, None).is_err(),
        "an edited readiness block must fail verification"
    );
}

#[test]
fn a_non_rig_readiness_certificate_verifies_and_reports_n_a_not_a_pass() {
    let p = profile::world_model_ready();
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
    let signed = certify_with(&d, &p);
    let v = veridex_core::verify(&signed, None, None).expect("verifies");
    let text = veridex_core::render_verified(&signed, &v, true);
    assert!(text.contains("N/A (profile does not apply)"), "{text}");
    assert!(
        !text.contains("READY"),
        "N/A must never read as ready: {text}"
    );
}

#[test]
fn a_criterion_whose_check_was_disabled_never_counts_as_passed() {
    // The attack this closes: a dataset that genuinely fails a criterion can otherwise be certified
    // ready by switching that check off, because a disabled check produces no findings.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    let short: Vec<i64> = (0..10).map(|i| i * 100_000_000).collect();
    d.episodes[0].streams[2] = sensor("imu", Modality::Imu, &short);
    d.canonicalize_order();
    let hash = content_hash(&d);

    let rc = RunConfig {
        tolerances: p.tolerances,
        disabled_checks: ["autonomy.rig-sync".to_string()].into_iter().collect(),
        ..RunConfig::default()
    };
    let engine =
        veridex_core::checks::default_engine_with(&p.tolerances).expect("unique check ids");
    let verdict = engine.run(&d, hash, &rc);
    assert!(
        !verdict
            .findings
            .iter()
            .any(|f| f.check_id == "autonomy.rig-sync"),
        "the disabled check must produce no findings"
    );

    let r = ReadinessReport::evaluate(&p, &verdict, &d);
    let sync = r
        .criteria
        .iter()
        .find(|c| c.check_id == "autonomy.rig-sync")
        .expect("criterion present");
    assert!(!sync.ran, "a disabled check did not run");
    assert!(
        !sync.passed,
        "silence from a check that never ran is not a pass"
    );
    assert!(
        !r.ready,
        "a dataset cannot become ready by disabling the check"
    );
}

#[test]
fn a_bus_only_measurement_is_not_a_world_model_candidate() {
    // A CAN or MF4 log is a "rig" by sensor count alone, but two of the four criteria abstain
    // without a perception sensor or an ego trajectory — so they would pass with nothing examined.
    let p = profile::world_model_ready();
    let ticks: Vec<i64> = (0..20).map(|i| i * 100_000_000).collect();
    let d = Dataset {
        id: "bus".into(),
        metadata: vec![],
        provenance: vec![],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(1_900_000_000),
            streams: vec![
                sensor("speed", Modality::CanSignal, &ticks),
                sensor("rpm", Modality::CanSignal, &ticks),
                sensor("gear", Modality::CanSignal, &ticks),
                sensor("brake", Modality::CanSignal, &ticks),
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
    assert!(
        !r.applicable,
        "a bus-only measurement carries none of what the criteria are about"
    );
    assert!(!r.ready, "not applicable is never ready");
}

#[test]
fn a_rig_without_a_decoded_ego_trajectory_is_not_a_readiness_candidate() {
    // The profile applies to a rig that carries what a world model is built from: a perception sensor
    // *and* an ego trajectory. This is the drift that silently turned the flagship demo's readiness
    // report into N/A when its Odometry payload stopped decoding — so it is worth pinning in both
    // directions.
    let mut d = healthy_rig();
    let profile = veridex_core::profile::world_model_ready();
    assert!(
        (profile.applies_to)(&d),
        "a rig with a perception sensor and an ego trajectory is a candidate"
    );
    d.episodes[0].ego_poses = None;
    assert!(
        !(profile.applies_to)(&d),
        "without a decoded ego trajectory the profile must abstain, not vacuously pass"
    );
}

#[test]
fn a_rig_whose_sensors_cannot_reach_the_camera_is_not_ready() {
    // Regression guard. `autonomy.calibration-completeness` defers its disconnected-tree report to
    // `autonomy.sensor-frame-resolution` whenever the sensors name their frames. If the profile does
    // not also watch that check, the defect moves out of every criterion the profile judges and a
    // rig that cannot be spatially fused certifies as ready — while the same verdict says FAIL.
    let p = profile::world_model_ready();

    // The LiDAR is in the tree, under a mount frame nothing joins to the camera's subtree.
    let mut d = healthy_rig();
    d.calibration = Some(Calibration {
        transforms: vec![
            xf("lidar_mount", "lidar_top"),
            xf("base_link", "camera_front"),
        ],
        intrinsics: d.calibration.as_ref().unwrap().intrinsics.clone(),
    });
    for s in &mut d.episodes[0].streams {
        s.frame_id = match s.name.as_str() {
            "lidar" => Some("lidar_top".into()),
            "cam" => Some("camera_front".into()),
            _ => None,
        };
    }

    let v = verdict_for(&d, &p);
    assert_eq!(
        v.status,
        veridex_core::Status::Fail,
        "the stranded LiDAR is an error finding"
    );
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(r.applicable);
    assert!(
        !r.ready,
        "a rig whose LiDAR cannot be projected into the camera is not world-model ready: {:?}",
        r.criteria
    );
    let frame = r
        .criteria
        .iter()
        .find(|c| c.check_id == "autonomy.sensor-frame-resolution")
        .expect("the profile must judge sensor-frame resolution");
    assert!(!frame.passed);
}

#[test]
fn a_sensor_naming_a_frame_the_tree_does_not_know_is_not_ready() {
    // The other half: a fully connected tree, recorded for a frame name the LiDAR does not publish.
    // Nothing about the tree's shape is wrong, so only the per-sensor check can see it.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    d.calibration = Some(Calibration {
        transforms: vec![
            xf("base_link", "lidar_top"),
            xf("base_link", "camera_front"),
        ],
        intrinsics: d.calibration.as_ref().unwrap().intrinsics.clone(),
    });
    for s in &mut d.episodes[0].streams {
        s.frame_id = match s.name.as_str() {
            "lidar" => Some("lidar_top_v2".into()), // the driver publishes a different name
            "cam" => Some("camera_front".into()),
            _ => None,
        };
    }

    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(!r.ready, "criteria: {:?}", r.criteria);
}

#[test]
fn a_certificate_never_says_ready_over_a_failing_verdict() {
    // The invariant behind both cases above, stated directly: `ready` is the field a consumer gates
    // on, and a signed document asserting `ready: true` beside `status: fail` is self-contradictory.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    d.calibration = Some(Calibration {
        transforms: vec![
            xf("lidar_mount", "lidar_top"),
            xf("base_link", "camera_front"),
        ],
        intrinsics: d.calibration.as_ref().unwrap().intrinsics.clone(),
    });
    for s in &mut d.episodes[0].streams {
        s.frame_id = match s.name.as_str() {
            "lidar" => Some("lidar_top".into()),
            "cam" => Some("camera_front".into()),
            _ => None,
        };
    }
    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(
        !(v.status == veridex_core::Status::Fail && r.ready),
        "a failing verdict must never carry ready=true"
    );
}

#[test]
fn a_rig_where_no_sensor_declares_a_frame_is_not_ready() {
    // The unconfigured-driver case: a well-formed, fully connected transform tree beside sensors
    // that never say which frame they are in — what a ROS driver publishing an empty
    // `header.frame_id` produces. `autonomy.sensor-frame-resolution` used to skip those streams
    // silently, so it found nothing, so the criterion it backs read as satisfied — and a signed
    // certificate went out attesting "every sensor's own frame resolves through the tree to a
    // camera" over a rig where not one sensor said where it was. Not one transform in the tree
    // could be applied to any data.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    for s in &mut d.episodes[0].streams {
        s.frame_id = None;
    }

    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(!r.ready, "criteria: {:?}", r.criteria);
    let frame = r
        .criteria
        .iter()
        .find(|c| c.check_id == "autonomy.sensor-frame-resolution")
        .expect("the profile must judge sensor-frame resolution");
    assert!(
        !frame.passed && frame.findings > 0,
        "the criterion must fail on evidence, not pass on silence: {frame:?}"
    );
}

#[test]
fn a_failing_verdict_is_never_ready_even_when_every_criterion_passes() {
    // The criteria name a subset of the catalog, so a rig can satisfy every autonomy criterion and
    // still fail the run on something the profile does not name. `ready: true` printed beside
    // `status: fail` is a certificate contradicting itself on the same page; whichever half a
    // reader believes, one of them misled them.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    // An inverted stored range — a statistical error, judged by no autonomy criterion.
    d.episodes[0].streams[0].stats = Some(veridex_core::cdm::StreamStats {
        min: 10.0,
        max: -10.0,
        mean: 0.0,
        std: 1.0,
    });

    let v = verdict_for(&d, &p);
    assert_eq!(
        v.status,
        veridex_core::Status::Fail,
        "findings: {:?}",
        v.findings
    );
    let r = ReadinessReport::evaluate(&p, &v, &d);
    assert!(
        r.criteria.iter().all(|c| c.passed),
        "every autonomy criterion still passes: {:?}",
        r.criteria
    );
    assert!(
        !r.ready,
        "a failing verdict must never be reported as world-model-ready"
    );
}
