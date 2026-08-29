//! Statistical checks over stored per-stream statistics.
//!
//! MVP scope: range/sanity and degenerate-distribution checks on the statistics a source records
//! (design keeps Veridex from decoding frame payloads). Stored-vs-recomputed comparison arrives
//! once adapters stream values.

use crate::cdm::{Dataset, Stream, StreamStats};
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
        // Keyed by the *measured values*, not by the stream name alone.
        //
        // The premise of a name-only key is that these statistics are dataset-level -- the adapter
        // recomputes one summary per stream and attaches it to every episode's copy. That holds for
        // LeRobot and is false for HDF5 and Zarr, which build a fresh accumulator per episode
        // group, so those numbers are genuinely per-episode facts. Deduping by name reported the
        // first affected episode and silently dropped the rest, including episodes carrying worse
        // defects than the one that was reported.
        //
        // Keying on the payload gets both: identical dataset-level stats collapse to one finding as
        // before, and distinct per-episode stats each report the episode a reader has to go and fix.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let Some(sat) = stream.observed_saturation else {
                    continue;
                };
                // Zero samples can't have a meaningful pinned fraction (and would make `at_*/n` a
                // NaN that slips past the `< min_fraction` gate); skip regardless of `min_samples`.
                if sat.sample_count == 0 || sat.sample_count < self.min_samples {
                    continue;
                }
                // A fully constant stream pins at both ends at once; that is DEGENERATE, not
                // saturation. Only flag when the two rails are genuinely distinct.
                if sat.min == sat.max {
                    continue;
                }
                // A **boolean** channel has two states and nothing between them, so "sits at its
                // rail" describes every value it will ever hold. RLDS carries `is_first` / `is_last`
                // on every step of every episode — 1 once and 0 for the rest — and LeRobot writes
                // `next.done` the same way, so grading them here reported two saturation warnings
                // on every well-formed dataset in the largest public robot corpus there is. What
                // *would* be a defect on such a channel is being constant, and `DEGENERATE` reports
                // that.
                if is_boolean_dtype(stream.dtype.as_deref()) {
                    continue;
                }
                // A non-finite rail is not a rail. `NaN == NaN` is false, so a NaN-railed stream
                // slipped past the check above and was reported as "90% of values sit exactly at
                // its minimum (NaN)" — a saturation claim about a value that is not a value. The
                // adapters hold non-finites out, but the CDM deserializes, so a JSON CDM reaches
                // here. Non-finite values are `STATISTICAL.NON_FINITE_OBSERVED`'s to report.
                if !sat.min.is_finite() || !sat.max.is_finite() {
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
                // Claim the stream only now that it has produced a finding — claiming it on sight
                // would let a clean episode-0 copy mask a saturated one in a later episode.
                if !reported.insert((stream.name.as_str(), format!("{sat:?}"))) {
                    continue;
                }
                // Name the dimension for a multi-DoF feature (e.g. the gripper joint); a scalar
                // stream saturates at dimension 0, where the qualifier would only add noise.
                let where_ = if sat.dim > 0 {
                    format!(" (dimension {})", sat.dim)
                } else {
                    String::new()
                };
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
                            "stream `{}`{where_}: {:.0}% of values sit exactly at its {end} ({value}) — a saturated or clamped channel",
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

/// Non-finite values in the **actual data**. When the adapter recomputes values from the source
/// ([`Stream::observed_non_finite`](crate::cdm::Stream::observed_non_finite)), any NaN or ±infinity
/// among a stream's scalars is counted — across every dimension of a multi-DoF cell, so a NaN buried
/// in one joint of an arm is still caught. A single non-finite value in a training tensor propagates
/// to a NaN loss and silently kills a run.
///
/// This is distinct from `STATISTICAL.NON_FINITE`, which inspects the source's **stored**
/// `stats.json`: a dataset whose stored summary is clean (or absent) can still hold NaN/inf in the
/// data itself, and only a recompute over the real values sees it. Because the non-finite values are
/// held out of `observed_stats` (a NaN would poison every summary), this count is their only record.
pub struct NonFiniteObserved;

impl Check for NonFiniteObserved {
    fn id(&self) -> &'static str {
        "statistical.non-finite-observed"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STATISTICAL.NON_FINITE_OBSERVED"]
    }
    fn title(&self) -> &'static str {
        "Non-finite values in the data"
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
        // Keyed by the measured value, not by the stream name alone: these counts are
        // dataset-level for LeRobot and genuinely per-episode for HDF5 and Zarr, which build a
        // fresh accumulator per episode group. See `Saturation::run` for the full reasoning.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // `None` means values were never read (e.g. MCAP); `Some(0)` means read and clean.
                let Some(count) = stream.observed_non_finite else {
                    continue;
                };
                if count == 0 {
                    continue;
                }
                if !reported.insert((stream.name.as_str(), format!("{count}"))) {
                    continue;
                }
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Statistical,
                        Severity::Error,
                        Location::Stream {
                            episode: ep.index,
                            stream: stream.name.clone(),
                        },
                        "STATISTICAL.NON_FINITE_OBSERVED",
                        format!(
                            "stream `{}`: {count} non-finite value(s) (NaN or ±inf) in the recorded data",
                            stream.name
                        ),
                    )
                    .with_risk(
                        "A NaN or infinity in a training tensor propagates to a NaN loss and gradient, \
                         silently destroying the run; stored summary statistics can hide it entirely.",
                    )
                    .with_remedy(
                        "Locate and drop or repair the affected frames (a failed sensor read or a \
                         divide-by-zero in a derived channel); never normalize over non-finite data.",
                    ),
                );
            }
        }
        findings
    }
}

