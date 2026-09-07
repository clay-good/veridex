//! Every finding code a demo variant's documentation names must be one that variant actually emits.
//!
//! `veridex-demo`'s own `variant_docs` guard holds the variant *lists* together — the generator's
//! `VARIANTS`, the module bullets, the two `Usage:` lines and the README. It says nothing about
//! whether a variant still does what its bullet claims, and the bullets are the most detailed
//! documentation these fixtures have: each one names the code it exists to produce, and every
//! quickstart, README example and docs page is written against those claims.
//!
//! Nothing checked them, and the drift they invite is silent in both directions. A rule that grew
//! too eager fails a variant documented as sound; one that grew too narrow stops firing on the
//! variant built to trip it, and the fixture then proves nothing while still reading as proof. Both
//! happened in one change here: a missing-feature rule, read literally, turned every LeRobot camera
//! into an absent feature — `video`, whose bullet promises a clean read of a real container, failed
//! with two `STRUCTURAL.EMPTY_STREAM` errors and a `VIDEO.FRAME_COUNT_MISMATCH`, and the whole test
//! suite was green.
//!
//! So this runs every variant of every generator through the real ingest and the real catalog, and
//! holds it to its own bullet.

use std::collections::BTreeSet;
use std::path::Path;

use veridex_core::adapter::{IngestOptions, Source};
use veridex_core::check::Severity;

const DEMO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../veridex-demo");

/// A variant's documented claim: the codes its bullet names, and whether it says the read is refused.
struct Claim {
    variant: String,
    codes: BTreeSet<String>,
    refused: bool,
    /// The bullet, joined into one line — read for the conventions the module doc spells out.
    text: String,
}

/// Extract each `//! - ` bullet's variant name and the finding codes it names.
///
/// The bullet is joined into one string before anything is matched. A bullet here runs to five or
/// six wrapped lines and its code often sits on a different line from its `→`, so a line-by-line
/// scan reads a wrapped claim as no claim at all — and passes, having checked nothing.
fn claims(source: &str) -> Vec<Claim> {
    let mut out: Vec<Claim> = Vec::new();
    let mut current: Option<(String, String)> = None;
    let flush = |out: &mut Vec<Claim>, cur: Option<(String, String)>| {
        let Some((variant, text)) = cur else { return };
        // Only a code the bullet points an arrow at is a claim. The bullets write the outcome as
        // `→ \`CODE\``, and they also name codes in passing — the one a check supersedes, the one a
        // sibling variant fires, the one that stays silent. Reading every backticked code as a
        // promise made the guard demand that a variant emit the very finding its bullet says it does
        // *not*.
        let codes: BTreeSet<String> = text
            .split('→')
            .skip(1)
            .filter_map(|after| {
                let code = after.trim_start().strip_prefix('`')?.split('`').next()?;
                (code.contains('.')
                    && code
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c == '_' || c == '.'))
                .then(|| code.to_string())
            })
            .collect();
        // Some fixtures exist to be *refused* at ingest, which produces no findings at all.
        let refused = text.contains("refuses it") || text.contains("being rejected");
        out.push(Claim {
            variant,
            codes,
            refused,
            text,
        });
    };
    for line in source.lines() {
        let Some(rest) = line.strip_prefix("//!") else {
            continue;
        };
        let rest = rest.trim_start();
        if let Some(bullet) = rest.strip_prefix("- ") {
            flush(&mut out, current.take());
            let bullet = bullet.strip_prefix("(default) ").unwrap_or(bullet);
            if let Some(name) = bullet.strip_prefix('`').and_then(|r| r.split('`').next()) {
                current = Some((name.to_string(), bullet.to_string()));
            }
        } else if let Some((_, text)) = current.as_mut() {
            if rest.is_empty() {
                flush(&mut out, current.take());
            } else {
                text.push(' ');
                text.push_str(rest);
            }
        }
    }
    flush(&mut out, current);
    out
}

