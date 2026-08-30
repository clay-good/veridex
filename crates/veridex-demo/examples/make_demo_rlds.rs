//! Write the demo RLDS/TFDS export. See [`veridex_demo::rlds`] for what each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_rlds -- <output-dir> [clean|truncated|desynced|corrupt]`

use std::path::Path;

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!(
            "usage: make_demo_rlds <output-dir> [clean|truncated|desynced|corrupt]\n\
             then: veridex check <output-dir>"
        );
        std::process::exit(2);
    };
    let variant = std::env::args().nth(2).unwrap_or_else(|| "clean".into());
    let dir = Path::new(&out);
    if let Err(e) = veridex_demo::rlds::write(dir, &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    println!(
        "wrote {} ({variant}): {} steps per episode",
        dir.display(),
        veridex_demo::rlds::STEPS_PER_EPISODE
    );
    println!("try: veridex check {}", dir.display());
}
