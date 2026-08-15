//! Behavior tests for the MCAP adapter, driven by real MCAP files written with the `mcap` crate.

use std::collections::BTreeMap;
use std::io::Cursor;

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{Adapter, Coverage, Detection, IngestOptions, Source};
use veridex_core::cdm::Modality;
use veridex_core::check::Check;
use veridex_core::checks::autonomy::{RigSync, SequenceComplete};
use veridex_core::checks::temporal::ClockSkew;

/// One channel's messages: (schema_name, topic, message_encoding, log_times_ns).
struct Chan {
    schema: &'static str,
    topic: &'static str,
    times: Vec<u64>,
}

/// Build an in-memory MCAP file from the given channels and return its bytes.
fn build_mcap(channels: &[Chan]) -> Vec<u8> {
    build_mcap_payload(channels, b"payload")
}

/// Like [`build_mcap`], but every message carries `payload` as its data bytes — so tests can vary
/// frame *content* while holding structure (channels, timestamps) fixed.
fn build_mcap_payload(channels: &[Chan], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut writer = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        for chan in channels {
            let schema_id = writer
                .add_schema(chan.schema, "ros2msg", b"")
                .expect("add schema");
            let channel_id = writer
                .add_channel(schema_id, chan.topic, "cdr", &BTreeMap::new())
                .expect("add channel");
            for (seq, &t) in chan.times.iter().enumerate() {
                writer
                    .write_to_known_channel(
                        &mcap::records::MessageHeader {
                            channel_id,
                            sequence: seq as u32,
                            log_time: t,
                            publish_time: t,
                        },
                        payload,
                    )
                    .expect("write message");
            }
        }
        writer.finish().expect("finish");
    }
    out
}

fn write_temp_mcap(bytes: &[u8]) -> tempfile::TempPath {
    use std::io::Write;
    let mut f = tempfile::Builder::new()
        .suffix(".mcap")
        .tempfile()
        .expect("temp file");
    f.write_all(bytes).expect("write");
    f.flush().expect("flush");
    f.into_temp_path()
}

#[test]
fn detects_mcap_by_extension() {
    let a = McapAdapter;
    assert_eq!(
        a.detect(&Source::Local("recording.mcap".into())),
        Detection::Yes {
            version: Some("0".into())
        }
    );
    assert_eq!(
        a.detect(&Source::Local("data.parquet".into())),
        Detection::No
    );
}

#[test]
fn maps_channels_to_streams_and_messages_to_frames() {
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/camera/image",
            times: vec![0, 33_000_000, 66_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/JointState",
            topic: "/joint_states",
            times: vec![0, 20_000_000, 40_000_000, 60_000_000],
        },
    ]);
    let path = write_temp_mcap(&bytes);

    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let d = &ingested.dataset;
    assert_eq!(d.episodes.len(), 1, "MCAP maps to a single episode");
    let ep = &d.episodes[0];
    assert_eq!(ep.streams.len(), 2);

    // Streams are canonicalized by name; find by topic.
    let cam = ep
        .streams
        .iter()
        .find(|s| s.name == "/camera/image")
        .unwrap();
    assert_eq!(cam.modality, Modality::Video);
    assert_eq!(cam.frames.len(), 3);
    assert_eq!(cam.clock_id, "mcap-log");
    assert_eq!(cam.frames[1].ts, 33_000_000);

    let joints = ep
        .streams
        .iter()
        .find(|s| s.name == "/joint_states")
        .unwrap();
    assert_eq!(joints.modality, Modality::ScalarState);
    assert_eq!(joints.frames.len(), 4);

    // Episode envelope spans all messages.
    assert_eq!(ep.start_ts, Some(0));
    assert_eq!(ep.end_ts, Some(66_000_000));
}