/// The default full check of `path`, computed once per test binary.
///
/// Nine properties below run over the same ~60 datasets, and re-ingesting each of them nine times
/// made this file the slowest in the suite for no reason — the sweep is over *what a dataset checks
/// as*, and that does not depend on which property is asking. `None` is cached too: a fixture built
/// to be refused at ingest is refused once.
fn checked_for(path: &Path) -> Option<std::sync::Arc<veridex_core::pipeline::CheckOutput>> {
    #[allow(clippy::type_complexity)]
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<
                std::path::PathBuf,
                Option<std::sync::Arc<veridex_core::pipeline::CheckOutput>>,
            >,
        >,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cache.lock().expect("cache").get(path) {
        return hit.clone();
    }
    let registry = veridex_core::adapter::default_registry();
    let out = veridex_core::pipeline::run_check(
        &registry,
        &Source::Local(path.to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .ok()
    .map(std::sync::Arc::new);
    cache
        .lock()
        .expect("cache")
        .insert(path.to_path_buf(), out.clone());
    out
}

/// Every dataset the property sweeps below run over: the generated demo variants, plus every
/// fixture committed under `tests/fixtures/`.
///
/// The second half matters more than it looks. The four generators cover four of the eight
/// adapters — nothing generated is HDF5, Zarr, rosbag2 or CAN+DBC — so a property held only over
/// them was being held over half the readers, and the CDM an HDF5 file or a Zarr replay buffer
/// produces is exactly the shape these properties have never been asked about.
///
/// Directories are swept rather than listed, so a fixture added later is covered without anyone
/// remembering to add it here. Most of the HDF5 ones are deliberately hostile (`bomb.h5`,
/// `btree_cycle.h5`) and are refused at ingest; every sweep below skips a refused source already,
/// so they cost a failed open and nothing else.
fn sweep_datasets(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out = generated_datasets(dir);
    out.extend(committed_datasets());
    out
}

/// The demo variants, written once per test binary and shared by every sweep.
///
/// Writing them per test meant generating all 39 fixtures nine times over, which cost far more than
/// the checks the sweeps exist to run. The directory lives for the process, so the paths stay valid
/// for every caller.
fn generated_datasets(_dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    static ONCE: std::sync::OnceLock<(tempfile::TempDir, Vec<(String, std::path::PathBuf)>)> =
        std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let built = write_generated(dir.path());
        (dir, built)
    })
    .1
    .clone()
}

fn write_generated(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();

    // CAN+DBC, the eighth adapter and the one with no generator and no committed fixture — its own
    // tests build their inputs inline, so without this the sweeps below would still be missing a
    // reader. Two text files, the pair `docs/formats.md` tells a reader to create, including the
    // undefined id `4A2` that makes the coverage disclosure real rather than hypothetical.
    let can = dir.join("can-drive");
    if std::fs::create_dir_all(&can).is_ok() {
        let dbc = "VERSION \"\"\n\n\
                   BO_ 291 EngineData: 8 ECU\n\
                    SG_ EngineRPM : 0|16@1+ (0.25,0) [0|16383.75] \"rpm\" Vector__XXX\n\
                    SG_ VehicleSpeed : 16|16@1+ (0.01,0) [0|655.35] \"km/h\" Vector__XXX\n";
        let log = "(1709294400.000000) can0 123#7017B80B00000000\n\
                   (1709294400.010000) can0 123#7A17BC0B00000000\n\
                   (1709294400.020000) can0 4A2#DEADBEEF\n\
                   (1709294400.030000) can0 123#8D17C40B00000000\n";
        if std::fs::write(can.join("vehicle.dbc"), dbc).is_ok()
            && std::fs::write(can.join("drive.log"), log).is_ok()
        {
            out.push(("candbc/drive".to_string(), can));
        }
    }

    for (label, variants, write, extension) in fixtures() {
        for variant in variants {
            let target = match extension {
                Some(ext) => dir.join(format!("{label}-{variant}.{ext}")),
                None => dir.join(format!("{label}-{variant}")),
            };
            let _ = std::fs::remove_dir_all(&target);
            if write(&target, variant).is_ok() {
                out.push((format!("{label}/{variant}"), target));
            }
        }
    }
    out
}

/// The fixtures committed under `tests/fixtures/`.
fn committed_datasets() -> Vec<(String, std::path::PathBuf)> {
    let mut out: Vec<(String, std::path::PathBuf)> = Vec::new();
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    for sub in ["hdf5", "zarr", "rosbag2"] {
        let Ok(entries) = std::fs::read_dir(format!("{root}/{sub}")) else {
            continue;
        };
        let mut paths: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                // Skip the generators and notes that live beside the fixtures.
                !matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("py") | Some("md") | Some("wal")
                )
            })
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();
            out.push((format!("{sub}/{name}"), path));
        }
    }
    out
}

/// Ingest `path` and run the standard catalog, returning every finding code it emitted and the
/// subset of those at error severity — or `None` when the ingest refused the source, which is itself
/// a documented outcome for some fixtures.
#[allow(clippy::type_complexity)]
fn codes_for(path: &Path) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let checked = checked_for(path)?;
    Some((
        checked
            .verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect(),
        // Errors only. A variant's bullet describes the fault it was built to trip, not the ambient
        // provenance and coverage notes every fixture carries, and those are all info or warning.
        checked
            .verdict
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| f.code.clone())
            .collect(),
    ))
}