/// Values measured against the range their **source declares** for them.
///
/// A DBC states each signal's physical span (`[0|16383.75]`), and that declaration is a fact about
/// the data separate from any summary of it: it says what the bus designer specified, before a
/// single frame was read. Comparing the two answers the question neither a summary nor a checksum
/// can — whether this log was decoded against the database that describes it.
///
/// That is the failure this exists for. A CAN log decoded against the wrong DBC does not error: the
/// bytes are the right length, every signal produces a number, and the timeline is intact. What
/// gives it away is that the numbers do not fit the ranges the database declares — a wheel speed of
/// 40,000 kph, a temperature of -3,000 °C. Wrong in every stream at once, and otherwise
/// indistinguishable from a good read.
///
/// A **warning**, not an error: a sensor can legitimately read past its specified span, and the
/// finding names how far past. Silent where nothing was declared (most formats declare no range) or
/// where no values were read — `STATISTICAL.UNMEASURED_VALUES` owns that second silence, and says so.
pub struct DeclaredRangeConformance;

impl Check for DeclaredRangeConformance {
    fn id(&self) -> &'static str {
        "statistical.declared-range"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STATISTICAL.OUT_OF_DECLARED_RANGE"]
    }
    fn title(&self) -> &'static str {
        "Values against the source's declared range"
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
        // Keyed by the stream and what was measured, for the same reason every other check in this
        // family is: a summary can be dataset-level (attached to every episode's copy of the stream)
        // or genuinely per-episode, and one finding per episode over the former is noise.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                let (Some(declared), Some(observed)) =
                    (stream.declared_range, stream.observed_stats)
                else {
                    continue;
                };
                if !declared.min.is_finite() || !declared.max.is_finite() {
                    continue;
                }
                // Only what falls outside, and only when it is outside by more than the slack the
                // rest of this family allows for float round-trips.
                let slack = rounding_tolerance(&observed);
                let below = (observed.min < declared.min - slack).then_some(observed.min);
                let above = (observed.max > declared.max + slack).then_some(observed.max);
                if below.is_none() && above.is_none() {
                    continue;
                }
                let key = format!("{declared:?}{:?}{:?}", observed.min, observed.max);
                if !reported.insert((stream.name.as_str(), key)) {
                    continue;
                }
                let breach = match (below, above) {
                    (Some(lo), Some(hi)) => format!("reaches {lo} and {hi}"),
                    (Some(lo), None) => format!("reaches {lo}"),
                    (None, Some(hi)) => format!("reaches {hi}"),
                    (None, None) => unreachable!("one bound is breached"),
                };
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Statistical,
                        Severity::Warning,
                        Location::Stream {
                            episode: ep.index,
                            stream: stream.name.clone(),
                        },
                        "STATISTICAL.OUT_OF_DECLARED_RANGE",
                        format!(
                            "stream `{}` in episode {}: the source declares [{}, {}] but the data \
                             {breach}",
                            stream.name, ep.index, declared.min, declared.max
                        ),
                    )
                    .with_risk(
                        "Values outside the range their own source declares usually mean the data \
                         was decoded against the wrong description of it — the wrong DBC for this \
                         bus, or a signal whose scaling changed. Every value in every stream is \
                         then plausible and wrong, which no checksum and no summary statistic can \
                         reveal. The narrower reading is a sensor operating out of spec.",
                    )
                    .with_remedy(
                        "Confirm the database matches the recording (bus, gateway and revision), \
                         then check the signal's factor, offset and length against it. If the \
                         decode is right, the sensor is out of spec and the excursion is the \
                         finding.",
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
        // Keyed by the stored values, not by the stream name alone: they are dataset-level for
        // LeRobot and genuinely per-episode for HDF5 and Zarr. See `Saturation::run`. The stream is
        // claimed only once it has actually produced a finding, so a clean earlier episode never
        // masks a corrupt later one.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // Element 0 is only the *first* dimension's summary. A multi-DoF feature's stored
                // stats are per-dimension arrays, and reading element 0 alone meant an inverted
                // range, a NaN, a negative standard deviation, or a dead joint on any axis above the
                // first passed clean — while `value-measurability` counted `dim_stats` as "stats
                // present", so nothing abstained either and the certificate listed this check as
                // executed. On a 7-DoF arm that is six unexamined joints.
                let per_dim = stream
                    .dim_stats
                    .iter()
                    .flatten()
                    .map(|d| (Some(d.dim), d.stats));
                let scalar = stream.stats.map(|s| (None, s));
                // The *most severe* across the scan, not the first. `evaluate` orders its rules
                // from "the numbers are corrupt" to "the numbers are fine but degenerate" because
                // later rules restate the same defect -- but that reasoning holds *within* one
                // dimension's stats, not across dimensions, and element 0 is scanned first. So a
                // constant element 0 -- a locked DoF or a gripper at rest, the ordinary case in
                // robot data -- emitted its Warning and the per-dimension stats were never examined
                // at all. An inverted range or a NaN on any other axis went unreported, and because
                // max severity drives the run's status, a corrupt dataset came back
                // pass-with-warnings instead of fail.
                let Some(finding) = scalar
                    .into_iter()
                    .chain(per_dim)
                    .filter_map(|(dim, stats)| self.evaluate(stream, ep.index, &stats, dim))
                    // `reduce`, not `max_by_key`: ties keep the *first*, so the lowest dimension is
                    // the one named, and the scan order stays the reported order.
                    .reduce(|a, b| if b.severity > a.severity { b } else { a })
                else {
                    continue;
                };
                let key = format!("{:?}{:?}", stream.stats, stream.dim_stats);
                if !reported.insert((stream.name.as_str(), key)) {
                    continue;
                }
                findings.push(finding);
            }
        }
        findings
    }
}

