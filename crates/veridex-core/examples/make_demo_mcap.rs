//! Generate a small demo MCAP recording for trying the CLI end-to-end.
//!
//! Writes a two-stream recording where the camera and robot clocks drift apart (a synthetic
//! `TEMPORAL.CLOCK_SKEW`), so `veridex check` has something to find.
//!
//! Usage: `cargo run -p veridex-core --example make_demo_mcap -- <output.mcap>`

use std::collections::BTreeMap;
use std::io::Cursor;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo.mcap".to_string());
    // Pass "clean" as the second arg for a single well-synchronized stream (no clock-skew error);
    // the default writes two streams whose clocks drift apart.
    let clean = std::env::args().nth(2).as_deref() == Some("clean");

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
            let t = i * 33_000_000; // 33 ms
            write_msg(&mut w, cam, i as u32, t);
        }

        if !clean {
            // Robot state at ~50 Hz but spanning ~1.20 s — a 200 ms clock drift vs the camera.
            let rob_schema = w
                .add_schema("sensor_msgs/msg/JointState", "ros2msg", b"")
                .unwrap();
            let rob = w
                .add_channel(rob_schema, "/joint_states", "cdr", &BTreeMap::new())
                .unwrap();
            for i in 0..61u64 {
                let t = i * 20_000_000; // 20 ms => 1.20 s total
                write_msg(&mut w, rob, i as u32, t);
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
) {
    w.write_to_known_channel(
        &mcap::records::MessageHeader {
            channel_id,
            sequence,
            log_time,
            publish_time: log_time,
        },
        b"payload",
    )
    .expect("write message");
}