/// Every generator, as (label, variants, writer, single-file extension). One list, so a generator
/// added later is swept by both tests below rather than by whichever one someone remembered.
#[allow(clippy::type_complexity)]
fn fixtures() -> Vec<(
    &'static str,
    &'static [&'static str],
    fn(&Path, &str) -> Result<(), veridex_demo::DemoError>,
    Option<&'static str>,
)> {
    vec![
        (
            "mcap",
            veridex_demo::mcap::VARIANTS,
            veridex_demo::mcap::write as fn(&Path, &str) -> Result<(), veridex_demo::DemoError>,
            Some("mcap"),
        ),
        (
            "lerobot",
            veridex_demo::lerobot::VARIANTS,
            veridex_demo::lerobot::write,
            None,
        ),
        (
            "mf4",
            veridex_demo::mf4::VARIANTS,
            veridex_demo::mf4::write,
            Some("mf4"),
        ),
        (
            "rlds",
            veridex_demo::rlds::VARIANTS,
            veridex_demo::rlds::write,
            None,
        ),
    ]
}

fn check_generator(
    label: &str,
    module_rel: &str,
    variants: &[&str],
    write: impl Fn(&Path, &str) -> Result<(), veridex_demo::DemoError>,
    // The file extension a single-file generator writes, or `None` for one that writes a directory.
    // Format detection reads it, so a fixture written to the wrong suffix is refused — which reads
    // exactly like the fixture having broken.
    extension: Option<&str>,
) {
    let source = std::fs::read_to_string(format!("{DEMO_ROOT}/{module_rel}"))
        .unwrap_or_else(|e| panic!("{module_rel} is readable: {e}"));
    let claims = claims(&source);
    let dir = tempfile::tempdir().expect("tempdir");

    for variant in variants {
        let claim = claims
            .iter()
            .find(|c| c.variant == *variant)
            .unwrap_or_else(|| panic!("{label}: `{variant}` has no documented bullet"));
        // Nothing to hold it to. The list guard in `veridex-demo` already requires the bullet to
        // exist; a bullet naming no code is a fixture whose behaviour is described in prose only.
        if claim.codes.is_empty() && !claim.refused {
            continue;
        }
        let target = match extension {
            Some(ext) => dir.path().join(format!("{label}-{variant}.{ext}")),
            None => dir.path().join(format!("{label}-{variant}")),
        };
        let _ = std::fs::remove_dir_all(&target);
        write(&target, variant).unwrap_or_else(|e| panic!("{label}/{variant} writes: {e:?}"));

        let emitted = codes_for(&target);
        if claim.refused {
            assert!(
                emitted.is_none(),
                "{label}/{variant}: documented as refused at ingest, and it was read",
            );
            continue;
        }
        let (emitted, errors) = emitted.unwrap_or_else(|| {
            panic!(
                "{label}/{variant}: documented to emit {:?}, and the ingest refused it instead — \
                    a fixture that no longer loads proves nothing",
                claim.codes
            )
        });
        for code in &claim.codes {
            assert!(
                emitted.contains(code),
                "{label}/{variant}: its documentation says it produces `{code}` and it did not. \
                 Emitted: {emitted:?}",
            );
        }
        // The other direction, and the one that catches a rule which grew too eager. Holding a
        // variant only to the codes it claims lets a new error appear on every fixture at once
        // without a single test noticing: a missing-feature rule read literally turned every LeRobot
        // camera into an absent feature, and `video` — documented as a clean read of a real
        // container — failed with two `STRUCTURAL.EMPTY_STREAM` errors while this file was green.
        // A bullet saying "the same rig" is layering one fault on top of a base variant and
        // describes only what it adds — the module doc states that convention explicitly. Its base's
        // findings still fire, so they are claimed too, or every layered variant would look like
        // drift.
        let mut allowed = claim.codes.clone();
        if claim.text.contains("the same rig") {
            for base in claims.iter().filter(|c| c.variant == "av") {
                allowed.extend(base.codes.iter().cloned());
            }
        }
        for code in &errors {
            assert!(
                allowed.contains(code),
                "{label}/{variant}: emits `{code}` at error severity and its documentation does \
                 not say so. Either the fixture changed meaning or a check now fires on it, and a \
                 fixture whose bullet no longer describes it is documentation a reader is misled \
                 by.",
            );
        }
    }
}

#[test]
fn every_mcap_variant_emits_what_its_documentation_claims() {
    check_generator(
        "mcap",
        "src/mcap.rs",
        veridex_demo::mcap::VARIANTS,
        veridex_demo::mcap::write,
        Some("mcap"),
    );
}

#[test]
fn every_lerobot_variant_emits_what_its_documentation_claims() {
    check_generator(
        "lerobot",
        "src/lerobot.rs",
        veridex_demo::lerobot::VARIANTS,
        veridex_demo::lerobot::write,
        None,
    );
}

#[test]
fn every_mf4_variant_emits_what_its_documentation_claims() {
    check_generator(
        "mf4",
        "src/mf4.rs",
        veridex_demo::mf4::VARIANTS,
        veridex_demo::mf4::write,
        Some("mf4"),
    );
}