impl RangeSanity {
    /// The first way this stream's stored statistics are unusable, if any. Rules are ordered from
    /// "the numbers are corrupt" to "the numbers are fine but the distribution is degenerate", and
    /// the first that fires is the finding — later rules read the same fields and would only restate
    /// the same defect.
    fn evaluate(
        &self,
        stream: &Stream,
        episode: u64,
        stats: &StreamStats,
        dim: Option<u64>,
    ) -> Option<Finding> {
        let at = || Location::Stream {
            episode,
            stream: stream.name.clone(),
        };
        // What the message calls the thing these numbers summarize. For a multi-DoF feature that is
        // one joint, not the whole feature, and saying so is the difference between a report a user
        // can act on and one that sends them auditing six healthy axes.
        let name = match dim {
            Some(d) => format!("{} dim {d}", stream.name),
            None => stream.name.clone(),
        };

        // Non-finite statistics are always a data-integrity error.
        if [stats.min, stats.max, stats.mean, stats.std]
            .iter()
            .any(|v| !v.is_finite())
        {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.NON_FINITE",
                    format!(
                        "stream `{}` in episode {} has non-finite stored statistics",
                        name, episode
                    ),
                )
                .with_risk(
                    "NaN/inf statistics poison normalization and any model that consumes them.",
                )
                .with_remedy(
                    "Recompute the statistics from clean values, or drop the corrupt stream.",
                ),
            );
        }

        // Inverted range: min must not exceed max; std must be non-negative.
        if stats.min > stats.max {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.RANGE_INVERTED",
                    format!(
                        "stream `{}` in episode {}: min {} exceeds max {}",
                        name, episode, stats.min, stats.max
                    ),
                )
                .with_risk("An inverted range means the stored statistics are corrupt.")
                .with_remedy("Re-derive the statistics from the data."),
            );
        }
        if stats.std < 0.0 {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.NEGATIVE_STD",
                    format!(
                        "stream `{}` in episode {}: negative std {}",
                        name, episode, stats.std
                    ),
                )
                .with_risk(
                    "A negative standard deviation is impossible; the statistics are corrupt.",
                )
                .with_remedy("Re-derive the statistics from the data."),
            );
        }

        // The mean must lie within the observed range; otherwise the statistics are internally
        // inconsistent (min/max and mean cannot describe the same values). Allow a small float
        // tolerance — a source's independently-rounded mean can land one ULP past a bound on a
        // near-constant stream, which is honest data, not corruption (mirrors the Popoviciu std
        // check below).
        let mean_tol = rounding_tolerance(stats);
        if stats.mean < stats.min - mean_tol || stats.mean > stats.max + mean_tol {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.MEAN_OUT_OF_RANGE",
                    format!(
                        "stream `{}` in episode {}: mean {} lies outside range [{}, {}]",
                        name, episode, stats.mean, stats.min, stats.max
                    ),
                )
                .with_risk("A mean outside its own min/max means the stored statistics are corrupt; normalization built on them will be wrong.")
                .with_remedy("Re-derive the statistics from the data."),
            );
        }

        // Popoviciu's inequality: for values bounded in [min, max], the standard deviation cannot
        // exceed (max - min) / 2. A stored std above that bound is mathematically impossible — the
        // min/max and std cannot describe the same values.
        let bound = (stats.max - stats.min) / 2.0;
        if stats.std > bound + rounding_tolerance(stats) {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.STD_IMPLAUSIBLE",
                    format!(
                        "stream `{}` in episode {}: std {} exceeds the maximum possible {} for range [{}, {}]",
                        name, episode, stats.std, bound, stats.min, stats.max
                    ),
                )
                .with_risk("An impossibly large std means the stored statistics don't match the data (often computed on the wrong dtype or stream); normalization built on them will be wrong.")
                .with_remedy("Re-derive the statistics from the data."),
            );
        }

        // The same contradiction from the other side, at its one exact point: values that are not
        // all identical have *some* spread, so a standard deviation of zero beside a min and max
        // that differ describes no possible set of values. Only exactly zero is impossible — a
        // million values at the mean and one at each extreme drives the std arbitrarily close to it
        // — which is why this is the lower bound and Popoviciu is the upper one, with the whole
        // range between them admissible.
        //
        // Worth naming rather than leaving to another check: `statistical.extreme-outlier` divides
        // by this std, so a stored zero made every value's z-score infinite. It steps aside for
        // "corrupt stats, someone else's finding" — and nobody else had one, so a source that
        // stored a spread of zero over a range of ten passed in silence.
        let slack = rounding_tolerance(stats);
        if stats.max > stats.min + slack && stats.std.abs() <= slack {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Error,
                    at(),
                    "STATISTICAL.STD_IMPLAUSIBLE",
                    format!(
                        "stream `{}` in episode {}: std {} is zero over range [{}, {}], which no \
                         set of values has",
                        name, episode, stats.std, stats.min, stats.max
                    ),
                )
                .with_risk("A std of zero beside a non-zero range means the stored statistics don't match the data (often carried over from a different stream, or never computed at all); normalization built on them divides by zero, and every z-score derived from them is infinite.")
                .with_remedy("Re-derive the statistics from the data."),
            );
        }

        // Stored stats must fit the stream's declared integer dtype: a `uint8` stream can't hold a
        // value of 300, so min/max outside the dtype's representable range means the stats don't
        // match the data (wrong dtype, or stats computed on rescaled values).
        if let Some((lo, hi)) = stream.dtype.as_deref().and_then(integer_dtype_range) {
            if stats.min < lo || stats.max > hi {
                let dtype = stream.dtype.as_deref().unwrap_or_default();
                return Some(
                    Finding::new(
                        self.id(),
                        Category::Statistical,
                        Severity::Error,
                        at(),
                        "STATISTICAL.DTYPE_RANGE",
                        format!(
                            "stream `{}` in episode {}: stored range [{}, {}] falls outside \
                             what `{dtype}` can represent [{lo}, {hi}]",
                            name, episode, stats.min, stats.max
                        ),
                    )
                    .with_risk("Stats outside the declared dtype's range mean the dtype or the stats are wrong; normalization and any dtype-based decoding will be incorrect.")
                    .with_remedy("Reconcile the declared dtype with the data, or re-derive the statistics."),
                );
            }
        }

        // Degenerate (constant) distribution: no signal to learn from.
        if is_degenerate(stats) {
            return Some(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Warning,
                    at(),
                    "STATISTICAL.DEGENERATE",
                    format!(
                        // Says `min == max`, which is exactly what was tested, rather than
                        // `std == 0`, which is not: the std only has to be within the rounding
                        // slack, and at magnitude 1e6 that slack is 1.0 — so this line asserted a
                        // std of zero over a stored std of 0.9, in a signed certificate.
                        "stream `{}` in episode {} is constant (min == max)",
                        name, episode
                    ),
                )
                .with_risk("A constant stream carries no information and can bias training.")
                .with_remedy("Confirm the sensor was active; consider excluding the stream."),
            );
        }

        None
    }
}

