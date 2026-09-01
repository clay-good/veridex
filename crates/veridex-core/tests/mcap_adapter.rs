//! Behavior tests for the MCAP adapter, driven by real MCAP files written with the `mcap` crate.

use std::collections::BTreeMap;
use std::fs;
use std::io::Cursor;

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{
    default_registry, Adapter, Coverage, Detection, IngestError, IngestOptions, Source,
};
use veridex_core::cdm::{Modality, ProvenanceClass};
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

/// A complete `sensor_msgs/msg/PointCloud2` body: header, `height`/`width`, four `PointField`s, and
/// the `is_bigendian`/`point_step`/`row_step`/`data` tail.
///
/// The tail is what makes it a cloud rather than a prefix that looks like one. The point-count
/// decode checks the message's own length invariants — `row_step` covers a row of `width` points and
/// `data` is `row_step × height` bytes — precisely so a stubbed or mislabelled body cannot be read
/// as a real count, so a fixture that stops after the fields is not a `PointCloud2` and must not be
/// used to prove anything about counts.
fn point_cloud2(height: u32, width: u32) -> Vec<u8> {
    const POINT_STEP: u32 = 16; // x, y, z, intensity as float32
    let mut pc = Cdr::new();
    pc.header("lidar");
    pc.u32(height);
    pc.u32(width);
    pc.u32(4);
    for (i, name) in ["x", "y", "z", "intensity"].iter().enumerate() {
        pc.string(name);
        pc.u32(i as u32 * 4);
        pc.u8(7); // FLOAT32
        pc.u32(1);
    }
    pc.u8(0); // is_bigendian
    pc.u32(POINT_STEP);
    let row_step = POINT_STEP * width;
    pc.u32(row_step);
    let data_len = row_step * height;
    pc.u32(data_len);
    pc.buf.resize(pc.buf.len() + data_len as usize, 0);
    pc.buf
}

#[test]
fn a_lidar_that_published_only_empty_clouds_is_caught_end_to_end() {
    // The same well-formed `PointCloud2` the working fixture writes, with `width` of zero: the
    // schema, the frame, the timestamps and the rate of a healthy LiDAR and no points. Run through
    // the real adapter and the real engine, because the whole claim is that everything *else*
    // passes on it — a unit test on a hand-built CDM cannot show that.
    let bytes = build_mcap_one_shot(&[
        (
            "sensor_msgs/msg/PointCloud2",
            "/lidar/points",
            point_cloud2(1, 0),
        ),
        (
            "sensor_msgs/msg/PointCloud2",
            "/lidar/points",
            point_cloud2(1, 0),
        ),
    ]);
    let path = write_temp_mcap(&bytes);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;

    let counts = d.episodes[0].streams[0]
        .observed_point_counts
        .expect("point counts decoded");
    assert_eq!(counts.message_count, 2);
    assert_eq!(counts.empty, 2);
    assert_eq!(counts.max, 0);

    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&d);
    let verdict = engine.run(&d, hash, &veridex_core::RunConfig::default());
    let empty: Vec<_> = verdict
        .findings
        .iter()
        .filter(|f| f.code == "AUTONOMY.POINT_CLOUD_EMPTY")
        .collect();
    assert_eq!(empty.len(), 1, "{:?}", verdict.findings);
    assert!(
        empty[0].message.contains("/lidar/points"),
        "{}",
        empty[0].message
    );
}

#[test]
fn ros_message_bodies_populate_the_autonomy_cdm_end_to_end() {
    // PointCloud2 with x/y/z/intensity fields.
    let pc = point_cloud2(1, 1000);
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
        ("sensor_msgs/msg/PointCloud2", "/lidar/points", pc),
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
    // And how many points each cloud actually held (`height × width`, stated in the message header
    // ahead of the bulk blob). The layout says what a record looks like; this says whether there
    // were any — a LiDAR whose driver lost its sensor publishes the same layout forever with
    // `width` of zero.
    let counts = lidar
        .observed_point_counts
        .expect("point counts decoded per message");
    assert_eq!(counts.message_count, 1);
    assert_eq!(counts.max, 1000);
    assert_eq!(counts.empty, 0);

    // Calibration: intrinsics + the transform tree.
    let calib = d.calibration.as_ref().expect("calibration decoded");
    assert_eq!(calib.intrinsics.len(), 1);
    assert_eq!(calib.intrinsics[0].fx, 600.0);
    // The image the matrix was computed for, stated in the same message and carried through: `cx`
    // and `cy` are pixel coordinates, and without these nothing says which image they index. The
    // decoder read both fields and discarded them, which left `AUTONOMY.CALIBRATION_IMPLAUSIBLE`
    // unable to judge a principal point that the source itself puts outside its own image.
    assert_eq!(calib.intrinsics[0].width, Some(640));
    assert_eq!(calib.intrinsics[0].height, Some(480));
    // And the model the coefficients belong to, which is what says how many of them there should
    // be. Read and discarded before, which left a truncated `d` array indistinguishable from a
    // complete one.
    assert_eq!(
        calib.intrinsics[0].distortion_model.as_deref(),
        Some("plumb_bob")
    );
    assert_eq!(calib.transforms.len(), 1);
    assert_eq!(calib.transforms[0].parent_frame, "base_link");
    assert_eq!(calib.transforms[0].child_frame, "lidar");

    // ...and the body frame the trajectory is *of*, from the same message's `child_frame_id`. The
    // stream's own `frame_id` is the reference frame the poses are expressed in; this is the
    // vehicle they are poses of, and the frame every sensor's extrinsics hang off. It was decoded
    // and discarded, which left nothing able to ask whether the trajectory and the sensors describe
    // the same vehicle.
    assert_eq!(d.episodes[0].ego_frame.as_deref(), Some("base_link"));

    // Ego trajectory.
    let ego = d.episodes[0].ego_poses.as_ref().expect("ego_poses decoded");
    assert_eq!(ego.len(), 1);
    assert_eq!(ego[0].pose.translation, [1.0, 2.0, 3.0]);

    // A recording that carries its own extrinsics and intrinsics identifies the calibration that
    // produced it — better than a reference to one, because these values are in the CDM and in its
    // content hash. Reported as *missing* provenance, a rig with a complete transform tree scored
    // zero on the element whose stated risk that tree is what removes.
    let calibration = d.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "calibration")
        .expect("in-band calibration is recorded as provenance");
    assert_eq!(calibration.class, veridex_core::cdm::ProvenanceClass::Known);
    let value = calibration.value.as_deref().unwrap_or_default();
    assert!(
        value.contains("in-band") && value.contains("1 transform") && value.contains("1 camera"),
        "the value names what the recording holds: {value}"
    );
}