#[test]
fn every_rlds_variant_emits_what_its_documentation_claims() {
    check_generator(
        "rlds",
        "src/rlds.rs",
        veridex_demo::rlds::VARIANTS,
        veridex_demo::rlds::write,
        None,
    );
}

/// Ingest `path` under a narrowing option and run the standard catalog, or `None` when the format
/// refuses the request (a CAN log has no manifest to read).
fn narrowed_codes_for(path: &Path, options: IngestOptions) -> Option<BTreeSet<String>> {
    let registry = veridex_core::adapter::default_registry();
    let checked = veridex_core::pipeline::run_check(
        &registry,
        &Source::Local(path.to_path_buf()),
        None,
        &options,
    )
    .ok()?;
    Some(
        checked
            .verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect(),
    )
}

/// Looking at **less** of a dataset must not produce findings about it that looking at all of it
/// does not.
///
/// Held over both narrowings a caller can ask for: `--metadata-only`, which opens no payload, and
/// `--sample-episodes`, which reads a subset of them.
///
/// `--metadata-only` opens no payload, so every check that reads values, timestamps, content hashes
/// or message bodies has nothing — and a check that reports that absence as a property of the *data*
/// says something the full read contradicts. `SEMANTIC.NO_TASKS` did exactly that on its first
/// version: the demo's `video` fixture read "no episode in this dataset carries one" under the flag
/// and reported nothing at all on a full run. Same recording, opposite claims, and only the request
/// had changed.
///
/// The one code allowed to appear here and nowhere else is `COVERAGE.METADATA_ONLY`, which is the
/// run describing itself rather than the dataset — and it is precisely what makes every other such
/// finding redundant as well as wrong.
#[test]
fn a_narrower_read_never_invents_a_finding_the_full_read_does_not() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut compared = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some((full, _)) = codes_for(&target) else {
            continue; // a fixture built to be refused at ingest
        };
        // Each narrowing, with the codes it is allowed to add: exactly those that name the
        // *run* as their own cause. `COVERAGE.METADATA_ONLY` and `COVERAGE.SAMPLE` are the run
        // describing itself, and `STRUCTURAL.UNCOMPARED_EPISODES` says "this run covers 1
        // episode(s)" — none of the three makes a claim about the recording, which is the
        // difference this test exists to hold.
        let narrowings: [(&str, IngestOptions, &[&str]); 2] = [
            (
                "--metadata-only",
                IngestOptions {
                    metadata_only: true,
                    ..IngestOptions::default()
                },
                &["COVERAGE.METADATA_ONLY"],
            ),
            (
                "--sample-episodes 1",
                IngestOptions {
                    sample: veridex_core::adapter::Sample::FirstEpisodes(1),
                    ..IngestOptions::default()
                },
                &["COVERAGE.SAMPLE", "STRUCTURAL.UNCOMPARED_EPISODES"],
            ),
        ];
        for (flag, options, allowed) in narrowings {
            let Some(narrow) = narrowed_codes_for(&target, options) else {
                continue; // a format that refuses this narrowing by name
            };
            compared += 1;
            let invented: Vec<&String> = narrow
                .iter()
                .filter(|c| !full.contains(*c))
                .filter(|c| !allowed.contains(&c.as_str()))
                .collect();
            assert!(
                    invented.is_empty(),
                    "{name}: `{flag}` reports {invented:?}, which the full read of the \
                     same bytes does not. A finding that appears only when Veridex looks at less is \
                     describing the request rather than the recording — and if it genuinely names \
                     the run as its cause, add it to that narrowing's allowed list with the reason.",
                );
        }
    }
    assert!(
        compared >= 4,
        "the sweep must actually reach some metadata-only-capable fixtures, compared {compared}"
    );
}

