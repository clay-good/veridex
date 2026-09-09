//! The demo **ROS 1 rosbag** (`.bag`): the same kind of sensor rig the MCAP demo records, written in
//! the container a generation of robot data actually sits in.
//!
//! Why a second rig fixture rather than a second copy of the first: a `.bag` is not a re-containered
//! MCAP. ROS 1 serializes the same messages differently — no encapsulation header, no alignment
//! padding, and a `uint32 seq` in front of every `std_msgs/Header` — so a rig recorded this way
//! exercises a decode path that no MCAP fixture can reach. Written from the format's own
//! specification, never from the reader's idea of it.
//!
//! Variants:
//!
//! - `rig` (the default) — a healthy six-topic recording over ~2.0 s: a LiDAR at 10 Hz, a camera and
//!   its `CameraInfo` at 20 Hz, an IMU at 100 Hz, wheel odometry and joint states at 20 Hz, and a
//!   latched `/tf_static` carrying the transform tree that relates them. Every sensor spans the same
//!   window, every body decodes, and the rig's calibration and ego trajectory come out of the
//!   messages themselves.
//! - `lossy-camera` — the same rig with a camera whose transport dropped one message in five. The
//!   publisher numbered every one of them in `header.seq`; the recording holds the rest, at the
//!   times they were published. That counter is the **only** record that anything is missing, and
//!   ROS 2 dropped the field — so this is a question a `.bag` answers about a rig and a rosbag2
//!   `.db3` of the same rig cannot → `AUTONOMY.SEQUENCE_DROPPED`.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_rosbag1 -- <output.bag> [rig|lossy-camera]`

use std::path::Path;

use crate::mcap::wave;
use crate::DemoError;

/// Every variant [`write()`] accepts. `rig` is the default the docs show.
pub const VARIANTS: &[&str] = &["rig", "lossy-camera"];

/// The wall-clock instant the recording is timed from: 2026-01-01T00:00:00Z, in nanoseconds.
///
/// The same instant the MCAP rig uses, and for the same reason: a real bag's log times are epoch
/// nanoseconds, and a recording that starts at 0 gives its first message a `header.stamp` of 0 —
/// indistinguishable from the driver that never stamped its data at all.
const RECORDING_EPOCH_NS: u64 = 1_767_225_600_000_000_000;

/// How long after a sensor samples that the recorder writes the message. Every rig has one; it is a
/// constant offset between the two clocks, not a disagreement between them.
const SENSOR_LATENCY_NS: u64 = 5_000_000; // 5 ms

/// A ROS 1 message body under construction: little-endian, no padding, no encapsulation header.
#[derive(Default)]
struct Msg {
    buf: Vec<u8>,
}

impl Msg {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// A ROS 1 string: a `u32` byte length and the bytes, with no NUL terminator (which is where CDR
    /// differs, and the difference the reader has to know).
    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }
    /// A `std_msgs/Header`: the publisher's `seq`, the sampling time, and the coordinate frame.
    fn header(&mut self, seq: u32, stamp_ns: u64, frame: &str) {
        self.u32(seq);
        self.u32((stamp_ns / 1_000_000_000) as u32);
        self.u32((stamp_ns % 1_000_000_000) as u32);
        self.string(frame);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }
}

/// `sensor_msgs/PointCloud2`: an `xyz` cloud of `points` returns, laid out as a single row.
fn point_cloud(seq: u32, stamp_ns: u64, points: u32) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "lidar_top");
    m.u32(1); // height: one row
    m.u32(points); // width
    m.u32(3); // three fields
    for (name, offset) in [("x", 0u32), ("y", 4), ("z", 8)] {
        m.string(name);
        m.u32(offset);
        m.u8(7); // FLOAT32
        m.u32(1);
    }
    m.u8(0); // is_bigendian
    m.u32(12); // point_step
    m.u32(12 * points); // row_step
    m.u32(12 * points); // data length
    for i in 0..points {
        // A ring of returns around the sensor, from rational arithmetic only: a transcendental here
        // would make the demo's content hash depend on the machine's libm.
        let t = f64::from(i) / f64::from(points.max(1));
        m.f32((10.0 * wave(t)) as f32);
        m.f32((10.0 * wave(t + 0.25)) as f32);
        m.f32(0.2);
    }
    m.u8(1); // is_dense
    m.buf
}