/// The element is recorded from *decoded content*, so a recording with no calibration in it gets
/// none. Provenance Veridex made up is worse than provenance it does not have.
#[test]
fn a_recording_without_calibration_is_not_given_a_calibration_element() {
    let bytes = build_mcap(&[Chan {
        schema: "sensor_msgs/msg/Imu",
        topic: "/imu/data",
        times: vec![0, 100_000_000],
    }]);
    let path = write_temp_mcap(&bytes);
    let d = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;
    assert!(d.calibration.is_none());
    assert!(!d.provenance[0]
        .elements
        .iter()
        .any(|e| e.key == "calibration"));
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
fn a_frame_with_two_parents_is_caught_end_to_end() {
    // Through the real adapter: two nodes each publish a transform for `lidar_top`, one from
    // `base_link` and one from a `lidar_mount` that is itself parented to `base_link`. The bag is
    // otherwise perfect — the tree is one connected component, every sensor declares a frame the
    // tree knows, and every sensor reaches the camera — so `AUTONOMY.CALIBRATION_INCOMPLETE` and
    // every `AUTONOMY.SENSOR_FRAME_*` code stay silent, which is exactly what makes the defect
    // invisible without this check. The adapter keys transforms by `(parent, child)`, so the two
    // conflicting edges both survive ingest as distinct parents for one frame.
    let dir = tempfile::tempdir().unwrap();
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

    let wired = &[
        ("base_link", "camera_front"),
        ("base_link", "lidar_mount"),
        ("lidar_mount", "lidar_top"),
        ("base_link", "gnss"),
        ("base_link", "imu_link"),
    ];
    let good = dir.path().join("one-parent.mcap");
    write_framed_rig(&good, RIG_SENSORS, wired);
    assert!(
        !codes(&good).contains(&"AUTONOMY.CALIBRATION_AMBIGUOUS".to_string()),
        "one parent per frame raises nothing: {:?}",
        codes(&good)
    );

    let mut doubled = wired.to_vec();
    doubled.push(("base_link", "lidar_top"));
    let bad = dir.path().join("two-parents.mcap");
    write_framed_rig(&bad, RIG_SENSORS, &doubled);
    let bad_codes = codes(&bad);
    assert!(
        bad_codes.contains(&"AUTONOMY.CALIBRATION_AMBIGUOUS".to_string()),
        "the doubly-parented LiDAR is flagged: {bad_codes:?}"
    );
    // And nothing else moved: the second parent is invisible to every other check, because they
    // all read the frame graph undirected and it is the same graph. (Both rigs also report
    // `AUTONOMY.CALIBRATION_INCOMPLETE` — this fixture writes no `CameraInfo` — which is precisely
    // why "some calibration finding fired" is not evidence the ambiguity was seen.)
    let mut only_in_bad: Vec<&String> = bad_codes
        .iter()
        .filter(|c| !codes(&good).contains(c))
        .collect();
    only_in_bad.sort();
    assert_eq!(
        only_in_bad,
        vec![&"AUTONOMY.CALIBRATION_AMBIGUOUS".to_string()],
        "the two rigs differ by exactly this finding: {bad_codes:?}"
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

/// Run `f` on a worker thread, failing the test if it does not finish within `secs`.
///
/// The two budget tests below timed themselves with an `Instant::elapsed()` assertion placed
/// *after* the call they were timing. That assertion can only fire once the call returns — so on
/// the exact failure it names, a refusal that never comes because the process is busy growing, it
/// never runs at all. The test does not go red; it hangs. Removing the budget charge made one of
/// them run 26 minutes of CPU without returning. In CI that is a job that burns its whole time
/// budget and reports a timeout, which is not a failure anyone can read back to a cause.
fn within<T: Send + 'static>(secs: u64, what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(std::time::Duration::from_secs(secs)) {
        Ok(v) => v,
        // The worker is left running; the test process is failing and about to exit anyway.
        Err(_) => panic!(
            "{what} did not finish within {secs}s — the bound this test exists to prove is gone"
        ),
    }
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
    veridex_demo::mcap::write(&good, "av").expect("write the demo rig recording");

    let bytes = std::fs::read(&good).unwrap();
    // The clean file still ingests, so the new pre-pass is not refusing valid chunks.
    default_registry()
        .ingest(&Source::Local(good.clone()), &IngestOptions::default())
        .expect("a well-formed chunked MCAP still ingests");

    // A byte inside the first chunk's *compressed payload*, past the region the chunk CRC covers in
    // a way the reader notices. Located by walking the record framing rather than hard-coded: the
    // offset was, and a change to the demo's message bodies moved the chunk and left this test
    // patching a byte that no longer meant anything.
    //
    // Framing past the 8-byte magic is `opcode: u8, len: u64le, payload`. A Chunk's payload is
    // `start(8) end(8) uncompressed_size(8) uncompressed_crc(4) compression(4+n) records_len(8)`,
    // then the compressed records themselves.
    let compressed_byte = {
        let mut at = 8usize;
        let mut found = None;
        while at + 9 <= bytes.len() {
            let len = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap()) as usize;
            if bytes[at] == 0x06 {
                let compression_len =
                    u32::from_le_bytes(bytes[at + 9 + 28..at + 9 + 32].try_into().unwrap())
                        as usize;
                let records_at = at + 9 + 28 + 4 + compression_len + 8;
                // Well inside the compressed stream, not at its first byte, so the damage is in the
                // middle of a frame rather than in its header.
                found = Some(records_at + 32);
                break;
            }
            at += 9 + len;
        }
        found.expect("the demo log is chunked; this test needs that")
    };
    let mut patched = bytes.clone();
    patched[compressed_byte] ^= 0x5A;
    let bad = dir.path().join("bad.mcap");
    std::fs::write(&bad, &patched).unwrap();

    let err = within(30, "the refusal", move || {
        default_registry()
            .ingest(&Source::Local(bad), &IngestOptions::default())
            .expect_err("a chunk whose stream outruns its own declaration must be refused")
    });
    match err {
        IngestError::Parse { message, .. } => assert!(
            message.contains("chunk") && (message.contains("corrupt") || message.contains("more")),
            "got {message}"
        ),
        other => panic!("expected a named parse error, got {other:?}"),
    }
}

