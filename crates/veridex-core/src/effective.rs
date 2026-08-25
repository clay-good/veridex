//! The effective configuration a run will use, with **where each value came from**.
//!
//! A verdict already records the configuration it ran under, but only after a run — and only the
//! resolved numbers, not their provenance. That is the wrong shape for the question people actually
//! ask: *why is this threshold 20 ms when my `veridex.toml` says 50?* The answer is a chain
//! (built-in default → config file → policy profile → command-line flag), and every step of it is
//! invisible in a bare number.
//!
//! So each setting is reported as a value **and** an [`Origin`]: the layer that last set it, plus a
//! note where one layer overrode another. The same rendering serves the CLI's `--print-config` and
//! the Python binding, so the two cannot disagree about what a config means.

use serde::Serialize;

use crate::config::{CheckConfig, FailOn};
use crate::engine::Tolerances;
use crate::profile::Profile;

/// The layer a setting's value came from. Later layers override earlier ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// Veridex's built-in default: nothing set this.
    Default,
    /// A `veridex.toml`.
    ConfigFile,
    /// A named policy profile (`--profile`).
    Profile,
    /// A command-line flag.
    Flag,
}

impl Origin {
    /// The label used in both renderings.
    pub fn label(self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::ConfigFile => "config file",
            Origin::Profile => "profile",
            Origin::Flag => "flag",
        }
    }
}

/// One resolved setting: its config key, its value, where the value came from, and — when a layer
/// overrode another — what it overrode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Setting {
    /// The key as it is written in `veridex.toml` (so a printed value can be pasted back).
    pub key: String,
    /// The resolved value, rendered the way the config file writes it.
    pub value: String,
    /// The layer that set it.
    pub origin: Origin,
    /// What this layer overrode, when it overrode something.
    pub note: Option<String>,
}

/// Everything needed to explain a run's configuration, as the front end assembled it.
pub struct Inputs<'a> {
    /// The config file that was read, if any (path as given, for the reader's benefit).
    pub config_path: Option<String>,
    /// The config file as parsed. `CheckConfig::default()` when there was none.
    pub file: &'a CheckConfig,
    /// The policy profile applied, if any.
    pub profile: Option<&'a Profile>,
    /// The tolerances the run will actually use, after every layer.
    pub tolerances: Tolerances,
    /// The failure threshold the run will use, and whether a flag set it.
    pub fail_on: FailOn,
    /// Whether `--fail-on` set it (as opposed to the file or the default).
    pub fail_on_from_flag: bool,
    /// The score gate the run will use, if any.
    pub min_score: Option<u8>,
    /// Whether `--min-score` set it.
    pub min_score_from_flag: bool,
}