/// The slack a stored statistic gets before it is called impossible, scaled to the magnitude of the
/// values it describes.
///
/// A source computes its statistics independently and in its own precision, so a value near the
/// bound can land a few ULPs past it. The scale has to follow the *values*, not the width of their
/// range: a channel sitting at 300.0 with a range of 0.0002 has a Popoviciu bound of 1e-4, while the
/// naive `E[x²] − E[x]²` any exporter might use loses about 3e-4 to cancellation at that magnitude —
/// so a range-scaled tolerance calls honest float noise mathematically impossible.
fn rounding_tolerance(stats: &StreamStats) -> f64 {
    let magnitude = 1e-9 + stats.min.abs().max(stats.max.abs()) * 1e-6;
    // Scaled to the magnitude, but capped relative to the range it is slack *within*. Uncapped, the
    // magnitude term swallowed the quantity it was guarding: at values around 1e9 — nanosecond
    // stamps carried as a feature, GPS in millimetres, encoder counts — it is 1000 units, so a
    // channel with `min = 1e9, max = 1e9 + 100` got a tolerance ten times its own range, and a
    // standard deviation eighteen times past the Popoviciu bound and a mean sitting 800 units
    // outside its own `[min, max]` both passed in silence. Four times the range is a deliberately
    // loose ceiling: it still admits every case the magnitude scaling exists for (the 0.0002-wide
    // channel at 300.0 above keeps its full 3e-4) and only binds where the magnitude term had grown
    // large enough to disable the check outright.
    let range = (stats.max - stats.min).abs();
    if range.is_finite() && range > 0.0 {
        magnitude.min(range * 4.0)
    } else {
        magnitude
    }
}