/// The declared-bytes charge must reach chunks the *reader* never got to.
///
/// The charge was the sum `read_records` collected, and that reader's loop stops at the first
/// record it cannot parse. So a file with one malformed record early on charged the budget nothing
/// for everything behind it, while `validate_chunks` — which walks the record framing directly and
/// so does reach those chunks — went on to drain each up to its own self-declared size, unbilled.
/// Measured at 16 GB over two seconds from a single flipped byte; the expansion is linear in a
/// number the file chooses, so a 500 MB input buys roughly half an hour of CPU.
#[test]
fn the_ambiguous_tf_demo_rig_differs_from_the_healthy_one_by_exactly_the_ambiguity() {
    // The demo variant the quickstart documents, through the real adapter. `av-ambiguous-tf` is the
    // `av` rig with a second broadcaster claiming `lidar_top` from a `lidar_mount` that is itself on
    // `base_link` — so the frame graph stays one connected component and the LiDAR still reaches the
    // camera, and every check that reads the graph undirected still passes. The two rigs must differ
    // by `AUTONOMY.CALIBRATION_AMBIGUOUS` and by nothing else, or the demo is not showing what the
    // documentation says it shows.
    let dir = tempfile::tempdir().unwrap();
    let codes = |variant: &str| -> Vec<String> {
        let path = dir.path().join(format!("{variant}.mcap"));
        veridex_demo::mcap::write(&path, variant).expect("write the demo rig recording");
        let d = ingest_rig(&path);
        let engine = veridex_core::checks::default_engine().unwrap();
        let hash = veridex_core::content_hash(&d);
        let mut out: Vec<String> = engine
            .run(&d, hash, &veridex_core::RunConfig::default())
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect();
        out.sort();
        out
    };
    let healthy = codes("av");
    let ambiguous = codes("av-ambiguous-tf");
    assert!(
        !healthy.contains(&"AUTONOMY.CALIBRATION_AMBIGUOUS".to_string()),
        "the healthy rig must not raise it: {healthy:?}"
    );
    let only_in_ambiguous: Vec<&String> =
        ambiguous.iter().filter(|c| !healthy.contains(c)).collect();
    assert_eq!(
        only_in_ambiguous,
        vec![&"AUTONOMY.CALIBRATION_AMBIGUOUS".to_string()],
        "the second parent is the only difference: {ambiguous:?}"
    );
    assert!(
        !ambiguous
            .iter()
            .any(|c| c.starts_with("AUTONOMY.SENSOR_FRAME")),
        "and the per-sensor frame check still passes — undirected reachability is unchanged: \
         {ambiguous:?}"
    );
}

#[test]
fn a_chunk_behind_a_malformed_record_is_still_charged_to_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.mcap");
    veridex_demo::mcap::write(&good, "av").expect("write the demo rig recording");

    // Overstate the first chunk's uncompressed size so it exceeds the budget's 64 MB floor, which
    // otherwise swallows anything a 7.7 KB demo log can declare. Record framing past the 8-byte
    // magic is `opcode: u8, len: u64le, payload`; a Chunk's payload holds its uncompressed size at
    // byte 16.
    let mut bytes = std::fs::read(&good).unwrap();
    let mut at = 8usize;
    let mut patched_chunk = false;
    while at + 9 <= bytes.len() {
        let len = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap()) as usize;
        if bytes[at] == 0x06 {
            let size_at = at + 9 + 16;
            bytes[size_at..size_at + 8].copy_from_slice(&(1u64 << 40).to_le_bytes());
            patched_chunk = true;
            break;
        }
        at += 9 + len;
    }
    assert!(
        patched_chunk,
        "the demo log is chunked; this test needs that"
    );

    // With the framing intact the reader reaches the chunk, so this was always refused.
    let overstated = dir.path().join("overstated.mcap");
    std::fs::write(&overstated, &bytes).unwrap();
    let err = default_registry()
        .ingest(&Source::Local(overstated), &IngestOptions::default())
        .expect_err("a chunk declaring a terabyte is over the budget");
    assert!(
        matches!(err, IngestError::DecompressionBudgetExceeded { format_id, .. } if format_id == "mcap"),
        "got {err:?}"
    );

    // Now break the file magic. `LinearReader::new` refuses the file outright, so `read_records`
    // returns its default and the declared sum it collected is zero — while `validate_chunks`,
    // which starts at byte 8 and walks the framing itself, still reaches that chunk. (An unknown
    // *opcode* would not do: the MCAP spec says skip unknown records, and the reader does.)
    bytes[3] = 0xFE;
    let hidden = dir.path().join("hidden.mcap");
    std::fs::write(&hidden, &bytes).unwrap();

    let err = within(30, "the refusal", move || {
        default_registry()
            .ingest(&Source::Local(hidden), &IngestOptions::default())
            .expect_err("a malformed leading record must not buy the chunks behind it a free pass")
    });
    assert!(
        matches!(err, IngestError::DecompressionBudgetExceeded { .. }),
        "the chunk behind the malformed record must still be charged, got {err:?}"
    );
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