/// Measuring the data **harder** can only add findings, never remove one — and so can only lower the
/// trust score, never raise it.
///
/// That is the whole argument for `--profile strict` being safe to gate on, and `docs/profiles.md`
/// sells it in those words: tightening "is not a narrowing: it emits no `SCOPE.NARROWED`, and
/// `check --profile strict --min-score 80` is a valid CI gate". A profile that could make a finding
/// *disappear* would turn that gate into a way to launder a failing dataset — the exact thing
/// `SCOPE.NARROWED` exists to stop a loosened threshold from doing, arriving through the one door
/// that is deliberately left open.
///
/// Nothing held the promise. This does, over every demo variant of every generator.
#[test]
fn tightening_a_threshold_never_removes_a_finding_or_raises_the_score() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = veridex_core::adapter::default_registry();
    let strict = veridex_core::profile::strict();
    let mut compared = 0;
    // The generated variants only, and deliberately: this property is about *thresholds*, and those
    // are the fixtures built to sit near one — a clock skew just over the limit, a jitter just under
    // it. The committed fixtures add two more full runs each for a question they were not built to
    // answer, and this is the one sweep here that cannot reuse the shared check, since each of its
    // runs carries a different config.
    for (name, target) in generated_datasets(dir.path()) {
        let run = |config: &veridex_core::RunConfig| {
            veridex_core::pipeline::run_check_with(
                &registry,
                &Source::Local(target.to_path_buf()),
                None,
                &IngestOptions::default(),
                config,
            )
            .ok()
        };
        let base = veridex_core::RunConfig::default();
        let tightened = veridex_core::RunConfig {
            tolerances: strict.apply_tolerances(base.tolerances),
            ..base.clone()
        };
        let (Some(loose), Some(tight)) = (run(&base), run(&tightened)) else {
            continue; // a fixture built to be refused at ingest
        };
        compared += 1;

        let before: BTreeSet<String> = loose
            .verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect();
        let after: BTreeSet<String> = tight
            .verdict
            .findings
            .iter()
            .map(|f| f.code.clone())
            .collect();
        let lost: Vec<&String> = before.difference(&after).collect();
        assert!(
            lost.is_empty(),
            "{name}: `--profile strict` loses {lost:?}. Measuring harder must never \
                 make a finding disappear — that would make a tightened run a way to launder a \
                 failing dataset through the one gate `SCOPE.NARROWED` deliberately leaves open.",
        );
        assert!(
            tight.trust.score <= loose.trust.score,
            "{name}: `--profile strict` raises the score from {} to {}",
            loose.trust.score,
            tight.trust.score,
        );
    }
    assert!(
        compared >= 30,
        "the sweep must reach the fixtures, got {compared}"
    );
}

/// Every renderer reports the same findings.
///
/// A finding reaches a reader through four surfaces — the terminal, the JSON report, SARIF, and the
/// self-contained HTML — and each is a separate rendering of the same verdict. A renderer that drops
/// a finding, or names it something the others do not, makes a dataset look different depending on
/// which output a team reads, and the one that disagrees is the one nothing compares against. This
/// repo has already had a renderer state a class the verdict did not: the trust label printed the
/// `asserted` provenance count under the word "attested".
///
/// So each renderer's set of finding codes is compared against the verdict's own, over every demo
/// variant of every generator. Codes rather than messages: the message wording is each renderer's
/// business (the terminal wraps, the HTML escapes), while *which* findings there are is not.
#[test]
fn every_renderer_reports_the_same_findings() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut compared = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue; // a fixture built to be refused at ingest
        };
        compared += 1;
        let verdict = &checked.verdict;
        let expected: BTreeSet<&str> = verdict.findings.iter().map(|f| f.code.as_str()).collect();

        // JSON and SARIF carry the code as a field, so they are compared exactly.
        let json: serde_json::Value =
            serde_json::from_str(&veridex_core::report::render_json(verdict, None))
                .expect("the JSON report parses");
        let in_json: BTreeSet<&str> = json["verdict"]["findings"]
            .as_array()
            .expect("findings array")
            .iter()
            .map(|f| f["code"].as_str().expect("a code"))
            .collect();
        assert_eq!(
            in_json, expected,
            "{name}: the JSON report's findings differ from the verdict's",
        );

        let sarif = veridex_core::report::render_sarif(verdict);
        let in_sarif: BTreeSet<&str> = sarif["runs"][0]["results"]
            .as_array()
            .expect("results array")
            .iter()
            .map(|r| r["ruleId"].as_str().expect("a ruleId"))
            .collect();
        assert_eq!(
            in_sarif, expected,
            "{name}: SARIF's results differ from the verdict's findings",
        );

        // The terminal and HTML render the code into prose, so each is checked for containment
        // of every code the verdict holds. That is the direction that matters: a renderer
        // *dropping* a finding is the failure, and a renderer cannot invent a code the catalog
        // does not define.
        let terminal = veridex_core::report::render_terminal(verdict, None, usize::MAX);
        let html = veridex_core::report::render_html(verdict, None);
        for code in &expected {
            assert!(
                terminal.contains(code),
                "{name}: the terminal report omits `{code}`",
            );
            assert!(
                html.contains(code),
                "{name}: the HTML report omits `{code}`",
            );
        }
    }
    assert!(
        compared >= 30,
        "the sweep must reach the fixtures, got {compared}"
    );
}

