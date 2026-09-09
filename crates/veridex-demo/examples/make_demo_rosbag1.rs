//! Write the demo ROS 1 bag. See [`veridex_demo::rosbag1`] for what each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_rosbag1 -- <output.bag> [rig|lossy-camera]`

use std::path::Path;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo.bag".to_string());
    let variant = std::env::args().nth(2).unwrap_or_else(|| "rig".into());
    if let Err(e) = veridex_demo::rosbag1::write(Path::new(&path), &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("wrote {path} ({bytes} bytes)");
}