/// A record length prefix hidden *inside* a compressed chunk aborted the whole process.
///
/// The outer framing walk refuses a record claiming more bytes than the file holds, because the
/// `mcap` reader sizes a buffer from that number before checking the bytes exist. The same number
/// inside a chunk was read from decompressed bytes and bounded by nothing: of the crate's reader
/// constructors, only `LinearReader`'s sets `with_record_length_limit`, and the chain Veridex reads
/// messages through — `MessageStream` → `RawMessageStream` → `ChunkFlattener` — is the one that
/// omits it. So the length reached `RwBuf::reserve_exact` unchecked and the allocator aborted.
///
/// This file is 182 bytes and its chunk header is *honest* — 76 declared uncompressed bytes, which
/// passes the decompression budget and the declared-size agreement check. Only the framing inside
/// is a lie. The abort happened with `--max-frames 1 --max-decompression-ratio 1` set, before any
/// budget was consulted: the process died with no finding, no exit code a CI gate could read, and
/// nothing said about the file that did it.
#[test]
fn a_record_length_inside_a_chunk_cannot_abort_the_run() {
    fn put_u64(v: &mut Vec<u8>, x: u64) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn put_u32(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn rec(out: &mut Vec<u8>, op: u8, data: &[u8]) {
        out.push(op);
        put_u64(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    // The chunk's contents: a Schema and Channel that parse, then a Message whose length prefix
    // claims ~117 TB. The bytes that follow it are 16 bytes of nothing — the reader allocates from
    // the prefix long before it discovers that.
    let mut inner = Vec::new();
    {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_le_bytes());
        put_u32(&mut d, 1);
        d.push(b's');
        put_u32(&mut d, 0);
        put_u32(&mut d, 0);
        rec(&mut inner, 0x03, &d);
    }
    {
        let mut d = Vec::new();
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&1u16.to_le_bytes());
        put_u32(&mut d, 2);
        d.extend_from_slice(b"/t");
        put_u32(&mut d, 0);
        put_u32(&mut d, 0);
        rec(&mut inner, 0x04, &d);
    }
    inner.push(0x05);
    put_u64(&mut inner, 117_647_744_172_064);
    inner.extend_from_slice(&[0u8; 16]);

    let uncompressed_size = inner.len() as u64;
    let compressed = zstd::stream::encode_all(&inner[..], 3).unwrap();

    let mut chunk = Vec::new();
    put_u64(&mut chunk, 0);
    put_u64(&mut chunk, 0);
    put_u64(&mut chunk, uncompressed_size); // honest, and small enough to pass every budget
    put_u32(&mut chunk, 0);
    put_u32(&mut chunk, 4);
    chunk.extend_from_slice(b"zstd");
    put_u64(&mut chunk, compressed.len() as u64);
    chunk.extend_from_slice(&compressed);

    let mut f = b"\x89MCAP0\r\n".to_vec();
    {
        let mut d = Vec::new();
        put_u32(&mut d, 0);
        put_u32(&mut d, 0);
        rec(&mut f, 0x01, &d);
    }
    rec(&mut f, 0x06, &chunk);
    {
        let mut d = Vec::new();
        put_u32(&mut d, 0);
        rec(&mut f, 0x0f, &d);
    }
    {
        let mut d = Vec::new();
        put_u64(&mut d, 0);
        put_u64(&mut d, 0);
        put_u32(&mut d, 0);
        rec(&mut f, 0x02, &d);
    }
    f.extend_from_slice(b"\x89MCAP0\r\n");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bomb.mcap");
    fs::write(&path, &f).unwrap();

    let err = McapAdapter
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect_err("a record longer than the chunk that holds it is corrupt framing");
    assert!(
        err.to_string().contains("framing is corrupt"),
        "the refusal must name the corrupt framing rather than abort: {err}"
    );
}

#[test]
fn an_mcap_channel_declaring_latched_qos_is_read_the_same_as_a_db3_one() {
    // rosbag2 writes bags through two storage plugins, and carries each publisher's QoS either way:
    // in a `.db3`'s `topics.offered_qos_profiles` column, and on an MCAP channel's metadata. Reading
    // it from one and not the other would make which plugin a team picked change the verdict — a
    // latched transform tree drawing `STRUCTURAL.SINGLE_FRAME_STREAM` and `TEMPORAL.END_OFFSET` in
    // MCAP and not in SQLite, for the same recording.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("qos.mcap");
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(std::io::Cursor::new(&mut out)).unwrap();
        let sid = w
            .add_schema("tf2_msgs/msg/TFMessage", "ros2msg", b"")
            .unwrap();
        let latched: std::collections::BTreeMap<String, String> = [(
            "offered_qos_profiles".to_string(),
            "- history: 3\n  depth: 1\n  reliability: 1\n  durability: 1\n".to_string(),
        )]
        .into_iter()
        .collect();
        let cid = w.add_channel(sid, "/tf_static", "cdr", &latched).unwrap();

        let sid2 = w.add_schema("sensor_msgs/msg/Imu", "ros2msg", b"").unwrap();
        let cid2 = w
            .add_channel(sid2, "/imu/data", "cdr", &std::collections::BTreeMap::new())
            .unwrap();
        w.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: cid,
                sequence: 0,
                log_time: 0,
                publish_time: 0,
            },
            b"x",
        )
        .unwrap();
        for (seq, t) in [0u64, 10_000_000, 20_000_000].iter().enumerate() {
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: cid2,
                    sequence: seq as u32,
                    log_time: *t,
                    publish_time: *t,
                },
                b"y",
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    std::fs::write(&path, &out).unwrap();

    let d = veridex_core::adapter::mcap::McapAdapter
        .ingest(
            &veridex_core::adapter::Source::Local(path),
            &veridex_core::adapter::IngestOptions::default(),
        )
        .expect("mcap ingest")
        .dataset;
    let by = |n: &str| {
        d.episodes[0]
            .streams
            .iter()
            .find(|s| s.name == n)
            .unwrap_or_else(|| panic!("stream {n}"))
    };
    assert_eq!(by("/tf_static").latched, Some(true));
    // A channel with no QoS metadata says nothing, and nothing is inferred for it.
    assert_eq!(by("/imu/data").latched, None);
}

