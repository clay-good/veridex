//! The end-to-end check pipeline, shared verbatim by the CLI and the Python bindings.
//!
//! Verdicts and certificates must be identical across the CLI and Python (design D1). The way to
//! guarantee that is to have exactly one implementation of "ingest → validate → score"; both
//! front-ends call [`run_check`] and render its output the same way.

use crate::adapter::{AdapterRegistry, Coverage, IngestError, IngestOptions, Ingested, Source};
use crate::canonical::content_hash;
use crate::certificate::{score, ProvenanceCoverage, TrustScore};
use crate::checks::default_engine_with;
use crate::engine::{CoverageNote, RunConfig, Verdict};

/// The result of a full check: the ingested dataset, its verdict, and its trust score.
pub struct CheckOutput {
    /// The ingested dataset and fidelity report.
    pub ingested: Ingested,
    /// The validation verdict.
    pub verdict: Verdict,
    /// The trust score.
    pub trust: TrustScore,
}

/// Ingest `source` (autodetecting, or forcing `format`), validate it with the standard checks under
/// the default configuration, and score it. This is the pipeline the CLI's default `check` and the
/// Python bindings both call, keeping them in lock-step.
pub fn run_check(
    registry: &AdapterRegistry,
    source: &Source,
    format: Option<&str>,
    options: &IngestOptions,
) -> Result<CheckOutput, IngestError> {
    run_check_with(registry, source, format, options, &RunConfig::default())
}

/// Like [`run_check`], but with an explicit [`RunConfig`] (from a `veridex.toml`).
pub fn run_check_with(
    registry: &AdapterRegistry,
    source: &Source,
    format: Option<&str>,
    options: &IngestOptions,
    run_config: &RunConfig,
) -> Result<CheckOutput, IngestError> {
    run_check_attested(registry, source, format, options, run_config, None)
}

/// As [`run_check_with`], applying a producer attestation the caller has already verified.
///
/// The attestation is a run *input*: it raises provenance coverage and is disclosed in the verdict,
/// and it does not touch the CDM or its content hash. Verification is the caller's job because only
/// the caller knows which producer key it trusts — this function is handed the result, never the
/// decision.
pub fn run_check_attested(
    registry: &AdapterRegistry,
    source: &Source,
    format: Option<&str>,
    options: &IngestOptions,
    run_config: &RunConfig,
    attestation: Option<&crate::engine::AppliedAttestation>,
) -> Result<CheckOutput, IngestError> {
    let ingested = match format {
        Some(f) => registry.ingest_as(f, source, options)?,
        None => registry.ingest(source, options)?,
    };
    Ok(check_ingested(ingested, run_config, attestation))
}

/// Validate and score a dataset that has already been ingested.
///
/// Split out because an attestation is verified against the dataset's *content hash*, which does not
/// exist until the ingest has happened — a front end that had to call the whole pipeline to get a
/// hash, and the whole pipeline again to apply what it verified, would read the data twice. Both
/// front ends call this, so a verdict is identical whichever one produced it.
pub fn check_ingested(
    mut ingested: Ingested,
    run_config: &RunConfig,
    attestation: Option<&crate::engine::AppliedAttestation>,
) -> CheckOutput {
    // Normalize episode/stream order before validating so the verdict and reports are
    // order-independent, matching the order-independent content hash. The hash itself is unaffected
    // (it canonicalizes order internally); this aligns what the checks and reports see with it.
    ingested.dataset.canonicalize_order();

    let hash = content_hash(&ingested.dataset);
    // Sanitize tolerances (a non-finite value would silently disable the checks that guard on it and
    // break certificate round-tripping) so the checks and the recorded snapshot agree.
    let mut run_config = run_config.clone();
    run_config.tolerances = run_config.tolerances.finite_or_default();
    // The standard check set has unique ids by construction (asserted by tests). Build it with the
    // run's tolerances so a configured threshold takes effect.
    let engine =
        default_engine_with(&run_config.tolerances).expect("standard checks have unique ids");
    // Carry what the ingest actually covered into the verdict, so a sampled run is never readable as
    // a whole-dataset one — in the report, and under a certificate's signature.
    let coverage = match &ingested.report.coverage {
        Coverage::Full => CoverageNote::Full,
        Coverage::Sample {
            sample,
            episodes_ingested,
        } => CoverageNote::Sample {
            request: sample.describe(),
            episodes_ingested: *episodes_ingested,
        },
        Coverage::MetadataOnly { episodes_declared } => CoverageNote::MetadataOnly {
            episodes_declared: *episodes_declared,
        },
    };
    let verdict = engine.run_over_attested(
        &ingested.dataset,
        hash,
        &run_config,
        coverage,
        &ingested.report.unread_sources,
        attestation,
    );
    let attested_keys: Vec<String> = attestation.map(|a| a.keys.clone()).unwrap_or_default();
    let trust = score(
        &verdict,
        &ProvenanceCoverage::of_with_attested(&ingested.dataset, &attested_keys),
    );

    CheckOutput {
        ingested,
        verdict,
        trust,
    }
}
