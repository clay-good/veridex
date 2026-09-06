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