// ---- Reading the summary section alone ----

fn metadata_only() -> IngestOptions {
    IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    }
}

fn ingest_with(path: &std::path::Path, options: &IngestOptions) -> veridex_core::Ingested {
    default_registry()
        .ingest(&Source::Local(path.to_path_buf()), options)
        .unwrap_or_else(|e| panic!("ingests: {e}"))
}

fn rig() -> Vec<Chan> {
    vec![
        Chan {
            schema: "sensor_msgs/msg/Image",
            topic: "/camera/image",
            times: vec![0, 100_000_000, 200_000_000],
        },
        Chan {
            schema: "sensor_msgs/msg/PointCloud2",
            topic: "/lidar/points",
            times: vec![0, 100_000_000],
        },
    ]
}

#[test]
fn the_summary_alone_yields_the_topic_inventory_a_full_read_finds() {
    // An MCAP writes its own index at the end of the file. What it declares there — the channels,
    // their schemas, and the message totals — is what a full read finds, and reading it costs three
    // seeks rather than the whole recording.
    let path = write_temp_mcap(&build_mcap(&rig()));
    let summary = ingest_with(&path, &metadata_only());
    let full = ingest_with(&path, &IngestOptions::default());

    assert_eq!(
        summary.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: 1
        }
    );
    let names = |i: &veridex_core::Ingested| -> Vec<String> {
        i.dataset.episodes[0]
            .streams
            .iter()
            .map(|s| s.name.clone())
            .collect()
    };
    assert_eq!(names(&summary), names(&full));
    let modalities = |i: &veridex_core::Ingested| -> Vec<Modality> {
        i.dataset.episodes[0]
            .streams
            .iter()
            .map(|s| s.modality)
            .collect()
    };
    assert_eq!(modalities(&summary), modalities(&full));
    // Nothing that lives in a message survives, because no message was read.
    assert!(summary.dataset.episodes[0]
        .streams
        .iter()
        .all(|s| s.frames.is_empty()));
    assert_eq!(summary.dataset.episodes[0].start_ts, None);
    // The declared total is the file's own claim, recorded and disclosed rather than left implicit.
    assert!(
        summary
            .dataset
            .metadata
            .iter()
            .any(|(k, v)| k == "mcap_message_count" && v == "5"),
        "{:?}",
        summary.dataset.metadata
    );
    assert!(
        summary
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.note.contains("were not read")),
        "{:?}",
        summary.report.unmapped_fields
    );
}

#[test]
fn a_summary_only_run_reads_none_of_the_recording() {
    // Proved rather than asserted: every byte between the header and the summary section is
    // overwritten. The summary-only run is unchanged — it never looks there — while a full read of
    // the same file no longer agrees with itself.
    // Written to one path, twice, because the dataset id comes from the file name: two temp files
    // would differ in the CDM for a reason that has nothing to do with what was read.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rig.mcap");
    let bytes = build_mcap(&rig());
    std::fs::write(&path, &bytes).unwrap();
    let expected =
        veridex_core::canonical::content_hash(&ingest_with(&path, &metadata_only()).dataset);

    let mut wrecked = bytes.clone();
    let summary_start = summary_start_of(&bytes) as usize;
    for byte in wrecked[64..summary_start].iter_mut() {
        *byte = 0x00;
    }
    std::fs::write(&path, &wrecked).unwrap();
    assert_eq!(
        veridex_core::canonical::content_hash(&ingest_with(&path, &metadata_only()).dataset),
        expected,
        "wrecking the records changed nothing, because none was read"
    );
    assert!(
        default_registry()
            .ingest(&Source::Local(path.clone()), &IngestOptions::default())
            .is_err(),
        "a full read of the same file must not be indifferent to those bytes"
    );
}

/// The `summary_start` offset out of a file's own footer.
fn summary_start_of(bytes: &[u8]) -> u64 {
    let footer_at = bytes.len() - 8 - 29;
    u64::from_le_bytes(bytes[footer_at + 9..footer_at + 17].try_into().unwrap())
}

