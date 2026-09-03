//! The demo MCAP recording for trying the CLI end-to-end. [`VARIANTS`]:
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
//! - `av-miscalibrated` — the same rig, but the LiDAR is parented to a `lidar_mount` frame that
//!   nothing joins to `base_link`. The transform tree is well-formed and the LiDAR is in it, yet no
//!   chain of transforms reaches the camera → `AUTONOMY.SENSOR_FRAME_UNRELATED`: the LiDAR-camera
//!   reprojection is undefined, which no check on the tree's own shape can see.
//! - `av-ambiguous-tf` — the same rig with **two** nodes publishing a transform for `lidar_top`,
//!   one from `base_link` and one from a `lidar_mount` that is itself parented to `base_link`. Every
//!   frame graph question already asked comes back clean: the tree is one connected component, the
//!   LiDAR is in it, and a chain reaches the camera — because all of those walk the graph
//!   *undirected*. It is still not a tree, and the LiDAR's pose depends on which of the two chains a
//!   consumer resolves → `AUTONOMY.CALIBRATION_AMBIGUOUS`, and nothing else moves.
//!
//! - `av-dead-lidar` — the same rig with a LiDAR whose driver lost its sensor. Every `PointCloud2`
//!   is well-formed, on time, in the right coordinate frame and declares the same four fields — and
//!   holds zero points. The structural family sees frames, the temporal family sees a clean 10 Hz,
//!   the frame checks place the sensor in the tree, and every one of them passes →
//!   `AUTONOMY.POINT_CLOUD_EMPTY` is the only thing that reports the sensor recorded nothing.
//!
//! - `av-unstamped` — the same rig whose LiDAR driver never set `header.stamp`. Every cloud is
//!   well-formed, full of points, on time and in the right frame; only the sensor's own capture time
//!   is missing, so the recorder's arrival clock is the only clock that stream has. Every timing
//!   check still passes — they read the recorder's clock either way —
//!   and `AUTONOMY.SENSOR_CLOCK_UNSET` is the only thing that reports the rig's sync result is
//!   about the recording host rather than about the sensors.
//!
//! - `av-uncalibrated-camera` — the same rig whose camera publishes a `CameraInfo` before anyone
//!   calibrated it: the model is named, the five coefficients are there, and every number is zero.
//!   Every presence check passes — the rig has intrinsics — and a projection through them divides by
//!   a focal length of zero → `AUTONOMY.CALIBRATION_IMPLAUSIBLE`.
//!
//! - `av-lossy-camera` — the same rig with a camera whose transport dropped one message in five.
//!   The publisher numbered every one of them; the recording holds the rest, at the times they were
//!   published. The bag itself is the only record that anything is missing →
//!   `AUTONOMY.SEQUENCE_DROPPED`, counted from the publisher's own `sequence` rather than estimated
//!   from the cadence, and **nothing else moves**: the report differs from the healthy rig's by that
//!   one finding, so nineteen percent of a camera goes missing with every timing check passing.
//! - `av-no-fix` — the same rig with a satellite receiver that lost the sky a fifth of the way in
//!   and said so: every message after that carries `NavSatStatus.STATUS_NO_FIX`, while the driver
//!   keeps publishing the last position it had. The messages arrive on time, so the stream's frame
//!   count, cadence and span are a healthy receiver's, and the fixes that did land are ordinary
//!   coordinates — `autonomy.gnss-plausibility` passes on them. Only the status byte says four
//!   fifths of the trajectory is not measured → `AUTONOMY.GNSS_NO_FIX`.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_mcap -- <output.mcap> [skew|clean|stuck|late-start|av|av-miscalibrated|av-ambiguous-tf|av-dead-lidar|av-unstamped|av-uncalibrated-camera|av-lossy-camera|av-no-fix]`

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use crate::DemoError;

/// Every variant `write` accepts. `skew` is the default the docs show.
pub const VARIANTS: &[&str] = &[
    "skew",
    "clean",
    "stuck",
    "late-start",
    "av",
    "av-miscalibrated",
    "av-ambiguous-tf",
    "av-dead-lidar",
    "av-unstamped",
    "av-uncalibrated-camera",
    "av-lossy-camera",
    "av-no-fix",
];

