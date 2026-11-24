//! Write the demo KITTI raw drive. See [`veridex_demo::kitti`] for what each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_kitti -- <output-dir> [rig|dead-lidar|unsynced|null-island|no-fix|missing-samples|shrinking-camera|wrong-resolution]`

use std::path::Path;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-kitti".to_string());
    let variant = std::env::args().nth(2).unwrap_or_else(|| "rig".into());
    let dir = Path::new(&dir);
    if let Err(e) = veridex_demo::kitti::write(dir, &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    println!(
        "wrote {} — check it with `veridex check {}`",
        dir.display(),
        veridex_demo::kitti::drive_path(dir).display()
    );
}