/// Every setting of the run, in a stable order.
pub fn settings(inputs: &Inputs<'_>) -> Vec<Setting> {
    let mut out = Vec::new();
    let file = inputs.file;
    // The tolerances the file alone implies, so a value that differs from them can only have come
    // from a later layer.
    let from_file = file.to_run_config().tolerances;

    /// A selection setting: set by the file, or left at its default.
    fn selection(out: &mut Vec<Setting>, key: &str, value: String, set_in_file: bool) {
        out.push(Setting {
            key: key.to_string(),
            value,
            origin: if set_in_file {
                Origin::ConfigFile
            } else {
                Origin::Default
            },
            note: None,
        });
    }

    selection(
        &mut out,
        "categories",
        match &file.categories {
            None => "all".to_string(),
            Some(c) => c
                .iter()
                .map(|c| format!("{c:?}").to_lowercase())
                .collect::<Vec<_>>()
                .join(", "),
        },
        file.categories.is_some(),
    );
    selection(
        &mut out,
        "only_checks",
        match &file.only_checks {
            None => "all".to_string(),
            Some(c) => c.join(", "),
        },
        file.only_checks.is_some(),
    );
    selection(
        &mut out,
        "disabled_checks",
        if file.disabled_checks.is_empty() {
            "none".to_string()
        } else {
            file.disabled_checks.join(", ")
        },
        !file.disabled_checks.is_empty(),
    );
    selection(
        &mut out,
        "severity_overrides",
        if file.severity_overrides.is_empty() {
            "none".to_string()
        } else {
            file.severity_overrides
                .iter()
                .map(|(id, sev)| format!("{id}={}", format!("{sev:?}").to_lowercase()))
                .collect::<Vec<_>>()
                .join(", ")
        },
        !file.severity_overrides.is_empty(),
    );

    // The exit threshold and the score gate: a flag beats the file, which beats the default.
    out.push(Setting {
        key: "fail_on".to_string(),
        value: match inputs.fail_on {
            FailOn::Error => "error".to_string(),
            FailOn::Warning => "warning".to_string(),
        },
        origin: if inputs.fail_on_from_flag {
            Origin::Flag
        } else if file.fail_on != FailOn::default() {
            Origin::ConfigFile
        } else {
            Origin::Default
        },
        note: (inputs.fail_on_from_flag && file.fail_on != inputs.fail_on)
            .then(|| format!("--fail-on overrides the config file's `{:?}`", file.fail_on)),
    });
    out.push(Setting {
        key: "min_score".to_string(),
        value: match inputs.min_score {
            None => "none".to_string(),
            Some(n) => n.to_string(),
        },
        origin: if inputs.min_score_from_flag {
            Origin::Flag
        } else if file.min_score.is_some() {
            Origin::ConfigFile
        } else {
            Origin::Default
        },
        note: match (inputs.min_score_from_flag, file.min_score) {
            (true, Some(configured)) if Some(configured) != inputs.min_score => Some(format!(
                "--min-score overrides the config file's {configured}"
            )),
            _ => None,
        },
    });

    // Tolerances. A value that differs from what the file (or the default) implies can only have
    // come from the profile, which is the layer applied last and may only tighten.
    let profile_name = inputs.profile.map(|p| p.name);
    let f = &inputs.tolerances;
    let c = &from_file;
    let t = &file.tolerances;
    let ms = |ns: i64| trim(ns as f64 / 1_000_000.0);
    // (toml key, set in file, unchanged since the file, final value, the file's value)
    let tolerances: Vec<(&str, bool, bool, String, String)> = vec![
        (
            "clock_skew_ms",
            t.clock_skew_ms.is_some(),
            f.clock_skew_ns == c.clock_skew_ns,
            ms(f.clock_skew_ns),
            ms(c.clock_skew_ns),
        ),
        (
            "start_offset_ms",
            t.start_offset_ms.is_some(),
            f.start_offset_ns == c.start_offset_ns,
            ms(f.start_offset_ns),
            ms(c.start_offset_ns),
        ),
        (
            "end_offset_ms",
            t.end_offset_ms.is_some(),
            f.end_offset_ns == c.end_offset_ns,
            ms(f.end_offset_ns),
            ms(c.end_offset_ns),
        ),
        (
            "rate_deviation",
            t.rate_deviation.is_some(),
            f.rate_deviation == c.rate_deviation,
            trim(f.rate_deviation),
            trim(c.rate_deviation),
        ),
        (
            "gap_factor",
            t.gap_factor.is_some(),
            f.gap_factor == c.gap_factor,
            trim(f.gap_factor),
            trim(c.gap_factor),
        ),
        (
            "jitter_cv",
            t.jitter_cv.is_some(),
            f.jitter_cv == c.jitter_cv,
            trim(f.jitter_cv),
            trim(c.jitter_cv),
        ),
        (
            "episode_duration_factor",
            t.episode_duration_factor.is_some(),
            f.episode_duration_factor == c.episode_duration_factor,
            trim(f.episode_duration_factor),
            trim(c.episode_duration_factor),
        ),
        (
            "saturation_fraction",
            t.saturation_fraction.is_some(),
            f.saturation_fraction == c.saturation_fraction,
            trim(f.saturation_fraction),
            trim(c.saturation_fraction),
        ),
        (
            "saturation_min_samples",
            t.saturation_min_samples.is_some(),
            f.saturation_min_samples == c.saturation_min_samples,
            f.saturation_min_samples.to_string(),
            c.saturation_min_samples.to_string(),
        ),
        (
            "outlier_z",
            t.outlier_z.is_some(),
            f.outlier_z == c.outlier_z,
            trim(f.outlier_z),
            trim(c.outlier_z),
        ),
        (
            "sequence_drop_fraction",
            t.sequence_drop_fraction.is_some(),
            f.sequence_drop_fraction == c.sequence_drop_fraction,
            trim(f.sequence_drop_fraction),
            trim(c.sequence_drop_fraction),
        ),
        (
            "ego_max_speed_mps",
            t.ego_max_speed_mps.is_some(),
            f.ego_max_speed_mps == c.ego_max_speed_mps,
            trim(f.ego_max_speed_mps),
            trim(c.ego_max_speed_mps),
        ),
    ];
    for (key, set_in_file, unchanged, value, file_value) in tolerances {
        let (origin, note) = if unchanged {
            let origin = if set_in_file {
                Origin::ConfigFile
            } else {
                Origin::Default
            };
            (origin, None)
        } else {
            // Only a profile moves a tolerance after the file, and only downward.
            let note = match profile_name {
                Some(name) => format!("profile `{name}` tightened it from {file_value}"),
                // Unreachable through the front ends, which move tolerances only by profile; said
                // plainly rather than mislabeled if a library caller does otherwise.
                None => format!("overridden from {file_value}"),
            };
            (Origin::Profile, Some(note))
        };
        out.push(Setting {
            key: format!("tolerances.{key}"),
            value,
            origin,
            note,
        });
    }

    out
}

/// Trim a float to a short, stable decimal form (`20`, `0.05`), matching the reports' number style.
fn trim(v: f64) -> String {
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// The schema tag on the machine-readable rendering.
pub const EFFECTIVE_CONFIG_SCHEMA_VERSION: &str = "veridex.config/1";

/// Render the effective configuration for a terminal.
pub fn render_effective_config(inputs: &Inputs<'_>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "Effective configuration");
    let _ = writeln!(
        out,
        "  Config file: {}",
        inputs
            .config_path
            .as_deref()
            .unwrap_or("(none — built-in defaults)")
    );
    if let Some(p) = inputs.profile {
        let _ = writeln!(out, "  Profile:     {}", p.name);
    }
    let _ = writeln!(out);
    let settings = settings(inputs);
    let key_width = settings.iter().map(|s| s.key.len()).max().unwrap_or(0);
    let value_width = settings.iter().map(|s| s.value.len()).max().unwrap_or(0);
    for s in &settings {
        let _ = write!(
            out,
            "  {:key_width$}  {:value_width$}  ({})",
            s.key,
            s.value,
            s.origin.label(),
        );
        if let Some(note) = &s.note {
            let _ = write!(out, " — {note}");
        }
        let _ = writeln!(out);
    }
    out
}

/// Render the effective configuration as JSON (`veridex.config/1`).
pub fn render_effective_config_json(inputs: &Inputs<'_>) -> String {
    let doc = serde_json::json!({
        "schema": EFFECTIVE_CONFIG_SCHEMA_VERSION,
        "veridex_version": crate::VERSION,
        "config_file": inputs.config_path,
        "profile": inputs.profile.map(|p| p.name),
        "settings": settings(inputs),
    });
    serde_json::to_string_pretty(&doc).expect("effective config serializes")
}
