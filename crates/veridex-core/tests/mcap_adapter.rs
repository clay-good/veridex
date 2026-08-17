//! Behavior tests for the MCAP adapter, driven by real MCAP files written with the `mcap` crate.

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{
    default_registry, Adapter, Coverage, Detection, IngestError, IngestOptions, Source,
};
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
    // Camera spans 1000 ms, robot 1500 ms, both recorded at 100 Hz => a clock-skew error via the
    // standard engine. The cadence matters: the span comparison allows for each stream's own sampling
    // period, so two-frame streams could not evidence the drift.
    let dense = |span_ns: i64| -> Vec<u64> {
        (0..=(span_ns / 10_000_000))
            .map(|i| (i * 10_000_000) as u64)
            .collect()
    };
    let bytes = build_mcap(&[
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/cam",
            times: dense(1_000_000_000),
        },
        Chan {
            schema: "sensor_msgs/msg/JointState",
            topic: "/robot",
            times: dense(1_500_000_000),
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

#[test]
fn scenario_metadata_becomes_episode_labels() {
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
        meta.insert("weather".to_string(), "rain".to_string());
        meta.insert("tod".to_string(), "night".to_string()); // → time_of_day
        w.write_metadata(&mcap::records::Metadata {
            name: "scene".to_string(),
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
    let label = |key: &str| {
        d.episodes[0]
            .labels
            .iter()
            .find(|l| l.key == key)
            .map(|l| l.value.clone())
    };
    assert_eq!(label("weather").as_deref(), Some("rain"));
    assert_eq!(label("time_of_day").as_deref(), Some("night"));
    // And the descriptive report picks them up.
    let cov = veridex_core::scenario::coverage(&d);
    assert_eq!(cov.len(), 2);
}

/// Sum of the *declared* uncompressed size of every chunk in an MCAP file, read from the chunk
/// headers without unpacking them — the same figure the adapter's budget charges.
fn declared_chunk_bytes(bytes: &[u8]) -> u64 {
    mcap::read::LinearReader::new(bytes)
        .expect("linear reader")
        .filter_map(|rec| match rec {
            Ok(mcap::records::Record::Chunk { header, .. }) => Some(header.uncompressed_size),
            _ => None,
        })
        .sum()
}

/// Rewrite every little-endian occurrence of `from` (the chunk header's declared uncompressed size,
/// which the chunk index repeats) to `to`, so a test can forge a decompression bomb's *claim*
/// without having to produce gigabytes of real data.
fn forge_declared_size(bytes: &mut [u8], from: u64, to: u64) -> usize {
    let (from, to) = (from.to_le_bytes(), to.to_le_bytes());
    let mut patched = 0;
    for i in 0..bytes.len().saturating_sub(8) {
        if bytes[i..i + 8] == from {
            bytes[i..i + 8].copy_from_slice(&to);
            patched += 1;
        }
    }
    patched
}

/// Without the budget this file does not merely cost memory: the MCAP reader keeps asking for the
/// 8 GiB the header promises and spins indefinitely on a few hundred bytes. The refusal has to come
/// before the reader is handed the file, which is why the budget is charged off the chunk headers.
#[test]
fn a_chunk_declaring_a_huge_expansion_is_refused_before_it_is_unpacked() {
    let mut bytes = build_mcap(&[Chan {
        topic: "/camera",
        schema: "sensor_msgs/msg/Image",
        times: vec![1_000, 2_000, 3_000],
    }]);
    let declared = declared_chunk_bytes(&bytes);
    assert!(declared > 0, "the fixture writer must produce a chunk");
    // Claim 8 GiB of contents inside a file of a few hundred bytes.
    assert!(forge_declared_size(&mut bytes, declared, 8 * 1024 * 1024 * 1024) > 0);
    let path = write_temp_mcap(&bytes);

    let err = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a chunk claiming 8 GiB must be refused");
    match err {
        veridex_core::adapter::IngestError::DecompressionBudgetExceeded {
            format_id,
            limit,
            requested,
        } => {
            assert_eq!(format_id, "mcap");
            assert_eq!(requested, 8 * 1024 * 1024 * 1024);
            assert!(
                limit < requested,
                "the budget must be the binding constraint"
            );
        }
        other => panic!("expected a decompression-budget error, got {other:?}"),
    }
}

#[test]
fn an_ordinary_recording_is_well_inside_the_decompression_budget() {
    let bytes = build_mcap_payload(
        &[Chan {
            topic: "/camera",
            schema: "sensor_msgs/msg/Image",
            times: vec![1_000, 2_000, 3_000],
        }],
        &[7u8; 4096],
    );
    let path = write_temp_mcap(&bytes);
    McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("a real recording must ingest under the default budget");
}

/// A `tf2_msgs/msg/TFMessage` CDR body: one identity `TransformStamped` per `(parent, child)` edge.
fn tf_body(edges: &[(&str, &str)]) -> Vec<u8> {
    let mut buf: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00];
    let align = |buf: &mut Vec<u8>, n: usize| {
        while (buf.len() - 4) % n != 0 {
            buf.push(0)
        }
    };
    let u32v = |buf: &mut Vec<u8>, v: u32| {
        align(buf, 4);
        buf.extend_from_slice(&v.to_le_bytes());
    };
    let strv = |buf: &mut Vec<u8>, s: &str| {
        u32v(buf, (s.len() + 1) as u32);
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    };
    let f64v = |buf: &mut Vec<u8>, v: f64| {
        align(buf, 8);
        buf.extend_from_slice(&v.to_le_bytes());
    };
    u32v(&mut buf, edges.len() as u32);
    for (parent, child) in edges {
        u32v(&mut buf, 0);
        u32v(&mut buf, 0);
        strv(&mut buf, parent);
        strv(&mut buf, child);
        for v in [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
            f64v(&mut buf, v);
        }
    }
    buf
}

/// A minimal header-first CDR body naming `frame_id`, with a varying payload tail.
fn header_body(frame_id: &str, payload: u64) -> Vec<u8> {
    let mut buf: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00];
    buf.extend_from_slice(&0u32.to_le_bytes()); // stamp.sec
    buf.extend_from_slice(&0u32.to_le_bytes()); // stamp.nanosec
    buf.extend_from_slice(&((frame_id.len() + 1) as u32).to_le_bytes());
    buf.extend_from_slice(frame_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&payload.to_le_bytes());
    buf
}

/// Write a four-sensor rig MCAP whose sensors stamp the given frames, with `tf_edges` as the static
/// transform tree.
fn write_framed_rig(
    path: &std::path::Path,
    sensor_frames: &[(&str, &str, &str)],
    tf_edges: &[(&str, &str)],
) {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).unwrap();
        let tf_schema = w
            .add_schema("tf2_msgs/msg/TFMessage", "ros2msg", b"")
            .unwrap();
        let tf_channel = w
            .add_channel(tf_schema, "/tf_static", "cdr", &BTreeMap::new())
            .unwrap();
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: tf_channel,
                sequence: 0,
                log_time: 0,
                publish_time: 0,
            },
            &tf_body(tf_edges),
        )
        .unwrap();

        for (i, (schema, topic, frame)) in sensor_frames.iter().enumerate() {
            let sid = w.add_schema(schema, "ros2msg", b"").unwrap();
            let cid = w.add_channel(sid, topic, "cdr", &BTreeMap::new()).unwrap();
            for seq in 0..11u32 {
                let t = seq as u64 * 100_000_000;
                w.write_to_known_channel(
                    &mcap::records::MessageHeader {
                        channel_id: cid,
                        sequence: seq,
                        log_time: t,
                        publish_time: t,
                    },
                    &header_body(frame, ((i as u64) << 32) | seq as u64),
                )
                .unwrap();
            }
        }
        w.finish().unwrap();
    }
    fs::write(path, &out).unwrap();
}