#[test]
fn a_file_with_no_summary_section_is_refused_by_name() {
    // A streaming writer that never finalized writes `summary_start = 0`. Its topics exist only in
    // the records themselves, so there is nothing to read without reading the file — said plainly,
    // rather than reported as a recording with no topics.
    let mut bytes = build_mcap(&rig());
    let footer_at = bytes.len() - 8 - 29;
    bytes[footer_at + 9..footer_at + 17].copy_from_slice(&0u64.to_le_bytes());
    let path = write_temp_mcap(&bytes);

    match default_registry().ingest(&Source::Local(path.to_path_buf()), &metadata_only()) {
        Err(IngestError::NotImplemented { what, hint }) => {
            assert!(what.contains("no summary section"), "{what}");
            assert!(hint.contains("drop --metadata-only"), "{hint}");
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn a_footer_pointing_outside_the_file_is_refused_not_followed() {
    // The offset comes out of the file, so it is a stranger's number. One past the end must be a
    // refusal rather than a read.
    let mut bytes = build_mcap(&rig());
    let footer_at = bytes.len() - 8 - 29;
    bytes[footer_at + 9..footer_at + 17].copy_from_slice(&u64::MAX.to_le_bytes());
    let path = write_temp_mcap(&bytes);

    match default_registry().ingest(&Source::Local(path.to_path_buf()), &metadata_only()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("outside the file"), "{message}")
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

#[test]
fn a_summary_whose_counts_do_not_add_up_is_refused_not_reported() {
    // The inventory has to be whole, and the file's own total is what proves it. Presenting one
    // channel out of two as the recording's contents is invisible to the caller.
    let mut bytes = build_mcap(&rig());
    patch_message_count(&mut bytes, 9_999);
    let path = write_temp_mcap(&bytes);

    match default_registry().ingest(&Source::Local(path.to_path_buf()), &metadata_only()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("9999"), "{message}");
            assert!(
                message.contains("did not read the whole inventory"),
                "{message}"
            );
        }
        other => panic!("expected a refusal, got ok={}", other.is_ok()),
    }
}

/// Rewrite the total in the summary's Statistics record, leaving everything else as written.
fn patch_message_count(bytes: &mut [u8], total: u64) {
    let summary_start = summary_start_of(bytes) as usize;
    let footer_at = bytes.len() - 8 - 29;
    let mut at = summary_start;
    while at + 9 <= footer_at {
        let opcode = bytes[at];
        let len = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap()) as usize;
        if opcode == 0x0B {
            bytes[at + 9..at + 17].copy_from_slice(&total.to_le_bytes());
            return;
        }
        at += 9 + len;
    }
    panic!("the fixture has no Statistics record to patch");
}

#[test]
fn a_full_read_is_reconciled_against_the_total_the_file_declares() {
    // An MCAP closes with a count of what it holds. A file truncated after that record was written,
    // or one whose chunks this reader could not walk to the end of, yields fewer messages — and
    // reading it as a complete recording is the failure this tool exists to prevent.
    let sound = write_temp_mcap(&build_mcap(&rig()));
    let out = ingest_with(&sound, &IngestOptions::default());
    assert!(
        out.report.unread_sources.is_empty(),
        "a sound file must produce no shortfall: {:?}",
        out.report.unread_sources
    );

    let mut bytes = build_mcap(&rig());
    patch_message_count(&mut bytes, 500);
    let short = write_temp_mcap(&bytes);
    let out = ingest_with(&short, &IngestOptions::default());
    let note = out
        .report
        .unread_sources
        .iter()
        .find(|u| u.source_path.contains("message_count"))
        .unwrap_or_else(|| panic!("expected a shortfall: {:?}", out.report.unread_sources));
    assert!(note.note.contains("500"), "{}", note.note);
    assert!(note.note.contains("495 are missing"), "{}", note.note);
}

#[test]
fn a_summary_that_over_reads_is_a_wrong_total_not_a_coverage_hole() {
    // The other direction: every message present was read, and it is the summary that is wrong.
    // Said, but not as unread data — a coverage hole is about data nobody looked at.
    let mut bytes = build_mcap(&rig());
    patch_message_count(&mut bytes, 1);
    let path = write_temp_mcap(&bytes);
    let out = ingest_with(&path, &IngestOptions::default());
    assert!(out.report.unread_sources.is_empty());
    assert!(
        out.report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("disagreeing")),
        "{:?}",
        out.report.unmapped_fields
    );
}

#[test]
fn a_file_with_no_summary_says_the_reconciliation_could_not_run() {
    // A streaming writer legitimately omits the summary. That disables the check rather than
    // failing the read — and the report says so, because a check that silently did not run reads
    // exactly like one that passed.
    let mut bytes = build_mcap(&rig());
    let footer_at = bytes.len() - 8 - 29;
    bytes[footer_at + 9..footer_at + 17].copy_from_slice(&0u64.to_le_bytes());
    let path = write_temp_mcap(&bytes);
    let out = ingest_with(&path, &IngestOptions::default());
    assert!(
        out.report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("could not be reconciled")),
        "{:?}",
        out.report.unmapped_fields
    );
}

#[test]
fn a_summary_only_read_finds_the_provenance_a_full_read_finds() {
    // Provenance is 30% of the trust score, and a summary-only run that reported none of it would
    // be making a claim about the file rather than about the read. The summary indexes the Metadata
    // records and the attachments, so both are reachable without opening a chunk.
    let path = write_temp_mcap(&build_mcap_with_provenance());
    let summary = ingest_with(&path, &metadata_only());
    let full = ingest_with(&path, &IngestOptions::default());

    let elements = |i: &veridex_core::Ingested| -> Vec<String> {
        let mut out: Vec<String> = i
            .dataset
            .provenance
            .iter()
            .flat_map(|p| p.elements.iter())
            .map(|e| format!("{}={:?} {:?}", e.key, e.value, e.class))
            .collect();
        out.sort();
        out
    };
    assert_eq!(
        elements(&summary),
        elements(&full),
        "the summary indexes everything a full read maps provenance from"
    );
    // Including the attachment-name heuristic, which stays `Asserted` here as it is there.
    assert!(summary.dataset.provenance.iter().any(|p| p
        .elements
        .iter()
        .any(|e| e.key == "calibration" && e.class == ProvenanceClass::Asserted)));
    assert!(
        summary
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("MetadataIndex")),
        "{:?}",
        summary.report.mapped_fields
    );
}

