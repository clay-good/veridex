//! The ROS 1 rosbag (`.bag`) reader, against bags built here byte by byte.
//!
//! Every bag in this file is assembled from the format's own record framing rather than by a
//! library, for the reason the `.db3` fixtures come from Python's `sqlite3`: a reader tested only
//! against a writer from the same head proves the two agree, not that either matches the format.

use std::io::Write;

use veridex_core::adapter::rosbag1::Rosbag1Adapter;
use veridex_core::adapter::{default_registry, Adapter, IngestOptions, Source};
use veridex_core::cdm::Modality;

/// One `header_len | header | data_len | data` record.
fn record(fields: &[(&str, Vec<u8>)], data: &[u8]) -> Vec<u8> {
    let mut header = Vec::new();
    for (name, value) in fields {
        let mut field = name.as_bytes().to_vec();
        field.push(b'=');
        field.extend_from_slice(value);
        header.extend_from_slice(&(field.len() as u32).to_le_bytes());
        header.extend_from_slice(&field);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A ROS 1 `time` header value: seconds then nanoseconds, both `u32` little-endian.
fn ros_time(ns: u64) -> Vec<u8> {
    let mut v = ((ns / 1_000_000_000) as u32).to_le_bytes().to_vec();
    v.extend_from_slice(&((ns % 1_000_000_000) as u32).to_le_bytes());
    v
}

/// A connection record for `conn`, declaring `topic` of ROS type `ros_type`.
fn connection(conn: u32, topic: &str, ros_type: &str, latching: bool) -> Vec<u8> {
    let mut inner: Vec<(&str, Vec<u8>)> = vec![
        ("topic", topic.as_bytes().to_vec()),
        ("type", ros_type.as_bytes().to_vec()),
        ("md5sum", b"0123456789abcdef0123456789abcdef".to_vec()),
        ("message_definition", b"# empty\n".to_vec()),
    ];
    if latching {
        inner.push(("latching", b"1".to_vec()));
    }
    // The connection's *data* is itself a header block.
    let mut data = Vec::new();
    for (name, value) in &inner {
        let mut field = name.as_bytes().to_vec();
        field.push(b'=');
        field.extend_from_slice(value);
        data.extend_from_slice(&(field.len() as u32).to_le_bytes());
        data.extend_from_slice(&field);
    }
    record(
        &[
            ("op", vec![0x07]),
            ("conn", conn.to_le_bytes().to_vec()),
            ("topic", topic.as_bytes().to_vec()),
        ],
        &data,
    )
}

fn message(conn: u32, ns: u64, body: &[u8]) -> Vec<u8> {
    record(
        &[
            ("op", vec![0x02]),
            ("conn", conn.to_le_bytes().to_vec()),
            ("time", ros_time(ns)),
        ],
        body,
    )
}

/// Wrap `inner` records in a chunk record with the given compression name.
fn chunk(compression: &str, inner: &[u8], payload: &[u8]) -> Vec<u8> {
    record(
        &[
            ("op", vec![0x05]),
            ("compression", compression.as_bytes().to_vec()),
            ("size", (inner.len() as u32).to_le_bytes().to_vec()),
        ],
        payload,
    )
}

/// A whole bag: the magic line, a bag header naming its recorder, then `body`.
fn bag(body: &[u8]) -> Vec<u8> {
    let mut out = b"#ROSBAG V2.0\n".to_vec();
    out.extend_from_slice(&record(
        &[
            ("op", vec![0x03]),
            ("conn_count", 1u32.to_le_bytes().to_vec()),
            ("chunk_count", 1u32.to_le_bytes().to_vec()),
            ("index_pos", 0u64.to_le_bytes().to_vec()),
            ("callerid", b"/rosbag_record".to_vec()),
        ],
        &[],
    ));
    out.extend_from_slice(body);
    out
}

/// A rig's worth of records: a LiDAR, an IMU and a latched transform tree.
fn rig_records() -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&connection(
        0,
        "/lidar/points",
        "sensor_msgs/PointCloud2",
        false,
    ));
    inner.extend_from_slice(&connection(1, "/imu/data", "sensor_msgs/Imu", false));
    inner.extend_from_slice(&connection(2, "/tf_static", "tf2_msgs/TFMessage", true));
    for i in 0..10u64 {
        inner.extend_from_slice(&message(0, 1_000_000_000 + i * 100_000_000, &[i as u8; 24]));
        inner.extend_from_slice(&message(1, 1_000_000_000 + i * 10_000_000, &[i as u8; 8]));
    }
    inner.extend_from_slice(&message(2, 1_000_000_000, b"tf"));
    inner
}