#[test]
fn av_rig_message_types_map_to_the_autonomy_modalities() {
    // A ROS 2 autonomy rig: LiDAR, IMU, GNSS, ego-odometry, and a CAN channel. Each schema name must
    // classify to its rig modality (A1), not fall back to the generic ScalarState.
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/PointCloud2",
            topic: "/lidar/points",
            times: vec![0, 100_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/Imu",
            topic: "/imu/data",
            times: vec![0, 10_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/NavSatFix",
            topic: "/gps/fix",
            times: vec![0, 200_000_000],
        },
        Chan {
            schema: "nav_msgs/msg/Odometry",
            topic: "/odom",
            times: vec![0, 50_000_000],
        },
        Chan {
            schema: "can_msgs/msg/Frame",
            topic: "/can/rx",
            times: vec![0, 5_000_000],
        },
    ]);
    let path = write_temp_mcap(&bytes);

    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let ep = &ingested.dataset.episodes[0];
    let modality = |name: &str| {
        ep.streams
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("stream {name} missing"))
            .modality
    };
    assert_eq!(modality("/lidar/points"), Modality::PointCloud);
    assert_eq!(modality("/imu/data"), Modality::Imu);
    assert_eq!(modality("/gps/fix"), Modality::Gnss);
    assert_eq!(modality("/odom"), Modality::EgoPose);
    assert_eq!(modality("/can/rx"), Modality::CanSignal);
}

#[test]
fn an_injected_single_sensor_sync_drift_is_flagged_on_an_av_rig() {
    // A rig where camera/LiDAR/GNSS span ~1.0 s but the IMU is cut to ~0.70 s — a single-sensor sync
    // drift of ~0.30 s. The duration-based cross-stream skew check must flag it and name the IMU.
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/camera/image",
            times: (0..31).map(|i| i * 33_000_000).collect(),
        },
        Chan {
            schema: "sensor_msgs/msg/PointCloud2",
            topic: "/lidar/points",
            times: (0..11).map(|i| i * 100_000_000).collect(),
        },
        Chan {
            schema: "sensor_msgs/msg/NavSatFix",
            topic: "/gps/fix",
            times: (0..11).map(|i| i * 100_000_000).collect(),
        },
        Chan {
            schema: "sensor_msgs/msg/Imu",
            topic: "/imu/data",
            times: (0..101).map(|i| i * 7_000_000).collect(),
        },
    ]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    // This is a rig (4 AV-native sensors), so the rig-wide check reports the drift as a single
    // finding naming the drifted IMU...
    let rig = RigSync::default().run(&ingested.dataset);
    assert_eq!(
        rig.len(),
        1,
        "one rig-wide sync finding, not O(n^2) pairwise"
    );
    assert_eq!(rig[0].code, "AUTONOMY.RIG_SYNC");
    assert!(
        rig[0].message.contains("/imu/data"),
        "the drifted sensor must be named: {}",
        rig[0].message
    );
    // ...and the pairwise clock-skew check stays quiet on a rig (RigSync supersedes it).
    assert!(
        ClockSkew::default().run(&ingested.dataset).is_empty(),
        "pairwise CLOCK_SKEW must not double-report on a rig"
    );
}

#[test]
fn header_library_is_extracted_as_recorder_provenance() {
    // The `mcap` writer stamps its library into the header; the adapter surfaces it as honest
    // `recorder` provenance (class Known) and as `mcap_library` dataset metadata.
    let bytes = build_mcap(&[Chan {
        schema: "sensor_msgs/msg/Image",
        topic: "/cam",
        times: vec![0, 1],
    }]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let recorder = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == "recorder")
        .expect("recorder provenance extracted from header");
    assert!(recorder.value.as_deref().unwrap_or("").contains("mcap"));
    assert!(ingested
        .dataset
        .metadata
        .iter()
        .any(|(k, _)| k == "mcap_library"));
    assert!(ingested
        .report
        .mapped_fields
        .iter()
        .any(|f| f.contains("header.library -> provenance.recorder")));
}

/// Build an MCAP carrying one channel plus a Metadata record and a calibration Attachment, to
/// exercise the richer provenance extraction.
fn build_mcap_with_provenance() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let schema = w
            .add_schema("sensor_msgs/msg/Image", "ros2msg", b"")
            .unwrap();
        let chan = w
            .add_channel(schema, "/cam", "cdr", &BTreeMap::new())
            .unwrap();
        for (seq, &t) in [0u64, 1].iter().enumerate() {
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: chan,
                    sequence: seq as u32,
                    log_time: t,
                    publish_time: t,
                },
                b"payload",
            )
            .unwrap();
        }
        let mut meta = BTreeMap::new();
        meta.insert("license".to_string(), "CC-BY-4.0".to_string());
        meta.insert("sensor".to_string(), "ZED2i".to_string());
        meta.insert("operator".to_string(), "alice".to_string());
        meta.insert("site".to_string(), "lab-3".to_string()); // not a known provenance key
        w.write_metadata(&mcap::records::Metadata {
            name: "recording_info".to_string(),
            metadata: meta,
        })
        .expect("write metadata");
        w.attach(&mcap::Attachment {
            log_time: 0,
            create_time: 0,
            name: "calibration.yaml".to_string(),
            media_type: "application/yaml".to_string(),
            data: (b"" as &[u8]).into(),
        })
        .expect("attach");
        w.finish().expect("finish");
    }
    out
}

