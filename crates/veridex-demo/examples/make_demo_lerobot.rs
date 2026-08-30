//! Write the demo LeRobot v3 dataset. See [`veridex_demo::lerobot`] for what each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_lerobot -- <output-dir> [clean|truncated|…]`

use std::path::Path;

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "demo-lerobot".to_string());
    let variant = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "non-monotonic".into());
    let dir = Path::new(&out);
    let what = match veridex_demo::lerobot::describe(&variant) {
        Ok(what) => what,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = veridex_demo::lerobot::write(dir, &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    println!("Wrote {what} LeRobot v3 dataset to {}", dir.display());
    println!("Try:  veridex check {}", dir.display());
}
