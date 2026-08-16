//! Generate a small demo MCAP recording for trying the CLI end-to-end. Pick a variant with the
//! second argument:
//!
//! - (default) `skew` — a camera (~30 Hz over ~1.0 s) and a robot stream (~50 Hz over ~1.2 s) that
//!   span different durations from a shared start, so their clocks drift → `TEMPORAL.CLOCK_SKEW`
//!   (and, because the tails also diverge, `TEMPORAL.END_OFFSET`).
//! - `clean` — a single well-synchronized camera stream, no findings.
//! - `late-start` — a camera from t=0 and a robot of the *same* ~1.0 s duration that comes online
//!   ~0.30 s late; the durations match (no clock skew) but the shared-clock start and end diverge →
//!   `TEMPORAL.START_OFFSET` (and its mirror `TEMPORAL.END_OFFSET`).
//! - `stuck` — a single camera whose feed is frozen: every frame is byte-identical while timestamps
//!   advance → `STRUCTURAL.STUCK_STREAM` (a freeze the timestamp-based temporal checks can't see).
//! - `av` — a five-sensor autonomy rig (camera, LiDAR, IMU, GNSS, ego-odometry) recorded over ~1.0 s,
//!   with a single-sensor sync drift injected: the IMU spans only ~0.70 s while the rest span ~1.0 s,
//!   so it drifts ~0.30 s from its peers → `TEMPORAL.CLOCK_SKEW`. The schema names classify to the
//!   autonomy modalities (point-cloud / imu / gnss / ego-pose), so `veridex inspect` shows a typed
//!   rig — proving the cross-domain neutrality claim on AV data end-to-end today.
//!
//! Each message's payload bytes vary per frame (so frames are content-distinct, as real recordings
//! are) except in `stuck`, where the camera deliberately repeats one frame.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_mcap -- <output.mcap> [clean|late-start|stuck|av]`

use std::collections::BTreeMap;
use std::io::Cursor;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo.mcap".to_string());
    let mode = std::env::args().nth(2);
    let stuck = mode.as_deref() == Some("stuck");
    // `stuck` is a single-camera dataset like `clean`, but with a frozen (byte-identical) feed.
    let clean = mode.as_deref() == Some("clean") || stuck;
    let late_start = mode.as_deref() == Some("late-start");
    let av = mode.as_deref() == Some("av");

    let mut buf = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut buf)).expect("writer");

        if av {
            write_av_rig(&mut w);
        } else {
            write_manipulation(&mut w, stuck, clean, late_start);
        }

        // Producer-written provenance: a Metadata record and a calibration attachment, which the
        // adapter surfaces (license/sensor/operator → typed provenance, calibration from the file).
        let mut meta = std::collections::BTreeMap::new();
        meta.insert("license".to_string(), "CC-BY-4.0".to_string());
        meta.insert("sensor".to_string(), "ZED2i stereo camera".to_string());
        meta.insert("operator".to_string(), "demo-operator".to_string());
        if av {
            // Autonomy rig lineage (A3): firmware, platform/drive identity, region, map, consent.
            meta.insert("firmware_version".to_string(), "sensorOS 4.2".to_string());
            meta.insert("vehicle_id".to_string(), "demo-av-07".to_string());
            meta.insert("drive_id".to_string(), "demo-run-3".to_string());
            meta.insert("region".to_string(), "us-ca-sf".to_string());
            meta.insert("map_version".to_string(), "demo-hdmap-1.9".to_string());
            meta.insert("consent_status".to_string(), "obtained".to_string());
            meta.insert("redaction".to_string(), "faces+plates".to_string());
            // Scenario dimensions (A3/A6): descriptive recording conditions.
            meta.insert("weather".to_string(), "rain".to_string());
            meta.insert("time_of_day".to_string(), "night".to_string());
            meta.insert("environment".to_string(), "urban".to_string());
            // Scenario/map/sim references (A3): what the run was recorded or replayed against.
            meta.insert("scenario".to_string(), "OpenSCENARIO 1.2".to_string());
            meta.insert("opendrive".to_string(), "maps/demo_town.xodr".to_string());
            meta.insert("osi_version".to_string(), "3.5.0".to_string());
            meta.insert("simulator".to_string(), "carla-0.9.15".to_string());
        }
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
            data: (b"# demo calibration\n" as &[u8]).into(),
        })
        .expect("attach");

        w.finish().expect("finish");
    }

    std::fs::write(&path, &buf).expect("write file");
    println!("wrote {} ({} bytes)", path, buf.len());
}

