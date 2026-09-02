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
    /// `TEMPORAL.END_OFFSET` max shared-clock end offset, in milliseconds.
    pub end_offset_ms: Option<f64>,
    /// `TEMPORAL.RATE` allowed relative rate deviation (0.10 = 10%).
    pub rate_deviation: Option<f64>,
    /// `TEMPORAL.GAP` gap factor: an interval this many times the expected one is a gap.
    pub gap_factor: Option<f64>,
    /// `TEMPORAL.JITTER` max coefficient of variation (std / mean) of inter-frame intervals.
    pub jitter_cv: Option<f64>,
    /// `TEMPORAL.EPISODE_DURATION_OUTLIER` multiple-of-median past which an episode duration is an
    /// outlier. Must be greater than 1.0 (a factor of 1.0 or less would flag every episode).
    pub episode_duration_factor: Option<f64>,
    /// `STATISTICAL.SATURATED` pinned-fraction threshold (0.50 = half the samples). Must be within
    /// (0.0, 1.0]: a threshold of 0 would flag every stream, and above 1.0 nothing could ever reach it.
    pub saturation_fraction: Option<f64>,
    /// `STATISTICAL.SATURATED` minimum sample count below which the check abstains.
    pub saturation_min_samples: Option<u64>,
    /// `STATISTICAL.OUTLIER` standard-deviations-from-mean at or beyond which a value is flagged.
    /// Must be greater than 1.0: at or below 1σ the Chebyshev bound says nothing, so every stream
    /// would be flagged.
    pub outlier_z: Option<f64>,
    /// `AUTONOMY.SEQUENCE_COMPLETE` tolerated fraction of a rig sensor's implied frames that may be
    /// missing (0.05 = 5%). Must be within [0.0, 1.0).
    pub sequence_drop_fraction: Option<f64>,
    /// `AUTONOMY.EGO_POSE_CONTINUITY` maximum plausible ego speed, in metres per second; a step
    /// implying more than this is a teleport.
    pub ego_max_speed_mps: Option<f64>,
    /// `STRUCTURAL.NEAR_DUPLICATE_EPISODE` shared-frame fraction at which two episodes are reported
    /// as near-duplicates. Must be within (0.0, 1.0]: at 0 every pair matches.
    pub near_duplicate_fraction: Option<f64>,
    /// `AUTONOMY.SENSOR_CLOCK_OFFSET` maximum tolerated disagreement between a sensor's own capture
    /// stamps and the recorder's clock, in milliseconds. Must be non-negative.
    pub sensor_clock_offset_ms: Option<f64>,
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
    /// The top-level keys the file actually carried.
    ///
    /// Three settings have no "unset" value to test for: `fail_on` defaults to `error`,
    /// `disabled_checks` and `severity_overrides` to empty. So asking "does this differ from the
    /// default?" answered a different question from "did the file set it?", and a file that wrote
    /// `fail_on = "error"` had its own setting reported as `(default)` in the effective
    /// configuration — the one place that answers "was this run configured, and by whom", and which
    /// is signed into every certificate.
    #[serde(skip)]
    pub keys_present: std::collections::BTreeSet<String>,
}

