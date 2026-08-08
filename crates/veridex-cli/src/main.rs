//! The `veridex` CLI.
//!
//! `check` and `inspect` are wired end-to-end over the core: ingest a dataset into the CDM, run the
//! standard checks, score it, and report. `certify`, `verify`, and `provenance` land as their core
//! capabilities (signing, Croissant emit) are implemented.
//!
//! Exit codes (documented, CI-friendly):
//! - `0`  pass
//! - `10` pass with warnings
//! - `20` fail (one or more errors)
//! - `2`  tool error (bad usage, unsupported/ambiguous format, ingest failure)

use std::path::PathBuf;
use std::process::ExitCode;

use veridex_core::adapter::{IngestOptions, Source};
use veridex_core::certificate::{score, ProvenanceCoverage};
use veridex_core::engine::Status;

const EXIT_PASS: u8 = 0;
const EXIT_WARN: u8 = 10;
const EXIT_FAIL: u8 = 20;
const EXIT_TOOL_ERROR: u8 = 2;

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

/// Parsed command line for the data-consuming commands.
struct Args {
    path: Option<String>,
    format: Option<String>,
    json: bool,
}

fn parse_args(rest: &[String]) -> Args {
    let mut path = None;
    let mut format = None;
    let mut json = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--format" => format = it.next().cloned(),
            other if !other.starts_with('-') => path = Some(other.to_string()),
            _ => {}
        }
    }
    Args { path, format, json }
}

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
        Some("check") => cmd_check(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        Some(cmd @ ("certify" | "verify" | "provenance")) => {
            eprintln!("veridex: `{cmd}` is not implemented yet in this build.");
            eprintln!("See openspec/changes/bootstrap-veridex-mvp/tasks.md for the roadmap.");
            ExitCode::from(EXIT_TOOL_ERROR)
        }
        Some(cmd) => {
            eprintln!("veridex: unknown command `{cmd}`.");
            print_help();
            ExitCode::from(EXIT_TOOL_ERROR)
        }
    }
}

/// Ingest the dataset named by `args`, autodetecting or honoring `--format`.
fn ingest(args: &Args) -> Result<veridex_core::Ingested, ExitCode> {
    let Some(path) = &args.path else {
        eprintln!("veridex: missing dataset path");
        return Err(ExitCode::from(EXIT_TOOL_ERROR));
    };
    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    let opts = IngestOptions::default();
    let result = match &args.format {
        Some(fmt) => registry.ingest_as(fmt, &source, &opts),
        None => registry.ingest(&source, &opts),
    };
    result.map_err(|e| {
        eprintln!("veridex: {e}");
        ExitCode::from(EXIT_TOOL_ERROR)
    })
}

fn cmd_check(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };

    let engine = match veridex_core::checks::default_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("veridex: internal error building checks: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    let trust = score(&verdict, &ProvenanceCoverage::of(&ingested.dataset));

    if args.json {
        println!("{}", veridex_core::render_json(&verdict, Some(trust)));
    } else {
        print!(
            "{}",
            veridex_core::render_terminal(&verdict, Some(trust), 10)
        );
    }

    ExitCode::from(match verdict.status {
        Status::Pass => EXIT_PASS,
        Status::PassWithWarnings => EXIT_WARN,
        Status::Fail => EXIT_FAIL,
    })
}

fn cmd_inspect(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };
    let d = &ingested.dataset;

    if args.json {
        // Reuse the CDM's own serialization for a machine-readable summary.
        match serde_json::to_string_pretty(d) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("veridex: {e}");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        }
        return ExitCode::SUCCESS;
    }

    println!("Dataset: {}", d.id);
    println!("  format:   {}", ingested.report.format_id);
    println!("  CDM hash: {}", veridex_core::content_hash(d));
    println!("  episodes: {}", d.episodes.len());
    for ep in &d.episodes {
        let frames: usize = ep.streams.iter().map(|s| s.frames.len()).sum();
        println!(
            "  · episode {} — {} stream(s), {} frame(s)",
            ep.index,
            ep.streams.len(),
            frames
        );
        for s in &ep.streams {
            println!(
                "      {} [{}] — {} frame(s), clock `{}`",
                s.name,
                s.modality.tag(),
                s.frames.len(),
                s.clock_id
            );
        }
    }
    if !ingested.report.unmapped_fields.is_empty() || !ingested.report.omitted_fields.is_empty() {
        println!("  coverage notes:");
        for u in &ingested.report.unmapped_fields {
            println!("      unmapped: {} ({})", u.source_path, u.note);
        }
        for o in &ingested.report.omitted_fields {
            println!("      omitted:  {o}");
        }
    }
    ExitCode::SUCCESS
}

fn print_help() {
    println!(
        "veridex {} — cross-format trust layer for physical-AI data",
        veridex_core::VERSION
    );
    println!();
    println!("USAGE:");
    println!("    veridex <command> [options] <dataset>");
    println!();
    println!("COMMANDS:");
    for (name, desc) in COMMANDS {
        println!("    {name:<12} {desc}");
    }
    println!();
    println!("OPTIONS:");
    println!("    --format <fmt>   force an adapter (e.g. mcap) instead of autodetecting");
    println!("    --json           machine-readable JSON output");
    println!("    --version        print the version");
    println!("    --help           print this help");
    println!();
    println!("EXIT CODES: 0 pass · 10 pass-with-warnings · 20 fail · 2 tool-error");
}