/// Write the demo MCAP recording to `path`, replacing anything already there.
pub fn write(path: &Path, variant: &str) -> Result<(), DemoError> {
    // A typo used to fall through to the default skew dataset, silently producing a different
    // fixture than the one asked for.
    crate::check_variant(variant, VARIANTS)?;
    let stuck = variant == "stuck";
    // `stuck` is a single-camera dataset like `clean`, but with a frozen (byte-identical) feed.
    let clean = variant == "clean" || stuck;
    let late_start = variant == "late-start";
    // `av-miscalibrated` is the same rig with the LiDAR stranded outside the camera's transform
    // subtree, so the LiDAR-camera reprojection is undefined.
    let miscalibrated = variant == "av-miscalibrated";
    // `av-ambiguous-tf` is the same rig with `lidar_top` claimed by two parents at once, so its
    // pose depends on which chain a consumer walks.
    let ambiguous_tf = variant == "av-ambiguous-tf";
    // `av-dead-lidar` is the same rig with a LiDAR whose driver lost its sensor: every cloud is
    // well-formed, on time, in the right frame — and holds no points.
    let dead_lidar = variant == "av-dead-lidar";
    // `av-unstamped` is the same rig with a LiDAR driver that never set `header.stamp`: the clouds
    // are full and on time, and the sensor says nothing about when it sampled them.
    let unstamped_lidar = variant == "av-unstamped";
    // `av-uncalibrated-camera` is the same rig whose camera publishes a `CameraInfo` before anyone
    // calibrated it: the model named, the coefficients present, and every number zero.
    let uncalibrated_camera = variant == "av-uncalibrated-camera";
    // `av-lossy-camera` is the same rig with a camera whose messages did not all reach the file: the
    // publisher numbered them and the recording holds fewer.
    let lossy_camera = variant == "av-lossy-camera";
    // `av-no-fix` is the same rig with a receiver that lost the sky partway through and stamped every
    // message after it `STATUS_NO_FIX`, while still publishing the last position it had.
    let no_fix = variant == "av-no-fix";
    let av = variant == "av"
        || miscalibrated
        || ambiguous_tf
        || dead_lidar
        || unstamped_lidar
        || uncalibrated_camera
        || lossy_camera
        || no_fix;

    let mut buf = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut buf)).expect("writer");

        if av {
            write_av_rig(
                &mut w,
                RigFaults {
                    miscalibrated,
                    ambiguous_tf,
                    dead_lidar,
                    unstamped_lidar,
                    uncalibrated_camera,
                    lossy_camera,
                    no_fix,
                },
            );
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

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, &buf)?;
    Ok(())
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

/// The wall-clock instant the rig recording is timed from: 2026-01-01T00:00:00Z, in nanoseconds.
///
/// A real bag's log times are epoch nanoseconds. Starting at 0 is not only unrealistic, it collides
/// with the value that means "this driver never stamped its data" — the first message of every
/// sensor would carry `header.stamp` 0 and be indistinguishable from an unstamped one.
const RECORDING_EPOCH_NS: u64 = 1_767_225_600_000_000_000;

/// How long after a sensor samples that the recorder writes the message, in nanoseconds.
///
/// Every rig has one: the sensor's own pipeline latency. It is a constant offset between the two
/// clocks, not a disagreement between them, which is why `autonomy.sensor-clock` grades the offset
/// against a tolerance three orders of magnitude above this rather than requiring it to be zero.
const SENSOR_LATENCY_NS: u64 = 5_000_000; // 5 ms

/// Write a five-sensor autonomy rig (camera + its `CameraInfo`, LiDAR, IMU, GNSS, ego-odometry) over
/// ~1.0 s. Every
/// The one fault an `av` variant injects, if any. Each field is a variant name; at most one is set,
/// and all-false is the healthy rig.
#[derive(Debug, Clone, Copy, Default)]
struct RigFaults {
    miscalibrated: bool,
    ambiguous_tf: bool,
    dead_lidar: bool,
    unstamped_lidar: bool,
    uncalibrated_camera: bool,
    lossy_camera: bool,
    no_fix: bool,
}

