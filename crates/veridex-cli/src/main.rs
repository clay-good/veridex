//! The `veridex` CLI.
//!
//! Scaffold stage: the command surface is wired to a single core (so CLI and Python bindings stay
//! in lock-step, design D1), but the subcommands land incrementally as their core capabilities are
//! implemented. Today the binary reports its version and the planned command surface; unimplemented
//! subcommands exit non-zero rather than pretending to succeed.

use std::process::ExitCode;

const COMMANDS: &[(&str, &str)] = &[
    ("check", "validate a dataset and report findings"),
    ("certify", "issue a signed trust certificate"),
    (
        "verify",
        "verify a certificate offline against a public key",
    ),
    (
        "provenance",
        "extract provenance and emit Croissant / W3C PROV",
    ),
    (
        "inspect",
        "summarize the Canonical Dataset Model of a dataset",
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") | Some("version") => {
            println!("veridex {}", veridex_core::VERSION);
            ExitCode::SUCCESS
        }
        Some(cmd) if COMMANDS.iter().any(|(name, _)| *name == cmd) => {
            eprintln!("veridex: `{cmd}` is not implemented yet in this build.");
            eprintln!("See the roadmap in openspec/changes/bootstrap-veridex-mvp/tasks.md.");
            ExitCode::from(2)
        }
        Some(cmd) => {
            eprintln!("veridex: unknown command `{cmd}`.");
            print_help();
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!(
        "veridex {} — cross-format trust layer for physical-AI data",
        veridex_core::VERSION
    );
    println!();
    println!("USAGE:");
    println!("    veridex <command> [options]");
    println!();
    println!("COMMANDS:");
    for (name, desc) in COMMANDS {
        println!("    {name:<12} {desc}");
    }
    println!();
    println!("    --version    print the version");
    println!("    --help       print this help");
}
