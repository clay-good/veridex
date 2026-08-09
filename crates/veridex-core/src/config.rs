//! Configuration: a `veridex.toml` that selects which checks run, disables checks, overrides
//! severities, and sets the CI failure threshold. Configuration is explicit and, once applied, is
//! recorded in the verdict's effective config so a run stays reproducible from it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::check::{Category, Severity};
use crate::engine::RunConfig;

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
        }
    }
}
