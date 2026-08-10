//! Configuration: a `veridex.toml` that selects which checks run, disables checks, overrides
//! severities, and sets the CI failure threshold. Configuration is explicit and, once applied, is
//! recorded in the verdict's effective config so a run stays reproducible from it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::check::{Category, Severity};
use crate::engine::{RunConfig, Tolerances};

/// Errors parsing a configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The TOML was malformed or had unexpected fields/values.
    #[error("invalid config: {0}")]
    Parse(String),
}

/// The failure threshold: which severity makes `veridex check` exit with a failure code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailOn {
    /// Only `error` findings fail the run (default).
    #[default]
    Error,
    /// `warning` (or worse) fails the run.
    Warning,
}

/// The `[tolerances]` table: per-check numeric thresholds. Each is optional and falls back to the
/// check's built-in default. Times are in milliseconds for readability.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TolerancesConfig {
    /// `TEMPORAL.CLOCK_SKEW` max cross-stream duration drift, in milliseconds.
    pub clock_skew_ms: Option<f64>,
    /// `TEMPORAL.START_OFFSET` max shared-clock start offset, in milliseconds.
    pub start_offset_ms: Option<f64>,
    /// `TEMPORAL.RATE` allowed relative rate deviation (0.10 = 10%).
    pub rate_deviation: Option<f64>,
    /// `TEMPORAL.GAP` gap factor: an interval this many times the expected one is a gap.
    pub gap_factor: Option<f64>,
}

/// A parsed `veridex.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckConfig {
    /// Failure threshold for the CLI exit code.
    pub fail_on: FailOn,
    /// If set, only these categories run.
    pub categories: Option<Vec<Category>>,
    /// If set, only these check ids run.
    pub only_checks: Option<Vec<String>>,
    /// Check ids to disable.
    pub disabled_checks: Vec<String>,
    /// Per-check severity overrides, id → severity.
    pub severity_overrides: BTreeMap<String, Severity>,
    /// Minimum trust score (0–100) required to pass; a lower score fails the run. `None` disables
    /// the gate. The `--min-score` CLI flag overrides this.
    pub min_score: Option<u8>,
    /// Per-check numeric tolerances. Unset entries use the check's default.
    pub tolerances: TolerancesConfig,
}

impl CheckConfig {
    /// Parse a config from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: CheckConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        if let Some(n) = config.min_score {
            if n > 100 {
                return Err(ConfigError::Parse(format!(
                    "min_score must be between 0 and 100, got {n}"
                )));
            }
        }
        config.tolerances.validate()?;
        Ok(config)
    }

    /// Validate that every check id this config references — `only_checks`, `disabled_checks`, and
    /// the keys of `severity_overrides` — names a real check in `known`. A typo would otherwise
    /// silently no-op (a "disabled" check that keeps running, or a severity override that never
    /// applies), so an unknown id is a hard error per the configuration spec. Returns the first
    /// unknown id encountered, in a stable order (only-checks, then disabled, then overrides).
    pub fn validate_check_ids<'a, I>(&self, known: I) -> Result<(), ConfigError>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let known: BTreeSet<&str> = known.into_iter().collect();
        let referenced = self
            .only_checks
            .iter()
            .flatten()
            .chain(self.disabled_checks.iter())
            .chain(self.severity_overrides.keys());
        for id in referenced {
            if !known.contains(id.as_str()) {
                return Err(ConfigError::Parse(format!(
                    "unknown check id `{id}` in config (run `veridex checks` to list valid ids)"
                )));
            }
        }
        Ok(())
    }

    /// The engine [`RunConfig`] this configuration implies (everything except the exit threshold,
    /// which the CLI applies).
    pub fn to_run_config(&self) -> RunConfig {
        RunConfig {
            categories: self
                .categories
                .as_ref()
                .map(|c| c.iter().copied().collect::<BTreeSet<_>>()),
            only_checks: self
                .only_checks
                .as_ref()
                .map(|c| c.iter().cloned().collect::<BTreeSet<_>>()),
            disabled_checks: self.disabled_checks.iter().cloned().collect(),
            severity_overrides: self.severity_overrides.clone(),
            tolerances: self.tolerances.resolve(),
        }
    }
}

impl TolerancesConfig {
    /// Reject non-finite or negative values, and a non-positive gap factor (which would flag every
    /// interval). Called before a run so a bad threshold is an error, not a silent misbehavior.
    fn validate(&self) -> Result<(), ConfigError> {
        let non_negative = [
            ("clock_skew_ms", self.clock_skew_ms),
            ("start_offset_ms", self.start_offset_ms),
            ("rate_deviation", self.rate_deviation),
        ];
        for (name, v) in non_negative {
            if let Some(v) = v {
                if !v.is_finite() || v < 0.0 {
                    return Err(ConfigError::Parse(format!(
                        "{name} must be a finite, non-negative number, got {v}"
                    )));
                }
            }
        }
        if let Some(g) = self.gap_factor {
            if !g.is_finite() || g <= 0.0 {
                return Err(ConfigError::Parse(format!(
                    "gap_factor must be a finite, positive number, got {g}"
                )));
            }
        }
        Ok(())
    }

    /// Resolve into concrete [`Tolerances`], filling unset entries from the defaults. Millisecond
    /// times are converted to nanoseconds (saturating on absurd values).
    fn resolve(&self) -> Tolerances {
        let d = Tolerances::default();
        Tolerances {
            clock_skew_ns: self
                .clock_skew_ms
                .map(|ms| (ms * 1_000_000.0) as i64)
                .unwrap_or(d.clock_skew_ns),
            start_offset_ns: self
                .start_offset_ms
                .map(|ms| (ms * 1_000_000.0) as i64)
                .unwrap_or(d.start_offset_ns),
            rate_deviation: self.rate_deviation.unwrap_or(d.rate_deviation),
            gap_factor: self.gap_factor.unwrap_or(d.gap_factor),
        }
    }
}