/// A constant stream: every value the same. The std is *reported* as zero-ish rather than exactly
/// zero by any exporter that computes it in floating point, so the same rounding slack applies —
/// otherwise a constant channel with a 1e-12 std escapes the degeneracy warning entirely, and one
/// with a 1e-8 std is called an impossible standard deviation instead.
fn is_degenerate(stats: &StreamStats) -> bool {
    stats.min == stats.max && stats.std.abs() <= rounding_tolerance(stats)
}

/// Whether a stream's declared dtype is a boolean — a channel with two states and nothing between
/// them. Matched against the spellings the adapters carry through from their sources (`bool` from
/// TFDS `features.json` and LeRobot `info.json`, `boolean` from an Arrow-derived schema).
fn is_boolean_dtype(dtype: Option<&str>) -> bool {
    matches!(
        dtype.map(|d| d.trim().to_ascii_lowercase()).as_deref(),
        Some("bool") | Some("boolean") | Some("bool_") | Some("bool8")
    )
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

    /// Whether the observed range escapes the stored range (below its min or above its max) beyond the
    /// rounding tolerance. Returns `None` when the stored bounds are non-finite (RangeSanity's concern).
    fn escapes(stored: &StreamStats, observed: &StreamStats) -> Option<bool> {
        if !stored.min.is_finite() || !stored.max.is_finite() {
            return None;
        }
        let tol = |x: f64| Self::REL_EPS * x.abs().max(1.0);
        Some(
            observed.min < stored.min - tol(stored.min)
                || observed.max > stored.max + tol(stored.max),
        )
    }
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
        // Both stored and recomputed stats are dataset-level (attached identically to every episode's
        // copy of a stream), so report each stream once — on the first episode carrying it — rather
        // than emitting the same finding per episode.
        // Keyed by the compared values, not by the stream name alone: they are dataset-level for
        // LeRobot and genuinely per-episode for HDF5 and Zarr. See `Saturation::run`.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // For a multi-DoF feature with both stored and recomputed per-dimension stats, compare
                // each dimension and report the first stale one — normalization is per dimension, so a
                // stale stat in any joint matters. Otherwise fall back to the scalar (element-0) pair.
                let hit: Option<(StreamStats, StreamStats, u64)> =
                    match (&stream.dim_stats, &stream.observed_dim_stats) {
                        (Some(stored_dims), Some(obs_dims)) => stored_dims.iter().find_map(|sd| {
                            let od = obs_dims.iter().find(|o| o.dim == sd.dim)?;
                            (Self::escapes(&sd.stats, &od.stats)?)
                                .then_some((sd.stats, od.stats, sd.dim))
                        }),
                        _ => match (stream.stats, stream.observed_stats) {
                            (Some(stored), Some(observed)) => Self::escapes(&stored, &observed)
                                .unwrap_or(false)
                                .then_some((stored, observed, 0)),
                            _ => None,
                        },
                    };
                let Some((stored, observed, dim)) = hit else {
                    continue;
                };
                let key = format!("{stored:?}{observed:?}{dim}");
                if !reported.insert((stream.name.as_str(), key)) {
                    continue;
                }
                // Name the dimension for a multi-DoF feature; a scalar/element-0 mismatch needs none.
                let where_ = if dim > 0 {
                    format!(" (dimension {dim})")
                } else {
                    String::new()
                };
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
                            "stream `{}`{where_}: actual values [{}, {}] fall outside the \
                             stored range [{}, {}] — the stored statistics don't match the data",
                            stream.name, observed.min, observed.max, stored.min, stored.max
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
        findings
    }
}

