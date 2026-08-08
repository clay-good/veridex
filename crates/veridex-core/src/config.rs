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
}

impl CheckConfig {
    /// Parse a config from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))
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