#[test]
fn a_metadata_index_pointing_outside_the_file_is_skipped_not_followed() {
    // Every offset in the index is the file's own number. One that points past the end is skipped —
    // the rest of the index is still usable, and the topic inventory is legible either way — but it
    // is never followed.
    let mut bytes = build_mcap_with_provenance();
    let summary_start = summary_start_of(&bytes) as usize;
    let footer_at = bytes.len() - 8 - 29;
    let mut at = summary_start;
    let mut patched = false;
    while at + 9 <= footer_at {
        let opcode = bytes[at];
        let len = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().unwrap()) as usize;
        if opcode == 0x0D {
            bytes[at + 9..at + 17].copy_from_slice(&u64::MAX.to_le_bytes());
            patched = true;
            break;
        }
        at += 9 + len;
    }
    assert!(patched, "the fixture has a MetadataIndex record to patch");
    let path = write_temp_mcap(&bytes);

    let out = ingest_with(&path, &metadata_only());
    // The channels still read, and the provenance that came from the metadata record is simply
    // absent rather than invented.
    assert!(!out.dataset.episodes[0].streams.is_empty());
    assert!(!out
        .dataset
        .provenance
        .iter()
        .any(|p| p.elements.iter().any(|e| e.key == "license")));
}

// ---- JointState values ----

/// Build an MCAP where one channel carries a message per entry of `payloads`, one every 10 ms.
fn build_mcap_series(schema: &str, topic: &str, payloads: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let sid = w.add_schema(schema, "ros2msg", b"").unwrap();
        let cid = w.add_channel(sid, topic, "cdr", &BTreeMap::new()).unwrap();
        for (i, payload) in payloads.iter().enumerate() {
            let t = (i as u64) * 10_000_000;
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: cid,
                    sequence: i as u32,
                    log_time: t,
                    publish_time: t,
                },
                payload,
            )
            .unwrap();
        }
        w.finish().unwrap();
    }
    out
}

/// One `sensor_msgs/msg/JointState` body: named joints and their positions (velocity/effort empty).
fn joint_state_body(names: &[&str], positions: &[f64]) -> Vec<u8> {
    let mut c = Cdr::new();
    c.header("");
    c.u32(names.len() as u32);
    for n in names {
        c.string(n);
    }
    c.u32(positions.len() as u32);
    for &p in positions {
        c.f64(p);
    }
    c.u32(0); // velocity[]
    c.u32(0); // effort[]
    c.buf
}

#[test]
fn a_joint_pinned_at_its_limit_in_a_bag_is_caught_end_to_end() {
    // A 40-sample arm recording where the elbow sits hard against its stop for 30 of 40 samples —
    // a saturated actuator, the exact defect that trains a policy to command a limit it can never
    // leave. Before JointState positions were read, the MCAP payload was opaque: every statistical
    // check abstained and this recording scored a clean 100.
    let payloads: Vec<Vec<u8>> = (0..40)
        .map(|i: i32| {
            let shoulder = i as f64 * 0.01;
            let elbow = if i < 30 { 2.0 } else { 2.0 - (i as f64) * 0.01 };
            joint_state_body(&["shoulder", "elbow"], &[shoulder, elbow])
        })
        .collect();
    let bytes = build_mcap_series("sensor_msgs/msg/JointState", "/joint_states", &payloads);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let stream = &ingested.dataset.episodes[0].streams[0];
    let stats = stream
        .observed_stats
        .expect("joint positions were measured");
    assert_eq!(stats.min, 0.0, "shoulder starts at 0");
    let sat = stream.observed_saturation.expect("saturation was measured");
    assert_eq!(
        sat.dim, 1,
        "the elbow, not the shoulder, is the pinned axis"
    );
    assert_eq!(sat.at_max, 30);
    assert_eq!(stream.observed_non_finite, Some(0));

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "STATISTICAL.SATURATED"),
        "a pinned joint must be reported, not abstained on: {:?}",
        verdict.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
    // And the stream is no longer counted among the ones whose values went unread.
    assert!(
        !verdict
            .findings
            .iter()
            .any(|f| f.code == "STATISTICAL.UNMEASURED_VALUES"),
        "the only stream in this bag was measured"
    );
}

#[test]
fn a_topic_whose_payload_stays_opaque_is_still_reported_unmeasured() {
    // Reading JointState must not turn into a claim about every other topic. A bag carrying an
    // arm beside a camera measures the arm and says so about the camera.
    let arm: Vec<Vec<u8>> = (0..40)
        .map(|i: i32| joint_state_body(&["elbow"], &[i as f64 * 0.01]))
        .collect();
    let mut bytes = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut bytes)).expect("writer");
        let js = w
            .add_schema("sensor_msgs/msg/JointState", "ros2msg", b"")
            .unwrap();
        let jc = w
            .add_channel(js, "/joint_states", "cdr", &BTreeMap::new())
            .unwrap();
        let im = w
            .add_schema("sensor_msgs/msg/Image", "ros2msg", b"")
            .unwrap();
        let ic = w.add_channel(im, "/cam", "cdr", &BTreeMap::new()).unwrap();
        for (i, payload) in arm.iter().enumerate() {
            let t = (i as u64) * 10_000_000;
            for (cid, data) in [(jc, payload.as_slice()), (ic, b"opaque-image".as_slice())] {
                w.write_to_known_channel(
                    &mcap::records::MessageHeader {
                        channel_id: cid,
                        sequence: i as u32,
                        log_time: t,
                        publish_time: t,
                    },
                    data,
                )
                .unwrap();
            }
        }
        w.finish().unwrap();
    }
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
    let unmeasured = verdict
        .findings
        .iter()
        .find(|f| f.code == "STATISTICAL.UNMEASURED_VALUES")
        .expect("the camera's payload is still opaque");
    assert!(
        unmeasured.message.contains("/cam") && !unmeasured.message.contains("/joint_states"),
        "only the opaque topic should be named: {}",
        unmeasured.message
    );
}

/// One `sensor_msgs/msg/Imu` body: orientation is declared absent (`covariance[0] = -1`), as a bare
/// gyro/accelerometer publishes it; angular velocity and linear acceleration are provided.
fn imu_body(angular: [f64; 3], linear: [f64; 3]) -> Vec<u8> {
    let mut c = Cdr::new();
    c.header("imu_link");
    for _ in 0..4 {
        c.f64(0.0); // orientation, zero-filled
    }
    c.f64(-1.0); // orientation_covariance[0]: not provided
    for _ in 0..8 {
        c.f64(0.0);
    }
    for v in angular {
        c.f64(v);
    }
    for _ in 0..9 {
        c.f64(0.0);
    }
    for v in linear {
        c.f64(v);
    }
    for _ in 0..9 {
        c.f64(0.0);
    }
    c.buf
}

