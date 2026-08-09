//! Behavior tests for the MCAP adapter, driven by real MCAP files written with the `mcap` crate.

use std::collections::BTreeMap;
use std::io::Cursor;

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{Adapter, Coverage, Detection, IngestOptions, Source};
use veridex_core::cdm::Modality;

/// One channel's messages: (schema_name, topic, message_encoding, log_times_ns).
struct Chan {
    schema: &'static str,
    topic: &'static str,
    times: Vec<u64>,
}

/// Build an in-memory MCAP file from the given channels and return its bytes.
fn build_mcap(channels: &[Chan]) -> Vec<u8> {
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
                        b"payload",
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