/// `sensor_msgs/Image`: a `width × height` `rgb8` frame. The pixels are filler; nothing reads them.
fn image(seq: u32, stamp_ns: u64, width: u32, height: u32) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "camera_front");
    m.u32(height);
    m.u32(width);
    m.string("rgb8");
    m.u8(0); // is_bigendian
    let step = width * 3;
    m.u32(step);
    m.u32(step * height);
    m.bytes(&vec![(seq % 251) as u8; (step * height) as usize]);
    m.buf
}

/// `sensor_msgs/CameraInfo`: the intrinsics of the camera above, and a plausible calibration.
fn camera_info(seq: u32, stamp_ns: u64, width: u32, height: u32) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "camera_front");
    m.u32(height);
    m.u32(width);
    m.string("plumb_bob");
    m.u32(5);
    for v in [-0.28, 0.07, 0.0, 0.0, 0.0] {
        m.f64(v);
    }
    // k: fx, 0, cx / 0, fy, cy / 0, 0, 1 — a principal point at the image centre.
    for v in [
        640.0,
        0.0,
        f64::from(width) / 2.0,
        0.0,
        640.0,
        f64::from(height) / 2.0,
        0.0,
        0.0,
        1.0,
    ] {
        m.f64(v);
    }
    m.buf
}

/// `sensor_msgs/Imu`: level and driving straight, with a little sway so the values are not constant.
fn imu(seq: u32, stamp_ns: u64, phase: f64) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "imu_link");
    let group = |m: &mut Msg, vs: &[f64]| {
        for v in vs {
            m.f64(*v);
        }
        // A leading `-1` in a covariance is ROS's "not provided"; zero means every value is measured.
        for _ in 0..9 {
            m.f64(0.0);
        }
    };
    group(&mut m, &[0.0, 0.0, 0.0, 1.0]);
    group(&mut m, &[0.0, 0.0, 0.02 * wave(phase)]);
    group(&mut m, &[0.1 * wave(phase + 0.25), 0.0, 9.81]);
    m.buf
}

/// `nav_msgs/Odometry`: the ego rolling forward at 1.5 m/s, in the `odom` frame.
fn odometry(seq: u32, stamp_ns: u64, t: f64) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "odom");
    m.string("base_link");
    for v in [1.5 * t, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
        m.f64(v);
    }
    for _ in 0..36 {
        m.f64(0.0);
    }
    // The ego's own velocity: forward speed and a gentle yaw rate, both measured quantities.
    for v in [1.5, 0.0, 0.0, 0.0, 0.0, 0.05 * wave(t)] {
        m.f64(v);
    }
    m.buf
}

/// `sensor_msgs/JointState`: a two-joint pan/tilt head, positions and velocities.
fn joint_state(seq: u32, stamp_ns: u64, t: f64) -> Vec<u8> {
    let mut m = Msg::default();
    m.header(seq, stamp_ns, "base_link");
    m.u32(2);
    m.string("head_pan");
    m.string("head_tilt");
    m.u32(2);
    m.f64(0.4 * wave(t));
    m.f64(0.2 * wave(t + 0.5));
    m.u32(2);
    m.f64(0.1 * wave(t + 0.25));
    m.f64(0.05 * wave(t + 0.75));
    m.u32(0); // effort[]: this head reports none
    m.buf
}

