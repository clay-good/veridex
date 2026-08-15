//! The checks catalog: concrete [`Check`](crate::check::Check) implementations grouped by family.
//!
//! MVP families: [`structural`], [`temporal`] (including the headline `TEMPORAL.CLOCK_SKEW`),
//! [`statistical`] (range/sanity over stored stats), [`semantic`] (task-string quality, stream-key clarity), and
//! [`provenance`] completeness.

pub mod autonomy;
pub mod provenance;
pub mod semantic;
pub mod statistical;
pub mod structural;
pub mod temporal;

use crate::check::Check;
use crate::engine::{Engine, RegistryError, Tolerances};

/// The standard MVP checks, in a stable order, each with default tolerances.
pub fn standard_checks() -> Vec<Box<dyn Check>> {
    standard_checks_with(&Tolerances::default())
}

/// The standard MVP checks, in a stable order, built with the given [`Tolerances`] for the checks
/// that take one (the rest ignore it).
pub fn standard_checks_with(t: &Tolerances) -> Vec<Box<dyn Check>> {
    vec![
        Box::new(structural::EpisodeBoundary),
        Box::new(structural::DegenerateEpisode),
        Box::new(structural::EpisodeContinuity),
        Box::new(structural::DeclaredEpisodeCount),
        Box::new(structural::DeclaredFrameCount),
        Box::new(structural::ShapeConsistency),
        Box::new(structural::StreamPresence),
        Box::new(structural::DuplicateEpisode),
        Box::new(structural::StuckStream),
        Box::new(temporal::Monotonicity),
        Box::new(temporal::RateValidity),
        Box::new(temporal::RateConformance {
            relative_tolerance: t.rate_deviation,
        }),
        Box::new(temporal::Gaps {
            gap_factor: t.gap_factor,
        }),
        Box::new(temporal::Jitter {
            max_cv: t.jitter_cv,
        }),
        Box::new(temporal::ClockSkew {
            tolerance_ns: t.clock_skew_ns,
        }),
        Box::new(temporal::StartOffset {
            tolerance_ns: t.start_offset_ns,
        }),
        Box::new(temporal::EndOffset {
            tolerance_ns: t.end_offset_ns,
        }),
        Box::new(temporal::RateConsistency),
        Box::new(temporal::EpisodeDuration {
            factor: t.episode_duration_factor,
        }),
        Box::new(autonomy::RigSync {
            // Shares the cross-sensor sync tolerance with ClockSkew: same semantics, one knob.
            tolerance_ns: t.clock_skew_ns,
        }),
        Box::new(autonomy::SequenceComplete::default()),
        Box::new(autonomy::EgoPoseContinuity::default()),
        Box::new(statistical::RangeSanity),
        Box::new(statistical::StoredVsObserved),
        Box::new(statistical::Saturation {
            min_fraction: t.saturation_fraction,
            min_samples: t.saturation_min_samples,
        }),
        Box::new(statistical::ExtremeOutlier::default()),
        Box::new(statistical::NonFiniteObserved),
        Box::new(semantic::TaskQuality),
        Box::new(semantic::StreamKeyClarity),
        Box::new(semantic::AnnotationIntegrity),
        Box::new(provenance::ProvenanceCompleteness),
    ]
}

/// Build an [`Engine`] preloaded with [`standard_checks`] (default tolerances).
///
/// Returns [`RegistryError`] only if the standard set ever contains a duplicate id — a programming
/// error caught by tests, never a runtime condition in released builds.
pub fn default_engine() -> Result<Engine, RegistryError> {
    default_engine_with(&Tolerances::default())
}

/// Build an [`Engine`] preloaded with the standard checks at the given [`Tolerances`].
pub fn default_engine_with(t: &Tolerances) -> Result<Engine, RegistryError> {
    let mut builder = Engine::builder();
    for check in standard_checks_with(t) {
        builder = builder.register(check)?;
    }
    Ok(builder.build())
}