#[test]
fn metadata_records_and_attachments_become_provenance() {
    let path = write_temp_mcap(&build_mcap_with_provenance());
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let elem = |key: &str| {
        ingested
            .dataset
            .provenance
            .iter()
            .flat_map(|r| &r.elements)
            .find(|e| e.key == key)
            .cloned()
    };
    let prov = |key: &str| elem(key).and_then(|e| e.value.clone());

    // Well-known metadata keys map to typed provenance (class Known).
    assert_eq!(prov("license").as_deref(), Some("CC-BY-4.0"));
    assert_eq!(prov("sensor").as_deref(), Some("ZED2i"));
    assert_eq!(prov("annotator").as_deref(), Some("alice")); // "operator" → annotator
                                                             // The calibration attachment supplies the calibration element.
    assert_eq!(prov("calibration").as_deref(), Some("calibration.yaml"));
    // It is inferred from the attachment *name*, not extracted content, so it's Asserted, not Known.
    assert_eq!(
        elem("calibration").map(|e| e.class),
        Some(veridex_core::cdm::ProvenanceClass::Asserted)
    );
    assert_eq!(
        elem("license").map(|e| e.class),
        Some(veridex_core::cdm::ProvenanceClass::Known)
    );

    // Every metadata key/value is preserved (even the non-mapped "site").
    assert!(ingested
        .dataset
        .metadata
        .iter()
        .any(|(k, v)| k == "mcap_meta.recording_info.site" && v == "lab-3"));
}

#[test]
fn report_declares_fidelity_and_omissions() {
    let bytes = build_mcap(&[Chan {
        schema: "sensor_msgs/msg/Image",
        topic: "/cam",
        times: vec![0, 1],
    }]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let r = &ingested.report;
    assert_eq!(r.format_id, "mcap");
    assert_eq!(r.coverage, Coverage::Full);
    assert!(!r.mapped_fields.is_empty());
    // The message bytes are fingerprinted into the frame content hash — disclosed as mapped.
    assert!(r.mapped_fields.iter().any(|m| m.contains("content_hash")));
    // MCAP has no episode concept or declared rates — these must be disclosed as omitted.
    assert!(r.omitted_fields.iter().any(|f| f.contains("episode")));
    assert!(r.omitted_fields.iter().any(|f| f.contains("rate")));
    // publish_time / sequence exist in MCAP but not in the CDM.
    assert!(r
        .unmapped_fields
        .iter()
        .any(|u| u.source_path.contains("publish_time")));
}

#[test]
fn ingested_mcap_flows_through_the_engine() {
    // camera spans 1000 ms, robot spans 1500 ms => a clock-skew error via the standard engine.
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/cam",
            times: vec![0, 1_000_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/JointState",
            topic: "/robot",
            times: vec![0, 1_500_000_000],
        },
    ]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
}

#[test]
fn a_late_starting_channel_trips_start_offset_not_clock_skew() {
    // Both channels span the same 1000 ms, but the robot comes online 300 ms late. All MCAP channels
    // share the `mcap-log` clock, so the diverging start is a START_OFFSET; equal durations mean no
    // CLOCK_SKEW. Proves the shared-clock assignment makes START_OFFSET reachable end-to-end.
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/cam",
            times: vec![0, 1_000_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/JointState",
            topic: "/robot",
            times: vec![300_000_000, 1_300_000_000],
        },
    ]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    assert!(verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.START_OFFSET"));
    assert!(
        !verdict
            .findings
            .iter()
            .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"),
        "equal durations must not trip clock skew"
    );
}