/// Write the manipulation-format variants (camera, optionally a robot-state stream).
fn write_manipulation<W: std::io::Write + std::io::Seek>(
    w: &mut mcap::Writer<W>,
    stuck: bool,
    clean: bool,
    late_start: bool,
) {
    {
        // Camera at ~30 Hz over ~1.00 s.
        let cam_schema = w
            .add_schema("sensor_msgs/msg/Image", "ros2msg", b"")
            .unwrap();
        let cam = w
            .add_channel(cam_schema, "/camera/image", "cdr", &BTreeMap::new())
            .unwrap();
        for i in 0..31u64 {
            // A frozen feed repeats one frame; a healthy feed's frames each differ.
            let payload = if stuck { 0u64 } else { i };
            let t = i * 33_000_000; // 33 ms
            write_msg(w, cam, i as u32, t, &payload.to_le_bytes());
        }

        if !clean {
            let rob_schema = w
                .add_schema("sensor_msgs/msg/JointState", "ros2msg", b"")
                .unwrap();
            let rob = w
                .add_channel(rob_schema, "/joint_states", "cdr", &BTreeMap::new())
                .unwrap();
            if late_start {
                // Same ~1.00 s duration as the camera (31 msgs @ 33 ms) but shifted ~0.30 s later —
                // equal spans (no clock skew) with a diverging shared-clock start/end.
                for i in 0..31u64 {
                    let t = 300_000_000 + i * 33_000_000;
                    write_msg(w, rob, i as u32, t, &i.to_le_bytes());
                }
            } else {
                // Robot state at ~50 Hz spanning ~1.20 s — a 200 ms clock drift vs the camera.
                for i in 0..61u64 {
                    let t = i * 20_000_000; // 20 ms => 1.20 s total
                    write_msg(w, rob, i as u32, t, &i.to_le_bytes());
                }
            }
        }
    }
}

/// Write a five-sensor autonomy rig (camera, LiDAR, IMU, GNSS, ego-odometry) over ~1.0 s. Every
/// sensor spans ~1.0 s from a shared start except the IMU, whose span is deliberately cut to ~0.70 s
/// — a single-sensor sync drift of ~0.30 s that the duration-based `TEMPORAL.CLOCK_SKEW` flags.
fn write_av_rig<W: std::io::Write + std::io::Seek>(w: &mut mcap::Writer<W>) {
    // (schema, topic, message count, inter-message interval ns). The IMU runs the same 100 msg count
    // as a healthy 100 Hz sensor but at a compressed 7 ms interval, so it finishes ~0.30 s early.
    let sensors: &[(&str, &str, u64, u64)] = &[
        ("sensor_msgs/msg/Image", "/camera/image", 31, 33_000_000), // ~30 Hz, ~0.99 s
        (
            "sensor_msgs/msg/PointCloud2",
            "/lidar/points",
            11,
            100_000_000,
        ), // 10 Hz, 1.00 s
        ("sensor_msgs/msg/NavSatFix", "/gps/fix", 11, 100_000_000), // 10 Hz, 1.00 s
        ("nav_msgs/msg/Odometry", "/odom", 51, 20_000_000),         // ~50 Hz, 1.00 s
        ("sensor_msgs/msg/Imu", "/imu/data", 101, 7_000_000),       // drifted: ~0.70 s span
    ];
    for (seq_base, (schema, topic, count, interval)) in sensors.iter().enumerate() {
        let schema_id = w.add_schema(schema, "ros2msg", b"").unwrap();
        let channel = w
            .add_channel(schema_id, topic, "cdr", &BTreeMap::new())
            .unwrap();
        for i in 0..*count {
            let t = i * interval;
            if *schema == "nav_msgs/msg/Odometry" {
                // A real CDR Odometry body, so the ego trajectory is genuinely decoded rather than
                // skipped: the demo drives ~10 m/s down +x, which is what makes the rig a
                // world-model-readiness *candidate* (the profile needs a perception sensor **and** an
                // ego trajectory). A dummy payload here left `ego_poses` empty, and the flagship demo
                // reported the profile as N/A.
                let x = i as f64 * 10.0 * (*interval as f64 / 1e9);
                write_msg(w, channel, i as u32, t, &odometry_body(x));
            } else {
                // Vary payload per (sensor, frame) so frames are content-distinct.
                let payload = ((seq_base as u64) << 32) | i;
                write_msg(w, channel, i as u32, t, &payload.to_le_bytes());
            }
        }
    }
}

/// A `nav_msgs/msg/Odometry` CDR body (little-endian, ROS 2 default) whose pose sits at `x` metres
/// along +x, level and unrotated. Only the prefix Veridex reads is written: `Header`,
/// `child_frame_id`, then `pose.pose` — the covariance and twist that follow are not decoded.
fn odometry_body(x: f64) -> Vec<u8> {
    let mut buf: Vec<u8> = vec![0x00, 0x01, 0x00, 0x00]; // encapsulation: CDR_LE
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
    // Header { stamp { sec, nanosec }, frame_id }
    u32v(&mut buf, 0);
    u32v(&mut buf, 0);
    strv(&mut buf, "odom");
    strv(&mut buf, "base_link"); // child_frame_id
                                 // pose.pose { position { x, y, z }, orientation { x, y, z, w } }
    for v in [x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
        f64v(&mut buf, v);
    }
    buf
}

fn write_msg<W: std::io::Write + std::io::Seek>(
    w: &mut mcap::Writer<W>,
    channel_id: u16,
    sequence: u32,
    log_time: u64,
    payload: &[u8],
) {
    w.write_to_known_channel(
        &mcap::records::MessageHeader {
            channel_id,
            sequence,
            log_time,
            publish_time: log_time,
        },
        payload,
    )
    .expect("write message");
}
