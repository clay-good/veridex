//! Statistical checks over stored per-stream statistics.
//!
//! MVP scope: range/sanity and degenerate-distribution checks on the statistics a source records
//! (design keeps Veridex from decoding frame payloads). Stored-vs-recomputed comparison arrives
//! once adapters stream values.

use crate::cdm::{Dataset, StreamStats};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Actuator/state saturation. When the adapter recomputes values from the data
/// ([`Stream::observed_saturation`](crate::cdm::Stream::observed_saturation)), a stream that spends a
/// large fraction of its samples **exactly** pinned at one extreme is a clamped or saturated channel
/// — a gripper commanded past its stop, an actuator against its limit, a state that flatlines at a
/// rail. The controller can't distinguish "at the limit" from "wants to go further," so the policy
/// learns from an observation that no longer tracks intent.
///
/// Exact equality is the signal (the same false-positive-free philosophy as
/// `STRUCTURAL.STUCK_STREAM`): a real, noisy sensor never lands on the identical float hundreds of
/// times, so a high pinned fraction is unambiguous. A fully constant stream (every value equal, so
/// both ends coincide) is `STATISTICAL.DEGENERATE`'s concern and is left to it.
pub struct Saturation {
    /// Fraction of samples pinned at a single extreme, at or above which the stream is flagged.
    pub min_fraction: f64,
    /// Minimum sample count below which the fraction isn't trustworthy and the check abstains.
    pub min_samples: u64,
}

impl Default for Saturation {
    fn default() -> Self {
        // Half an episode's samples pinned at one rail is well past incidental contact; 20 samples
        // is the floor below which a "fraction" says little.
        Self {
            min_fraction: 0.5,
            min_samples: 20,
        }
    }
}

impl Check for Saturation {
    fn id(&self) -> &'static str {
        "statistical.saturation"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STATISTICAL.SATURATED"]
    }
    fn title(&self) -> &'static str {
        "Actuator/state saturation"
    }
    fn category(&self) -> Category {
        Category::Statistical
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        // The adapter recomputes one saturation summary per stream (dataset-level), so report each
        // stream once — on the first episode that carries it — rather than repeating per episode.
        let mut reported: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let Some(sat) = stream.observed_saturation else {
                    continue;
                };
                if sat.sample_count < self.min_samples {
                    continue;
                }
                // A fully constant stream pins at both ends at once; that is DEGENERATE, not
                // saturation. Only flag when the two rails are genuinely distinct.
                if sat.min == sat.max {
                    continue;
                }
                if !reported.insert(stream.name.as_str()) {
                    continue;
                }
                let n = sat.sample_count as f64;
                let hi = sat.at_max as f64 / n;
                let lo = sat.at_min as f64 / n;
                let (frac, value, end) = if hi >= lo {
                    (hi, sat.max, "maximum")
                } else {
                    (lo, sat.min, "minimum")
                };
                if frac < self.min_fraction {
                    continue;
                }
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Statistical,
                        Severity::Warning,
                        Location::Stream {
                            episode: ep.index,
                            stream: stream.name.clone(),
                        },
                        "STATISTICAL.SATURATED",
                        format!(
                            "stream `{}`: {:.0}% of values sit exactly at its {end} ({value}) — a saturated or clamped channel",
                            stream.name,
                            frac * 100.0
                        ),
                    )
                    .with_risk(
                        "A channel pinned at its limit can't express intent past that limit; the policy \
                         learns from observations that no longer track the command, and imitation of a \
                         saturated actuator transfers poorly.",
                    )
                    .with_remedy(
                        "Check for a mis-scaled or clipped command range, a mechanical end-stop, or a \
                         mis-calibrated sensor; rescale or exclude the stream before training.",
                    ),
                );
            }
        }
        findings
    }
}

/// Range, sanity, and degeneracy of stored per-stream statistics.
pub struct RangeSanity;

