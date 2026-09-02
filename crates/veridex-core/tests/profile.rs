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
        dim_names: None,
        frames: frames(ts),
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        // A point-cloud sensor read in full carries a per-message point count, and a healthy one's
        // counts are non-zero. Left absent, `autonomy.point-cloud-density` abstains out loud — and
        // rightly refuses the `world-model-ready` criterion, because "every point-cloud sensor
        // actually recorded points" cannot be attested over counts nobody read. A fixture standing
        // in for a healthy rig has to carry what a healthy rig carries.
        observed_point_counts: (modality == Modality::PointCloud).then_some(
            veridex_core::cdm::PointCounts {
                message_count: ts.len() as u64,
                min: 19_800,
                max: 24_000,
                empty: 0,
            },
        ),
        // Likewise: a sensor read in full says when it sampled, and a healthy one's stamps sit a
        // constant pipeline latency behind the recorder's clock. Left absent,
        // `autonomy.sensor-clock` abstains out loud and refuses its readiness criterion.
        observed_header_stamps: modality
            .is_sensor()
            .then_some(veridex_core::cdm::HeaderStamps {
                message_count: ts.len() as u64,
                unset: 0,
                min_offset_ns: 5_000_000,
                max_offset_ns: 6_000_000,
                regressions: 0,
            }),
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
        distortion_model: None,
        width: None,
        height: None,
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
            // The body the trajectory is of, and a frame the tree below actually contains — the
            // static link between the trajectory and every sensor's extrinsics.
            ego_frame: Some("base_link".into()),
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
    assert_eq!(r.criteria.len(), 8);
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
            ego_frame: None,
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

    let text = veridex_core::render_verified(&signed, &v, true, true);
    assert!(text.contains("certificate verified"), "{text}");
    assert!(text.contains("world-model-ready profile: READY"), "{text}");
    for (id, threshold) in p.criteria {
        assert!(text.contains(id), "criterion {id} must be reported: {text}");
        assert!(text.contains(threshold), "{text}");
    }

    // The machine-readable form carries the same signed readiness block.
    let doc: serde_json::Value =
        serde_json::from_str(&veridex_core::verified_json(&signed, &v, true, true)).expect("json");
    assert_eq!(doc["verified"], true);
    assert_eq!(doc["readiness"]["ready"], true);
    assert_eq!(doc["readiness"]["profile"], "world-model-ready");
    assert_eq!(doc["readiness"]["criteria"].as_array().unwrap().len(), 8);
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
    let text = veridex_core::render_verified(&signed, &v, true, true);
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
            ego_frame: None,
            declared_frame_count: None,
        }],
        calibration: None,
    };
    let signed = certify_with(&d, &p);
    let v = veridex_core::verify(&signed, None, None).expect("verifies");
    let text = veridex_core::render_verified(&signed, &v, true, true);
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
            ego_frame: None,
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

/// A profile is built as `Tolerances { clock_skew_ns: 20ms, ..default() }`, so every field it does
/// not name holds a default rather than an absence of opinion. Assigning the whole struct therefore
/// reverted thresholds the operator had deliberately tightened, making `--profile` *loosen* the run.
#[test]
fn a_profile_does_not_revert_thresholds_it_never_names() {
    let p = veridex_core::profile::world_model_ready();
    let configured = veridex_core::Tolerances {
        ego_max_speed_mps: 1.0,
        outlier_z: 2.0,
        gap_factor: 1.5,
        // Something the profile *does* name, set looser than the profile demands.
        clock_skew_ns: 900_000_000,
        ..Default::default()
    };

    let applied = p.apply_tolerances(configured);

    // The threshold the profile names wins: readiness is only meaningful at 20 ms.
    assert_eq!(applied.clock_skew_ns, 20_000_000);
    // The ones it does not name are none of its business, and must survive.
    assert_eq!(applied.ego_max_speed_mps, 1.0);
    assert_eq!(applied.outlier_z, 2.0);
    assert_eq!(applied.gap_factor, 1.5);
}

/// The other direction, which the test above never covered: a threshold the operator set *tighter*
/// than the profile's.
///
/// `docs/profiles.md` sells `world-model-ready` as tightening cross-sensor sync — "stricter than the
/// 50 ms default" — but keeping the profile's value unconditionally moved thresholds both ways, so
/// an operator asking for 5 ms had it loosened to the profile's 20 ms. A 10 ms drift that failed
/// their run passed once they added the flag that advertises strictness. A profile may only tighten.
#[test]
fn a_profile_never_loosens_a_threshold_the_operator_set_tighter() {
    let p = veridex_core::profile::world_model_ready();
    let configured = veridex_core::Tolerances {
        // Ten times stricter than the 20 ms the profile requires.
        clock_skew_ns: 5_000_000,
        ..Default::default()
    };

    let applied = p.apply_tolerances(configured);

    assert_eq!(
        applied.clock_skew_ns, 5_000_000,
        "the operator's stricter threshold must stand — the profile's guarantee still holds at it"
    );
}