/// The certificate must not disagree with the verdict it attests.
///
/// A certificate is the one artifact that travels without Veridex beside it: the reader who most
/// needs it is the one who cannot re-run the check. It carries findings **by code** for exactly that
/// reason — a family count cannot tell a check that measured nothing from one that measured
/// something wrong — and `tests/certificate.rs` holds that map against the coarser rollups sitting
/// beside it in the same document.
///
/// What nothing held is the map against the *run*. A certificate that is internally consistent and
/// disagrees with the verdict it was issued from is a signed statement about a dataset that Veridex
/// did not make, and it verifies cleanly, because the signature covers the document rather than the
/// run behind it. So: over every demo variant, the code histogram in the certificate is compared to
/// the code histogram of the verdict it came from — counts included, since a dropped duplicate is
/// the same defect one grain finer.
#[test]
fn a_certificate_names_the_findings_of_the_run_it_attests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut compared = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue; // a fixture built to be refused at ingest
        };
        compared += 1;

        let coverage = veridex_core::certificate::ProvenanceCoverage::of(&checked.ingested.dataset);
        let cert = veridex_core::certificate::Certificate::build(
            checked.ingested.dataset.id.clone(),
            &checked.verdict,
            checked.trust.clone(),
            coverage,
            veridex_core::certificate::Issuance {
                key_id: "test".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
        );

        let mut from_verdict: std::collections::BTreeMap<&str, u64> = Default::default();
        for f in &checked.verdict.findings {
            *from_verdict.entry(f.code.as_str()).or_default() += 1;
        }
        let in_cert: std::collections::BTreeMap<&str, u64> = cert
            .findings_summary
            .by_code
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        assert_eq!(
            in_cert, from_verdict,
            "{name}: the certificate's findings do not match the run it attests",
        );
    }
    assert!(
        compared >= 30,
        "the sweep must reach the fixtures, got {compared}"
    );
}

/// Does `text` contain `needle` as a standalone token — not merely as a substring of a longer word?
///
/// Substring matching is not good enough here and the difference is not academic: the demo rig
/// records `weather: rain`, and every risk sentence in the report talks about *training*. A test
/// that reported that would be measuring English, not the redactor.
fn contains_token(text: &str, needle: &str) -> bool {
    let boundary = |c: Option<char>| match c {
        None => true,
        Some(c) => !c.is_alphanumeric() && c != '_',
    };
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if boundary(before) && boundary(after) {
            return true;
        }
        // Advance past this occurrence; `find` works on char boundaries, so `start + 1` may land
        // inside a multi-byte char — step to the next boundary instead.
        from = start + 1;
        while from < bytes.len() && !text.is_char_boundary(from) {
            from += 1;
        }
        if from >= text.len() {
            break;
        }
    }
    false
}