impl CheckConfig {
    /// Parse a config from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let mut config: CheckConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        // Which keys the document carried, read from the document rather than inferred from the
        // parsed values — see `keys_present`.
        if let Ok(toml::Value::Table(table)) = toml::from_str::<toml::Value>(text) {
            config.keys_present = table.keys().cloned().collect();
        }
        config.validate()?;
        Ok(config)
    }

    /// Validate the values this config carries, independent of where they came from.
    ///
    /// Public because a config is no longer only parsed from a file: the environment layer merges
    /// onto a parsed config, and a value that arrives that way has to meet exactly the same bar as
    /// one written in the file. One validator, so the two cannot drift.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(n) = self.min_score {
            if n > 100 {
                return Err(ConfigError::Parse(format!(
                    "min_score must be between 0 and 100, got {n}"
                )));
            }
        }
        self.tolerances.validate()
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
            ("sensor_clock_offset_ms", self.sensor_clock_offset_ms),
            ("start_offset_ms", self.start_offset_ms),
            ("end_offset_ms", self.end_offset_ms),
            ("rate_deviation", self.rate_deviation),
            ("jitter_cv", self.jitter_cv),
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
        if let Some(f) = self.episode_duration_factor {
            if !f.is_finite() || f <= 1.0 {
                return Err(ConfigError::Parse(format!(
                    "episode_duration_factor must be a finite number greater than 1.0, got {f}"
                )));
            }
        }
        if let Some(f) = self.saturation_fraction {
            if !f.is_finite() || f <= 0.0 || f > 1.0 {
                return Err(ConfigError::Parse(format!(
                    "saturation_fraction must be a finite number in (0.0, 1.0], got {f}"
                )));
            }
        }
        if let Some(z) = self.outlier_z {
            if !z.is_finite() || z <= 1.0 {
                return Err(ConfigError::Parse(format!(
                    "outlier_z must be a finite number greater than 1.0, got {z}"
                )));
            }
        }
        if let Some(f) = self.sequence_drop_fraction {
            if !f.is_finite() || !(0.0..1.0).contains(&f) {
                return Err(ConfigError::Parse(format!(
                    "sequence_drop_fraction must be a finite number in [0.0, 1.0), got {f}"
                )));
            }
        }
        if let Some(v) = self.ego_max_speed_mps {
            if !v.is_finite() || v <= 0.0 {
                return Err(ConfigError::Parse(format!(
                    "ego_max_speed_mps must be a finite, positive number, got {v}"
                )));
            }
        }
        if let Some(f) = self.near_duplicate_fraction {
            if !f.is_finite() || f <= 0.0 || f > 1.0 {
                return Err(ConfigError::Parse(format!(
                    "near_duplicate_fraction must be a finite number in (0.0, 1.0], got {f}"
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
            end_offset_ns: self
                .end_offset_ms
                .map(|ms| (ms * 1_000_000.0) as i64)
                .unwrap_or(d.end_offset_ns),
            rate_deviation: self.rate_deviation.unwrap_or(d.rate_deviation),
            gap_factor: self.gap_factor.unwrap_or(d.gap_factor),
            jitter_cv: self.jitter_cv.unwrap_or(d.jitter_cv),
            episode_duration_factor: self
                .episode_duration_factor
                .unwrap_or(d.episode_duration_factor),
            saturation_fraction: self.saturation_fraction.unwrap_or(d.saturation_fraction),
            saturation_min_samples: self
                .saturation_min_samples
                .unwrap_or(d.saturation_min_samples),
            outlier_z: self.outlier_z.unwrap_or(d.outlier_z),
            sequence_drop_fraction: self
                .sequence_drop_fraction
                .unwrap_or(d.sequence_drop_fraction),
            ego_max_speed_mps: self.ego_max_speed_mps.unwrap_or(d.ego_max_speed_mps),
            near_duplicate_fraction: self
                .near_duplicate_fraction
                .unwrap_or(d.near_duplicate_fraction),
            sensor_clock_offset_ns: self
                .sensor_clock_offset_ms
                .map(|ms| (ms * 1_000_000.0) as i64)
                .unwrap_or(d.sensor_clock_offset_ns),
        }
    }
}

/// The environment layer: `VERIDEX_*` variables merged onto a parsed config.
///
/// The configuration spec's precedence is built-in defaults, then the config file, then the
/// environment, then explicit flags. The environment is the layer a container or a CI job can set
/// without writing a file, so every `veridex.toml` key has exactly one `VERIDEX_` twin — a partial
/// mapping would mean a setting that looks configured and is not, which is the failure this whole
/// module exists to prevent.
pub mod env {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{CheckConfig, ConfigError, FailOn};
    use crate::check::{Category, Severity};

    /// The `veridex.toml` key each variable sets, paired with the variable name.
    ///
    /// `VERIDEX_CONFIG` and `VERIDEX_PROFILE` are not in this table: they select *which* config file
    /// and *which* profile, rather than setting a value inside one, and the CLI reads them directly.
    pub const VARIABLES: &[(&str, &str)] = &[
        ("VERIDEX_FAIL_ON", "fail_on"),
        ("VERIDEX_MIN_SCORE", "min_score"),
        ("VERIDEX_CATEGORIES", "categories"),
        ("VERIDEX_ONLY_CHECKS", "only_checks"),
        ("VERIDEX_DISABLED_CHECKS", "disabled_checks"),
        ("VERIDEX_SEVERITY_OVERRIDES", "severity_overrides"),
    ];

    /// The tolerance keys, each settable as `VERIDEX_TOLERANCE_<KEY>` (upper-cased).
    pub const TOLERANCE_KEYS: &[&str] = &[
        "clock_skew_ms",
        "start_offset_ms",
        "end_offset_ms",
        "rate_deviation",
        "gap_factor",
        "jitter_cv",
        "episode_duration_factor",
        "saturation_fraction",
        "saturation_min_samples",
        "outlier_z",
        "sequence_drop_fraction",
        "ego_max_speed_mps",
        "near_duplicate_fraction",
        "sensor_clock_offset_ms",
    ];

    /// Merge the environment onto `base`, returning the merged config and the `veridex.toml` keys
    /// the environment set (so a reader can be told which layer a value came from).
    ///
    /// Takes the variables as an iterator rather than reading the process environment, so a caller
    /// — and a test — decides what the environment *is*. The merged result is validated exactly as
    /// a parsed file is: a value that arrives through the environment meets the same bar.
    pub fn merge<I, K, V>(
        base: CheckConfig,
        vars: I,
    ) -> Result<(CheckConfig, BTreeSet<String>), ConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut config = base;
        let mut set: BTreeSet<String> = BTreeSet::new();
        // Sorted, so two environments with the same variables always produce the same errors.
        let vars: BTreeMap<String, String> = vars
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
            .collect();

        for (name, value) in &vars {
            let value = value.trim();
            // A tolerance variable naming no known key is a typo, and a typo here is silent: the
            // threshold the operator meant to move simply does not move. Refuse it by name.
            if let Some(suffix) = name.strip_prefix("VERIDEX_TOLERANCE_") {
                let key = suffix.to_ascii_lowercase();
                if !TOLERANCE_KEYS.contains(&key.as_str()) {
                    return Err(ConfigError::Parse(format!(
                        "unknown environment variable `{name}` (known tolerances: {})",
                        TOLERANCE_KEYS.join(", ")
                    )));
                }
                set_tolerance(&mut config, &key, name, value)?;
                set.insert(format!("tolerances.{key}"));
                continue;
            }
            let Some((_, key)) = VARIABLES.iter().find(|(var, _)| var == name) else {
                // Any other `VERIDEX_*` name belongs to something else (the test harness's
                // `VERIDEX_BIN`, a user's own tooling) and is left alone.
                continue;
            };
            if value.is_empty() {
                return Err(ConfigError::Parse(format!(
                    "{name} is empty; unset it to leave `{key}` alone rather than setting it to nothing"
                )));
            }
            match *key {
                "fail_on" => {
                    config.fail_on = match value {
                        "error" => FailOn::Error,
                        "warning" => FailOn::Warning,
                        v => {
                            return Err(ConfigError::Parse(format!(
                                "{name}: invalid fail_on `{v}` (expected `error` or `warning`)"
                            )))
                        }
                    }
                }
                "min_score" => {
                    config.min_score = Some(value.parse::<u8>().map_err(|_| {
                        ConfigError::Parse(format!(
                            "{name}: invalid min_score `{value}` (expected an integer 0-100)"
                        ))
                    })?)
                }
                "categories" => {
                    let mut categories = Vec::new();
                    for item in list(value) {
                        categories.push(parse_category(&item).ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{name}: unknown category `{item}` (known: structural, temporal, \
                                 statistical, semantic, video, provenance, autonomy)"
                            ))
                        })?);
                    }
                    config.categories = Some(categories);
                }
                "only_checks" => config.only_checks = Some(list(value)),
                "disabled_checks" => config.disabled_checks = list(value),
                "severity_overrides" => {
                    let mut overrides = BTreeMap::new();
                    for item in list(value) {
                        let (id, severity) = item.split_once('=').ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{name}: `{item}` is not a `check-id=severity` pair"
                            ))
                        })?;
                        let severity = parse_severity(severity.trim()).ok_or_else(|| {
                            ConfigError::Parse(format!(
                                "{name}: unknown severity `{severity}` (expected info, warning, or error)"
                            ))
                        })?;
                        overrides.insert(id.trim().to_string(), severity);
                    }
                    config.severity_overrides = overrides;
                }
                other => unreachable!("unmapped environment key {other}"),
            }
            set.insert(key.to_string());
        }

        // The same validation a file gets: an out-of-range tolerance is an error wherever it came
        // from. Check ids are validated by the caller, which is the one holding the check registry.
        config.validate()?;
        Ok((config, set))
    }

    /// Split a comma-separated list, trimming each entry and dropping empty ones.
    fn list(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn parse_category(s: &str) -> Option<Category> {
        [
            Category::Structural,
            Category::Temporal,
            Category::Statistical,
            Category::Semantic,
            Category::Video,
            Category::Provenance,
            Category::Autonomy,
        ]
        .into_iter()
        .find(|c| c.tag() == s)
    }

    fn parse_severity(s: &str) -> Option<Severity> {
        [Severity::Info, Severity::Warning, Severity::Error]
            .into_iter()
            .find(|sev| sev.tag() == s)
    }

    /// Apply one `VERIDEX_TOLERANCE_<KEY>` value.
    fn set_tolerance(
        config: &mut CheckConfig,
        key: &str,
        name: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        let number = || -> Result<f64, ConfigError> {
            value.parse::<f64>().map_err(|_| {
                ConfigError::Parse(format!(
                    "{name}: invalid {key} `{value}` (expected a number)"
                ))
            })
        };
        let t = &mut config.tolerances;
        match key {
            "clock_skew_ms" => t.clock_skew_ms = Some(number()?),
            "start_offset_ms" => t.start_offset_ms = Some(number()?),
            "end_offset_ms" => t.end_offset_ms = Some(number()?),
            "rate_deviation" => t.rate_deviation = Some(number()?),
            "gap_factor" => t.gap_factor = Some(number()?),
            "jitter_cv" => t.jitter_cv = Some(number()?),
            "episode_duration_factor" => t.episode_duration_factor = Some(number()?),
            "saturation_fraction" => t.saturation_fraction = Some(number()?),
            "saturation_min_samples" => {
                t.saturation_min_samples = Some(value.parse::<u64>().map_err(|_| {
                    ConfigError::Parse(format!(
                        "{name}: invalid saturation_min_samples `{value}` (expected a whole number)"
                    ))
                })?)
            }
            "outlier_z" => t.outlier_z = Some(number()?),
            "sequence_drop_fraction" => t.sequence_drop_fraction = Some(number()?),
            "ego_max_speed_mps" => t.ego_max_speed_mps = Some(number()?),
            "near_duplicate_fraction" => t.near_duplicate_fraction = Some(number()?),
            "sensor_clock_offset_ms" => t.sensor_clock_offset_ms = Some(number()?),
            other => unreachable!("unmapped tolerance key {other}"),
        }
        Ok(())
    }
}
