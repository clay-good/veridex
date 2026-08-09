//! Statistical checks over stored per-stream statistics.
//!
//! MVP scope: range/sanity and degenerate-distribution checks on the statistics a source records
//! (design keeps Veridex from decoding frame payloads). Stored-vs-recomputed comparison arrives
//! once adapters stream values.

use crate::cdm::{Dataset, StreamStats};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Range, sanity, and degeneracy of stored per-stream statistics.
pub struct RangeSanity;

impl Check for RangeSanity {
    fn id(&self) -> &'static str {
        "statistical.range-sanity"
    }
    fn title(&self) -> &'static str {
        "Stored-statistics range and sanity"
    }
    fn category(&self) -> Category {
        Category::Statistical
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let Some(stats) = stream.stats else {
                    continue;
                };
                let at = || Location::Stream {
                    episode: ep.index,
                    stream: stream.name.clone(),
                };

                // Non-finite statistics are always a data-integrity error.
                if [stats.min, stats.max, stats.mean, stats.std]
                    .iter()
                    .any(|v| !v.is_finite())
                {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            at(),
                            "STATISTICAL.NON_FINITE",
                            format!(
                                "stream `{}` in episode {} has non-finite stored statistics",
                                stream.name, ep.index
                            ),
                        )
                        .with_risk("NaN/inf statistics poison normalization and any model that consumes them.")
                        .with_remedy("Recompute the statistics from clean values, or drop the corrupt stream."),
                    );
                    continue;
                }

                // Inverted range: min must not exceed max; std must be non-negative.
                if stats.min > stats.max {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            at(),
                            "STATISTICAL.RANGE_INVERTED",
                            format!(
                                "stream `{}` in episode {}: min {} exceeds max {}",
                                stream.name, ep.index, stats.min, stats.max
                            ),
                        )
                        .with_risk("An inverted range means the stored statistics are corrupt.")
                        .with_remedy("Re-derive the statistics from the data."),
                    );
                    continue;
                }
                if stats.std < 0.0 {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            at(),
                            "STATISTICAL.NEGATIVE_STD",
                            format!(
                                "stream `{}` in episode {}: negative std {}",
                                stream.name, ep.index, stats.std
                            ),
                        )
                        .with_risk("A negative standard deviation is impossible; the statistics are corrupt.")
                        .with_remedy("Re-derive the statistics from the data."),
                    );
                    continue;
                }

                // The mean must lie within the observed range; otherwise the statistics are
                // internally inconsistent (min/max and mean cannot describe the same values).
                if stats.mean < stats.min || stats.mean > stats.max {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            at(),
                            "STATISTICAL.MEAN_OUT_OF_RANGE",
                            format!(
                                "stream `{}` in episode {}: mean {} lies outside range [{}, {}]",
                                stream.name, ep.index, stats.mean, stats.min, stats.max
                            ),
                        )
                        .with_risk("A mean outside its own min/max means the stored statistics are corrupt; normalization built on them will be wrong.")
                        .with_remedy("Re-derive the statistics from the data."),
                    );
                    continue;
                }

                // Degenerate (constant) distribution: no signal to learn from.
                if is_degenerate(&stats) {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Warning,
                            at(),
                            "STATISTICAL.DEGENERATE",
                            format!(
                                "stream `{}` in episode {} is constant (min == max, std == 0)",
                                stream.name, ep.index
                            ),
                        )
                        .with_risk(
                            "A constant stream carries no information and can bias training.",
                        )
                        .with_remedy(
                            "Confirm the sensor was active; consider excluding the stream.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

fn is_degenerate(stats: &StreamStats) -> bool {
    stats.min == stats.max && stats.std == 0.0
}