/// Nothing a redacted report is meant to hide survives into it, in any renderer.
///
/// `--redact` is a promise about what a report does *not* contain, and a broken promise here is
/// silent: the report looks fine, and the identifier it leaked is only noticed by whoever should not
/// have seen it. `tests/redact.rs` is thorough about the substitution rules, but it works over one
/// hand-built `sensitive_dataset()` — so it covers the identifier shapes that fixture happens to
/// carry, and a real CAN signal name, MF4 channel, RLDS feature path or rig coordinate frame is not
/// among them.
///
/// This takes the identifiers out of the *real* CDM of every demo variant — the dataset id, every
/// stream name and `frame_id`, every task and label value, every provenance value and metadata value
/// — and requires that none of them appears in the redacted terminal, JSON, SARIF or HTML report.
///
/// Matching is on whole tokens, not substrings, and only identifiers carrying a separator or a digit
/// are checked — an identifier that is an ordinary English word cannot be told from Veridex's own
/// prose, and no redactor can remove a word from its own sentences. See [`contains_token`].
#[test]
fn a_redacted_report_leaks_no_identifier_the_cdm_carries() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Every word Veridex's own catalog uses, from the check ids and finding codes it can print.
    let catalog_tokens: BTreeSet<String> = {
        let engine = veridex_core::checks::default_engine().expect("the standard catalog");
        engine
            .catalog()
            .iter()
            .flat_map(|c| {
                std::iter::once(c.id.to_string())
                    .chain(c.finding_codes.iter().map(|s| s.to_string()))
            })
            // The two disclosures that are deliberately *not* registered checks — a run's own
            // coverage and its own narrowing — so that configuration cannot switch them off. They
            // are printed like any check id, so their words belong here too: the MF4 demo names its
            // recorder `veridex`, which is exactly the first token of both.
            .chain([
                veridex_core::engine::COVERAGE_CHECK_ID.to_string(),
                veridex_core::engine::SCOPE_CHECK_ID.to_string(),
            ])
            // And the format ids. Every adapter records the format it read as a `source_format`
            // provenance element, so the dataset "carries" the word `rosbag2` — which the report
            // also prints on its own `format:` line, and in the sentence refusing a bare `.db3`,
            // whatever the dataset is.
            .chain(
                veridex_core::adapter::default_registry()
                    .supported_formats()
                    .into_iter()
                    .map(|f| f.to_string()),
            )
            .flat_map(|s| {
                s.split(|c: char| !c.is_alphanumeric())
                    .map(|t| t.to_ascii_lowercase())
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let mut compared = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue; // a fixture built to be refused at ingest
        };
        compared += 1;

        let d = &checked.ingested.dataset;
        let mut secrets: BTreeSet<String> = BTreeSet::new();
        secrets.insert(d.id.clone());
        for (k, v) in &d.metadata {
            secrets.insert(k.clone());
            secrets.insert(v.clone());
        }
        for record in &d.provenance {
            for el in &record.elements {
                if let Some(v) = el.value.as_ref() {
                    secrets.insert(v.clone());
                }
            }
        }
        for ep in &d.episodes {
            if let Some(t) = ep.task.as_ref() {
                secrets.insert(t.clone());
            }
            for l in &ep.labels {
                secrets.insert(l.value.clone());
            }
            for s in &ep.streams {
                secrets.insert(s.name.clone());
                if let Some(f) = s.frame_id.as_ref() {
                    secrets.insert(f.clone());
                }
            }
        }
        // Only identifiers that cannot be mistaken for English: something carrying a separator or a
        // digit. A dataset is free to name a stream `timestamps` — an HDF5 fixture here does — and
        // the report says "per-frame timestamps" in its own prose whatever the dataset is called.
        // No redactor can remove a word from Veridex's own sentences, and a test demanding it would
        // be measuring English rather than the promise. What is left is everything a leak actually
        // looks like: `/camera/image`, `camera_front`, `demo-operator`, `maps/demo_town.xodr`,
        // `CC-BY-4.0`, `observation.state`.
        secrets.retain(|s| {
            s.len() >= 4
                && s.chars()
                    .any(|c| c.is_ascii_digit() || matches!(c, '/' | '.' | '-' | '_' | ':'))
        });
        // A dataset is free to name a frame `gnss` or a stream `camera`, and Veridex's own
        // vocabulary says those words too — `autonomy.gnss-plausibility` is a check id, and it
        // is in the report whatever the dataset is called. An identifier that collides with the
        // catalog's own words cannot be told apart in the rendered output and is not something
        // redaction failed to remove, so it is excluded rather than reported as a leak. Names
        // that do not collide — `camera_front`, `/lidar/points`, `demo-operator` — are still
        // held to the promise.
        secrets.retain(|s| !catalog_tokens.contains(&s.to_ascii_lowercase()));

        let mut redactor = veridex_core::Redactor::for_dataset(d);
        let redacted = redactor.redact_verdict(&checked.verdict);
        let rendered = [
            (
                "terminal",
                veridex_core::report::render_terminal(&redacted, None, usize::MAX),
            ),
            ("json", veridex_core::report::render_json(&redacted, None)),
            (
                "sarif",
                veridex_core::report::render_sarif(&redacted).to_string(),
            ),
            ("html", veridex_core::report::render_html(&redacted, None)),
        ];
        for (surface, text) in &rendered {
            for secret in &secrets {
                assert!(
                    !contains_token(text, secret.as_str()),
                    "{name}: the redacted {surface} report still contains \
                         `{secret}`, which the CDM carries and redaction promises to remove",
                );
            }
        }
    }
    assert!(
        compared >= 30,
        "the sweep must reach the fixtures, got {compared}"
    );
}

/// The sweep reaches every adapter the registry supports.
///
/// The properties below are only as broad as the datasets they run over, and a dataset that fails to
/// ingest is skipped by every one of them — silently, and indistinguishably from one that passed. So
/// a fixture that stopped loading, or a reader with no fixture at all, would quietly narrow all nine
/// properties to the formats that still worked while the file stayed green. Four of the eight
/// adapters had no generated fixture until the committed ones were swept in, and CAN+DBC had neither
/// until its two text files were written here.
///
/// Checked by the format each dataset actually ingested as, not by filename, so a fixture that reads
/// as the wrong format counts for the wrong reader and is caught.
#[test]
fn the_sweep_reaches_every_adapter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut reached: BTreeSet<String> = BTreeSet::new();
    for (_, target) in sweep_datasets(dir.path()) {
        if let Some(checked) = checked_for(&target) {
            reached.insert(checked.ingested.report.format_id.to_string());
        }
    }
    let supported: BTreeSet<String> = veridex_core::adapter::default_registry()
        .supported_formats()
        .into_iter()
        .map(|f| f.to_string())
        .collect();
    let missing: Vec<&String> = supported.difference(&reached).collect();
    assert!(
        missing.is_empty(),
        "no dataset in the sweep ingests as {missing:?}, so every property in this file is being \
         held over the other readers only — add a fixture for it rather than letting the coverage \
         narrow in silence. Reached: {reached:?}",
    );
}