#[test]
fn re_ingesting_the_same_bytes_yields_the_same_content_hash() {
    let bytes = build_mcap(&[Chan {
        schema: "sensor_msgs/msg/JointState",
        topic: "/j",
        times: vec![0, 10, 20],
    }]);
    let p1 = write_temp_mcap(&bytes);
    let p2 = write_temp_mcap(&bytes);
    // Content hash is over the CDM, which uses file_stem as the id; use the same stem by design of
    // the test: different temp names differ, so compare the episodes/streams structure via a
    // normalized id instead.
    let mut a = McapAdapter
        .ingest(&Source::Local(p1.to_path_buf()), &IngestOptions::default())
        .unwrap()
        .dataset;
    let mut b = McapAdapter
        .ingest(&Source::Local(p2.to_path_buf()), &IngestOptions::default())
        .unwrap()
        .dataset;
    a.id = "fixed".into();
    b.id = "fixed".into();
    assert_eq!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b)
    );
}

#[test]
fn frames_carry_a_content_hash_of_the_message_bytes() {
    let bytes = build_mcap(&[Chan {
        schema: "sensor_msgs/msg/JointState",
        topic: "/j",
        times: vec![0, 10, 20],
    }]);
    let path = write_temp_mcap(&bytes);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .unwrap()
        .dataset;
    // Every frame is fingerprinted, and identical message bytes hash identically.
    let hashes: Vec<[u8; 32]> = d.episodes[0].streams[0]
        .frames
        .iter()
        .map(|f| {
            f.value_ref
                .content_hash
                .expect("frame carries a content hash")
        })
        .collect();
    assert_eq!(hashes.len(), 3);
    assert!(
        hashes.iter().all(|h| *h == hashes[0]),
        "same payload → same hash"
    );
}

#[test]
fn different_frame_content_changes_the_cdm_hash() {
    // Structure held fixed (same channels + timestamps); only the message payload differs. Because
    // frames now carry a content hash that feeds canonicalization, the CDM content hash must differ —
    // so a tampered recording can no longer verify against a certificate bound to the original.
    let chans = [Chan {
        schema: "sensor_msgs/msg/JointState",
        topic: "/j",
        times: vec![0, 10, 20],
    }];
    let p1 = write_temp_mcap(&build_mcap_payload(&chans, b"original"));
    let p2 = write_temp_mcap(&build_mcap_payload(&chans, b"tampered"));
    let mut a = McapAdapter
        .ingest(&Source::Local(p1.to_path_buf()), &IngestOptions::default())
        .unwrap()
        .dataset;
    let mut b = McapAdapter
        .ingest(&Source::Local(p2.to_path_buf()), &IngestOptions::default())
        .unwrap()
        .dataset;
    // Normalize the id (from the temp file stem) so only frame content differs.
    a.id = "fixed".into();
    b.id = "fixed".into();
    assert_ne!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b),
        "differing frame content must change the CDM hash"
    );
}

#[test]
fn a_frame_dropping_sensor_is_flagged_incomplete_end_to_end() {
    // A rig where LiDAR and GNSS record 20 steady 100 ms ticks, but the IMU (nominally 100 ms too)
    // drops 5 of its 20 frames — a 25% aggregate drop with no single huge gap. The completeness check
    // must flag the IMU by name, through the real MCAP adapter.
    let full: Vec<u64> = (0..20).map(|i| i * 100_000_000).collect();
    let dropped: Vec<u64> = full
        .iter()
        .enumerate()
        .filter(|(i, _)| ![3usize, 7, 11, 15, 17].contains(i))
        .map(|(_, t)| *t)
        .collect();
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/PointCloud2",
            topic: "/lidar/points",
            times: full.clone(),
        },
        Chan {
            schema: "sensor_msgs/msg/NavSatFix",
            topic: "/gps/fix",
            times: full,
        },
        Chan {
            schema: "sensor_msgs/msg/Imu",
            topic: "/imu/data",
            times: dropped,
        },
    ]);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let f = SequenceComplete::default().run(&ingested.dataset);
    assert_eq!(f.len(), 1, "only the dropping sensor is flagged");
    assert_eq!(f[0].code, "AUTONOMY.SEQUENCE_COMPLETE");
    assert!(
        f[0].message.contains("/imu/data"),
        "names the incomplete sensor: {}",
        f[0].message
    );
}