/// Extreme-outlier detection from summary statistics alone. A per-stream extreme (min or max) that
/// sits many standard deviations from the mean is, by **Chebyshev's inequality**, necessarily a rare
/// value: no more than `1/z²` of the samples can be `z` standard deviations or further from the mean.
/// So a `z` of 10 guarantees the flagged extreme is at most 1% of the data — a sparse spike (a sensor
/// glitch, a unit error, a dropped-to-zero frame), not the fat tail of a wide-but-normal distribution.
///
/// This reads only the recorded/recomputed `min`/`max`/`mean`/`std`, never frame payloads. It stays
/// out of `RangeSanity`'s way: corrupt or degenerate stats (non-finite, inverted, `std == 0`) are that
/// check's concern and are skipped here.
pub struct ExtremeOutlier {
    /// Standard-deviations-from-mean at or beyond which an extreme is flagged. The Chebyshev tail
    /// bound is `1/z²`, so `z = 10` means the flagged value is ≤1% of the samples.
    pub z_threshold: f64,
}

impl Default for ExtremeOutlier {
    fn default() -> Self {
        Self { z_threshold: 10.0 }
    }
}

impl ExtremeOutlier {
    fn check_stats(&self, stats: &StreamStats) -> Option<(f64, f64, &'static str)> {
        // Leave corrupt/degenerate stats to RangeSanity; a zero/non-finite std has no z-scale.
        if !(stats.min.is_finite()
            && stats.max.is_finite()
            && stats.mean.is_finite()
            && stats.std.is_finite())
            || stats.std <= 0.0
            || stats.min > stats.max
            || stats.mean < stats.min
            || stats.mean > stats.max
        {
            return None;
        }
        let z_hi = (stats.max - stats.mean) / stats.std;
        let z_lo = (stats.mean - stats.min) / stats.std;
        let (z, value, end) = if z_hi >= z_lo {
            (z_hi, stats.max, "maximum")
        } else {
            (z_lo, stats.min, "minimum")
        };
        (z >= self.z_threshold).then_some((z, value, end))
    }
}

