//! The checks catalog: concrete [`Check`](crate::check::Check) implementations grouped by family.
//!
//! MVP families implemented so far: [`structural`] and [`temporal`] (including the headline
//! `TEMPORAL.CLOCK_SKEW`). Statistical and provenance-completeness checks follow as the CDM grows
//! to carry stored statistics and richer provenance.

pub mod structural;
pub mod temporal;

use crate::check::Check;
use crate::engine::{Engine, RegistryError};

/// The standard MVP checks, in a stable order, each with default tolerances.
pub fn standard_checks() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(structural::EpisodeBoundary),
        Box::new(structural::DegenerateEpisode),
        Box::new(temporal::Monotonicity),
        Box::new(temporal::RateConformance::default()),
        Box::new(temporal::Gaps::default()),
        Box::new(temporal::ClockSkew::default()),
    ]
}

/// Build an [`Engine`] preloaded with [`standard_checks`].
///
/// Returns [`RegistryError`] only if the standard set ever contains a duplicate id — a programming
/// error caught by tests, never a runtime condition in released builds.
pub fn default_engine() -> Result<Engine, RegistryError> {
    let mut builder = Engine::builder();
    for check in standard_checks() {
        builder = builder.register(check)?;
    }
    Ok(builder.build())
}