// ---- ROS message-body decode (CDR) end-to-end ----

/// A minimal CDR (little-endian, ROS 2 default) writer for building message-body fixtures.
struct Cdr {
    buf: Vec<u8>,
}
impl Cdr {
    fn new() -> Cdr {
        Cdr {
            buf: vec![0x00, 0x01, 0x00, 0x00],
        }
    }
    fn align(&mut self, n: usize) {
        while (self.buf.len() - 4) % n != 0 {
            self.buf.push(0);
        }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.align(8);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn string(&mut self, s: &str) {
        self.u32((s.len() + 1) as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }
    fn header(&mut self, frame: &str) {
        self.u32(0);
        self.u32(0);
        self.string(frame);
    }
}

/// Build an MCAP where each channel carries one custom-payload message at t=0.
fn build_mcap_one_shot(channels: &[(&str, &str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        for (schema, topic, payload) in channels {
            let sid = w.add_schema(schema, "ros2msg", b"").unwrap();
            let cid = w.add_channel(sid, topic, "cdr", &BTreeMap::new()).unwrap();
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: cid,
                    sequence: 0,
                    log_time: 0,
                    publish_time: 0,
                },
                payload,
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    out
}

#[test]
fn ros_message_bodies_populate_the_autonomy_cdm_end_to_end() {
    // PointCloud2 with x/y/z/intensity fields.
    let mut pc = Cdr::new();
    pc.header("lidar");
    pc.u32(1);
    pc.u32(1000);
    pc.u32(4);
    for name in ["x", "y", "z", "intensity"] {
        pc.string(name);
        pc.u32(0);
        pc.u8(7); // FLOAT32
        pc.u32(1);
    }
    // CameraInfo with fx=600, fy=600, cx=320, cy=240.
    let mut cam = Cdr::new();
    cam.header("cam");
    cam.u32(480);
    cam.u32(640);
    cam.string("plumb_bob");
    cam.u32(0); // no distortion coeffs
    for v in [600.0, 0.0, 320.0, 0.0, 600.0, 240.0, 0.0, 0.0, 1.0] {
        cam.f64(v);
    }
    // Odometry pose at (1,2,3).
    let mut odom = Cdr::new();
    odom.header("odom");
    odom.string("base_link");
    for v in [1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0] {
        odom.f64(v);
    }
    // TF: base_link -> lidar.
    let mut tf = Cdr::new();
    tf.u32(1);
    tf.header("base_link");
    tf.string("lidar");
    for v in [0.5, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0] {
        tf.f64(v);
    }

    let bytes = build_mcap_one_shot(&[
        ("sensor_msgs/msg/PointCloud2", "/lidar/points", pc.buf),
        ("sensor_msgs/msg/CameraInfo", "/cam/info", cam.buf),
        ("nav_msgs/msg/Odometry", "/odom", odom.buf),
        ("tf2_msgs/msg/TFMessage", "/tf", tf.buf),
    ]);
    let path = write_temp_mcap(&bytes);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;

    // point_fields on the LiDAR stream.
    let lidar = d.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/lidar/points")
        .unwrap();
    let pf = lidar.point_fields.as_ref().expect("point_fields decoded");
    let names: Vec<&str> = pf.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["x", "y", "z", "intensity"]);

    // Calibration: intrinsics + the transform tree.
    let calib = d.calibration.as_ref().expect("calibration decoded");
    assert_eq!(calib.intrinsics.len(), 1);
    assert_eq!(calib.intrinsics[0].fx, 600.0);
    assert_eq!(calib.transforms.len(), 1);
    assert_eq!(calib.transforms[0].parent_frame, "base_link");
    assert_eq!(calib.transforms[0].child_frame, "lidar");

    // Ego trajectory.
    let ego = d.episodes[0].ego_poses.as_ref().expect("ego_poses decoded");
    assert_eq!(ego.len(), 1);
    assert_eq!(ego[0].pose.translation, [1.0, 2.0, 3.0]);
}

#[test]
fn a_teleporting_ego_trajectory_is_flagged_end_to_end() {
    use veridex_core::checks::autonomy::EgoPoseContinuity;

    // Three Odometry messages: smooth, then a 500 m jump in 100 ms (a teleport).
    let poses = [(0u64, 0.0f64), (100_000_000, 0.1), (200_000_000, 500.0)];
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let sid = w
            .add_schema("nav_msgs/msg/Odometry", "ros2msg", b"")
            .unwrap();
        let cid = w
            .add_channel(sid, "/odom", "cdr", &BTreeMap::new())
            .unwrap();
        for (seq, (t, x)) in poses.iter().enumerate() {
            let mut c = Cdr::new();
            c.header("odom");
            c.string("base_link");
            for v in [*x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
                c.f64(v);
            }
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: cid,
                    sequence: seq as u32,
                    log_time: *t,
                    publish_time: *t,
                },
                &c.buf,
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    let path = write_temp_mcap(&out);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;
    let ego = d.episodes[0].ego_poses.as_ref().expect("ego_poses decoded");
    assert_eq!(ego.len(), 3);
    let f = EgoPoseContinuity::default().run(&d);
    assert_eq!(f.len(), 1, "the teleport must be flagged");
    assert_eq!(f[0].code, "AUTONOMY.EGO_POSE_CONTINUITY");
}

#[test]
fn a_rig_without_a_transform_tree_is_flagged_incomplete_end_to_end() {
    use veridex_core::checks::autonomy::CalibrationCompleteness;
    // A rig with LiDAR + GNSS + IMU (3 AV-native sensors, LiDAR is a spatial sensor) but no TF or
    // CameraInfo messages, so no calibration is decoded — fusion is impossible.
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/PointCloud2",
            topic: "/lidar/points",
            times: vec![0, 100_000_000, 200_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/NavSatFix",
            topic: "/gps/fix",
            times: vec![0, 100_000_000, 200_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/Imu",
            topic: "/imu/data",
            times: vec![0, 100_000_000, 200_000_000],
        },
    ]);
    let path = write_temp_mcap(&bytes);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;
    assert!(d.calibration.is_none(), "no TF/CameraInfo → no calibration");
    let f = CalibrationCompleteness.run(&d);
    assert!(
        f.iter()
            .any(|x| x.code == "AUTONOMY.CALIBRATION_INCOMPLETE"),
        "a spatial rig with no transform tree must be flagged"
    );
}

#[test]
fn autonomy_provenance_keys_are_extracted() {
    use veridex_core::cdm::ProvenanceClass;
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let s = w.add_schema("sensor_msgs/msg/Imu", "ros2msg", b"").unwrap();
        let c = w.add_channel(s, "/imu", "cdr", &BTreeMap::new()).unwrap();
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: c,
                sequence: 0,
                log_time: 0,
                publish_time: 0,
            },
            b"x",
        )
        .unwrap();
        let mut meta = BTreeMap::new();
        meta.insert("firmware_version".to_string(), "sensorOS 4.2".to_string());
        meta.insert("vehicle_id".to_string(), "av-07".to_string());
        meta.insert("drive_id".to_string(), "2026-08-15-run3".to_string());
        meta.insert("region".to_string(), "us-ca-sf".to_string());
        meta.insert("map_version".to_string(), "hd-map-1.9".to_string());
        meta.insert("consent_status".to_string(), "obtained".to_string());
        meta.insert("redaction".to_string(), "faces+plates".to_string());
        w.write_metadata(&mcap::records::Metadata {
            name: "rig_info".to_string(),
            metadata: meta,
        })
        .expect("write metadata");
        w.finish().expect("finish");
    }
    let path = write_temp_mcap(&out);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;
    let el = |key: &str| {
        d.provenance
            .iter()
            .flat_map(|r| &r.elements)
            .find(|e| e.key == key)
            .cloned()
    };
    for (key, val) in [
        ("firmware", "sensorOS 4.2"),
        ("platform", "av-07"),
        ("drive", "2026-08-15-run3"),
        ("region", "us-ca-sf"),
        ("map_version", "hd-map-1.9"),
        ("consent", "obtained"),
        ("redaction", "faces+plates"),
    ] {
        let e = el(key).unwrap_or_else(|| panic!("provenance `{key}` not extracted"));
        assert_eq!(e.value.as_deref(), Some(val), "value for {key}");
        assert_eq!(e.class, ProvenanceClass::Known, "class for {key}");
    }
}