/// With no config of its own, a profile still applies its thresholds over the defaults.
#[test]
fn a_profile_still_tightens_over_the_defaults() {
    let p = veridex_core::profile::world_model_ready();
    let applied = p.apply_tolerances(veridex_core::Tolerances::default());
    assert_eq!(applied.clock_skew_ns, 20_000_000);
    assert_eq!(
        applied.ego_max_speed_mps,
        veridex_core::Tolerances::default().ego_max_speed_mps
    );
}

/// Loosening a threshold the profile does not name must not buy a READY verdict.
///
/// `world-model-ready` names exactly one tolerance, `clock_skew_ns`. Every other threshold its
/// criteria depend on passes through from `veridex.toml` however loose, and each criterion's
/// `threshold` is static prose unrelated to the tolerance actually used. So one config line took a
/// rig that drops a seventh of its LiDAR frames from `ready: false` to a signed `ready: true` whose
/// criterion still read "no rig sensor dropping more than 5% of its frames" — a signature over a
/// sentence that was false about the run it described.
///
/// The guard is `scope_narrowed`, which is meaningful precisely because it is directional: it means
/// a threshold was *loosened*, a check deselected, or a severity overridden — never that the profile
/// tightened something. So it blocks this without blocking the profile runs readiness exists for,
/// which `a_healthy_rig_is_world_model_ready` holds down.
#[test]
fn a_loosened_threshold_cannot_buy_a_ready_verdict() {
    let p = profile::world_model_ready();
    let mut d = healthy_rig();

    // Drop ~14% of the LiDAR's frames: enough to fail `autonomy.sequence-complete` at its default.
    for ep in &mut d.episodes {
        for s in &mut ep.streams {
            if s.modality == Modality::PointCloud {
                let keep = s.frames.len() * 6 / 7;
                s.frames.truncate(keep.max(1));
            }
        }
    }
    d.canonicalize_order();
    let hash = content_hash(&d);

    // The honest run: the criterion fires, so the rig is not ready.
    let strict = RunConfig {
        tolerances: p.apply_tolerances(veridex_core::Tolerances::default()),
        ..RunConfig::default()
    };
    let engine_strict = veridex_core::checks::default_engine_with(&strict.tolerances).unwrap();
    let v_strict = engine_strict.run(&d, hash, &strict);
    assert!(
        !ReadinessReport::evaluate(&p, &v_strict, &d).ready,
        "a rig dropping a seventh of its LiDAR frames is not world-model ready"
    );

    // Now the attack: loosen the threshold the profile never names, and apply the profile on top.
    let loose_base = veridex_core::Tolerances {
        sequence_drop_fraction: 0.9,
        ..veridex_core::Tolerances::default()
    };
    let loose = RunConfig {
        tolerances: p.apply_tolerances(loose_base),
        ..RunConfig::default()
    };
    let engine_loose = veridex_core::checks::default_engine_with(&loose.tolerances).unwrap();
    let v_loose = engine_loose.run(&d, hash, &loose);

    let report = ReadinessReport::evaluate(&p, &v_loose, &d);
    assert!(
        !report.ready,
        "a run that loosened its way past the criterion is not ready: {:?}",
        report.criteria
    );
    assert!(
        !report.applicable,
        "readiness cannot be judged at all over a narrowed run: {report:?}"
    );
}

#[test]
fn a_lidar_whose_density_was_never_measured_is_not_a_lidar_known_to_have_recorded() {
    // The criterion reads "every point-cloud sensor actually recorded points". Over counts nobody
    // read, that is not a claim anyone can make — and the shape this guards against is the one that
    // certifies anyway: the check finds no counts, produces nothing, the criterion counts zero
    // findings and reports green, and a rig whose LiDAR was never measured is signed as ready to
    // build a world model from. `AUTONOMY.POINT_CLOUD_UNMEASURED` is what stops that, so it has to
    // reach the criterion, not merely the report.
    let p = profile::world_model_ready();
    let mut d = healthy_rig();
    for ep in &mut d.episodes {
        for s in &mut ep.streams {
            if s.modality == Modality::PointCloud {
                s.observed_point_counts = None;
            }
        }
    }
    let v = verdict_for(&d, &p);
    let r = ReadinessReport::evaluate(&p, &v, &d);
    let density = r
        .criteria
        .iter()
        .find(|c| c.check_id == "autonomy.point-cloud-density")
        .expect("the density criterion");
    assert!(
        !density.passed,
        "a criterion cannot be satisfied by a measurement that was never taken: {density:?}"
    );
    assert!(!r.ready, "and the rig is therefore not ready");

    // Every other criterion is untouched, so this is the density question failing and not a rig
    // that fell over for some unrelated reason.
    assert!(
        r.criteria
            .iter()
            .filter(|c| c.check_id != "autonomy.point-cloud-density")
            .all(|c| c.passed),
        "{:?}",
        r.criteria
    );
}

