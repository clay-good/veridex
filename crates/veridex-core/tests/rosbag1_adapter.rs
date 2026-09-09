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
fn every_compression_rosbag_writes_is_read() {
    // The three a `rosbag` produces: plain, `--lz4`, and `rosbag compress`'s bz2 — which is how most
    // archived ROS 1 data is stored, and which was disclosed as unread until this reader carried a
    // decompressor for it. All three must yield the same recording; which flag a team passed cannot
    // change what Veridex sees.
    let inner = rig_records();
    let plain = ingest(&bag(&chunk("none", &inner, &inner)));

    let mut lz4 = Vec::new();
    {
        let mut enc = lz4_flex::frame::FrameEncoder::new(&mut lz4);
        enc.write_all(&inner).unwrap();
        enc.finish().unwrap();
    }
    let mut bz2 = Vec::new();
    {
        let mut enc = bzip2::write::BzEncoder::new(&mut bz2, bzip2::Compression::default());
        enc.write_all(&inner).unwrap();
        enc.finish().unwrap();
    }
    assert!(bz2.starts_with(b"BZh"), "a real bzip2 stream");

    for (name, packed) in [("lz4", lz4), ("bz2", bz2)] {
        let ingested = ingest(&bag(&chunk(name, &inner, &packed)));
        assert!(
            ingested.report.unread_sources.is_empty(),
            "{name}: {:?}",
            ingested.report.unread_sources
        );
        let streams = |i: &veridex_core::adapter::Ingested| -> Vec<(String, usize)> {
            i.dataset.episodes[0]
                .streams
                .iter()
                .map(|s| (s.name.clone(), s.frames.len()))
                .collect()
        };
        assert_eq!(streams(&ingested), streams(&plain), "{name}");
    }
}