impl Check for RangeSanity {
    fn id(&self) -> &'static str {
        "statistical.range-sanity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "STATISTICAL.NON_FINITE",
            "STATISTICAL.RANGE_INVERTED",
            "STATISTICAL.NEGATIVE_STD",
            "STATISTICAL.MEAN_OUT_OF_RANGE",
            "STATISTICAL.STD_IMPLAUSIBLE",
            "STATISTICAL.DTYPE_RANGE",
            "STATISTICAL.DEGENERATE",
        ]
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

                // Popoviciu's inequality: for values bounded in [min, max], the standard
                // deviation cannot exceed (max - min) / 2. A stored std above that bound is
                // mathematically impossible — the min/max and std cannot describe the same values.
                let bound = (stats.max - stats.min) / 2.0;
                let tol = 1e-9 + bound.abs() * 1e-6;
                if stats.std > bound + tol {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            at(),
                            "STATISTICAL.STD_IMPLAUSIBLE",
                            format!(
                                "stream `{}` in episode {}: std {} exceeds the maximum possible {} for range [{}, {}]",
                                stream.name, ep.index, stats.std, bound, stats.min, stats.max
                            ),
                        )
                        .with_risk("An impossibly large std means the stored statistics don't match the data (often computed on the wrong dtype or stream); normalization built on them will be wrong.")
                        .with_remedy("Re-derive the statistics from the data."),
                    );
                    continue;
                }

                // Stored stats must fit the stream's declared integer dtype: a `uint8` stream can't
                // hold a value of 300, so min/max outside the dtype's representable range means the
                // stats don't match the data (wrong dtype, or stats computed on rescaled values).
                if let Some((lo, hi)) = stream.dtype.as_deref().and_then(integer_dtype_range) {
                    if stats.min < lo || stats.max > hi {
                        let dtype = stream.dtype.as_deref().unwrap_or_default();
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Statistical,
                                Severity::Error,
                                at(),
                                "STATISTICAL.DTYPE_RANGE",
                                format!(
                                    "stream `{}` in episode {}: stored range [{}, {}] falls outside \
                                     what `{dtype}` can represent [{lo}, {hi}]",
                                    stream.name, ep.index, stats.min, stats.max
                                ),
                            )
                            .with_risk("Stats outside the declared dtype's range mean the dtype or the stats are wrong; normalization and any dtype-based decoding will be incorrect.")
                            .with_remedy("Reconcile the declared dtype with the data, or re-derive the statistics."),
                        );
                        continue;
                    }
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

/// The representable `[min, max]` of a declared integer dtype, as f64. Returns `None` for float,
/// non-integer, or unrecognized dtypes (nothing to bound), and for 64-bit integers whose extremes
/// f64 can't represent exactly (a bound there would risk false positives). Matched case-insensitively
/// against common spellings (`uint8`, `u8`, `int16`, `i16`, `bool`).
fn integer_dtype_range(dtype: &str) -> Option<(f64, f64)> {
    match dtype.trim().to_ascii_lowercase().as_str() {
        "bool" => Some((0.0, 1.0)),
        "uint8" | "u8" => Some((0.0, 255.0)),
        "int8" | "i8" => Some((-128.0, 127.0)),
        "uint16" | "u16" => Some((0.0, 65_535.0)),
        "int16" | "i16" => Some((-32_768.0, 32_767.0)),
        "uint32" | "u32" => Some((0.0, 4_294_967_295.0)),
        "int32" | "i32" => Some((-2_147_483_648.0, 2_147_483_647.0)),
        _ => None,
    }
}

/// Stored-vs-recomputed statistics. When an adapter records both the source's stored per-stream
/// statistics ([`Stream::stats`](crate::cdm::Stream::stats), e.g. LeRobot's `meta/stats.json`) and
/// the statistics Veridex recomputed from the actual feature values
/// ([`Stream::observed_stats`](crate::cdm::Stream::observed_stats)), the stored range must **contain**
/// the data. If a real value falls outside the stored `[min, max]`, the stored statistics are stale
/// or were computed on different data — and any normalization built from them clips or distorts the
/// true values.
///
/// Only `min`/`max` are compared: they are convention-free (unlike `mean`/`std`, whose exact value
/// depends on population-vs-sample and precision, which would risk false positives). A small relative
/// epsilon absorbs float rounding between the stored value and the recompute.
pub struct StoredVsObserved;

impl StoredVsObserved {
    /// Relative tolerance for the range comparison — absorbs float rounding, not real excursions.
    const REL_EPS: f64 = 1e-6;
}

impl Check for StoredVsObserved {
    fn id(&self) -> &'static str {
        "statistical.stored-vs-observed"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STATISTICAL.STATS_STALE"]
    }
    fn title(&self) -> &'static str {
        "Stored statistics match the data"
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
                let (Some(stored), Some(observed)) = (stream.stats, stream.observed_stats) else {
                    continue;
                };
                // Ignore corrupt stored stats (RangeSanity's concern) and non-finite recomputes.
                if !stored.min.is_finite() || !stored.max.is_finite() {
                    continue;
                }
                let tol = |x: f64| Self::REL_EPS * x.abs().max(1.0);
                let below = observed.min < stored.min - tol(stored.min);
                let above = observed.max > stored.max + tol(stored.max);
                if below || above {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Statistical,
                            Severity::Error,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "STATISTICAL.STATS_STALE",
                            format!(
                                "stream `{}` in episode {}: actual values [{}, {}] fall outside the \
                                 stored range [{}, {}] — the stored statistics don't match the data",
                                stream.name, ep.index, observed.min, observed.max, stored.min, stored.max
                            ),
                        )
                        .with_risk(
                            "Stored statistics that don't bound the real values were computed on \
                             different or stale data; normalization built from them clips or distorts \
                             the true inputs.",
                        )
                        .with_remedy(
                            "Recompute the stored statistics from the current data (e.g. regenerate \
                             `meta/stats.json`).",
                        ),
                    );
                }
            }
        }
        findings
    }
}
