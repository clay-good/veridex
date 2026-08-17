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

use veridex_core::adapter::{IngestOptions, Sample, Source};
use veridex_core::certificate::{
    sign, verify, Certificate, Issuance, ProvenanceCoverage, ReadinessReport, SignedCertificate,
    SigningKeypair,
};
use veridex_core::engine::Status;

const EXIT_PASS: u8 = 0;
const EXIT_WARN: u8 = 10;
const EXIT_FAIL: u8 = 20;
const EXIT_TOOL_ERROR: u8 = 2;

const COMMANDS: &[(&str, &str)] = &[
    (
        "check",
        "validate a dataset and report findings (--max-frames <n> / --max-decompression-ratio <n> raise the ingest ceilings; --sample-episodes / --sample-fraction check a subset)",
    ),
    (
        "certify",
        "issue a signed trust certificate (--key <secret>; --profile world-model-ready)",
    ),
    (
        "verify",
        "verify a certificate offline (--certificate <c.json> --key <pub>; --json for machine output)",
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
    allow_any_issuer: bool,
    max_frames: Option<String>,
    max_decompression_ratio: Option<String>,
    sample_episodes: Option<String>,
    sample_fraction: Option<String>,
    sample_seed: Option<String>,
    metadata_only: bool,
    profile: Option<String>,
    fail_on_regression: bool,
    /// Every positional argument, in order. `path` is the first; `diff` takes two.
    positionals: Vec<String>,
}

impl Args {
    /// Whether any sampling flag was given. `certify` refuses these with its own explanation.
    fn any_sampling_flag(&self) -> bool {
        self.sample_episodes.is_some()
            || self.sample_fraction.is_some()
            || self.sample_seed.is_some()
    }

    /// Every flag the parser knows, paired with whether this invocation gave it.
    ///
    /// The single source of truth for [`reject_flags_except`]. Adding a flag to the parser without
    /// adding it here means it is never checked, so the two lists are kept adjacent, and a test
    /// asserts this covers the parser's whole flag set.
    fn given_flags(&self) -> [(&'static str, bool); 22] {
        [
            ("--json", self.json),
            ("--sarif", self.sarif),
            ("--html", self.html),
            ("--force", self.force),
            ("--allow-any-issuer", self.allow_any_issuer),
            ("--fail-on-regression", self.fail_on_regression),
            ("--config", self.config.is_some()),
            ("--format", self.format.is_some()),
            ("--key", self.key.is_some()),
            ("--certificate", self.certificate.is_some()),
            ("--out", self.out.is_some()),
            ("--timestamp", self.timestamp.is_some()),
            ("--emit", self.emit.is_some()),
            ("--fail-on", self.fail_on.is_some()),
            ("--min-score", self.min_score.is_some()),
            ("--profile", self.profile.is_some()),
            ("--max-frames", self.max_frames.is_some()),
            (
                "--max-decompression-ratio",
                self.max_decompression_ratio.is_some(),
            ),
            ("--sample-episodes", self.sample_episodes.is_some()),
            ("--sample-fraction", self.sample_fraction.is_some()),
            ("--sample-seed", self.sample_seed.is_some()),
            ("--metadata-only", self.metadata_only),
        ]
    }
}

/// The flags every command that ingests a dataset honors, on top of its own.
const INGEST_FLAGS: &[&str] = &["--format", "--max-frames", "--max-decompression-ratio"];

/// The sampling flags, honored only where a partial verdict is meaningful (`check`, `inspect`).
const SAMPLING_FLAGS: &[&str] = &["--sample-episodes", "--sample-fraction", "--sample-seed"];

/// The metadata-only flag, honored alongside sampling wherever a partial verdict is meaningful
/// (`check`, `inspect`). Separate from [`SAMPLING_FLAGS`] because it is a different kind of partial:
/// every episode, none of the data — rather than some episodes, all of their data.
const METADATA_ONLY_FLAG: &[&str] = &["--metadata-only"];

/// Parse the shared flag set. Rejects unknown `--`-flags and value-flags whose value is missing or
/// looks like another flag, so a typo can never silently disable a gate (e.g. `--min-scor 90` would
/// otherwise drop the score threshold) nor swallow the next flag as a value (`--key --format`).
fn parse_args(rest: &[String]) -> Result<Args, String> {
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
    let mut allow_any_issuer = false;
    let mut max_frames = None;
    let mut max_decompression_ratio = None;
    let mut sample_episodes = None;
    let mut sample_fraction = None;
    let mut sample_seed = None;
    let mut metadata_only = false;
    let mut profile = None;
    let mut fail_on_regression = false;
    let mut positionals: Vec<String> = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        // Take the value for a value-flag, rejecting a missing value or one that starts with `-`
        // (which is almost always the next flag, accidentally swallowed).
        let mut value = |flag: &str| -> Result<String, String> {
            match it.next() {
                Some(v) if !v.starts_with('-') => Ok(v.clone()),
                // A negative number is a value the user meant, not the next flag swallowed — and
                // "--max-frames requires a value" pointed at the wrong problem for `--max-frames -5`.
                // It is still rejected, by the parser that knows what the flag accepts.
                Some(v)
                    if v.len() > 1 && v[1..].chars().all(|c| c.is_ascii_digit() || c == '.') =>
                {
                    Ok(v.clone())
                }
                _ => Err(format!("{flag} requires a value")),
            }
        };
        match arg.as_str() {
            "--json" => json = true,
            "--sarif" => sarif = true,
            "--html" => html = true,
            "--force" => force = true,
            "--allow-any-issuer" => allow_any_issuer = true,
            "--fail-on-regression" => fail_on_regression = true,
            "--metadata-only" => metadata_only = true,
            "--config" => config = Some(value("--config")?),
            "--format" => format = Some(value("--format")?),
            "--key" => key = Some(value("--key")?),
            "--certificate" => certificate = Some(value("--certificate")?),
            "--out" => out = Some(value("--out")?),
            "--timestamp" => timestamp = Some(value("--timestamp")?),
            "--emit" => emit = Some(value("--emit")?),
            "--fail-on" => fail_on = Some(value("--fail-on")?),
            "--min-score" => min_score = Some(value("--min-score")?),
            "--profile" => profile = Some(value("--profile")?),
            "--max-frames" => max_frames = Some(value("--max-frames")?),
            "--max-decompression-ratio" => {
                max_decompression_ratio = Some(value("--max-decompression-ratio")?)
            }
            "--sample-episodes" => sample_episodes = Some(value("--sample-episodes")?),
            "--sample-fraction" => sample_fraction = Some(value("--sample-fraction")?),
            "--sample-seed" => sample_seed = Some(value("--sample-seed")?),
            other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
            other => positionals.push(other.to_string()),
        }
    }
    let path = positionals.first().cloned();
    Ok(Args {
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
        allow_any_issuer,
        max_frames,
        max_decompression_ratio,
        sample_episodes,
        sample_fraction,
        sample_seed,
        metadata_only,
        profile,
        fail_on_regression,
        positionals,
    })
}

/// Reject every flag the user gave that `command` does not act on.
///
/// The shared parser accepts one flag set for every command, so without this a command silently
/// tolerates flags it has no use for — `inspect --min-score 90` looks like a gate and is not one, and
/// `check --out report.json` looks like it writes a file and does not. Naming the flag is better than
/// ignoring it: the user asked for something that was not going to happen.
///
/// `supported` is an **allow-list**, deliberately. The earlier deny-list form had to be extended by
/// hand every time a flag was added to the parser, and every miss was silent by construction — the
/// failure mode was a flag that did nothing, which is the exact thing this function exists to prevent.
/// Inverted, a flag missing from a command's list is rejected, which is loud and trivially fixed.
/// Reject surplus positional arguments for a command that acts on exactly one.
///
/// The parser was meticulous about flags — unknown ones, unsupported ones, ones missing their value
/// — and then took `positionals.first()` and dropped the rest without a word. The shell makes that
/// an easy mistake to make and an expensive one to miss: `veridex check datasets/*.mcap` expands to
/// several paths, checks only the first, and exits 0 on it while never opening the rest. A CI job
/// reads that as "all my datasets passed". `veridex keygen k1 k2` writes one keypair and silently
/// ignores the second name.
///
/// Same treatment as an unsupported flag, for the same reason: the user asked for something that was
/// not going to happen, and naming it beats ignoring it. `diff` already validated its own two-path
/// form; this is the one-path equivalent.
fn reject_extra_positionals(command: &str, args: &Args, noun: &str) -> Result<(), ExitCode> {
    if args.positionals.len() > 1 {
        eprintln!(
            "veridex: {command} takes one {noun} (got {}: {})",
            args.positionals.len(),
            args.positionals.join(", ")
        );
        return Err(ExitCode::from(EXIT_TOOL_ERROR));
    }
    Ok(())
}

fn reject_flags_except(command: &str, args: &Args, supported: &[&[&str]]) -> Result<(), ExitCode> {
    for (flag, given) in args.given_flags() {
        if given && !supported.iter().any(|group| group.contains(&flag)) {
            eprintln!("veridex: {command} does not support {flag}");
            return Err(ExitCode::from(EXIT_TOOL_ERROR));
        }
    }
    Ok(())
}

/// Parse the shared flags or print the error and return a tool-error exit code. Used by every
/// data-consuming command so a bad flag fails loudly and identically everywhere.
fn parse_args_or_exit(rest: &[String]) -> Result<Args, ExitCode> {
    parse_args(rest).map_err(|e| {
        eprintln!("veridex: {e}");
        ExitCode::from(EXIT_TOOL_ERROR)
    })
}

/// Put `SIGPIPE` back to the default disposition Rust's runtime turns off.
///
/// Rust ignores `SIGPIPE` so a write to a closed pipe returns `EPIPE`, and `println!` turns that
/// into a panic -- so `veridex checks | head -5`, or quitting `less` partway through a report,
/// aborted with a Rust backtrace and exit 101. Neither is in the documented `0`/`10`/`20`/`2`
/// contract, and both are ordinary usage. With the default restored the process dies silently on
/// the signal, the way every other command-line tool does.
#[cfg(unix)]
fn restore_default_sigpipe() {
    // SAFETY: `signal` with `SIG_DFL` on `SIGPIPE` is async-signal-safe and is called once, before
    // any thread is spawned or any output is written.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

fn main() -> ExitCode {
    restore_default_sigpipe();
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

/// The sampling request `args` asks for.
///
/// `--sample-episodes` and `--sample-fraction` are mutually exclusive — accepting both would mean
/// silently picking one, and the user would not know which. `--sample-seed` on its own selects
/// nothing, so it is refused rather than ignored.
fn sample_from(args: &Args) -> Result<Sample, String> {
    match (&args.sample_episodes, &args.sample_fraction) {
        (Some(_), Some(_)) => {
            Err("--sample-episodes and --sample-fraction cannot both be given".into())
        }
        (Some(n), None) => {
            if args.sample_seed.is_some() {
                return Err(
                    "--sample-seed applies to --sample-fraction; --sample-episodes is not a random \
                     draw"
                        .into(),
                );
            }
            let n: u64 = n.parse().map_err(|_| {
                format!("invalid --sample-episodes `{n}` (expected a positive integer)")
            })?;
            Ok(Sample::FirstEpisodes(n))
        }
        (None, Some(f)) => {
            let fraction: f64 = f.parse().map_err(|_| {
                format!("invalid --sample-fraction `{f}` (expected a number in (0, 1])")
            })?;
            let seed: u64 = match &args.sample_seed {
                None => 0,
                Some(s) => s
                    .parse()
                    .map_err(|_| format!("invalid --sample-seed `{s}` (expected an integer)"))?,
            };
            Ok(Sample::Fraction { fraction, seed })
        }
        (None, None) => {
            if args.sample_seed.is_some() {
                return Err("--sample-seed requires --sample-fraction".into());
            }
            Ok(Sample::All)
        }
    }
}

/// The ingest options `args` asks for. `0` removes the ceiling on either budget; anything else must
/// be a positive integer, so a typo can never silently disable a guard.
fn ingest_options(args: &Args) -> Result<IngestOptions, String> {
    /// Parse one budget flag: absent keeps the default, `0` removes the limit.
    fn budget(
        flag: &str,
        given: Option<&str>,
        default: Option<u64>,
    ) -> Result<Option<u64>, String> {
        match given {
            None => Ok(default),
            Some("0") => Ok(None),
            Some(v) => v.parse::<u64>().map(Some).map_err(|_| {
                format!("invalid {flag} `{v}` (expected a positive integer, or 0 for no limit)")
            }),
        }
    }
    let defaults = IngestOptions::default();
    Ok(IngestOptions {
        metadata_only: args.metadata_only,
        sample: sample_from(args)?,
        max_frames: budget(
            "--max-frames",
            args.max_frames.as_deref(),
            defaults.max_frames,
        )?,
        max_decompression_ratio: budget(
            "--max-decompression-ratio",
            args.max_decompression_ratio.as_deref(),
            defaults.max_decompression_ratio,
        )?,
    })
}

/// Ingest the dataset named by `args`, autodetecting or honoring `--format`.
fn ingest(args: &Args) -> Result<veridex_core::Ingested, ExitCode> {
    let Some(path) = &args.path else {
        eprintln!("veridex: missing dataset path");
        return Err(ExitCode::from(EXIT_TOOL_ERROR));
    };
    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    let opts = match ingest_options(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return Err(ExitCode::from(EXIT_TOOL_ERROR));
        }
    };
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
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // `check` neither signs nor emits: the certificate and provenance flags have no effect here.
    if let Err(code) = reject_flags_except(
        "check",
        &args,
        &[
            &[
                "--json",
                "--sarif",
                "--html",
                "--config",
                "--fail-on",
                "--min-score",
                "--profile",
            ],
            INGEST_FLAGS,
            SAMPLING_FLAGS,
            METADATA_ONLY_FLAG,
        ],
    ) {
        return code;
    }
    if let Err(code) = reject_extra_positionals("check", &args, "dataset path") {
        return code;
    }
    // One run writes one report. The dispatch below is an if/else chain, so `--json --sarif` emitted
    // SARIF and dropped `--json` without a word -- a CI job doing `check --json --sarif > report.json`
    // silently got the wrong document, and `veridex diff` then rejected it as not a Veridex report.
    // Silently ignoring a flag is exactly what `reject_flags_except` exists to prevent.
    let formats: Vec<&str> = [
        ("--json", args.json),
        ("--sarif", args.sarif),
        ("--html", args.html),
    ]
    .into_iter()
    .filter(|(_, given)| *given)
    .map(|(name, _)| name)
    .collect();
    if formats.len() > 1 {
        eprintln!(
            "veridex: {} cannot be combined; one run writes one report",
            formats.join(" and ")
        );
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
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
    let cli_min_score: Option<u8> = match args.min_score.as_deref().map(parse_min_score) {
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

    // Reject a config that names checks that don't exist (a typo in disabled_checks or a severity
    // override would otherwise silently no-op), before running anything.
    let engine = match veridex_core::checks::default_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    if let Err(e) = config.validate_check_ids(engine.check_ids()) {
        eprintln!("veridex: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }

    // A named profile applies its own (tighter) tolerances to the run, exactly as `certify` does.
    // Accepting the flag and ignoring it meant `check --profile world-model-ready` silently judged the
    // data at the looser defaults, and an unknown profile name passed without a word.
    let profile = match args.profile.as_deref() {
        None => None,
        Some(name) => match veridex_core::profile::by_name(name) {
            Some(p) => Some(p),
            None => {
                eprintln!("veridex: unknown profile `{name}` (known: world-model-ready)");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        },
    };

    // The CLI flag overrides the config's min_score (which defaults to no gate).
    let min_score = cli_min_score.or(config.min_score);

    let ingest_opts = match ingest_options(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    // The thresholds the profile *names* win over the config's: a readiness judgement is only
    // meaningful at those. The ones it does not name stay as the operator configured them — see
    // `Profile::apply_tolerances`, which is why this is not a whole-struct assignment.
    let mut run_config = config.to_run_config();
    if let Some(p) = &profile {
        run_config.tolerances = p.apply_tolerances(run_config.tolerances);
    }
    let out = match veridex_core::run_check_with(
        &registry,
        &source,
        args.format.as_deref(),
        &ingest_opts,
        &run_config,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // Capture the score before rendering, which consumes `out.trust`.
    let trust_score = out.trust.score;

    // The readiness verdict, when a profile was named. Rendered for *every* output shape, not only
    // the terminal one: a profile is what the run is "judged against", and a CI consumer -- which
    // is precisely who reads --json, --sarif and --html -- was given the profile's tolerances and
    // none of its criterion verdicts, so the one thing the flag names was unreportable to a machine.
    let readiness = profile
        .as_ref()
        .map(|p| ReadinessReport::evaluate(p, &out.verdict, &out.ingested.dataset));

    if args.html {
        println!(
            "{}",
            veridex_core::render_html_with_readiness(
                &out.verdict,
                Some(out.trust),
                readiness.as_ref()
            )
        );
    } else if args.sarif {
        println!(
            "{}",
            serde_json::to_string_pretty(&veridex_core::render_sarif(&out.verdict)).unwrap()
        );
    } else if args.json {
        println!(
            "{}",
            veridex_core::render_json_with_readiness(
                &out.verdict,
                Some(out.trust),
                readiness.as_ref()
            )
        );
    } else {
        print!(
            "{}",
            veridex_core::render_terminal(&out.verdict, Some(out.trust), 10)
        );
        // `--help` says a profile is what the run is "judged against", and until now `check` only
        // borrowed its tolerances — it printed no criterion verdicts at all, so the one thing the
        // flag names was the one thing it did not report. `certify` renders the same block from the
        // same helper; the difference is that this one is not signed.
        if let Some(readiness) = &readiness {
            print!("\n{}", veridex_core::render_readiness(readiness, "  "));
        }
    }

    // A score gate over a run that read no data is not a gate. Under `--metadata-only` the data
    // axis is computed from checks that overwhelmingly had nothing to measure, so it lands near 100
    // whatever the data holds — and `--min-score 90 --metadata-only` is then a one-flag way to
    // satisfy the gate on a dataset whose values are garbage. A *sample* is different in kind: it
    // scores real data, just less of it, so it keeps working. Refused rather than silently ignored,
    // because a gate that quietly does nothing is worse than one that is absent.
    if min_score.is_some() && !out.verdict.coverage.frames_read() {
        eprintln!(
            "veridex: --min-score cannot gate a --metadata-only run — its data score is computed \
             over checks that had no data to measure, so it says nothing about the dataset"
        );
        return ExitCode::from(EXIT_TOOL_ERROR);
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
    ExitCode::from(exit_code_for_status(out.verdict.status, fail_on_warning))
}

/// Map a verdict status to a CI exit code under the configured failure threshold: `0` pass, `10`
/// pass-with-warnings, `20` fail. When `fail_on_warning` is set, warnings escalate to the fail code.
/// This is the CI contract, kept as a pure function so it is unit-tested directly.
fn exit_code_for_status(status: Status, fail_on_warning: bool) -> u8 {
    match status {
        Status::Pass => EXIT_PASS,
        Status::PassWithWarnings if fail_on_warning => EXIT_FAIL,
        Status::PassWithWarnings => EXIT_WARN,
        Status::Fail => EXIT_FAIL,
    }
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
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // `checks` reads no dataset and runs nothing; only the output format applies.
    if let Err(code) = reject_flags_except("checks", &args, &[&["--json"]]) {
        return code;
    }
    let engine = match veridex_core::checks::default_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let catalog = engine.catalog();

    if args.json {
        println!("{}", veridex_core::render_catalog_json(&catalog));
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
        // The finding codes this check can emit, indented under it.
        println!("  {:<id_w$} {}", "", c.finding_codes.join(", "));
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
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // `inspect` runs no checks, so nothing about gating, scoring, or signing applies to it.
    if let Err(code) = reject_flags_except(
        "inspect",
        &args,
        &[
            &["--json"],
            INGEST_FLAGS,
            SAMPLING_FLAGS,
            METADATA_ONLY_FLAG,
        ],
    ) {
        return code;
    }
    if let Err(code) = reject_extra_positionals("inspect", &args, "dataset path") {
        return code;
    }
    let mut ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };
    // Canonicalize before rendering: the content hash treats these collections as sets, so two
    // ingests that hash identically must also render identically.
    ingested.dataset.canonicalize_order();
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
    // A sampled inspect describes a subset. Said up front, next to the hash it produced, so the
    // summary below is not read as the shape of the whole dataset.
    match &ingested.report.coverage {
        veridex_core::Coverage::Full => {}
        veridex_core::Coverage::Sample { sample, .. } => {
            println!(
                "  coverage: SAMPLE — {} (this summary covers only the episodes listed below)",
                sample.describe()
            );
        }
        // Without this the listing reads "episode 0 — 2 stream(s), 0 frame(s)" for every episode,
        // which is indistinguishable from a dataset whose episodes are genuinely empty — the exact
        // defect `STRUCTURAL.EMPTY_STREAM` exists to report. The zeros are the request, not the data.
        veridex_core::Coverage::MetadataOnly { .. } => {
            println!(
                "  coverage: METADATA-ONLY — no stream payload was read, so every frame count \
                 below is 0 by request, not by defect"
            );
        }
    }
    println!("  episodes: {}", d.episodes.len());
    for ep in &d.episodes {
        let frames: usize = ep.streams.iter().map(|s| s.frames.len()).sum();
        let span = match ep.duration_ns() {
            Some(ns) => format!(", {:.3}s", ns as f64 / 1e9),
            None => String::new(),
        };
        let task = match &ep.task {
            Some(t) => format!("  task: \"{t}\""),
            None => String::new(),
        };
        println!(
            "  · episode {} — {} stream(s), {} frame(s){}{}",
            ep.index,
            ep.streams.len(),
            frames,
            span,
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
    print!("{}", provenance_summary(d));
    print!("{}", veridex_core::simref::render_references(d));
    print!("{}", veridex_core::scenario::render_coverage(d));
    ExitCode::SUCCESS
}

/// Render the dataset's provenance coverage: the count known/asserted/unknown (the certificate's 30%
/// axis) followed by each expected element's real value and class, or `missing`. Placeholder values
/// don't count and are shown as missing, matching how the score treats them.
fn provenance_summary(d: &veridex_core::cdm::Dataset) -> String {
    use std::fmt::Write;
    use veridex_core::cdm::ProvenanceClass;
    let cov = veridex_core::ProvenanceCoverage::of(d);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "  provenance: {}/{} covered ({}% — known {}, asserted {}, unknown {})",
        cov.known + cov.asserted,
        cov.total(),
        cov.covered_pct(),
        cov.known,
        cov.asserted,
        cov.unknown,
    );
    for key in veridex_core::certificate::EXPECTED_PROVENANCE_KEYS {
        // Show the *strongest* covering element for the key (known > asserted), matching how
        // ProvenanceCoverage counts it — a plain `.find()` would show whichever happens to come first,
        // so the displayed `[class]` could disagree with the counted coverage above.
        let class_rank = |c: ProvenanceClass| match c {
            ProvenanceClass::Known => 2,
            ProvenanceClass::Asserted => 1,
            ProvenanceClass::Unknown => 0,
        };
        let best = d
            .provenance
            .iter()
            .flat_map(|r| &r.elements)
            .filter(|e| e.key == *key && e.class != ProvenanceClass::Unknown && e.has_real_value())
            .max_by_key(|e| class_rank(e.class));
        match best {
            Some(e) => {
                let _ = writeln!(
                    out,
                    "      {key}: {} [{}]",
                    e.value.as_deref().unwrap_or_default(),
                    e.class.tag()
                );
            }
            None => {
                let _ = writeln!(out, "      {key}: missing");
            }
        }
    }
    out
}

/// Where `out_path` would land, if that is inside the dataset directory `source` names.
///
/// `None` when the dataset is a single file (writing beside it is not writing into it), when either
/// path cannot be resolved, or when the output is somewhere else entirely.
fn writes_inside_dataset(source: &str, out_path: &str) -> Option<String> {
    let root = std::path::Path::new(source).canonicalize().ok()?;
    if !root.is_dir() {
        return None;
    }
    let out = std::path::Path::new(out_path);
    // The file does not exist yet, so resolve its parent and rejoin the name.
    let parent = out.parent().filter(|p| !p.as_os_str().is_empty());
    let resolved = match parent {
        Some(p) => p.canonicalize().ok()?,
        None => std::env::current_dir().ok()?,
    };
    resolved
        .starts_with(&root)
        .then(|| resolved.display().to_string())
}

fn cmd_certify(rest: &[String]) -> ExitCode {
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // Sampling gets its own message: "certify does not support --sample-episodes" is true but does
    // not say why, and the why is the whole point — a certificate is a claim about a dataset, and the
    // episodes a sample never read are where the problem it would wave through is.
    if args.any_sampling_flag() {
        eprintln!(
            "veridex: certify does not support sampling — a certificate speaks for the whole \
             dataset, so issue it from a full check"
        );
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    // Same reasoning, different omission: a metadata-only run never opened the data a certificate
    // would be attesting.
    if args.metadata_only {
        eprintln!(
            "veridex: certify does not support --metadata-only — a certificate speaks for a \
             dataset's data, and a metadata-only run checks only its manifest"
        );
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    // Otherwise: no report-format flags — `certify` writes a signed document, not a report.
    if let Err(code) = reject_flags_except(
        "certify",
        &args,
        &[
            &["--key", "--out", "--timestamp", "--profile", "--config"],
            INGEST_FLAGS,
        ],
    ) {
        return code;
    }
    if let Err(code) = reject_extra_positionals("certify", &args, "dataset path") {
        return code;
    }
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
    // Resolve a readiness profile if one was requested; an unknown name is a tool error.
    let profile = match &args.profile {
        None => None,
        Some(name) => match veridex_core::profile::by_name(name) {
            Some(p) => Some(p),
            None => {
                eprintln!("veridex: unknown profile `{name}` (known: world-model-ready)");
                return ExitCode::from(EXIT_TOOL_ERROR);
            }
        },
    };

    let source = Source::Local(PathBuf::from(path));
    let registry = veridex_core::default_registry();
    let ingest_opts = match ingest_options(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    // Honor the same configuration `check` does — from `--config` or an auto-discovered
    // `veridex.toml`. Ignoring it meant a certificate could disagree with the `check` a user had just
    // run on the same data in the same directory, and that a config naming a nonexistent check was
    // silently accepted here while `check` rejected it.
    let config = match load_config(args.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    let engine = match veridex_core::checks::default_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };
    if let Err(e) = config.validate_check_ids(engine.check_ids()) {
        eprintln!("veridex: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }
    // A profile applies the tolerances it names (e.g. tighter cross-sensor sync) over the config's —
    // a readiness judgement means nothing at looser thresholds than it names. Thresholds it does not
    // name are left as configured rather than reset to the defaults.
    let mut run_config = config.to_run_config();
    if let Some(p) = &profile {
        run_config.tolerances = p.apply_tolerances(run_config.tolerances);
    }
    let out = match veridex_core::run_check_with(
        &registry,
        &source,
        args.format.as_deref(),
        &ingest_opts,
        &run_config,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("veridex: {e}");
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    };

    // A certificate speaks for a whole dataset. Refuse to mint one from a run that only looked at
    // part of it, rather than issuing a portable claim wider than the evidence behind it.
    if let Err(e) = Certificate::certifiable(&out.verdict) {
        eprintln!("veridex: {e}");
        return ExitCode::from(EXIT_TOOL_ERROR);
    }

    // Timestamp is caller-supplied (the core never reads a clock). Default to unix seconds.
    let timestamp = args.timestamp.clone().unwrap_or_else(unix_timestamp);
    let mut cert = Certificate::build(
        out.ingested.dataset.id.clone(),
        &out.verdict,
        out.trust.clone(),
        ProvenanceCoverage::of(&out.ingested.dataset),
        Issuance {
            key_id: keypair.public_hex(),
            timestamp,
        },
    );
    // Attach the per-criterion readiness report when a profile was requested (design A4).
    if let Some(p) = &profile {
        cert.readiness = Some(ReadinessReport::evaluate(
            p,
            &out.verdict,
            &out.ingested.dataset,
        ));
    }
    let signed = sign(cert, &keypair);

    let out_path = args
        .out
        .clone()
        .unwrap_or_else(|| format!("{}.veridex.json", out.ingested.dataset.id));
    // The default output name is relative, so it lands in the working directory -- which is *inside
    // the dataset* when the user ran `cd my-dataset && veridex certify .`, the most natural way to
    // do it. "Veridex only reads and reports. It never mutates your dataset" is a promise the README
    // makes and the adoption guide repeats, and writing a certificate into the tree breaks it. The
    // CDM hash is unaffected, so nothing is corrupted -- but a promise that holds except when it is
    // inconvenient is not one a user can rely on, so this is refused with the one-flag fix rather
    // than warned about.
    if args.out.is_none() {
        if let Some(inside) = writes_inside_dataset(path, &out_path) {
            eprintln!(
                "veridex: the default certificate path `{out_path}` would be written inside the \
                 dataset at `{inside}`, and Veridex never writes into a dataset. Pass `--out \
                 <path>` to choose somewhere else."
            );
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    }
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
    // Per-criterion readiness, when a profile was evaluated — rendered by the shared helper the
    // `verify` side uses, so issuing and verifying report the criteria identically.
    if let Some(r) = &signed.certificate.readiness {
        print!("{}", veridex_core::render_readiness(r, "  "));
    }
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
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // No sampling: a certificate binds to the whole dataset's hash, so a sampled re-ingest would
    // hash to something else and read as a transplant, which is not what the user asked about.
    if let Err(code) = reject_flags_except(
        "verify",
        &args,
        &[
            &["--json", "--certificate", "--key", "--allow-any-issuer"],
            INGEST_FLAGS,
        ],
    ) {
        return code;
    }
    if let Err(code) = reject_extra_positionals("verify", &args, "dataset path") {
        return code;
    }
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

    // A signature only proves the certificate came from *some* key. Without a trusted issuer key,
    // anyone can mint a certificate that says whatever they like about a dataset they hold — and it
    // will verify, because it really is self-consistent and really is bound to that data. So a trust
    // decision is mandatory: name the issuer with `--key`, or say explicitly that you accept any.
    if args.key.is_none() && !args.allow_any_issuer {
        eprintln!(
            "veridex: verify needs a trusted issuer: pass --key <public-key|file>, or \
             --allow-any-issuer to check only that the certificate is internally consistent"
        );
        return ExitCode::from(EXIT_TOOL_ERROR);
    }

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
            // Everything printed here is covered by the signature that just verified, including the
            // readiness block — a tampered certificate never reaches this branch.
            let issuer_verified = expected_issuer.is_some();
            // Whether the transplant check actually ran. With no dataset path there is nothing to
            // compare the bound hash against, and saying so is the difference between confirming a
            // binding and echoing the certificate's claim about one.
            let dataset_checked = presented_hash.is_some();
            if args.json {
                println!(
                    "{}",
                    veridex_core::verified_json(&signed, &v, issuer_verified, dataset_checked)
                );
            } else {
                print!(
                    "{}",
                    veridex_core::render_verified(&signed, &v, issuer_verified, dataset_checked)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            // A machine consumer asked for JSON; give it JSON on failure too, or it has nothing to
            // parse and must fall back to scraping stderr.
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({ "verified": false, "error": e.to_string() })
                );
            }
            eprintln!("✗ verification failed: {e}");
            ExitCode::from(EXIT_FAIL)
        }
    }
}

/// Write a secret key file. On Unix the file is created `0600` (owner-only) so another local user
/// on a shared host or CI runner cannot read the issuer's private signing key and forge certificates.
#[cfg(unix)]
fn write_secret_key(path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies at creation, so a `--force` overwrite of a pre-existing world-readable
    // file would otherwise write a fresh secret seed into it and leave it readable.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    f.write_all(contents.as_bytes())
}

/// Non-Unix fallback: the `0600` permission bit has no portable equivalent, so write normally.
#[cfg(not(unix))]
fn write_secret_key(path: &str, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

fn cmd_keygen(rest: &[String]) -> ExitCode {
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // `keygen` touches no dataset; only the overwrite guard applies.
    if let Err(code) = reject_flags_except("keygen", &args, &[&["--force"]]) {
        return code;
    }
    if let Err(code) = reject_extra_positionals("keygen", &args, "output path") {
        return code;
    }
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
    if let Err(e) = write_secret_key(path, &format!("{}\n", keypair.secret_hex())) {
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
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // No sampling: emitted provenance describes a dataset, and from a sample it would describe a
    // subset while carrying the dataset's name.
    if let Err(code) =
        reject_flags_except("provenance", &args, &[&["--emit", "--out"], INGEST_FLAGS])
    {
        return code;
    }
    if let Err(code) = reject_extra_positionals("provenance", &args, "dataset path") {
        return code;
    }
    let mut ingested = match ingest(&args) {
        Ok(i) => i,
        Err(code) => return code,
    };
    // Canonicalize before rendering: the content hash treats these collections as sets, so two
    // ingests that hash identically must also render identically.
    ingested.dataset.canonicalize_order();
    let d = &ingested.dataset;

    let emit = args.emit.as_deref().unwrap_or("croissant");
    let json = match veridex_core::render_provenance(d, emit) {
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

/// A diff is a regression when the new report introduced any finding or its trust score dropped.
fn is_regression(diff: &veridex_core::ReportDiff) -> bool {
    // A coverage change is a regression on its own, whichever direction the numbers moved.
    // Substituting a metadata-only or sampled report for a full one silences most of the catalog,
    // so the diff reads as findings *resolved* and a trust score that went up — a gate that passes
    // precisely because the new run stopped looking.
    diff.coverage_differs()
        || !diff.introduced.is_empty()
        || diff.score_delta().is_some_and(|d| d < 0)
}

fn cmd_diff(rest: &[String]) -> ExitCode {
    // Parse through the shared validator like every other command. Scanning the raw argv for known
    // flags meant an unknown one was silently dropped, so `--fail-on-regresion` (one letter short)
    // disabled the CI gate and still exited 0.
    let args = match parse_args_or_exit(rest) {
        Ok(a) => a,
        Err(code) => return code,
    };
    // `diff` reads two reports, not a dataset: nothing about ingestion, scoring, or signing applies.
    if let Err(code) = reject_flags_except("diff", &args, &[&["--json", "--fail-on-regression"]]) {
        return code;
    }
    let json_out = args.json;
    let fail_on_regression = args.fail_on_regression;
    let paths: Vec<&String> = args.positionals.iter().collect();
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
    // A file that carries no findings array is not a report with no findings — it is not a report.
    // Reading one as empty made a truncated artifact look like "everything resolved" and pass a
    // `--fail-on-regression` gate.
    for (label, value) in [(old_path, &old), (new_path, &new)] {
        if !veridex_core::is_report_shaped(value) {
            eprintln!(
                "veridex: {label} is not a Veridex report (no findings array) — expected the output of `veridex check --json`"
            );
            return ExitCode::from(EXIT_TOOL_ERROR);
        }
    }
    // Diffing two different datasets compares nothing meaningful; say so rather than printing a
    // score movement between unrelated runs.
    let bound = |v: &serde_json::Value| -> Option<String> {
        v.get("verdict")
            .and_then(|x| x.get("cdm_content_hash"))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };
    if let (Some(a), Some(b)) = (bound(&old), bound(&new)) {
        if a != b {
            eprintln!("veridex: note — these reports describe different dataset content ({a} vs {b}); the diff compares findings across two different datasets");
        }
    }

    let diff = veridex_core::diff_reports(&old, &new);
    if json_out {
        println!("{}", veridex_core::render_diff_json(&old, &new));
    } else {
        print!("{}", veridex_core::render_diff(&diff));
    }

    // For CI: optionally fail when the new report regressed — any finding introduced, or a lower
    // trust score. Without the flag, diff is purely informational and always exits 0.
    if fail_on_regression && is_regression(&diff) {
        if diff.coverage_differs() {
            eprintln!(
                "veridex: regression — the two reports cover different amounts of their dataset \
                 ({} -> {}), so the comparison is between unlike runs",
                diff.old_coverage.as_deref().unwrap_or("unknown"),
                diff.new_coverage.as_deref().unwrap_or("unknown"),
            );
            return ExitCode::from(EXIT_FAIL);
        }
        eprintln!(
            "veridex: regression — {} finding(s) introduced, score delta {}",
            diff.introduced.len(),
            diff.score_delta()
                .map(|d| d.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        );
        return ExitCode::from(EXIT_FAIL);
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
    --min-score <0-100>  fail (exit 20) if the trust score is below this (check)
    --fail-on-regression fail (exit 20) if the new report introduced findings or a lower score (diff)"
    );
    println!("    --config <file>      veridex.toml (auto-discovered in cwd if present)");
    println!("    --profile <name>     policy profile to judge against: world-model-ready (check, certify)");
    println!(
        "    --max-frames <n>     ceiling on frames an ingest may materialize (0 = no limit)
    --max-decompression-ratio <n>
                         ceiling on compressed expansion, as a multiple of the file's size (0 = no limit)"
    );
    println!(
        "    --sample-episodes <n>
                         check only the first n episodes (check, inspect; LeRobot, RLDS, HDF5, Zarr)
    --sample-fraction <f>
                         check a deterministic fraction of episodes, f in (0, 1]
    --sample-seed <n>    fix the --sample-fraction draw (default 0)
    --metadata-only      check the manifest, stored stats, and provenance without reading any
                         stream payload (check, inspect; LeRobot)"
    );
    println!("    --allow-any-issuer   verify without pinning an issuer key — accepts ANY signer (verify)");
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
    fn regression_is_introduced_findings_or_a_lower_score() {
        use serde_json::json;
        let base = veridex_core::ReportDiff {
            introduced: vec![],
            resolved: vec![],
            unchanged: vec![],
            old_score: Some(80),
            new_score: Some(80),
            old_coverage: Some("full".into()),
            new_coverage: Some("full".into()),
        };
        // No change → not a regression.
        assert!(!super::is_regression(&base));
        // A coverage change is a regression on its own: the new run may report fewer findings and a
        // higher score precisely because it stopped looking.
        let narrowed = veridex_core::ReportDiff {
            new_coverage: Some("metadata_only".into()),
            ..base.clone()
        };
        assert!(super::is_regression(&narrowed));
        // An introduced finding → regression, even at an unchanged score.
        let mut with_finding = base.clone();
        with_finding.introduced = vec![json!({"code": "X"})];
        assert!(super::is_regression(&with_finding));
        // A score drop with no new findings → regression (e.g. lost provenance coverage).
        let mut lower_score = base.clone();
        lower_score.new_score = Some(70);
        assert!(super::is_regression(&lower_score));
        // A score improvement → not a regression.
        let mut higher_score = base.clone();
        higher_score.new_score = Some(90);
        assert!(!super::is_regression(&higher_score));
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

    #[test]
    fn provenance_summary_reports_coverage_and_treats_placeholders_as_missing() {
        use veridex_core::cdm::{Dataset, ProvenanceClass, ProvenanceElement, ProvenanceScope};
        let element = |key: &str, value: Option<&str>, class| ProvenanceElement {
            key: key.into(),
            value: value.map(Into::into),
            class,
        };
        let d = Dataset {
            id: "t".into(),
            calibration: None,
            metadata: vec![],
            provenance: vec![veridex_core::cdm::Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![
                    element("license", Some("apache-2.0"), ProvenanceClass::Known),
                    // A placeholder sensor must be treated as missing, not counted.
                    element("sensor", Some("unknown"), ProvenanceClass::Known),
                ],
            }],
            episodes: vec![],
        };
        let s = super::provenance_summary(&d);
        // One real element (license) out of six expected.
        assert!(s.contains("1/6 covered"), "unexpected: {s}");
        assert!(s.contains("license: apache-2.0 [known]"));
        // The placeholder sensor is shown as missing, matching the coverage score.
        assert!(s.contains("sensor: missing"), "unexpected: {s}");
    }

    #[test]
    fn exit_codes_follow_the_ci_contract() {
        use super::{exit_code_for_status, EXIT_FAIL, EXIT_PASS, EXIT_WARN};
        use veridex_core::Status;
        // Default threshold (error): pass=0, warnings=10, fail=20.
        assert_eq!(exit_code_for_status(Status::Pass, false), EXIT_PASS);
        assert_eq!(
            exit_code_for_status(Status::PassWithWarnings, false),
            EXIT_WARN
        );
        assert_eq!(exit_code_for_status(Status::Fail, false), EXIT_FAIL);
        // --fail-on warning: warnings escalate to the fail code; pass and fail are unchanged.
        assert_eq!(exit_code_for_status(Status::Pass, true), EXIT_PASS);
        assert_eq!(
            exit_code_for_status(Status::PassWithWarnings, true),
            EXIT_FAIL
        );
        assert_eq!(exit_code_for_status(Status::Fail, true), EXIT_FAIL);
    }
}
