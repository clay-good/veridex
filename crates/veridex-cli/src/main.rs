//! The `veridex` CLI.
//!
//! All commands are wired end-to-end over `veridex-core`: `check` / `inspect` ingest and validate,
//! `certify` / `verify` / `keygen` handle signed certificates, `provenance` emits Croissant / PROV,
//! and `diff` compares two reports. `check` and `certify` share the exact `run_check` pipeline the
//! Python bindings use, so their output is identical.
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
    sign, verify, Certificate, Issuance, ProvenanceCoverage, SignedCertificate, SigningKeypair,
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
        "diff",
        "compare two check reports (--json for machine output)",
    ),
    (
        "provenance",
        "emit extracted provenance (--emit croissant|prov)",
    ),
    (
        "inspect",
        "summarize the Canonical Dataset Model of a dataset",
    ),
    ("checks", "list the built-in check catalog"),
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
    emit: Option<String>,
    fail_on: Option<String>,
    min_score: Option<String>,
    sarif: bool,
    html: bool,
    config: Option<String>,
    force: bool,
}

fn parse_args(rest: &[String]) -> Args {
    let mut path = None;
    let mut format = None;
    let mut json = false;
    let mut key = None;
    let mut certificate = None;
    let mut out = None;
    let mut timestamp = None;
    let mut emit = None;
    let mut fail_on = None;
    let mut min_score = None;
    let mut sarif = false;
    let mut html = false;
    let mut config = None;
    let mut force = false;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--sarif" => sarif = true,
            "--html" => html = true,
            "--force" => force = true,
            "--config" => config = it.next().cloned(),
            "--format" => format = it.next().cloned(),
            "--key" => key = it.next().cloned(),
            "--certificate" => certificate = it.next().cloned(),
            "--out" => out = it.next().cloned(),
            "--timestamp" => timestamp = it.next().cloned(),
            "--emit" => emit = it.next().cloned(),
            "--fail-on" => fail_on = it.next().cloned(),
            "--min-score" => min_score = it.next().cloned(),
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
        emit,
        fail_on,
        min_score,
        sarif,
        html,
        config,
        force,
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
        Some("checks") => cmd_checks(&args[1..]),
        Some("certify") => cmd_certify(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("keygen") => cmd_keygen(&args[1..]),
        Some("diff") => cmd_diff(&args[1..]),
        Some("provenance") => cmd_provenance(&args[1..]),
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

/// Parse and range-check a `--min-score` value: an integer 0–100. Returns a human-readable error
/// (without the `veridex:` prefix) on anything else.
fn parse_min_score(v: &str) -> Result<u8, String> {
    match v.parse::<u8>() {
        Ok(n) if n <= 100 => Ok(n),
        _ => Err(format!(
            "invalid --min-score `{v}` (expected an integer 0-100)"
        )),
    }
}

fn cmd_check(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let Some(path) = &args.path else {
        eprintln!("veridex: missing dataset path");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };

    // Validate --fail-on up front: an unrecognized value must be an error, never a silent fallback
    // to the default threshold (a `--fail-on warn` typo would otherwise quietly disable strictness).
    if let Some(v) = args.fail_on.as_deref() {
        if v != "error" && v != "warning" {
            eprintln!("veridex: invalid --fail-on `{v}` (expected `error` or `warning`)");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    }

    // Validate --min-score up front: an out-of-range or non-numeric value must be a tool error, not
    // a silently-ignored gate that would let low-scoring data through CI.
    let min_score: Option<u8> = match args.min_score.as_deref().map(parse_min_score) {
        None => None,
        Some(Ok(n)) => Some(n),
        Some(Err(e)) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // Load config from --config, else auto-discover veridex.toml in the cwd.
    let config = match load_config(args.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    let out = match veridex_core::run_check_with(
        &registry,
        &source,
        args.format.as_deref(),
        &IngestOptions::default(),
        &config.to_run_config(),
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // Capture the score before rendering, which consumes `out.trust`.
    let trust_score = out.trust.score;

    if args.html {
        println!(
            "{}",
            veridex_core::render_html(&out.verdict, Some(out.trust))
        );
    } else if args.sarif {
        println!(
            "{}",
            serde_json::to_string_pretty(&veridex_core::render_sarif(&out.verdict)).unwrap()
        );
    } else if args.json {
        println!(
            "{}",
            veridex_core::render_json(&out.verdict, Some(out.trust))
        );
    } else {
        print!(
            "{}",
            veridex_core::render_terminal(&out.verdict, Some(out.trust), 10)
        );
    }

    // A trust score below --min-score fails the run regardless of finding severities, so CI can
    // enforce a minimum score directly. Reported on stderr so it is visible above the exit code.
    if let Some(min) = min_score {
        if trust_score < min {
            eprintln!("veridex: trust score {trust_score} is below the required minimum {min}");
            return ExitCode::from(EXIT_FAIL);
        }
    }

    // Failure threshold: --fail-on overrides the config, which defaults to `error`.
    let fail_on_warning = match args.fail_on.as_deref() {
        Some("warning") => true,
        Some(_) => false,
        None => config.fail_on == veridex_core::FailOn::Warning,
    };
    ExitCode::from(match out.verdict.status {
        Status::Pass => EXIT_PASS,
        Status::PassWithWarnings if fail_on_warning => EXIT_FAIL,
        Status::PassWithWarnings => EXIT_WARN,
        Status::Fail => EXIT_FAIL,
    })
}

/// Load config from an explicit path, or auto-discover `veridex.toml` in the current directory.
/// Returns the default config when neither is present.
fn load_config(explicit: Option<&str>) -> Result<veridex_core::CheckConfig, String> {
    let path = match explicit {
        Some(p) => Some(p.to_string()),
        None => {
            let default = "veridex.toml";
            std::path::Path::new(default)
                .is_file()
                .then(|| default.to_string())
        }
    };
    match path {
        None => Ok(veridex_core::CheckConfig::default()),
        Some(p) => {
            let text =
                std::fs::read_to_string(&p).map_err(|e| format!("cannot read config {p}: {e}"))?;
            veridex_core::CheckConfig::from_toml(&text).map_err(|e| e.to_string())
        }
    }
}

/// `veridex checks` — list the built-in check catalog (id, category, default severity, scope,
/// title), so users can discover what runs without validating a dataset. `--json` emits the
/// structured catalog.
fn cmd_checks(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let engine = match veridex_core::checks::default_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let catalog = engine.catalog();

    if args.json {
        match serde_json::to_string_pretty(&catalog) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("veridex: {e}");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        }
        return ExitCode::SUCCESS;
    }

    println!("{} built-in checks:", catalog.len());
    // Size the id column to the longest id so every row aligns regardless of id length.
    let id_w = catalog.iter().map(|c| c.id.len()).max().unwrap_or(0);
    for c in &catalog {
        println!(
            "  {:<id_w$} {:<11} {:<8} {:<8} {}",
            c.id,
            c.category.tag(),
            c.default_severity.tag(),
            c.scope.tag(),
            c.title,
        );
    }
    ExitCode::SUCCESS
}

/// Render a stream's declared dtype/shape as a trailing `, <dtype> [<dims>]` note for `inspect`.
/// Empty when the source declares neither (Veridex never infers a schema).
fn describe_schema(dtype: &Option<String>, shape: &Option<Vec<u64>>) -> String {
    let mut parts = Vec::new();
    if let Some(d) = dtype {
        parts.push(d.clone());
    }
    if let Some(s) = shape {
        let dims: Vec<String> = s.iter().map(|d| d.to_string()).collect();
        parts.push(format!("[{}]", dims.join(",")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(", {}", parts.join(" "))
    }
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
        let task = match &ep.task {
            Some(t) => format!("  task: \"{t}\""),
            None => String::new(),
        };
        println!(
            "  · episode {} — {} stream(s), {} frame(s){}",
            ep.index,
            ep.streams.len(),
            frames,
            task
        );
        for s in &ep.streams {
            println!(
                "      {} [{}] — {} frame(s), clock `{}`{}",
                s.name,
                s.modality.tag(),
                s.frames.len(),
                s.clock_id,
                describe_schema(&s.dtype, &s.shape),
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

    let Some(path) = &args.path else {
        eprintln!("veridex: missing dataset path");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };
    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    let out = match veridex_core::run_check(
        &registry,
        &source,
        args.format.as_deref(),
        &IngestOptions::default(),
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // Timestamp is caller-supplied (the core never reads a clock). Default to unix seconds.
    let timestamp = args.timestamp.clone().unwrap_or_else(unix_timestamp);
    let cert = Certificate::build(
        out.ingested.dataset.id.clone(),
        &out.verdict,
        out.trust.clone(),
        ProvenanceCoverage::of(&out.ingested.dataset),
        Issuance {
            key_id: keypair.public_hex(),
            timestamp,
        },
    );
    let signed = sign(cert, &keypair);

    let out_path = args
        .out
        .clone()
        .unwrap_or_else(|| format!("{}.veridex.json", out.ingested.dataset.id));
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

/// Resolve a `--key` argument to a trusted issuer public key. The argument is either a 64-char hex
/// key given inline, or a path to a file containing one. This is unambiguous: if the value is not
/// itself a 64-char hex string, it is treated as a file path — and a missing/unreadable file is a
/// clear tool error, never silently reinterpreted as a (bogus) key that would fail verification.
fn resolve_public_key(arg: &str) -> Result<String, String> {
    let trimmed = arg.trim();
    if is_hex_key(trimmed) {
        return Ok(trimmed.to_string());
    }
    match std::fs::read_to_string(arg) {
        Ok(s) => {
            let key = s.trim().to_string();
            if is_hex_key(&key) {
                Ok(key)
            } else {
                Err(format!(
                    "key file {arg} does not contain a 64-character hex public key"
                ))
            }
        }
        Err(e) => Err(format!("cannot read key {arg}: {e}")),
    }
}

/// A 64-character lowercase/uppercase hex string, i.e. an Ed25519 public key.
fn is_hex_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
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

    // Optional trusted issuer key: a 64-char hex public key, or a path to a file containing one.
    let expected_issuer = match &args.key {
        Some(k) => match resolve_public_key(k) {
            Ok(key) => Some(key),
            Err(e) => {
                eprintln!("veridex: {e}");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        },
        None => None,
    };

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
    let pub_path = format!("{path}.pub");
    // Never silently clobber an existing signing key: overwriting a secret key is unrecoverable and
    // invalidates every certificate it issued. Require --force to replace an existing key.
    if !args.force {
        for existing in [path.as_str(), pub_path.as_str()] {
            if std::path::Path::new(existing).exists() {
                eprintln!(
                    "veridex: {existing} already exists; refusing to overwrite a key. \
                     Choose another path or pass --force."
                );
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        }
    }
    let keypair = SigningKeypair::generate();
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

fn cmd_provenance(rest: &[String]) -> ExitCode {
    let args = parse_args(rest);
    let ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };
    let d = &ingested.dataset;
    let hash = veridex_core::content_hash(d).to_hex();

    let emit = args.emit.as_deref().unwrap_or("croissant");
    let doc = match emit {
        "croissant" => veridex_core::to_croissant(d, &hash),
        "prov" => veridex_core::to_prov(d),
        other => {
            eprintln!("veridex: unknown --emit `{other}` (expected `croissant` or `prov`)");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let json = match serde_json::to_string_pretty(&doc) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    match &args.out {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &json) {
                eprintln!("veridex: cannot write {path}: {e}");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
            println!("wrote {emit} to {path}");
        }
        None => println!("{json}"),
    }
    ExitCode::SUCCESS
}

fn cmd_diff(rest: &[String]) -> ExitCode {
    let json_out = rest.iter().any(|a| a == "--json");
    let paths: Vec<&String> = rest.iter().filter(|a| !a.starts_with('-')).collect();
    let [old_path, new_path] = paths.as_slice() else {
        eprintln!("veridex: diff requires two report files: veridex diff <old.json> <new.json>");
        return ExitCode::from(EXIT_TOOL_ERROR);
    };

    let parse = |p: &str| -> Result<serde_json::Value, ExitCode> {
        let bytes = std::fs::read(p).map_err(|e| {
            eprintln!("veridex: cannot read {p}: {e}");
            ExitCode::from(EXIT_TOOL_ERROR)
        })?;
        serde_json::from_slice(&bytes).map_err(|e| {
            eprintln!("veridex: {p} is not valid JSON: {e}");
            ExitCode::from(EXIT_TOOL_ERROR)
        })
    };
    let old = match parse(old_path) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let new = match parse(new_path) {
        Ok(v) => v,
        Err(c) => return c,
    };

    let diff = veridex_core::diff_reports(&old, &new);
    if json_out {
        let doc = serde_json::json!({
            "introduced": diff.introduced,
            "resolved": diff.resolved,
            "unchanged_count": diff.unchanged.len(),
            "score_delta": diff.score_delta(),
        });
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        print!("{}", veridex_core::render_diff(&diff));
    }
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
    println!(
        "    --json               machine-readable JSON output (check, inspect, diff, checks)"
    );
    println!("    --sarif              SARIF 2.1.0 output for CI code scanning (check)");
    println!("    --html               self-contained HTML report (check)");
    println!("    --key <file>         issuer secret key (certify) or trusted public key (verify)");
    println!("    --certificate <file> certificate to verify");
    println!("    --out <file>         certificate output path (certify)");
    println!("    --timestamp <ts>     issuance timestamp (certify; defaults to now)");
    println!("    --emit <fmt>         provenance format: croissant (default) or prov");
    println!(
        "    --fail-on <sev>      check failure threshold: error (default) or warning
    --min-score <0-100>  fail (exit 20) if the trust score is below this (check)"
    );
    println!("    --config <file>      veridex.toml (auto-discovered in cwd if present)");
    println!("    --force              overwrite existing key files (keygen)");
    println!("    --version            print the version");
    println!("    --help               print this help");
    println!();
    println!("EXIT CODES: 0 pass · 10 pass-with-warnings · 20 fail · 2 tool-error");
}

#[cfg(test)]
mod tests {
    use super::describe_schema;

    #[test]
    fn schema_note_renders_dtype_and_shape() {
        assert_eq!(
            describe_schema(&Some("float32".into()), &Some(vec![3, 480, 640])),
            ", float32 [3,480,640]"
        );
        assert_eq!(describe_schema(&None, &Some(vec![6])), ", [6]");
        assert_eq!(describe_schema(&Some("int64".into()), &None), ", int64");
    }

    #[test]
    fn schema_note_is_empty_when_nothing_declared() {
        assert_eq!(describe_schema(&None, &None), "");
    }

    #[test]
    fn hex_key_recognized_only_at_64_hex_chars() {
        assert!(super::is_hex_key(&"a".repeat(64)));
        assert!(super::is_hex_key(&"F".repeat(64)));
        assert!(!super::is_hex_key(&"a".repeat(63)));
        assert!(!super::is_hex_key(&"a".repeat(65)));
        assert!(!super::is_hex_key(&"g".repeat(64))); // non-hex char
        assert!(!super::is_hex_key("/tmp/issuer.pub"));
    }

    #[test]
    fn resolve_public_key_takes_inline_hex_without_touching_the_filesystem() {
        let hex = "b".repeat(64);
        assert_eq!(super::resolve_public_key(&hex).unwrap(), hex);
        // Surrounding whitespace is tolerated.
        assert_eq!(
            super::resolve_public_key(&format!("  {hex}\n")).unwrap(),
            hex
        );
    }

    #[test]
    fn min_score_accepts_0_to_100_and_rejects_the_rest() {
        assert_eq!(super::parse_min_score("0").unwrap(), 0);
        assert_eq!(super::parse_min_score("100").unwrap(), 100);
        assert_eq!(super::parse_min_score("82").unwrap(), 82);
        assert!(super::parse_min_score("101").is_err()); // above range
        assert!(super::parse_min_score("-1").is_err()); // negative
        assert!(super::parse_min_score("bad").is_err()); // non-numeric
        assert!(super::parse_min_score("").is_err()); // empty
    }

    #[test]
    fn resolve_public_key_errors_clearly_on_a_missing_file() {
        // A non-hex value is treated as a path; a missing path is a clear error, never a bogus key.
        let err = super::resolve_public_key("/no/such/issuer.pub").unwrap_err();
        assert!(err.contains("cannot read key"), "unexpected: {err}");
    }
}