/// sensor spans ~1.0 s from a shared start except the IMU, whose span is deliberately cut to ~0.70 s
/// — a single-sensor sync drift of ~0.30 s that the duration-based `TEMPORAL.CLOCK_SKEW` flags.
fn write_av_rig<W: std::io::Write + std::io::Seek>(w: &mut mcap::Writer<W>, faults: RigFaults) {
    let RigFaults {
        miscalibrated,
        ambiguous_tf,
        dead_lidar,
        unstamped_lidar,
        uncalibrated_camera,
        lossy_camera,
        no_fix,
    } = faults;
    // (schema, topic, message count, inter-message interval ns, coordinate frame). The IMU runs the
    // same 100 msg count as a healthy 100 Hz sensor but at a compressed 7 ms interval, so it finishes
    // ~0.30 s early.
    let sensors: &[(&str, &str, u64, u64, &str)] = &[
        (
            "sensor_msgs/msg/Image",
            "/camera/image",
            31,
            33_000_000,
            "camera_front",
        ), // ~30 Hz, ~0.99 s
        // The camera's calibration, published beside its images the way a real driver publishes it.
        // Without it the rig has a camera nothing can project into, so every variant here — the
        // healthy one included — reported `AUTONOMY.CALIBRATION_INCOMPLETE` and the flagship demo
        // could never show a calibrated rig.
        (
            "sensor_msgs/msg/CameraInfo",
            "/camera/camera_info",
            31,
            33_000_000,
            "camera_front",
        ),
        (
            "sensor_msgs/msg/PointCloud2",
            "/lidar/points",
            11,
            100_000_000,
            "lidar_top",
        ), // 10 Hz, 1.00 s
        (
            "sensor_msgs/msg/NavSatFix",
            "/gps/fix",
            11,
            100_000_000,
            "gnss",
        ), // 10 Hz, 1.00 s
        ("nav_msgs/msg/Odometry", "/odom", 51, 20_000_000, "odom"), // ~50 Hz, 1.00 s
        (
            "sensor_msgs/msg/Imu",
            "/imu/data",
            101,
            7_000_000,
            "imu_link",
        ), // drifted: ~0.70 s span
    ];

    // The static transform tree. In the healthy rig every sensor hangs off `base_link`, so a chain
    // exists from the LiDAR to the camera. In the miscalibrated rig the LiDAR is parented to a
    // `lidar_mount` frame that nothing joins to `base_link` — the tree is well-formed and the LiDAR is
    // in it, but no chain reaches the camera, so points cannot be projected into the image.
    let lidar_parent = if miscalibrated {
        "lidar_mount"
    } else {
        "base_link"
    };
    let mut tf_edges: Vec<(&str, &str)> = vec![
        ("base_link", "camera_front"),
        (lidar_parent, "lidar_top"),
        ("base_link", "gnss"),
        ("base_link", "imu_link"),
        ("odom", "base_link"),
    ];
    if ambiguous_tf {
        // A second broadcaster claims the LiDAR, from a mount frame that is itself on `base_link`.
        // The mount edge keeps the graph connected and keeps the LiDAR reachable from the camera, so
        // every existing frame-graph check still passes — which is the point. What breaks is
        // uniqueness: `lidar_top` now has two parents at the same time.
        tf_edges.push(("base_link", "lidar_mount"));
        tf_edges.push(("lidar_mount", "lidar_top"));
    }
    let tf_schema = w
        .add_schema("tf2_msgs/msg/TFMessage", "ros2msg", b"")
        .unwrap();
    // The QoS a real ROS 2 stack offers `/tf_static`: transient-local, i.e. published once at
    // startup and retained for late subscribers. rosbag2's MCAP writer carries each publisher's
    // profile on the channel, so a demo that claims to model a rig log has to carry it too —
    // without it the one-message transform tree is read as a sensor that fired once and stopped.
    let tf_channel = w
        .add_channel(tf_schema, "/tf_static", "cdr", &latched_qos())
        .unwrap();
    write_msg(
        w,
        tf_channel,
        0,
        RECORDING_EPOCH_NS,
        &tf_message_body(&tf_edges, RECORDING_EPOCH_NS),
    );

    for (seq_base, (schema, topic, count, interval, frame_id)) in sensors.iter().enumerate() {
        let schema_id = w.add_schema(schema, "ros2msg", b"").unwrap();
        let channel = w
            .add_channel(schema_id, topic, "cdr", &BTreeMap::new())
            .unwrap();
        for i in 0..*count {
            // The recorder's clock. The sensor sampled `SENSOR_LATENCY_NS` earlier, and says so in
            // its own `header.stamp` — the two clocks a bag carries, which `autonomy.sensor-clock`
            // compares.
            let t = RECORDING_EPOCH_NS + i * interval;
            let stamp = t - SENSOR_LATENCY_NS;
            // The `av-lossy-camera` fault: the publisher numbered every message and one in five
            // never reached the file. Nothing marks the hole except the numbering itself — the
            // surviving messages keep the times they were published at, so the camera still spans
            // the whole recording alongside every other sensor.
            if lossy_camera && *schema == "sensor_msgs/msg/Image" && i % 5 == 4 {
                continue;
            }
            if *schema == "nav_msgs/msg/Odometry" {
                // A real CDR Odometry body, so the ego trajectory is genuinely decoded rather than
                // skipped: the demo drives ~10 m/s down +x, which is what makes the rig a
                // world-model-readiness *candidate* (the profile needs a perception sensor **and** an
                // ego trajectory). A dummy payload here left `ego_poses` empty, and the flagship demo
                // reported the profile as N/A.
                let x = i as f64 * 10.0 * (*interval as f64 / 1e9);
                write_msg(w, channel, i as u32, t, &odometry_body(x, stamp));
            } else if *schema == "sensor_msgs/msg/NavSatFix" {
                // A real CDR NavSatFix body, for the same reason the Odometry one is real: the
                // adapter decodes latitude/longitude/altitude into measured values, and a dummy
                // payload left the rig's GNSS stream fingerprinted — so every statistical check
                // abstained on the one sensor whose failure mode (a frozen or unset fix) is the
                // easiest to have and the hardest to see.
                let drive = i as f64 * 10.0 * (*interval as f64 / 1e9);
                // The `av-no-fix` fault: after the first fifth of the drive the receiver loses the
                // sky and says so, while the driver keeps publishing the last position it had. Every
                // message still arrives on time, so the stream's frame count, cadence and span are
                // those of a healthy receiver, and the fixes that did land are ordinary coordinates.
                let lost_sky = no_fix && i * 5 >= *count;
                let held = if lost_sky {
                    // Frozen where the fix was lost, which is what a driver leaves behind.
                    (*count / 5) as f64 * 10.0 * (*interval as f64 / 1e9)
                } else {
                    drive
                };
                write_msg(
                    w,
                    channel,
                    i as u32,
                    t,
                    // ~37.4°N, 122.1°W, moving north at the odometry's 10 m/s (1 m ≈ 9e-6°).
                    &nav_sat_fix_body(
                        if lost_sky { -1 } else { 0 },
                        37.4 + held * 9.0e-6,
                        -122.1,
                        12.0,
                        stamp,
                    ),
                );
            } else if *schema == "sensor_msgs/msg/CameraInfo" {
                write_msg(
                    w,
                    channel,
                    i as u32,
                    t,
                    &camera_info_body(frame_id, stamp, uncalibrated_camera),
                );
            } else if *schema == "sensor_msgs/msg/Imu" {
                // Likewise real: the adapter decodes an Imu body in full, so a dummy payload left
                // the rig's IMU fingerprinted and every statistical check abstaining on it — on the
                // very sensor this demo exists to show drifting.
                let phase = i as f64 * (*interval as f64 / 1e9);
                write_msg(w, channel, i as u32, t, &imu_body(phase, stamp));
            } else if *schema == "sensor_msgs/msg/PointCloud2" {
                // Likewise real, and for the reason the others became real: a stub body left the
                // rig's LiDAR with no declared point layout and no point counts at all, so
                // `autonomy.point-cloud-density` abstained on the one sensor a world model is
                // mostly built from. The point count is read from the message's own
                // `height × width` and believed only when the body's length invariants hold, which
                // a stub prefix does not satisfy — so the flagship rig demo has to write a whole
                // cloud, tail included. The payload bytes are zero and never read; only their
                // *count* is asserted.
                let points = if dead_lidar { 0 } else { 1024 };
                // The `av-unstamped` fault: a driver that publishes the epoch instead of the time it
                // sampled. Nothing else about the message changes, which is the point.
                let cloud_stamp = if unstamped_lidar { 0 } else { stamp };
                write_msg(
                    w,
                    channel,
                    i as u32,
                    t,
                    &point_cloud2_body(frame_id, points, i as u32, cloud_stamp),
                );
            } else {
                // A real header-first CDR body, so the sensor's coordinate frame is genuinely
                // decoded into the CDM (that is what the frame-resolution check reads). The trailing
                // payload varies per (sensor, frame) so frames stay content-distinct. Enough for the
                // `Image` reader, which takes the header and never the pixels.
                let payload = ((seq_base as u64) << 32) | i;
                write_msg(
                    w,
                    channel,
                    i as u32,
                    t,
                    &header_body(frame_id, stamp, payload),
                );
            }
        }
    }
}