fn write_temp(bytes: &[u8]) -> tempfile::TempPath {
    let mut f = tempfile::Builder::new()
        .suffix(".bag")
        .tempfile()
        .expect("temp file");
    f.write_all(bytes).expect("write bag");
    f.flush().expect("flush");
    f.into_temp_path()
}

fn ingest(bytes: &[u8]) -> veridex_core::adapter::Ingested {
    let path = write_temp(bytes);
    Rosbag1Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("the bag ingests")
}

#[test]
fn a_bag_is_detected_by_its_magic_not_its_extension() {
    // By content: a `.bag` is claimed because it says it is one, and a file named `.bag` that is
    // something else is left to whichever reader really owns it.
    let good = write_temp(&bag(&chunk("none", &rig_records(), &rig_records())));
    assert!(matches!(
        Rosbag1Adapter.detect(&Source::Local(good.to_path_buf())),
        veridex_core::adapter::Detection::Yes { .. }
    ));

    let mut impostor = tempfile::Builder::new().suffix(".bag").tempfile().unwrap();
    impostor
        .write_all(b"not a bag at all, despite the name")
        .unwrap();
    impostor.flush().unwrap();
    assert!(matches!(
        Rosbag1Adapter.detect(&Source::Local(impostor.path().to_path_buf())),
        veridex_core::adapter::Detection::No
    ));
}

#[test]
fn topics_become_streams_typed_by_their_ros_type() {
    let inner = rig_records();
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let ep = &ingested.dataset.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["/imu/data", "/lidar/points", "/tf_static"]);

    // The same classifier the two ROS 2 readers use, over the same type names — so a rig recorded
    // to a bag types the way the same rig recorded to an MCAP does.
    let by = |n: &str| ep.streams.iter().find(|s| s.name == n).unwrap();
    assert_eq!(by("/lidar/points").modality, Modality::PointCloud);
    assert_eq!(by("/imu/data").modality, Modality::Imu);
    assert_eq!(by("/tf_static").modality, Modality::Calibration);
    // A latched topic is published once and retained, which several checks abstain on.
    assert_eq!(by("/tf_static").latched, Some(true));
}

#[test]
fn messages_become_frames_on_the_recorders_clock() {
    let inner = rig_records();
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let lidar = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "/lidar/points")
        .unwrap();
    assert_eq!(lidar.frames.len(), 10);
    assert_eq!(lidar.frames[0].ts, 1_000_000_000);
    assert_eq!(lidar.frames[9].ts, 1_900_000_000);
    assert_eq!(lidar.clock_id, "rosbag1-log");

    // Each body is fingerprinted, so the CDM hash tracks content the reader never interprets — and
    // two messages that differ do not fingerprint alike.
    assert!(lidar
        .frames
        .iter()
        .all(|f| f.value_ref.content_hash.is_some()));
    assert_ne!(
        lidar.frames[0].value_ref.content_hash,
        lidar.frames[1].value_ref.content_hash
    );
    assert_eq!(ingested.dataset.episodes[0].start_ts, Some(1_000_000_000));
}

#[test]
fn the_bag_header_supplies_the_recorder_it_names() {
    let inner = rig_records();
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let recorder = ingested.dataset.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "recorder")
        .and_then(|e| e.value.clone());
    assert_eq!(recorder.as_deref(), Some("/rosbag_record"));
}

