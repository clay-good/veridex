//! Write the demo MCAP recording. See [`veridex_demo::mcap`] for what each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_mcap -- <output.mcap> [skew|clean|stuck|late-start|av|av-miscalibrated|av-ambiguous-tf|av-dead-lidar|av-truncated-lidar|av-unstamped|av-uncalibrated-camera|av-lossy-camera|av-no-fix]`

use std::path::Path;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo.mcap".to_string());
    let variant = std::env::args().nth(2).unwrap_or_else(|| "skew".into());
    if let Err(e) = veridex_demo::mcap::write(Path::new(&path), &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("wrote {path} ({bytes} bytes)");
}