/// A smooth periodic wave in `[-1, 1]`, built from arithmetic a machine cannot disagree about.
///
/// `f64::sin` and `f64::cos` are **not** guaranteed to give bit-identical results across platforms —
/// Rust defers them to the platform's libm, and macOS and Linux differ in the last unit in the last
/// place. That is invisible until the value is written into a message body and hashed: a
/// one-ULP difference changes the message's bytes, so its content fingerprint, so the CDM hash of
/// the whole recording. The demo rig then had a different content hash on every operating system,
/// which is intolerable in a fixture for a tool whose central claim is that a certificate binds to
/// the data and travels with it.
///
/// Multiplication, subtraction and `abs` are exact in IEEE-754 and identical everywhere, so this
/// parabolic wave — zero at `t = 0`, peaking at `t = ¼`, zero again at `t = ½` — is reproducible by
/// construction. It is not a sine, and does not need to be: its whole job is to give the IMU a
/// smooth, varying, non-degenerate signal for the statistical family to grade.
pub(crate) fn wave(t: f64) -> f64 {
    let frac = t - t.floor(); // one cycle, in [0, 1)
    let x = 2.0 * frac - 1.0; // in [-1, 1)
    4.0 * x * (1.0 - x.abs()) // parabolic, in [-1, 1]
}