impl Check for ExtremeOutlier {
    fn id(&self) -> &'static str {
        "statistical.extreme-outlier"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STATISTICAL.OUTLIER"]
    }
    fn title(&self) -> &'static str {
        "Extreme value outlier"
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
        // Both stored and recomputed stats are dataset-level (attached identically to every episode's
        // copy of a stream), so report each stream once — on the first episode carrying it — rather
        // than emitting the same finding per episode.
        // Keyed by the compared values, not by the stream name alone: they are dataset-level for
        // LeRobot and genuinely per-episode for HDF5 and Zarr. See `Saturation::run`.
        let mut reported: std::collections::BTreeSet<(&str, String)> =
            std::collections::BTreeSet::new();
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // For a multi-DoF feature, scan every dimension and keep the most extreme outlier, so
                // a spike in a non-first joint is caught and named. Otherwise fall back to the stream's
                // scalar summary (Veridex's recompute if present, else the source's stats).
                // Veridex's recompute if present, else the source's stored arrays -- the same
                // precedence the scalar path below uses. Reading only `observed_dim_stats` meant
                // that under `--metadata-only`, where nothing is recomputed and only the source's
                // `dim_stats` exist, the scan fell through to element 0 and every non-first joint
                // went unexamined -- while the docs promise this check "scans every dimension and
                // names the outlying one" and list metadata-only as covering every stored-statistics
                // check. `range-sanity` consults `dim_stats` already; the two disagreed on what
                // they read.
                let dims = stream
                    .observed_dim_stats
                    .as_ref()
                    .or(stream.dim_stats.as_ref());
                let hit = if let Some(dims) = dims {
                    dims.iter()
                        .filter_map(|d| {
                            self.check_stats(&d.stats).map(|(z, v, e)| (z, v, e, d.dim))
                        })
                        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
                } else {
                    stream
                        .observed_stats
                        .or(stream.stats)
                        .and_then(|s| self.check_stats(&s).map(|(z, v, e)| (z, v, e, 0)))
                };
                let Some((z, value, end, dim)) = hit else {
                    continue;
                };
                let key = format!("{z:?}{value:?}{end}{dim}");
                if !reported.insert((stream.name.as_str(), key)) {
                    continue;
                }
                // Name the dimension for a multi-DoF feature; a scalar/element-0 extreme needs none.
                let where_ = if dim > 0 {
                    format!(" (dimension {dim})")
                } else {
                    String::new()
                };
                let tail_pct = 100.0 / (z * z);
                // Rendered in scientific notation past four digits. A stream whose std is
                // `f64::MIN_POSITIVE` — which clears the `std <= 0.0` guard — gives a z around
                // 2.2e307, and `{z:.1}` expands that to 310 digits of decimal, all of which end up
                // in the message, the JSON, the SARIF, and the certificate.
                let z_text = if z >= 10_000.0 {
                    format!("{z:.1e}")
                } else {
                    format!("{z:.1}")
                };
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Statistical,
                        Severity::Warning,
                        Location::Stream {
                            episode: ep.index,
                            stream: stream.name.clone(),
                        },
                        "STATISTICAL.OUTLIER",
                        format!(
                            "stream `{}`{where_}: its {end} ({value}) is {z_text}σ from the mean — \
                             an extreme outlier (at most {tail_pct:.2}% of samples can lie this far out)",
                            stream.name
                        ),
                    )
                    .with_risk(
                        "A lone extreme far from the mean dominates min/max normalization — it squashes \
                         the real signal into a sliver of the range — and destabilizes training; it is \
                         usually a sensor glitch or a unit error, not real data.",
                    )
                    .with_remedy(
                        "Inspect the extreme; clip/winsorize it or fix the unit/scale error, then \
                         recompute the statistics.",
                    ),
                );
            }
        }
        findings
    }
}

