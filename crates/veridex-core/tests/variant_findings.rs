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

/// Ingest `path` and run the standard catalog, returning every finding code it emitted and the
/// subset of those at error severity — or `None` when the ingest refused the source, which is itself
/// a documented outcome for some fixtures.
#[allow(clippy::type_complexity)]
fn codes_for(path: &Path) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
    let registry = veridex_core::adapter::default_registry();
    let checked = veridex_core::pipeline::run_check(
        &registry,
        &Source::Local(path.to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .ok()?;
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
    for (label, variants, write, extension) in fixtures() {
        for variant in variants {
            let target = match extension {
                Some(ext) => dir.path().join(format!("{label}-{variant}.{ext}")),
                None => dir.path().join(format!("{label}-{variant}")),
            };
            let _ = std::fs::remove_dir_all(&target);
            if write(&target, variant).is_err() {
                continue;
            }
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
                    "{label}/{variant}: `{flag}` reports {invented:?}, which the full read of the \
                     same bytes does not. A finding that appears only when Veridex looks at less is \
                     describing the request rather than the recording — and if it genuinely names \
                     the run as its cause, add it to that narrowing's allowed list with the reason.",
                );
            }
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
    for (label, variants, write, extension) in fixtures() {
        for variant in variants {
            let target = match extension {
                Some(ext) => dir.path().join(format!("{label}-{variant}.{ext}")),
                None => dir.path().join(format!("{label}-{variant}")),
            };
            let _ = std::fs::remove_dir_all(&target);
            if write(&target, variant).is_err() {
                continue;
            }
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
                "{label}/{variant}: `--profile strict` loses {lost:?}. Measuring harder must never \
                 make a finding disappear — that would make a tightened run a way to launder a \
                 failing dataset through the one gate `SCOPE.NARROWED` deliberately leaves open.",
            );
            assert!(
                tight.trust.score <= loose.trust.score,
                "{label}/{variant}: `--profile strict` raises the score from {} to {}",
                loose.trust.score,
                tight.trust.score,
            );
        }
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
    let registry = veridex_core::adapter::default_registry();
    let mut compared = 0;
    for (label, variants, write, extension) in fixtures() {
        for variant in variants {
            let target = match extension {
                Some(ext) => dir.path().join(format!("{label}-{variant}.{ext}")),
                None => dir.path().join(format!("{label}-{variant}")),
            };
            let _ = std::fs::remove_dir_all(&target);
            if write(&target, variant).is_err() {
                continue;
            }
            let Some(checked) = veridex_core::pipeline::run_check(
                &registry,
                &Source::Local(target.to_path_buf()),
                None,
                &IngestOptions::default(),
            )
            .ok() else {
                continue; // a fixture built to be refused at ingest
            };
            compared += 1;
            let verdict = &checked.verdict;
            let expected: BTreeSet<&str> =
                verdict.findings.iter().map(|f| f.code.as_str()).collect();

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
                "{label}/{variant}: the JSON report's findings differ from the verdict's",
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
                "{label}/{variant}: SARIF's results differ from the verdict's findings",
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
                    "{label}/{variant}: the terminal report omits `{code}`",
                );
                assert!(
                    html.contains(code),
                    "{label}/{variant}: the HTML report omits `{code}`",
                );
            }
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
    let registry = veridex_core::adapter::default_registry();
    let mut compared = 0;
    for (label, variants, write, extension) in fixtures() {
        for variant in variants {
            let target = match extension {
                Some(ext) => dir.path().join(format!("{label}-{variant}.{ext}")),
                None => dir.path().join(format!("{label}-{variant}")),
            };
            let _ = std::fs::remove_dir_all(&target);
            if write(&target, variant).is_err() {
                continue;
            }
            let Some(checked) = veridex_core::pipeline::run_check(
                &registry,
                &Source::Local(target.to_path_buf()),
                None,
                &IngestOptions::default(),
            )
            .ok() else {
                continue; // a fixture built to be refused at ingest
            };
            compared += 1;

            let coverage =
                veridex_core::certificate::ProvenanceCoverage::of(&checked.ingested.dataset);
            let cert = veridex_core::certificate::Certificate::build(
                checked.ingested.dataset.id.clone(),
                &checked.verdict,
                checked.trust,
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
                "{label}/{variant}: the certificate's findings do not match the run it attests",
            );
        }
    }
    assert!(
        compared >= 30,
        "the sweep must reach the fixtures, got {compared}"
    );
}