/// `tf2_msgs/TFMessage`: where each sensor sits on the vehicle, as `/tf_static` publishes it once.
fn tf_static(stamp_ns: u64) -> Vec<u8> {
    let mut m = Msg::default();
    let edges: [(&str, &str, [f64; 3]); 3] = [
        ("base_link", "lidar_top", [0.0, 0.0, 1.6]),
        ("base_link", "camera_front", [1.2, 0.0, 1.4]),
        ("base_link", "imu_link", [0.0, 0.0, 0.3]),
    ];
    m.u32(edges.len() as u32);
    for (parent, child, t) in edges {
        m.header(0, stamp_ns, parent);
        m.string(child);
        for v in t {
            m.f64(v);
        }
        // A unit quaternion: identity. A rotation that is not unit-norm composes a scale into every
        // placement, which `autonomy.calibration-plausibility` reports.
        for v in [0.0, 0.0, 0.0, 1.0] {
            m.f64(v);
        }
    }
    m.buf
}

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
    let mut out = (header.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(&header);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// A connection record: the topic in the record header, its ROS type in the record's data.
fn connection(conn: u32, topic: &str, ros_type: &str, latching: bool) -> Vec<u8> {
    let mut inner: Vec<(&str, Vec<u8>)> = vec![
        ("topic", topic.as_bytes().to_vec()),
        ("type", ros_type.as_bytes().to_vec()),
        ("md5sum", b"0123456789abcdef0123456789abcdef".to_vec()),
        (
            "message_definition",
            b"# see the ROS message definition\n".to_vec(),
        ),
    ];
    if latching {
        inner.push(("latching", b"1".to_vec()));
    }
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

/// A message record: its connection, the time the recorder wrote it, and the serialized body.
fn message(conn: u32, log_ns: u64, body: &[u8]) -> Vec<u8> {
    let mut time = ((log_ns / 1_000_000_000) as u32).to_le_bytes().to_vec();
    time.extend_from_slice(&((log_ns % 1_000_000_000) as u32).to_le_bytes());
    record(
        &[
            ("op", vec![0x02]),
            ("conn", conn.to_le_bytes().to_vec()),
            ("time", time),
        ],
        body,
    )
}

/// Write the demo ROS 1 bag to `path`, replacing anything already there.
pub fn write(path: &Path, variant: &str) -> Result<(), DemoError> {
    crate::check_variant(variant, VARIANTS)?;
    let lossy_camera = variant == "lossy-camera";

    let mut records = Vec::new();
    for (conn, topic, ros_type, latching) in [
        (0u32, "/lidar/points", "sensor_msgs/PointCloud2", false),
        (1, "/camera/image_raw", "sensor_msgs/Image", false),
        (2, "/camera/camera_info", "sensor_msgs/CameraInfo", false),
        (3, "/imu/data", "sensor_msgs/Imu", false),
        (4, "/odom", "nav_msgs/Odometry", false),
        (5, "/joint_states", "sensor_msgs/JointState", false),
        (6, "/tf_static", "tf2_msgs/TFMessage", true),
    ] {
        records.extend_from_slice(&connection(conn, topic, ros_type, latching));
    }

    // The transform tree, published once and retained — which is what a latched topic is for, and
    // why the single-frame rule exempts one.
    records.extend_from_slice(&message(
        6,
        RECORDING_EPOCH_NS,
        &tf_static(RECORDING_EPOCH_NS),
    ));

    // Every sensor spans the same ~2.0 s window, at its own rate.
    for i in 0..20u32 {
        let sample = RECORDING_EPOCH_NS + u64::from(i) * 100_000_000; // 10 Hz
        records.extend_from_slice(&message(
            0,
            sample + SENSOR_LATENCY_NS,
            &point_cloud(i, sample, 1024),
        ));
    }
    for i in 0..40u32 {
        let sample = RECORDING_EPOCH_NS + u64::from(i) * 50_000_000; // 20 Hz
        let log = sample + SENSOR_LATENCY_NS;
        let t = f64::from(i) / 40.0;
        // The camera's transport drops one message in five in the `lossy-camera` variant. The
        // publisher still numbered it — `seq` is `i` either way — so the hole is in the recording
        // and the numbering is the only place it shows.
        if !(lossy_camera && i % 5 == 4) {
            records.extend_from_slice(&message(
                1,
                log,
                &image(
                    i,
                    sample,
                    crate::mcap::DEMO_IMAGE_WIDTH,
                    crate::mcap::DEMO_IMAGE_HEIGHT,
                ),
            ));
        }
        records.extend_from_slice(&message(
            2,
            log,
            &camera_info(
                i,
                sample,
                crate::mcap::DEMO_IMAGE_WIDTH,
                crate::mcap::DEMO_IMAGE_HEIGHT,
            ),
        ));
        records.extend_from_slice(&message(4, log, &odometry(i, sample, t * 2.0)));
        records.extend_from_slice(&message(5, log, &joint_state(i, sample, t)));
    }
    for i in 0..200u32 {
        let sample = RECORDING_EPOCH_NS + u64::from(i) * 10_000_000; // 100 Hz
        records.extend_from_slice(&message(
            3,
            sample + SENSOR_LATENCY_NS,
            &imu(i, sample, f64::from(i) / 200.0),
        ));
    }

    let mut bag = b"#ROSBAG V2.0\n".to_vec();
    bag.extend_from_slice(&record(
        &[
            ("op", vec![0x03]),
            ("conn_count", 7u32.to_le_bytes().to_vec()),
            ("chunk_count", 1u32.to_le_bytes().to_vec()),
            ("index_pos", 0u64.to_le_bytes().to_vec()),
            ("callerid", b"/rosbag_record".to_vec()),
        ],
        &[],
    ));
    bag.extend_from_slice(&record(
        &[
            ("op", vec![0x05]),
            ("compression", b"none".to_vec()),
            ("size", (records.len() as u32).to_le_bytes().to_vec()),
        ],
        &records,
    ));
    std::fs::write(path, &bag)?;
    Ok(())
}