/// Streams whose values this run never read — and therefore what the rest of this family could not
/// measure.
///
/// The same reasoning as [`ClockMeasurability`](crate::checks::temporal::ClockMeasurability), one
/// family over. Every statistical check reads either the source's *stored* summary statistics or the
/// ones the adapter recomputed while fingerprinting the data. Four adapters populate neither: MCAP,
/// rosbag2, CAN+DBC, ASAM MF4, and RLDS/TFDS fingerprint payload bytes without interpreting them, so
/// `stats`, `observed_stats`, `observed_saturation`, and `observed_non_finite` are all `None`. Every
/// check in the family then hits its `let Some(...) else { continue }` and produces nothing.
///
/// What that looked like: a CAN log with a wheel-speed signal pinned exactly at 655.35 km/h for 70%
/// of its frames, and a constant throttle beside it, reported `pass-with-warnings`, `data 100`, and
/// findings from the provenance family alone — while the certificate listed all five statistical
/// checks under `checks_run` with `categories_skipped: []`. A saturated actuator, a NaN buried in a
/// signal, and a stale stored statistic all sign as checked-and-clean.
///
/// HDF5 and Zarr are a narrower case of the same thing: they recompute statistics but carry no
/// stored ones, so the two checks that compare the source's own summary against the data — the
/// stored-vs-observed comparison and the stored-range sanity rules — can never fire on them either.
/// Both are reported, because the difference between "checked and clean" and "never looked" is the
/// whole value of the result.
///
/// Informational, not a defect: a dataset is not worse for the container it was published in. What
/// it changes is what a passing verdict is evidence of.
pub struct ValueMeasurability;

impl Check for ValueMeasurability {
    fn id(&self) -> &'static str {
        "statistical.value-measurability"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "STATISTICAL.UNMEASURED_VALUES",
            "STATISTICAL.NO_STORED_STATS",
        ]
    }
    fn title(&self) -> &'static str {
        "Values were read at all"
    }
    fn category(&self) -> Category {
        Category::Statistical
    }
    fn default_severity(&self) -> Severity {
        Severity::Info
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // Reported once for the dataset: whether values are read is a property of the source format,
        // so one finding per episode would repeat the same fact for every episode.
        let mut unread: std::collections::BTreeSet<&str> = Default::default();
        let mut no_stored: std::collections::BTreeSet<&str> = Default::default();
        for ep in &dataset.episodes {
            for s in &ep.streams {
                let recomputed = s.observed_stats.is_some()
                    || s.observed_saturation.is_some()
                    || s.observed_non_finite.is_some()
                    || s.observed_dim_stats.is_some();
                if !recomputed && s.stats.is_none() && s.dim_stats.is_none() {
                    unread.insert(s.name.as_str());
                } else if recomputed && s.stats.is_none() && s.dim_stats.is_none() {
                    no_stored.insert(s.name.as_str());
                }
            }
        }

        let mut findings = Vec::new();
        if !unread.is_empty() {
            let names: Vec<&str> = unread.into_iter().collect();
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Info,
                    Location::Dataset,
                    "STATISTICAL.UNMEASURED_VALUES",
                    format!(
                        "{} stream(s) carry neither stored nor recomputed statistics, so the \
                         saturation, outlier, non-finite, stored-vs-observed and range-sanity \
                         checks had nothing to measure on them ({})",
                        names.len(),
                        list_a_few(&names),
                    ),
                )
                .with_risk(
                    "Nothing in this run can tell you whether these streams hold a saturated \
                     actuator, a NaN, a stuck sensor, or a unit error. A clean statistical result \
                     here is the absence of a measurement, not evidence of good values.",
                )
                .with_remedy(
                    "Treat the statistical result as unverified for these streams. If value \
                     integrity matters, check them in a format whose values Veridex reads \
                     (LeRobot, HDF5, Zarr, CAN+DBC, MF4).",
                ),
            );
        }
        if !no_stored.is_empty() {
            let names: Vec<&str> = no_stored.into_iter().collect();
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Statistical,
                    Severity::Info,
                    Location::Dataset,
                    "STATISTICAL.NO_STORED_STATS",
                    format!(
                        "{} stream(s) carry recomputed statistics but none stored by the source, \
                         so the stored-vs-observed comparison and the stored-range sanity rules had \
                         nothing to compare against ({})",
                        names.len(),
                        list_a_few(&names),
                    ),
                )
                .with_risk(
                    "The recomputed statistics still catch bad values, but nothing here can tell \
                     you whether the summary statistics the source publishes agree with its own \
                     data — the mismatch that silently mis-normalizes training.",
                )
                .with_remedy(
                    "Export the source's summary statistics alongside the data if downstream \
                     tooling relies on them.",
                ),
            );
        }
        findings
    }
}

/// Name a few of a list and count the rest. Naming four is useful; naming four hundred is not.
fn list_a_few(names: &[&str]) -> String {
    let shown = names.iter().take(4).copied().collect::<Vec<_>>().join(", ");
    match names.len().saturating_sub(4) {
        0 => shown,
        rest => format!("{shown} and {rest} more"),
    }
}
