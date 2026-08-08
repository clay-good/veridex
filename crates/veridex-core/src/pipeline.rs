//! The end-to-end check pipeline, shared verbatim by the CLI and the Python bindings.
//!
//! Verdicts and certificates must be identical across the CLI and Python (design D1). The way to
//! guarantee that is to have exactly one implementation of "ingest → validate → score"; both
//! front-ends call [`run_check`] and render its output the same way.

use crate::adapter::{AdapterRegistry, IngestError, IngestOptions, Ingested, Source};
use crate::canonical::content_hash;
use crate::certificate::{score, ProvenanceCoverage, TrustScore};
use crate::checks::default_engine;
use crate::engine::{RunConfig, Verdict};

/// The result of a full check: the ingested dataset, its verdict, and its trust score.
pub struct CheckOutput {
    /// The ingested dataset and fidelity report.
    pub ingested: Ingested,
    /// The validation verdict.
    pub verdict: Verdict,
    /// The trust score.
    pub trust: TrustScore,
}

/// Ingest `source` (autodetecting, or forcing `format`), validate it with the standard checks, and
/// score it. This is the single pipeline the CLI and the Python bindings both call.
pub fn run_check(
    registry: &AdapterRegistry,
    source: &Source,
    format: Option<&str>,
    options: &IngestOptions,
) -> Result<CheckOutput, IngestError> {
    let ingested = match format {
        Some(f) => registry.ingest_as(f, source, options)?,
        None => registry.ingest(source, options)?,
    };

    let hash = content_hash(&ingested.dataset);
    // The standard check set has unique ids by construction (asserted by tests).
    let engine = default_engine().expect("standard checks have unique ids");
    let verdict = engine.run(&ingested.dataset, hash, &RunConfig::default());
    let trust = score(&verdict, &ProvenanceCoverage::of(&ingested.dataset));

    Ok(CheckOutput {
        ingested,
        verdict,
        trust,
    })
}
