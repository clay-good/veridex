//! Demo dataset generators: the fixtures `docs/formats.md` tells a reader to run, and the ones the
//! test suite damages.
//!
//! These lived as `examples/` under `veridex-core`, which meant a test could only build one by
//! spawning `cargo run --example`. That works until several test binaries do it at once: they
//! contend on the build lock and the shared target directory, and an invocation fails — in an
//! unrelated test, reading as a real regression. It happened.
//!
//! So they live here instead, as ordinary functions a test calls directly. The `examples/` binaries
//! remain, as thin wrappers, because the docs point at them by name.
//!
//! Nothing here depends on `veridex-core`. A generator that shared the reader's idea of the format
//! could not catch the reader being wrong about it — each one writes the on-disk layout from the
//! format's own specification.

use std::path::Path;

pub mod lerobot;
pub mod mcap;
pub mod mf4;
pub mod rlds;

/// What went wrong writing a demo dataset.
#[derive(Debug)]
pub enum DemoError {
    /// A variant name no generator knows. Carries the names it does know, so a caller can say so.
    UnknownVariant {
        /// The name that was asked for.
        asked: String,
        /// Every variant this generator accepts.
        known: &'static [&'static str],
    },
    /// The dataset could not be written.
    Io(std::io::Error),
}

impl std::fmt::Display for DemoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DemoError::UnknownVariant { asked, known } => {
                write!(f, "unknown variant `{asked}` — known: {}", known.join(", "))
            }
            DemoError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DemoError {}

impl From<std::io::Error> for DemoError {
    fn from(e: std::io::Error) -> Self {
        DemoError::Io(e)
    }
}

/// Check a requested variant against the ones a generator knows.
///
/// A typo used to fall through to the default fixture, silently producing a different dataset than
/// the one asked for — and a test that asked for `saturated` and got `clean` passes for the wrong
/// reason.
pub(crate) fn check_variant(asked: &str, known: &'static [&'static str]) -> Result<(), DemoError> {
    if known.contains(&asked) {
        Ok(())
    } else {
        Err(DemoError::UnknownVariant {
            asked: asked.to_string(),
            known,
        })
    }
}

/// Create `dir`, removing anything already there, so a generator always writes a clean dataset.
pub(crate) fn fresh_dir(dir: &Path) -> Result<(), DemoError> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    Ok(())
}