#[test]
fn an_lz4_chunk_is_read_and_a_bz2_one_is_disclosed() {
    // lz4 is what `rosbag record --lz4` writes, and it is read.
    let inner = rig_records();
    let mut packed = Vec::new();
    {
        let mut enc = lz4_flex::frame::FrameEncoder::new(&mut packed);
        enc.write_all(&inner).unwrap();
        enc.finish().unwrap();
    }
    let ingested = ingest(&bag(&chunk("lz4", &inner, &packed)));
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 3);

    // bz2 is `rosbag compress`'s default, and this workspace carries no decompressor for it. The
    // messages are in the file and nobody read them, which is a coverage hole rather than a shape
    // the CDM cannot hold — so it is disclosed as unread, not skipped in silence.
    let mut body = chunk("none", &inner, &inner);
    body.extend_from_slice(&chunk("bz2", &inner, b"BZh9compressed"));
    let ingested = ingest(&bag(&body));
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("`bz2` is not decompressed")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_message_naming_a_connection_the_bag_never_declared_is_reported() {
    // The topic and type those messages belong to are unknown, so they cannot be counted into a
    // stream — and counting them into one anyway would attribute frames to a topic nothing names.
    let mut inner = rig_records();
    inner.extend_from_slice(&message(99, 1_500_000_000, b"orphan"));
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 3);
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("no connection record declares")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_chunk_cut_short_keeps_the_records_before_the_cut_and_says_so() {
    // A recorder killed mid-write leaves a chunk whose record stream ends in a fragment too short
    // to be a record. The records before it are what the file said, and refusing the whole bag
    // would throw them away — so they are kept and the shortfall is disclosed.
    let inner = rig_records();
    let cut = &inner[..inner.len() - 3];
    let ingested = ingest(&bag(&chunk("none", &inner, cut)));
    // The cut lands inside the last record — the latched `/tf_static` message — so its topic is
    // lost with it, and the two written whole before it survive. Records before the cut are what
    // the file said; nothing after it is invented.
    let names: Vec<&str> = ingested.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, vec!["/imu/data", "/lidar/points"]);
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("truncated")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_bag_whose_only_chunk_is_unreadable_is_refused_rather_than_reported_empty() {
    // Cutting the *outer* record leaves a chunk whose data is not all there, so nothing inside it
    // can be located. A dataset with no streams is not a dataset that was checked, and returning
    // one would be a clean verdict over a file nobody read.
    let inner = rig_records();
    let mut whole = bag(&chunk("none", &inner, &inner));
    whole.truncate(whole.len() - 3);
    let path = write_temp(&whole);
    let err = Rosbag1Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a bag nothing could be read from is refused");
    assert!(
        format!("{err}").contains("no topic"),
        "the refusal names why: {err}"
    );
}

#[test]
fn the_registry_autodetects_a_bag() {
    let inner = rig_records();
    let path = write_temp(&bag(&chunk("none", &inner, &inner)));
    let ingested = default_registry()
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("the registry ingests a bag");
    assert_eq!(ingested.report.format_id, "rosbag1");
    assert_eq!(ingested.report.source_version.as_deref(), Some("2.0"));
}

#[test]
fn a_corrupted_or_hostile_bag_errors_or_yields_nothing_but_never_panics() {
    // Every length in a bag is a number the file chose. Flip bytes through a well-formed one and
    // require each result to be a verdict or an error — never a panic, and never an allocation the
    // file talked this reader into.
    let whole = bag(&chunk("none", &rig_records(), &rig_records()));
    for i in (0..whole.len()).step_by(7) {
        for mask in [0xff, 0x01, 0x80] {
            let mut broken = whole.clone();
            broken[i] ^= mask;
            let path = write_temp(&broken);
            let _ = Rosbag1Adapter.ingest(
                &Source::Local(path.to_path_buf()),
                &IngestOptions::default(),
            );
        }
    }
    // And a length claiming far more than the file holds is refused rather than allocated for.
    let mut absurd = whole.clone();
    let at = b"#ROSBAG V2.0\n".len();
    absurd[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let path = write_temp(&absurd);
    let _ = Rosbag1Adapter.ingest(
        &Source::Local(path.to_path_buf()),
        &IngestOptions::default(),
    );
}
