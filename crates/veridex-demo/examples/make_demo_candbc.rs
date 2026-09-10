//! Write the demo CAN+DBC drive, recorded as a Vector BLF. See [`veridex_demo::candbc`] for what
//! each variant holds.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_candbc -- <output-dir> [drive|railed-wheel]`

use std::path::Path;

fn main() {
    let Some(out) = std::env::args().nth(1) else {
        eprintln!(
            "usage: make_demo_candbc <output-dir> [drive|railed-wheel]\n\
             then: veridex check <output-dir>"
        );
        std::process::exit(2);
    };
    let variant = std::env::args().nth(2).unwrap_or_else(|| "drive".into());
    let dir = Path::new(&out);
    if let Err(e) = veridex_demo::candbc::write(dir, &variant) {
        eprintln!("{e}");
        std::process::exit(2);
    }
    println!("wrote {} ({variant}): a .dbc and a .blf", dir.display());
    println!("try: veridex check {}", dir.display());
}