/// Ingest an MCAP file into the CDM, canonicalized as the pipeline does.
fn ingest_rig(path: &std::path::Path) -> veridex_core::cdm::Dataset {
    let mut d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .unwrap()
        .dataset;
    d.canonicalize_order();
    d
}

/// The rig's sensors, as (schema, topic, declared frame).
const RIG_SENSORS: &[(&str, &str, &str)] = &[
    ("sensor_msgs/msg/Image", "/camera/image", "camera_front"),
    ("sensor_msgs/msg/PointCloud2", "/lidar/points", "lidar_top"),
    ("sensor_msgs/msg/NavSatFix", "/gps/fix", "gnss"),
    ("sensor_msgs/msg/Imu", "/imu/data", "imu_link"),
];

#[test]
fn sensor_frames_are_decoded_from_message_headers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rig.mcap");
    write_framed_rig(
        &path,
        RIG_SENSORS,
        &[("base_link", "camera_front"), ("base_link", "lidar_top")],
    );

    let d = ingest_rig(&path);
    let frames: Vec<(&str, Option<&str>)> = d.episodes[0]
        .streams
        .iter()
        .map(|s| (s.name.as_str(), s.frame_id.as_deref()))
        .collect();
    assert!(
        frames.contains(&("/lidar/points", Some("lidar_top")))
            && frames.contains(&("/camera/image", Some("camera_front"))),
        "each sensor's header.frame_id reaches the CDM: {frames:?}"
    );
}