/// Every finding teaches: it names the training risk it is about and a remedy to act on.
///
/// `openspec/specs/checks-catalog/spec.md` requires it of the catalog — "each catalog check SHALL
/// ship with a stable ID, the training-time risk it addresses, and a suggested remedy, so findings
/// teach rather than merely flag" — and it is the difference between a report a team acts on and a
/// list of codes they look up. It was asserted in a dozen places on the *first* finding of a
/// hand-built fixture, which leaves every other finding of every other shape unguarded, and a new
/// code shipped without a remedy would pass all of them.
///
/// Held over every finding of every dataset in the sweep instead.
#[test]
fn every_finding_names_a_risk_and_a_remedy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut seen = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue;
        };
        for f in &checked.verdict.findings {
            seen += 1;
            assert!(
                !f.risk.trim().is_empty(),
                "{name}: `{}` names no training risk",
                f.code
            );
            assert!(
                !f.remedy.trim().is_empty(),
                "{name}: `{}` suggests no remedy",
                f.code
            );
        }
    }
    assert!(seen >= 100, "the sweep must reach findings, saw {seen}");
}

/// Every finding points at something the dataset actually has.
///
/// A finding's location is the half a reader acts on: "episode 7, stream `/lidar/points`" is what
/// sends someone to the right place in a recording. A location naming an episode index the dataset
/// does not hold, or a stream name no episode carries, sends them nowhere — and it is silent, since
/// nothing downstream resolves the location against the CDM. An off-by-one in an episode index or a
/// check that names the stream it was *comparing against* rather than the one at fault both look
/// exactly like a correct finding in every renderer.
///
/// A frame range is checked against the stream's own frame count for the same reason, and a time
/// range only for its ordering: a range whose end precedes its start is wrong on its face, while
/// whether a timestamp falls inside the recording is the temporal family's own question.
#[test]
fn every_finding_points_at_something_the_dataset_has() {
    use veridex_core::check::Location;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut seen = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue;
        };
        let d = &checked.ingested.dataset;
        let episode = |index: u64| d.episodes.iter().find(|e| e.index == index);
        for f in &checked.verdict.findings {
            seen += 1;
            let (ep_index, stream_name) = match &f.location {
                Location::Dataset => continue,
                Location::Episode { episode } => (*episode, None),
                Location::Stream { episode, stream } => (*episode, Some(stream)),
                Location::FrameRange {
                    episode,
                    stream,
                    start_frame,
                    end_frame,
                } => {
                    let ep = episode;
                    assert!(
                        start_frame <= end_frame,
                        "{name}: `{}` names frames {start_frame}..={end_frame}, which runs backwards",
                        f.code
                    );
                    (*ep, Some(stream))
                }
                Location::TimeRange {
                    episode,
                    stream,
                    start_ts,
                    end_ts,
                } => {
                    assert!(
                        start_ts <= end_ts,
                        "{name}: `{}` names a time range that runs backwards",
                        f.code
                    );
                    (*episode, Some(stream))
                }
            };
            let ep = episode(ep_index).unwrap_or_else(|| {
                panic!(
                    "{name}: `{}` points at episode {ep_index}, which this dataset does not have",
                    f.code
                )
            });
            if let Some(stream) = stream_name {
                assert!(
                    ep.streams.iter().any(|s| &s.name == stream),
                    "{name}: `{}` points at stream `{stream}` in episode {ep_index}, which carries \
                     no such stream",
                    f.code
                );
            }
        }
    }
    assert!(seen >= 100, "the sweep must reach findings, saw {seen}");
}

/// A fault the run found always costs data-quality score.
///
/// `docs/rubric-v1.md` is explicit: the data axis starts at 100 and deducts for each non-provenance
/// finding. The consequence is what makes `--min-score` usable as a CI gate — a dataset Veridex
/// found something wrong in cannot present a perfect data score. A deduction that failed to apply
/// would be silent in every renderer, since the finding is still printed beside the score that
/// ignored it, and the gate would wave through exactly the dataset it exists to stop.
///
/// Only errors and warnings count here. An informational finding is by design not a fault — the
/// abstentions are informational precisely so that "we could not measure this" does not read as a
/// defect — so requiring one to move the score would contradict the rubric rather than hold it.
#[test]
fn a_fault_always_costs_data_score() {
    use veridex_core::check::{Category, Severity};
    let dir = tempfile::tempdir().expect("tempdir");
    let mut with_faults = 0;
    for (name, target) in sweep_datasets(dir.path()) {
        let Some(checked) = checked_for(&target) else {
            continue;
        };
        let faults: Vec<&str> = checked
            .verdict
            .findings
            .iter()
            .filter(|f| f.category != Category::Provenance)
            .filter(|f| matches!(f.severity, Severity::Error | Severity::Warning))
            .map(|f| f.code.as_str())
            .collect();
        if faults.is_empty() {
            continue;
        }
        with_faults += 1;
        assert!(
            checked.trust.data_score < 100,
            "{name}: data score is a perfect 100 beside {faults:?} — a fault the run found has to \
             cost something, or `--min-score` passes the dataset it exists to stop",
        );
    }
    assert!(
        with_faults >= 10,
        "the sweep must reach datasets that carry faults, got {with_faults}"
    );
}