/// A real CDR `sensor_msgs/msg/Imu` body: `Header`, then orientation, angular velocity and linear
/// acceleration, each followed by its nine-element covariance. A leading `-1` in a covariance is
/// ROS's "not provided"; these are zero, so every value is measured.
fn imu_body(phase: f64, stamp_ns: u64) -> Vec<u8> {
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
    let f64v = |buf: &mut Vec<u8>, v: f64| {
        align(buf, 8);
        buf.extend_from_slice(&v.to_le_bytes());
    };
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
    u32v(&mut buf, 9);
    buf.extend_from_slice(b"imu_link\0");
    // Level and driving straight: identity orientation, no rotation, 1 g down with a little sway.
    let group = |vs: &[f64], buf: &mut Vec<u8>| {
        for v in vs {
            f64v(buf, *v);
        }
        for _ in 0..9 {
            f64v(buf, 0.0);
        }
    };
    group(&[0.0, 0.0, 0.0, 1.0], &mut buf);
    group(&[0.0, 0.0, 0.02 * wave(phase)], &mut buf);
    group(&[0.1 * wave(phase + 0.25), 0.0, 9.81], &mut buf);
    buf
}

/// A real CDR `sensor_msgs/msg/NavSatFix` body: `Header`, `NavSatStatus { int8, uint16 }`, then
/// latitude, longitude and altitude as doubles, the covariance, and its type.
///
/// `status` is the receiver's own verdict — `0` is `STATUS_FIX`, `-1` is `STATUS_NO_FIX`. A driver
/// with no fix still fills the coordinate fields, with the last position it had, which is what makes
/// the outage invisible to everything but the status byte.
fn nav_sat_fix_body(
    status: i8,
    latitude: f64,
    longitude: f64,
    altitude: f64,
    stamp_ns: u64,
) -> Vec<u8> {
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
    let f64v = |buf: &mut Vec<u8>, v: f64| {
        align(buf, 8);
        buf.extend_from_slice(&v.to_le_bytes());
    };
    // Header { stamp { sec, nanosec }, frame_id }
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
    u32v(&mut buf, 5);
    buf.extend_from_slice(b"gnss\0");
    // NavSatStatus { int8 status, uint16 service = SERVICE_GPS (1) }
    buf.push(status as u8);
    align(&mut buf, 2);
    buf.extend_from_slice(&1u16.to_le_bytes());
    for v in [latitude, longitude, altitude] {
        f64v(&mut buf, v);
    }
    for _ in 0..9 {
        f64v(&mut buf, 0.0); // position_covariance
    }
    buf.push(0); // position_covariance_type = COVARIANCE_TYPE_UNKNOWN
    buf
}