#[test]
fn a_lidar_stranded_from_the_camera_is_caught_end_to_end() {
    // The miscalibration class the reprojection check exists for, through the real adapter: the TF
    // tree is well-formed and the LiDAR is in it, but nothing joins `lidar_mount` to `base_link`, so
    // no chain of transforms reaches the camera.
    let dir = tempfile::tempdir().unwrap();

    let good = dir.path().join("good.mcap");
    write_framed_rig(
        &good,
        RIG_SENSORS,
        &[
            ("base_link", "camera_front"),
            ("base_link", "lidar_top"),
            ("base_link", "gnss"),
            ("base_link", "imu_link"),
        ],
    );
    let codes = |p: &std::path::Path| -> Vec<String> {
        let d = ingest_rig(p);
        let engine = veridex_core::checks::default_engine().unwrap();
        let hash = veridex_core::content_hash(&d);
        engine
            .run(&d, hash, &veridex_core::RunConfig::default())
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect()
    };
    assert!(
        !codes(&good)
            .iter()
            .any(|c| c.starts_with("AUTONOMY.SENSOR_FRAME")),
        "a correctly wired rig raises nothing"
    );

    let bad = dir.path().join("bad.mcap");
    write_framed_rig(
        &bad,
        RIG_SENSORS,
        &[
            ("base_link", "camera_front"),
            ("lidar_mount", "lidar_top"),
            ("base_link", "gnss"),
            ("base_link", "imu_link"),
        ],
    );
    assert!(
        codes(&bad).contains(&"AUTONOMY.SENSOR_FRAME_UNRELATED".to_string()),
        "the stranded LiDAR is flagged: {:?}",
        codes(&bad)
    );

    // And the two rigs do not hash alike — the frame the sensor claims is bound into the CDM hash,
    // so a certificate for the wired rig cannot verify against the stranded one.
    assert_ne!(
        veridex_core::content_hash(&ingest_rig(&good)),
        veridex_core::content_hash(&ingest_rig(&bad))
    );
}

#[test]
fn an_absurdly_long_frame_name_is_declined_rather_than_retained() {
    // The CDR reader's slice is bounded by the message body, but invalid UTF-8 expands 3x on the way
    // out (each bad byte becomes a 3-byte U+FFFD) and the decoded string is *retained* in the CDM,
    // while the ingest budget charges the raw body. 63 channels each carrying 1 MiB of 0xFF measured
    // 198 MB retained from a 19.8 KB file — right past the budget meant to cap exactly that.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("longname.mcap");
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).unwrap();
        let sid = w.add_schema("sensor_msgs/Image", "ros2msg", b"").unwrap();
        let cid = w
            .add_channel(sid, "/camera", "cdr", &BTreeMap::new())
            .unwrap();
        // A header whose frame_id is 1 MiB of invalid UTF-8.
        let mut body: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00];
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        let n = 1024 * 1024usize;
        body.extend_from_slice(&(n as u32).to_le_bytes());
        body.extend(std::iter::repeat(0xFFu8).take(n));
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: cid,
                sequence: 0,
                log_time: 0,
                publish_time: 0,
            },
            &body,
        )
        .unwrap();
        w.finish().unwrap();
    }
    fs::write(&path, &out).unwrap();

    let d = ingest_rig(&path);
    let retained: usize = d.episodes[0]
        .streams
        .iter()
        .filter_map(|s| s.frame_id.as_ref().map(|f| f.len()))
        .sum();
    assert_eq!(
        retained, 0,
        "a megabyte-long frame name is not a frame name; it must be declined, not retained"
    );
}

