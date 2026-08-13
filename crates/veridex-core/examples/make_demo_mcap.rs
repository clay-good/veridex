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
//!
//! Each message's payload bytes vary per frame (so frames are content-distinct, as real recordings
//! are) except in `stuck`, where the camera deliberately repeats one frame.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_mcap -- <output.mcap> [clean|late-start|stuck]`

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

    let mut buf = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut buf)).expect("writer");

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
            write_msg(&mut w, cam, i as u32, t, &payload.to_le_bytes());
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
                    write_msg(&mut w, rob, i as u32, t, &i.to_le_bytes());
                }
            } else {
                // Robot state at ~50 Hz spanning ~1.20 s — a 200 ms clock drift vs the camera.
                for i in 0..61u64 {
                    let t = i * 20_000_000; // 20 ms => 1.20 s total
                    write_msg(&mut w, rob, i as u32, t, &i.to_le_bytes());
                }
            }
        }

        // Producer-written provenance: a Metadata record and a calibration attachment, which the
        // adapter surfaces (license/sensor/operator → typed provenance, calibration from the file).
        let mut meta = std::collections::BTreeMap::new();
        meta.insert("license".to_string(), "CC-BY-4.0".to_string());
        meta.insert("sensor".to_string(), "ZED2i stereo camera".to_string());
        meta.insert("operator".to_string(), "demo-operator".to_string());
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