#[test]
fn an_accelerometer_clipping_at_its_rail_is_caught_end_to_end() {
    // A ±16 g accelerometer railed at 156.9 m/s² for 30 of 40 samples: the sensor is reporting its
    // own limit, not the world, and every measurement above it is lost. Nothing about the
    // recording's structure or timing says so — only the values.
    let payloads: Vec<Vec<u8>> = (0..40)
        .map(|i: i32| {
            let z = if i < 30 { 156.9 } else { 9.81 + i as f64 * 0.1 };
            imu_body([0.01 * i as f64, 0.0, 0.0], [0.0, 0.0, z])
        })
        .collect();
    let bytes = build_mcap_series("sensor_msgs/msg/Imu", "/imu/data", &payloads);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let stream = &ingested.dataset.episodes[0].streams[0];
    let sat = stream
        .observed_saturation
        .expect("imu values were measured");
    assert_eq!(sat.dim, 9, "the z accelerometer axis is dimension 9");
    assert_eq!(sat.at_max, 30);

    // The orientation the driver declared absent was held out, so it contributes no dimension —
    // ten slots, of which only the six provided ones carry statistics.
    let dims = stream
        .observed_dim_stats
        .as_ref()
        .expect("per-dimension statistics");
    assert!(
        dims.iter().all(|d| d.dim >= 4),
        "a quaternion the IMU never published must not be summarized: {dims:?}"
    );

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "STATISTICAL.SATURATED"),
        "a railed accelerometer must reach the verdict: {:?}",
        verdict.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}

#[test]
fn a_topic_that_reorders_its_joints_is_refused_rather_than_mismeasured() {
    // `JointState` guarantees only that `position[i]` belongs to `name[i]` *in that message*.
    // Nothing says two messages order their joints alike, and a publisher aggregating several
    // sources is exactly where they might not. Accumulating positionally across a reordering folds
    // two joints into one dimension — a statistic for a joint that does not exist, named after
    // whichever came first. Veridex declines the stream and says so instead.
    let payloads: Vec<Vec<u8>> = (0..40)
        .map(|i: i32| {
            let (a, b) = (i as f64 * 0.01, 2.0);
            // Halfway through, the publisher swaps the two joints round.
            if i < 20 {
                joint_state_body(&["shoulder", "elbow"], &[a, b])
            } else {
                joint_state_body(&["elbow", "shoulder"], &[b, a])
            }
        })
        .collect();
    let bytes = build_mcap_series("sensor_msgs/msg/JointState", "/joint_states", &payloads);
    let path = write_temp_mcap(&bytes);
    let ingested = McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");

    let stream = &ingested.dataset.episodes[0].streams[0];
    assert_eq!(stream.observed_stats, None, "nothing was summarized");
    assert_eq!(stream.observed_saturation, None);
    assert_eq!(stream.dim_names, None);
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.source_path == "/joint_states" && u.note.contains("joint set")),
        "the refusal must be disclosed: {:?}",
        ingested.report.unread_sources
    );

    // And it reaches the verdict, where a reader will actually meet it.
    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run_over_with_unread(
        &ingested.dataset,
        hash,
        &veridex_core::RunConfig::default(),
        veridex_core::CoverageNote::Full,
        &ingested.report.unread_sources,
    );
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "COVERAGE.SOURCE_UNREAD"),
        "{:?}",
        verdict.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}

#[test]
fn a_gnss_stream_is_measured_rather_than_only_fingerprinted() {
    // `NavSatFix` was the one AV message body the CDR decoder did not read, so a rig's GNSS stream
    // was fingerprinted and every statistical check abstained on it: a receiver frozen at one fix,
    // publishing NaNs, or railed at a coordinate limit reported nothing at all, while the same
    // faults on the IMU beside it were caught.
    let dir = tempfile::tempdir().unwrap();
    let rig = dir.path().join("av.mcap");
    veridex_demo::mcap::write(&rig, "av").expect("write the demo rig");
    let ingested = default_registry()
        .ingest(&Source::Local(rig), &IngestOptions::default())
        .expect("the rig ingests");

    let gnss = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/gps/fix")
        .expect("the GNSS stream");
    let stats = gnss
        .observed_stats
        .expect("latitude/longitude/altitude are decoded and summarized");
    // The demo drives north from ~37.4°N, so latitude is the moving one and the summary spans it.
    assert!(
        stats.min >= -180.0 && stats.max <= 180.0,
        "coordinates are in range: {stats:?}"
    );
    assert!(
        stats.max > stats.min,
        "the fix moves, so a frozen receiver would look different: {stats:?}"
    );
    // Per dimension, so a fault in longitude alone is visible rather than averaged away.
    let dims = gnss
        .observed_dim_stats
        .as_ref()
        .expect("per-dimension statistics");
    assert_eq!(dims.len(), 3, "latitude, longitude, altitude");

    // And the check family no longer abstains on it.
    let hash = veridex_core::content_hash(&ingested.dataset);
    let engine = veridex_core::checks::default_engine().unwrap();
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    let unmeasured = verdict
        .findings
        .iter()
        .find(|f| f.code == "STATISTICAL.UNMEASURED_VALUES")
        .map(|f| f.message.clone())
        .unwrap_or_default();
    assert!(
        !unmeasured.contains("/gps/fix"),
        "the GNSS stream is measured now: {unmeasured}"
    );
    assert!(
        !unmeasured.contains("/imu/data"),
        "and so is the IMU the demo exists to show drifting: {unmeasured}"
    );
}
