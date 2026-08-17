//! Throwaway audit probes.

use veridex_core::cdm::*;
use veridex_core::certificate::{score, ProvenanceCoverage};
use veridex_core::{content_hash, RunConfig};

fn frames(ts: &[i64], uri: &str) -> Vec<Frame> {
    ts.iter()
        .map(|t| Frame {
            ts: *t,
            value_ref: ValueRef {
                uri: uri.into(),
                byte_offset: None,
                byte_len: None,
                content_hash: None,
            },
        })
        .collect()
}

fn stream(name: &str, m: Modality, clock: &str, ts: &[i64], frame_id: Option<&str>) -> Stream {
    Stream {
        name: name.into(),
        modality: m,
        declared_rate_hz: Some(100.0),
        clock_id: clock.into(),
        clock_kind: ClockKind::Measured,
        dtype: Some("f32".into()),
        shape: Some(vec![7]),
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        point_fields: None,
        media: None,
        frame_id: frame_id.map(str::to_string),
        frames: frames(ts, name),
    }
}

fn pose(x: f64) -> Pose {
    Pose {
        translation: [x, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
    }
}

fn dense(span: i64, step: i64) -> Vec<i64> {
    (0..=(span / step)).map(|i| i * step).collect()
}

fn rig_dataset() -> Dataset {
    let ts = dense(1_000_000_000, 10_000_000);
    let ep = |idx: u64| Episode {
        index: idx,
        start_ts: Some(0),
        end_ts: Some(1_000_000_000),
        streams: vec![
            stream("lidar", Modality::PointCloud, "rig", &ts, Some("lidar_top")),
            stream("cam", Modality::Video, "rig", &ts, Some("cam_front")),
            // duplicate stream name, different content: exercises the tie-break
            stream("cam", Modality::Video, "rig", &ts[..ts.len() - 3], Some("cam_front")),
            stream("speed", Modality::CanSignal, "rig", &ts, None),
        ],
        task: Some("drive".into()),
        labels: vec![
            Label { key: "weather".into(), value: "rain".into(), ts: None },
            Label { key: "weather".into(), value: "fog".into(), ts: Some(5) },
            Label { key: "time_of_day".into(), value: "night".into(), ts: None },
        ],
        ego_poses: Some(vec![
            EgoPose { ts: 0, pose: pose(0.0) },
            EgoPose { ts: 500_000_000, pose: pose(1.0) },
            // same ts, different pose: exercises the tie-break
            EgoPose { ts: 500_000_000, pose: pose(2.0) },
            EgoPose { ts: 1_000_000_000, pose: pose(3.0) },
        ]),
        declared_frame_count: Some(101),
    };
    Dataset {
        id: "rig".into(),
        metadata: vec![
            ("a".into(), "1".into()),
            ("a".into(), "2".into()),
            ("b".into(), "3".into()),
        ],
        provenance: vec![
            Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![
                    ProvenanceElement { key: "license".into(), value: Some("cc".into()), class: ProvenanceClass::Known },
                    ProvenanceElement { key: "sensor".into(), value: Some("velodyne".into()), class: ProvenanceClass::Asserted },
                ],
            },
            Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![
                    ProvenanceElement { key: "clock".into(), value: Some("ptp".into()), class: ProvenanceClass::Known },
                    ProvenanceElement { key: "license".into(), value: Some("mit".into()), class: ProvenanceClass::Known },
                ],
            },
        ],
        // duplicate index 0 twice, plus 1
        episodes: vec![ep(0), ep(0), ep(1)],
        calibration: Some(Calibration {
            transforms: vec![
                Transform { parent_frame: "base_link".into(), child_frame: "lidar_top".into(), pose: pose(1.0), valid_from: None, valid_to: None },
                Transform { parent_frame: "base_link".into(), child_frame: "cam_front".into(), pose: pose(2.0), valid_from: None, valid_to: None },
                Transform { parent_frame: "base_link".into(), child_frame: "cam_front".into(), pose: pose(3.0), valid_from: Some(0), valid_to: Some(9) },
            ],
            intrinsics: vec![
                CameraIntrinsics { stream: "cam".into(), fx: 1.0, fy: 1.0, cx: 0.5, cy: 0.5, distortion: vec![0.0], valid_from: None, valid_to: None },
                CameraIntrinsics { stream: "cam".into(), fx: 2.0, fy: 1.0, cx: 0.5, cy: 0.5, distortion: vec![], valid_from: None, valid_to: None },
            ],
        }),
    }
}

/// Deterministic xorshift so permutations are reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn shuffle<T>(&mut self, v: &mut Vec<T>) {
        for i in (1..v.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

fn permute(d: &mut Dataset, rng: &mut Rng) {
    rng.shuffle(&mut d.metadata);
    rng.shuffle(&mut d.provenance);
    for p in &mut d.provenance {
        rng.shuffle(&mut p.elements);
    }
    rng.shuffle(&mut d.episodes);
    for ep in &mut d.episodes {
        rng.shuffle(&mut ep.streams);
        rng.shuffle(&mut ep.labels);
        if let Some(p) = &mut ep.ego_poses {
            rng.shuffle(p);
        }
    }
    if let Some(c) = &mut d.calibration {
        rng.shuffle(&mut c.transforms);
        rng.shuffle(&mut c.intrinsics);
    }
}

#[test]
fn permuting_every_order_insensitive_collection_never_moves_the_verdict_hash() {
    let engine = veridex_core::checks::default_engine().unwrap();
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let mut baseline_cdm = None;
    let mut baseline_verdict = None;
    let mut baseline_score = None;
    for i in 0..200 {
        let mut d = rig_dataset();
        permute(&mut d, &mut rng);
        d.canonicalize_order();
        let h = content_hash(&d);
        let v = engine.run(&d, h, &RunConfig::default());
        let s = score(&v, &ProvenanceCoverage::of(&d));
        let cdm = h.to_hex();
        let vh = v.result_content_hash.clone();
        match (&baseline_cdm, &baseline_verdict, &baseline_score) {
            (None, ..) => {
                baseline_cdm = Some(cdm);
                baseline_verdict = Some(vh);
                baseline_score = Some(s);
            }
            (Some(bc), Some(bv), Some(bs)) => {
                assert_eq!(*bc, cdm, "cdm hash moved on permutation {i}");
                assert_eq!(*bv, vh, "verdict hash moved on permutation {i}");
                assert_eq!(*bs, s, "score moved on permutation {i}");
            }
            _ => unreachable!(),
        }
    }
}
