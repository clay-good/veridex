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
use veridex_core::certificate::{
    score, sign, verify, Certificate, Issuance, ProvenanceCoverage, SignedCertificate,
    SigningKeypair,
};
use veridex_core::engine::Status;

const EXIT_PASS: u8 = 0;
const EXIT_WARN: u8 = 10;
const EXIT_FAIL: u8 = 20;
const EXIT_TOOL_ERROR: u8 = 2;

const COMMANDS: &[(&str, &str)] = &[
    ("check", "validate a dataset and report findings"),
    (
        "certify",
        "issue a signed trust certificate (--key <secret>)",
    ),
    (
        "verify",
        "verify a certificate offline (--certificate <c.json>)",
    ),
    ("keygen", "generate an Ed25519 issuer keypair"),
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
    key: Option<String>,
    certificate: Option<String>,
    out: Option<String>,
    timestamp: Option<String>,
}

fn parse_args(rest: &[String]) -> Args {
    let mut path = None;
    let mut format = None;
    let mut json = false;
    let mut key = None;
    let mut certificate = None;
    let mut out = None;
    let mut timestamp = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--format" => format = it.next().cloned(),
            "--key" => key = it.next().cloned(),
            "--certificate" => certificate = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "--timestamp" => timestamp = it.next().cloned(),
            other if !other.starts_with('-') => path = Some(other.to_string()),
            _ => {}
        }
    }
    Args {
        path,
        format,
        json,
        key,
        certificate,
        out,
        timestamp,
    }
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
        Some("certify") => cmd_certify(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("keygen") => cmd_keygen(&args[1..]),
        Some("provenance") => {
            eprintln!("veridex: `provenance` is not implemented yet in this build.");
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

fn cmd_certify(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };

    let Some(key_path) = &args.key else {
        eprintln!("veridex: certify requires --key <secret-key-file>");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };
    let secret = match std::fs::read_to_string(key_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veridex: cannot read key {key_path}: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let Some(keypair) = SigningKeypair::from_secret_hex(&secret) else {
        eprintln!("veridex: {key_path} is not a valid 32-byte hex secret key");
        return ExitCode::from(EXIT_TOOL_ERROR);
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

    // Timestamp is caller-supplied (the core never reads a clock). Default to unix seconds.
    let timestamp = args.timestamp.clone().unwrap_or_else(unix_timestamp);
    let cert = Certificate::build(
        ingested.dataset.id.clone(),
        &verdict,
        trust,
        ProvenanceCoverage::of(&ingested.dataset),
        Issuance {
            key_id: keypair.public_hex(),
            timestamp,
        },
    );
    let signed = sign(cert, &keypair);

    let out_path = args
        .out
        .clone()
        .unwrap_or_else(|| format!("{}.veridex.json", ingested.dataset.id));
    let json = match serde_json::to_string_pretty(&signed) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    if let Err(e) = std::fs::write(&out_path, json) {
        eprintln!("veridex: cannot write {out_path}: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    println!(
        "certified {} — grade {} ({}), bound to {}",
        signed.certificate.dataset_id,
        signed.certificate.trust_score.grade.letter(),
        signed.certificate.trust_score.score,
        &signed.certificate.cdm_content_hash[..16]
    );
    println!("wrote {out_path}");
    ExitCode::SUCCESS
}

fn cmd_verify(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let Some(cert_path) = &args.certificate else {
        eprintln!("veridex: verify requires --certificate <cert.json>");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };
    let cert_json = match std::fs::read_to_string(cert_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veridex: cannot read {cert_path}: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let signed: SignedCertificate = match serde_json::from_str(&cert_json) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("veridex: {cert_path} is not a valid certificate: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // Bind to the presented dataset, if one was given.
    let presented_hash = if args.path.is_some() {
        match ingest(&args) {
            Ok(i) => Some(veridex_core::content_hash(&i.dataset).to_hex()),
            Err(code) => return code,
        }
    } else {
        None
    };

    // Optional trusted issuer key (a hex public key, or a file containing one).
    let expected_issuer = args.key.as_ref().map(|k| {
        std::fs::read_to_string(k)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| k.trim().to_string())
    });

    match verify(
        &signed,
        presented_hash.as_deref(),
        expected_issuer.as_deref(),
    ) {
        Ok(v) => {
            println!("✓ certificate verified");
            println!("  issuer key: {}", v.key_id);
            println!("  issued at:  {}", v.timestamp);
            println!("  dataset:    {}", signed.certificate.dataset_id);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ verification failed: {e}");
            ExitCode::from(EXIT_FAIL)
        }
    }
}

fn cmd_keygen(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let Some(path) = &args.path else {
        eprintln!("veridex: keygen requires an output path, e.g. `veridex keygen issuer`");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };
    let keypair = SigningKeypair::generate();
    let pub_path = format!("{path}.pub");
    if let Err(e) = std::fs::write(path, format!("{}\n", keypair.secret_hex())) {
        eprintln!("veridex: cannot write {path}: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    if let Err(e) = std::fs::write(&pub_path, format!("{}\n", keypair.public_hex())) {
        eprintln!("veridex: cannot write {pub_path}: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    println!("wrote secret key: {path}");
    println!("wrote public key: {pub_path}");
    println!("issuer key id:    {}", keypair.public_hex());
    ExitCode::SUCCESS
}

/// Seconds since the Unix epoch as a string. The CLI is the "caller" that supplies the time; the
/// core never reads a clock (design D6).
fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
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
    println!("    --format <fmt>       force an adapter (e.g. mcap) instead of autodetecting");
    println!("    --json               machine-readable JSON output (check, inspect)");
    println!("    --key <file>         issuer secret key (certify) or trusted public key (verify)");
    println!("    --certificate <file> certificate to verify");
    println!("    --out <file>         certificate output path (certify)");
    println!("    --timestamp <ts>     issuance timestamp (certify; defaults to now)");
    println!("    --version            print the version");
    println!("    --help               print this help");
    println!();
    println!("EXIT CODES: 0 pass · 10 pass-with-warnings · 20 fail · 2 tool-error");
}