/// Every autonomy check in the catalog must be a criterion of `world-model-ready`.
///
/// The profile's own doc says so: "Every autonomy check that can fail a rig belongs here. A check
/// missing from this list is a check the profile does not judge, so a defect that moves from a
/// listed check to an unlisted one becomes invisible to `ready` while still failing the verdict."
///
/// The existing tests assert `criteria.len() == 7`, which catches *removing* a criterion and not
/// adding a seventh autonomy check without one — the count still reads 6 and the profile silently
/// stops judging the new check. Asserting against the live catalog closes that direction.
#[test]
fn every_autonomy_check_is_a_world_model_ready_criterion() {
    let p = profile::world_model_ready();
    let engine = veridex_core::checks::default_engine().expect("unique check ids");

    let judged: std::collections::BTreeSet<&str> = p.criteria.iter().map(|(id, _)| *id).collect();
    let autonomy: Vec<&str> = engine
        .catalog()
        .iter()
        .filter(|c| c.category == veridex_core::Category::Autonomy)
        .map(|c| c.id)
        .collect();

    assert!(!autonomy.is_empty(), "the catalog has autonomy checks");
    for id in &autonomy {
        assert!(
            judged.contains(id),
            "`{id}` can fail a rig but is not a `world-model-ready` criterion, so a rig failing it \
             would still certify as ready"
        );
    }
    // ...and the reverse, so a criterion cannot name a check that no longer exists.
    let catalog: std::collections::BTreeSet<&str> = engine.catalog().iter().map(|c| c.id).collect();
    for (id, _) in p.criteria {
        assert!(
            catalog.contains(id),
            "the profile judges `{id}`, which is not in the catalog — it can never run, and a \
             criterion that never runs blocks `ready` forever"
        );
    }
}

// ---------------------------------------------------------------------------
// The named threshold profiles: `standard` and `strict`, and the one refused.
// ---------------------------------------------------------------------------

#[test]
fn strict_only_tightens_and_claims_nothing_about_readiness() {
    let strict = veridex_core::profile::by_name("strict").expect("strict exists");
    assert!(
        !strict.judges_readiness(),
        "a threshold profile has no criteria, so it must not produce a readiness verdict"
    );
    assert!(strict.criteria.is_empty());

    // Every threshold it names is tighter than the default — which is what keeps it out of
    // `SCOPE.NARROWED` and usable with `--min-score`.
    let d = veridex_core::Tolerances::default();
    let applied = strict.apply_tolerances(d);
    assert!(applied.clock_skew_ns < d.clock_skew_ns);
    assert!(applied.rate_deviation < d.rate_deviation);
    assert!(applied.gap_factor < d.gap_factor);
    assert!(applied.jitter_cv < d.jitter_cv);
    assert!(applied.outlier_z < d.outlier_z);
    assert!(applied.sequence_drop_fraction < d.sequence_drop_fraction);

    // And it cannot relax a threshold an operator set tighter still.
    let tighter = veridex_core::Tolerances {
        clock_skew_ns: 1_000_000,
        outlier_z: 3.0,
        ..d
    };
    let applied = strict.apply_tolerances(tighter);
    assert_eq!(applied.clock_skew_ns, 1_000_000);
    assert_eq!(applied.outlier_z, 3.0);
}

#[test]
fn standard_is_the_defaults_under_a_name() {
    let standard = veridex_core::profile::by_name("standard").expect("standard exists");
    assert!(!standard.judges_readiness());
    let d = veridex_core::Tolerances::default();
    assert_eq!(standard.apply_tolerances(d), d);
    // Naming the default must not change a configured value either.
    let configured = veridex_core::Tolerances {
        gap_factor: 1.5,
        ..d
    };
    assert_eq!(standard.apply_tolerances(configured), configured);
}

#[test]
fn a_loosening_profile_is_refused_with_its_reason() {
    // `lenient` is the name people reach for, and "unknown profile" would read as an oversight
    // rather than a refusal. A profile that loosens is a narrowing, and Veridex discloses a
    // narrowing per threshold rather than letting a reassuring name carry it.
    for name in ["lenient", "relaxed", "permissive"] {
        assert!(veridex_core::profile::by_name(name).is_none());
        let reason = veridex_core::profile::refusal_reason(name)
            .unwrap_or_else(|| panic!("`{name}` must be refused with a reason"));
        assert!(reason.contains("SCOPE.NARROWED"), "{reason}");
        assert!(reason.contains("veridex.toml"), "{reason}");
    }
    // An actual typo still gets the plain unknown-name treatment.
    assert!(veridex_core::profile::refusal_reason("wrold-model-ready").is_none());

    // Every name the catalog advertises resolves.
    for name in veridex_core::profile::KNOWN_PROFILES {
        assert!(
            veridex_core::profile::by_name(name).is_some(),
            "`{name}` is advertised but does not resolve"
        );
    }
}