#[test]
fn a_compressed_chunk_that_will_not_stop_unpacking_is_refused_by_its_own_declaration() {
    // A chunk's `size` is what the reader charges the decompression budget before pointing a
    // decompressor at anything, and the read is capped one byte past it. So a chunk that declares a
    // little and unpacks to a lot — a bomb, or a corrupt stream — is stopped at a size the file did
    // not choose, and the messages it holds are disclosed as unread rather than trusted.
    let inner = rig_records();
    let mut packed = Vec::new();
    {
        let mut enc = bzip2::write::BzEncoder::new(&mut packed, bzip2::Compression::default());
        enc.write_all(&inner).unwrap();
        enc.finish().unwrap();
    }
    // The chunk header claims 64 bytes; the stream holds the whole rig.
    let lying = record(
        &[
            ("op", vec![0x05]),
            ("compression", b"bz2".to_vec()),
            ("size", 64u32.to_le_bytes().to_vec()),
        ],
        &packed,
    );
    // A readable chunk beside it, so the bag still produces a dataset to report against.
    let mut body = chunk("none", &inner, &inner);
    body.extend_from_slice(&lying);
    let ingested = ingest(&bag(&body));
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("produces more")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_chunk_in_a_compression_this_reader_has_none_for_is_disclosed() {
    // Not every rosbag will only ever write three compressions. One it does not know is a coverage
    // hole — the messages are in the file and nobody read them — not a shape the CDM cannot hold.
    let inner = rig_records();
    let mut body = chunk("none", &inner, &inner);
    body.extend_from_slice(&chunk("brotli", &inner, b"whatever a future rosbag writes"));
    let ingested = ingest(&bag(&body));
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("`brotli` is not decompressed")),
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

/// A ROS 1 message body, written the way a ROS 1 publisher serializes one: no encapsulation header,
/// no alignment padding anywhere, and a `uint32 seq` at the front of every `std_msgs/Header`.
///
/// Written out by hand for the same reason the records above are: the point of these tests is that
/// the decoders read what a `.bag` really holds, which a writer sharing their assumptions could not
/// prove.
struct R1 {
    buf: Vec<u8>,
}

impl R1 {
    fn new() -> R1 {
        R1 { buf: Vec::new() }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// A ROS 1 string: a `u32` byte length and the bytes, with no NUL terminator.
    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }
    /// A `std_msgs/Header`: `uint32 seq`, `time stamp`, `string frame_id`.
    fn header(&mut self, seq: u32, stamp_ns: u64, frame_id: &str) {
        self.u32(seq);
        self.u32((stamp_ns / 1_000_000_000) as u32);
        self.u32((stamp_ns % 1_000_000_000) as u32);
        self.string(frame_id);
    }
}

/// A `sensor_msgs/Imu`: orientation, angular velocity and linear acceleration, each behind the
/// covariance whose first element says whether the driver provides the field at all.
fn imu_body(seq: u32, stamp_ns: u64, accel_z: f64) -> Vec<u8> {
    let mut w = R1::new();
    // An eight-character frame, so the doubles behind the header start at 24 with no padding
    // between. A reader that aligned them to 8 the way CDR does would read every value that
    // follows out of the middle of two doubles.
    w.header(seq, stamp_ns, "imu_link");
    for v in [0.0, 0.0, 0.0, 1.0] {
        w.f64(v);
    }
    w.f64(0.01); // orientation_covariance[0]: provided
    for _ in 0..8 {
        w.f64(0.0);
    }
    for v in [0.1, 0.2, 0.3] {
        w.f64(v);
    }
    w.f64(0.01);
    for _ in 0..8 {
        w.f64(0.0);
    }
    for v in [0.0, 0.0, accel_z] {
        w.f64(v);
    }
    w.f64(0.01);
    for _ in 0..8 {
        w.f64(0.0);
    }
    w.buf
}

/// A `sensor_msgs/PointCloud2` carrying `width` points of one `float32 x` field each.
fn point_cloud_body(stamp_ns: u64, width: u32) -> Vec<u8> {
    let mut w = R1::new();
    w.header(0, stamp_ns, "lidar");
    w.u32(1); // height
    w.u32(width);
    w.u32(1); // one field
    w.string("x");
    w.u32(0); // offset
    w.u8(7); // float32
    w.u32(1); // count
    w.u8(0); // is_bigendian
    w.u32(4); // point_step
    w.u32(4 * width); // row_step
    w.u32(4 * width); // data length
    for _ in 0..width {
        w.buf.extend_from_slice(&1.0f32.to_le_bytes());
    }
    w.u8(1); // is_dense
    w.buf
}

/// A `sensor_msgs/CameraInfo` for a 640x480 camera.
fn camera_info_body(stamp_ns: u64) -> Vec<u8> {
    let mut w = R1::new();
    w.header(0, stamp_ns, "camera");
    w.u32(480); // height
    w.u32(640); // width
    w.string("plumb_bob");
    w.u32(5);
    for _ in 0..5 {
        w.f64(0.0);
    }
    for v in [600.0, 0.0, 320.0, 0.0, 600.0, 240.0, 0.0, 0.0, 1.0] {
        w.f64(v); // k
    }
    w.buf
}

/// A `nav_msgs/Odometry`: the pose, its covariance, and the ego's own velocity.
fn odometry_body(stamp_ns: u64, x: f64) -> Vec<u8> {
    let mut w = R1::new();
    w.header(0, stamp_ns, "odom");
    w.string("base_link");
    for v in [x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
        w.f64(v);
    }
    for _ in 0..36 {
        w.f64(0.0);
    }
    for v in [1.5, 0.0, 0.0, 0.0, 0.0, 0.1] {
        w.f64(v); // twist: 1.5 m/s forward, 0.1 rad/s yaw
    }
    w.buf
}

/// A rig whose bodies are real ROS 1 messages rather than filler bytes.
fn decodable_rig() -> Vec<u8> {
    let mut inner = Vec::new();
    inner.extend_from_slice(&connection(0, "/imu/data", "sensor_msgs/Imu", false));
    inner.extend_from_slice(&connection(
        1,
        "/lidar/points",
        "sensor_msgs/PointCloud2",
        false,
    ));
    inner.extend_from_slice(&connection(
        2,
        "/camera/camera_info",
        "sensor_msgs/CameraInfo",
        false,
    ));
    inner.extend_from_slice(&connection(3, "/odom", "nav_msgs/Odometry", false));
    for i in 0..5u64 {
        let ts = 1_000_000_000 + i * 100_000_000;
        inner.extend_from_slice(&message(0, ts, &imu_body(i as u32, ts, 9.81)));
        inner.extend_from_slice(&message(1, ts, &point_cloud_body(ts, 100)));
        inner.extend_from_slice(&message(2, ts, &camera_info_body(ts)));
        inner.extend_from_slice(&message(3, ts, &odometry_body(ts, i as f64)));
    }
    inner
}

#[test]
fn a_bag_body_is_read_the_way_a_ros_2_body_is() {
    let inner = decodable_rig();
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let ep = &ingested.dataset.episodes[0];
    let by = |n: &str| ep.streams.iter().find(|s| s.name == n).unwrap();

    // The IMU's whole payload is its measurement, and it is measured — with no padding between the
    // odd-length `frame_id` and the doubles behind it, which is where a CDR reader would go wrong.
    let imu = by("/imu/data");
    assert!(imu.observed_stats.is_some(), "the IMU is measured");
    let names = imu.dim_names.as_ref().expect("the dimensions are named");
    let z = names
        .iter()
        .position(|n| n == "linear_acceleration.z")
        .expect("the acceleration is one of them");
    let dim = imu
        .observed_dim_stats
        .as_ref()
        .expect("per-dimension stats")
        .iter()
        .find(|d| d.dim as usize == z)
        .expect("the acceleration's own summary");
    assert_eq!(
        (dim.stats.min, dim.stats.max),
        (9.81, 9.81),
        "the acceleration is read exactly, out of a body with no padding behind its frame_id"
    );
    let decodes = imu.observed_body_decodes.expect("bodies were decoded");
    assert_eq!((decodes.attempted, decodes.failed), (5, 0));

    // The sensor's own clock, out of the header the recorder's clock stands beside.
    let stamps = imu.observed_header_stamps.expect("stamps were read");
    assert_eq!(stamps.message_count, 5);
    assert_eq!(stamps.unset, 0);
    assert_eq!(imu.frame_id.as_deref(), Some("imu_link"));

    // The LiDAR's returns are counted, which is what catches a driver publishing empty sweeps.
    let counts = by("/lidar/points")
        .observed_point_counts
        .expect("points were counted");
    assert_eq!((counts.min, counts.max), (100, 100));
    assert_eq!(
        by("/lidar/points")
            .point_fields
            .as_ref()
            .map(|f| f.len())
            .unwrap_or(0),
        1
    );

    // The rig the recording describes: intrinsics out of `CameraInfo`, a trajectory out of
    // `Odometry`, both decoded from bodies rather than claimed by a sidecar.
    let calib = ingested
        .dataset
        .calibration
        .as_ref()
        .expect("a calibration");
    assert_eq!(calib.intrinsics.len(), 1);
    assert_eq!(calib.intrinsics[0].fx, 600.0);
    assert_eq!(calib.intrinsics[0].width, Some(640u64));
    let poses = ep.ego_poses.as_ref().expect("a trajectory");
    assert_eq!(poses.len(), 5);
    assert_eq!(poses[4].pose.translation[0], 4.0);
    assert_eq!(ep.ego_frame.as_deref(), Some("base_link"));
    // The ego's own velocity is a measurement too.
    assert!(by("/odom").observed_stats.is_some());
}

#[test]
fn a_body_that_is_not_the_message_it_claims_is_counted_as_a_failure() {
    // Truncated `Imu` bodies: present, and their own invariants do not hold. The count has to say
    // so, because everything summarized about a stream is otherwise computed from whichever bodies
    // did survive and reported as a property of the stream.
    let mut inner = Vec::new();
    inner.extend_from_slice(&connection(0, "/imu/data", "sensor_msgs/Imu", false));
    for i in 0..4u64 {
        let ts = 1_000_000_000 + i * 100_000_000;
        let mut body = imu_body(i as u32, ts, 9.81);
        body.truncate(40);
        inner.extend_from_slice(&message(0, ts, &body));
    }
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let imu = &ingested.dataset.episodes[0].streams[0];
    let decodes = imu.observed_body_decodes.expect("the attempts are counted");
    assert_eq!((decodes.attempted, decodes.failed), (4, 4));
    assert!(
        imu.observed_stats.is_none(),
        "nothing is summarized out of bodies that did not decode"
    );
}

#[test]
fn the_seq_on_a_ros_1_header_counts_what_the_publisher_sent() {
    // ROS 1 keeps the `seq` counter ROS 2 dropped, so a `.bag` holds the one direct evidence of a
    // message that never reached the recorder: a hole in the publisher's own numbering.
    let mut inner = Vec::new();
    inner.extend_from_slice(&connection(0, "/imu/data", "sensor_msgs/Imu", false));
    for (i, seq) in [0u32, 1, 2, 5].iter().enumerate() {
        let ts = 1_000_000_000 + i as u64 * 100_000_000;
        inner.extend_from_slice(&message(0, ts, &imu_body(*seq, ts, 9.81)));
    }
    let ingested = ingest(&bag(&chunk("none", &inner, &inner)));
    let seq = ingested.dataset.episodes[0].streams[0]
        .observed_sequence
        .expect("the publisher's numbering was read");
    assert_eq!(seq.message_count, 4);
    assert_eq!(seq.missing, 2, "3 and 4 never reached the bag");
    assert_eq!(seq.non_increasing, 0);
}

/// The demo ROS 1 rig, through the whole pipeline.
///
/// The tests above prove each decoder against a body built for it. This one proves the *recording*:
/// a seven-topic bag the demo generator writes from the format's specification, ingested and checked
/// the way a user's would be.
fn demo_bag(dir: &std::path::Path, variant: &str) -> veridex_core::cdm::Dataset {
    let path = dir.join(format!("{variant}.bag"));
    veridex_demo::rosbag1::write(&path, variant).expect("write the demo bag");
    Rosbag1Adapter
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect("the demo bag ingests")
        .dataset
}

fn finding_codes(d: &veridex_core::cdm::Dataset) -> Vec<String> {
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(d);
    let mut codes: Vec<String> = engine
        .run(d, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .map(|f| f.code)
        .collect();
    codes.sort();
    codes
}

#[test]
fn the_demo_rig_describes_itself_out_of_its_own_message_bodies() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = demo_bag(dir.path(), "rig");

    // The rig's geometry, decoded from the recording rather than claimed by a sidecar: three
    // transforms off `/tf_static` and the camera's intrinsics off its `CameraInfo`.
    let calib = d.calibration.as_ref().expect("a calibration");
    assert_eq!(calib.transforms.len(), 3);
    assert_eq!(calib.intrinsics.len(), 1);
    assert_eq!(calib.intrinsics[0].fx, 640.0);

    // The ego trajectory, off `/odom`, in the frame the messages name.
    let ep = &d.episodes[0];
    assert_eq!(ep.ego_poses.as_ref().map(|p| p.len()), Some(40));
    assert_eq!(ep.ego_frame.as_deref(), Some("base_link"));

    let by = |n: &str| ep.streams.iter().find(|s| s.name == n).expect(n);
    // The streams whose payload *is* their measurement are measured.
    for name in ["/imu/data", "/joint_states", "/odom"] {
        assert!(by(name).observed_stats.is_some(), "{name} is measured");
    }
    // The two whose payload is bulk are counted, not interpreted: returns per sweep, pixels per
    // frame. That is what catches a driver that kept publishing after it lost its sensor.
    assert_eq!(
        by("/lidar/points")
            .observed_point_counts
            .map(|c| (c.min, c.max)),
        Some((1024, 1024))
    );
    let dims = by("/camera/image_raw")
        .observed_image_dims
        .expect("the camera's frames are sized");
    assert_eq!((dims.min_width, dims.min_height), (64, 48));
    // Every body decoded — a rig fixture whose bodies fail is not exercising the decoders.
    for s in &ep.streams {
        if let Some(b) = s.observed_body_decodes {
            assert_eq!(b.failed, 0, "{} had bodies that did not decode", s.name);
        }
    }

    // A healthy rig, and the checks that grade one had something to grade.
    let codes = finding_codes(&d);
    assert!(
        !codes.iter().any(|c| c.starts_with("AUTONOMY.")),
        "the healthy rig raises no autonomy finding: {codes:?}"
    );
}

#[test]
fn a_camera_losing_a_fifth_of_its_messages_moves_nothing_but_the_count() {
    // The same rig with a camera whose transport dropped one message in five. The survivors keep
    // the times they were published at, so nothing in the timeline records the loss — and ROS 2
    // dropped the field that does. On a `.bag` the publisher's `header.seq` still carries it, which
    // is the whole point of reading it.
    let dir = tempfile::tempdir().expect("tempdir");
    let healthy = finding_codes(&demo_bag(dir.path(), "rig"));
    let lossy = finding_codes(&demo_bag(dir.path(), "lossy-camera"));
    let added: Vec<&String> = lossy.iter().filter(|c| !healthy.contains(c)).collect();
    let removed: Vec<&String> = healthy.iter().filter(|c| !lossy.contains(c)).collect();
    assert_eq!(
        added,
        ["AUTONOMY.SEQUENCE_DROPPED"].iter().collect::<Vec<_>>(),
        "losing a fifth of a camera must add exactly the finding that counts it"
    );
    assert!(
        removed.is_empty(),
        "and must take nothing away: {removed:?}"
    );
}