#[test]
fn a_corrupt_chunk_stream_is_refused_rather_than_unpacked_forever() {
    // The chunk *record header* declares an uncompressed size, and the decompression budget charges
    // the sum of those — which bounds an honest file and not a corrupt one, because the compressed
    // stream inside the chunk carries its own length claims and the reader trusts those. One flipped
    // byte inside the zstd frame of the 7,756-byte demo log, with the record header still declaring
    // a truthful 17 KB, sent the reader into an allocation loop that had passed 700 MB after five
    // minutes and was still going — under both ingest budgets and under a 2 GB address-space limit.
    // Not a slow check and not an error: a process that grows until it is killed.
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.mcap");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "veridex-core",
            "--example",
            "make_demo_mcap",
            "--",
        ])
        .arg(&good)
        .arg("av")
        .status()
        .expect("run the demo generator");
    assert!(status.success());

    let bytes = std::fs::read(&good).unwrap();
    // The clean file still ingests, so the new pre-pass is not refusing valid chunks.
    default_registry()
        .ingest(&Source::Local(good.clone()), &IngestOptions::default())
        .expect("a well-formed chunked MCAP still ingests");

    // Byte 2044 sits inside the chunk's compressed payload, past the region the chunk CRC covers in
    // a way the reader notices. Every byte here is decompressed under a bound or refused.
    let mut patched = bytes.clone();
    patched[2044] = 0x76;
    let bad = dir.path().join("bad.mcap");
    std::fs::write(&bad, &patched).unwrap();

    let started = std::time::Instant::now();
    let err = default_registry()
        .ingest(&Source::Local(bad), &IngestOptions::default())
        .expect_err("a chunk whose stream outruns its own declaration must be refused");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the refusal must be prompt, not the result of exhausting memory"
    );
    match err {
        IngestError::Parse { message, .. } => assert!(
            message.contains("chunk") && (message.contains("corrupt") || message.contains("more")),
            "got {message}"
        ),
        other => panic!("expected a named parse error, got {other:?}"),
    }
}

/// A record declaring `u64::MAX` bytes. `usize::try_from` succeeds on a 64-bit target, so the walk's
/// `at + 9 + len` overflowed and aborted the process in any debug or CI build. In release it wrapped
/// to a reversed range and was rejected exactly like a truncated record, so the fix changes only
/// which of those two a debug build does.
#[test]
fn a_record_declaring_an_absurd_length_does_not_abort_the_run() {
    let mut bytes = b"\x89MCAP0\r\n".to_vec();
    bytes.push(0x01); // some record opcode
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absurd.mcap");
    std::fs::write(&path, &bytes).unwrap();

    // Returning at all is the point; naming the corruption is the improvement.
    let err = McapAdapter
        .ingest(
            &Source::Local(path),
            &veridex_core::adapter::IngestOptions::default(),
        )
        .expect_err("a record longer than the file is corrupt framing");
    assert!(
        err.to_string().contains("framing is corrupt"),
        "the refusal must say what is wrong: {err}"
    );
}

/// The same, one byte below the wrap, which already worked — kept so a later refactor cannot fix one
/// and lose the other.
#[test]
fn a_record_declaring_a_merely_huge_length_does_not_abort_the_run() {
    let mut bytes = b"\x89MCAP0\r\n".to_vec();
    bytes.push(0x01);
    bytes.extend_from_slice(&(u64::MAX - 20).to_le_bytes());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.mcap");
    std::fs::write(&path, &bytes).unwrap();
    assert!(McapAdapter
        .ingest(
            &Source::Local(path),
            &veridex_core::adapter::IngestOptions::default(),
        )
        .is_err());
}