/// A minimal header-first CDR body: `Header { stamp, frame_id }` followed by a varying `u64` so each
/// frame's bytes differ. Enough for the adapter to recover the sensor's coordinate frame without
/// pretending to encode a full `Image` / `PointCloud2` / `Imu` message.
fn header_body(frame_id: &str, stamp_ns: u64, payload: u64) -> Vec<u8> {
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
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
    u32v(&mut buf, (frame_id.len() + 1) as u32);
    buf.extend_from_slice(frame_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&payload.to_le_bytes());
    buf
}

/// A real `sensor_msgs/msg/CameraInfo` CDR body for the rig's front camera: `Header`, the declared
/// image `height`/`width`, the `distortion_model` and its coefficients, then the 3x3 intrinsic
/// matrix `k` (and the `r`/`p` matrices behind it, which Veridex does not read).
///
/// A rig without one is a rig whose LiDAR points cannot be projected into its image, so a demo that
/// omitted it could never show a calibrated rig — and never ran the intrinsics decode on the
/// flagship fixture, which is where the rules that read the declared resolution and the distortion
/// model live. `uncalibrated` writes what a driver publishes before anyone calibrates it: the model
/// named, the coefficients present, and every number zero — which satisfies every presence test and
/// is arithmetically unusable.
fn camera_info_body(frame_id: &str, stamp_ns: u64, uncalibrated: bool) -> Vec<u8> {
    // 1280x720 with a focal length and a principal point at the image centre: an ordinary pinhole.
    const WIDTH: u32 = 1280;
    const HEIGHT: u32 = 720;
    let (fx, fy, cx, cy) = if uncalibrated {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (1050.0, 1050.0, 640.0, 360.0)
    };
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
    let f64v = |buf: &mut Vec<u8>, v: f64| {
        align(buf, 8);
        buf.extend_from_slice(&v.to_le_bytes());
    };
    let strv = |buf: &mut Vec<u8>, s: &str| {
        align(buf, 4);
        buf.extend_from_slice(&((s.len() + 1) as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    };
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
    strv(&mut buf, frame_id);
    u32v(&mut buf, HEIGHT);
    u32v(&mut buf, WIDTH);
    // `plumb_bob` takes exactly five coefficients, which is what makes the pair checkable.
    strv(&mut buf, "plumb_bob");
    u32v(&mut buf, 5);
    for v in [-0.28, 0.07, 0.0, 0.0, 0.0] {
        f64v(&mut buf, v);
    }
    // k: [fx, 0, cx, 0, fy, cy, 0, 0, 1]
    for v in [fx, 0.0, cx, 0.0, fy, cy, 0.0, 0.0, 1.0] {
        f64v(&mut buf, v);
    }
    // r (3x3 rectification) and p (3x4 projection): written so the body is a whole `CameraInfo`
    // rather than a prefix of one. Veridex reads neither.
    for _ in 0..9 {
        f64v(&mut buf, 0.0);
    }
    for _ in 0..12 {
        f64v(&mut buf, 0.0);
    }
    buf
}

/// A complete `sensor_msgs/msg/PointCloud2` CDR body: `Header`, `height`/`width`, four
/// `PointField`s (`x`/`y`/`z`/`intensity` as float32), then `is_bigendian`, `point_step`, `row_step`
/// and the `data` blob.
///
/// The tail past the fields is what makes it a cloud rather than a prefix that looks like one: the
/// point-count decode checks the message's own invariants — `row_step` covers a row of `width`
/// points, `data` is `row_step × height` bytes and those bytes are present — precisely so a stubbed
/// body cannot be read as a real count. `seed` varies one byte of the payload so each sweep's
/// content hash differs, the way successive real sweeps do.
///
/// A `width` of 0 is an organized-cloud-shaped message holding nothing, which is what a driver that
/// lost its sensor publishes: it keeps the schema, the rate and the frame of a working LiDAR.
fn point_cloud2_body(frame_id: &str, width: u32, seed: u32, stamp_ns: u64) -> Vec<u8> {
    const POINT_STEP: u32 = 16; // x, y, z, intensity as float32
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
        align(buf, 4);
        buf.extend_from_slice(&((s.len() + 1) as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
    };
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
    strv(&mut buf, frame_id);
    u32v(&mut buf, 1); // height — an unorganized sweep is one row
    u32v(&mut buf, width);
    u32v(&mut buf, 4); // fields
    for (i, name) in ["x", "y", "z", "intensity"].iter().enumerate() {
        strv(&mut buf, name);
        u32v(&mut buf, i as u32 * 4); // offset
        buf.push(7); // datatype: FLOAT32
        u32v(&mut buf, 1); // count
    }
    buf.push(0); // is_bigendian
    u32v(&mut buf, POINT_STEP);
    let row_step = POINT_STEP * width;
    u32v(&mut buf, row_step);
    u32v(&mut buf, row_step); // data length: row_step * height, and height is 1
    let start = buf.len();
    buf.resize(start + row_step as usize, 0);
    if row_step > 0 {
        buf[start] = seed as u8;
    }
    buf
}

/// A `tf2_msgs/msg/TFMessage` CDR body holding one `TransformStamped` per `(parent, child)` edge,
/// each an identity transform — the demo cares about the tree's *shape*, not its geometry.
/// `offered_qos_profiles` as rosbag2 writes it for a latched (transient-local) publisher.
fn latched_qos() -> BTreeMap<String, String> {
    [(
        "offered_qos_profiles".to_string(),
        "- history: 3\n  depth: 1\n  reliability: 1\n  durability: 1\n".to_string(),
    )]
    .into_iter()
    .collect()
}

fn tf_message_body(edges: &[(&str, &str)], stamp_ns: u64) -> Vec<u8> {
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
    u32v(&mut buf, edges.len() as u32);
    for (parent, child) in edges {
        u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
        u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
        strv(&mut buf, parent); // header.frame_id = parent
        strv(&mut buf, child); // child_frame_id
                               // translation {x,y,z} + rotation {x,y,z,w} (identity)
        for v in [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
            f64v(&mut buf, v);
        }
    }
    buf
}

/// A `nav_msgs/msg/Odometry` CDR body (little-endian, ROS 2 default) whose pose sits at `x` metres
/// along +x, level and unrotated. Only the prefix Veridex reads is written: `Header`,
/// `child_frame_id`, then `pose.pose` — the covariance and twist that follow are not decoded.
fn odometry_body(x: f64, stamp_ns: u64) -> Vec<u8> {
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
    u32v(&mut buf, (stamp_ns / 1_000_000_000) as u32); // stamp.sec
    u32v(&mut buf, (stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
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
