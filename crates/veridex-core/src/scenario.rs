//! Scenario-dimension coverage: a **descriptive** report of the conditions a dataset was recorded
//! under (weather, time of day, environment, …), read from episode labels (design A3/A6).
//!
//! Coverage here is descriptive, never prescriptive: Veridex surfaces the distribution of each
//! scenario dimension across episodes and flags sparse cells, but it never declares a required
//! balance and never blocks — the right target distribution is the training team's call, so this
//! produces no findings and does not touch the trust score.

use std::collections::BTreeMap;

use crate::cdm::Dataset;

/// The canonical scenario dimensions Veridex recognizes, in a stable display order.
pub const SCENARIO_DIMENSIONS: &[&str] = &[
    "weather",
    "time_of_day",
    "environment",
    "lighting",
    "season",
    "traffic",
];

/// Map a source metadata/label key (case-insensitive) to a canonical scenario dimension, or `None`
/// if it names no recognized dimension. Conservative: only well-known spellings map.
pub fn scenario_dim_for(key: &str) -> Option<&'static str> {
    match key.trim().to_ascii_lowercase().as_str() {
        "weather" | "weather_condition" => Some("weather"),
        "time_of_day" | "tod" | "daytime" | "time-of-day" => Some("time_of_day"),
        "environment" | "scene" | "scene_type" | "area" | "road_type" => Some("environment"),
        "lighting" | "illumination" => Some("lighting"),
        "season" => Some("season"),
        "traffic" | "traffic_density" => Some("traffic"),
        _ => None,
    }
}

/// One scenario dimension's distribution over episodes: each value and how many episodes carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionCoverage {
    /// The canonical dimension name (e.g. `weather`).
    pub dimension: String,
    /// `(value, episode count)` pairs, most frequent first (ties broken by value for determinism).
    pub values: Vec<(String, u64)>,
    /// Episodes that carry a value for this dimension.
    pub episodes_covered: u64,
}

/// Compute the scenario-dimension distribution across a dataset's episodes, from labels whose key is
/// a recognized scenario dimension. Only dimensions that actually appear are returned, in
/// [`SCENARIO_DIMENSIONS`] order.
pub fn coverage(dataset: &Dataset) -> Vec<DimensionCoverage> {
    let mut out = Vec::new();
    for &dim in SCENARIO_DIMENSIONS {
        // value -> distinct-episode count.
        let mut counts: BTreeMap<&str, u64> = BTreeMap::new();
        let mut episodes_covered = 0u64;
        for ep in &dataset.episodes {
            // At most one contribution per episode per value (an episode is one sample of the mix).
            let mut seen_here: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
            for label in &ep.labels {
                if label.key == dim && !label.value.trim().is_empty() {
                    seen_here.insert(label.value.as_str());
                }
            }
            if !seen_here.is_empty() {
                episodes_covered += 1;
            }
            for v in seen_here {
                *counts.entry(v).or_insert(0) += 1;
            }
        }
        if counts.is_empty() {
            continue;
        }
        // Most frequent first; ties by value for determinism.
        let mut values: Vec<(String, u64)> = counts
            .into_iter()
            .map(|(v, n)| (v.to_string(), n))
            .collect();
        values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out.push(DimensionCoverage {
            dimension: dim.to_string(),
            values,
            episodes_covered,
        });
    }
    out
}

/// Render the scenario coverage as a human-readable block (empty string when no scenario dimensions
/// are present). Descriptive only: for a multi-episode dataset it shows each value's episode count and
/// marks a **sparse** cell (a value in under 10% of the dimension's covered episodes, and more than
/// one value present), never as a pass/fail.
pub fn render_coverage(dataset: &Dataset) -> String {
    use std::fmt::Write;
    let dims = coverage(dataset);
    if dims.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "  scenario coverage (descriptive):");
    for d in &dims {
        let rendered: Vec<String> = d
            .values
            .iter()
            .map(|(v, n)| {
                // Flag a sparse cell only when there's a distribution to be sparse within.
                let sparse = d.values.len() > 1 && *n * 10 < d.episodes_covered;
                if sparse {
                    format!("{v} ({n}, sparse)")
                } else {
                    format!("{v} ({n})")
                }
            })
            .collect();
        let _ = writeln!(out, "      {}: {}", d.dimension, rendered.join(", "));
    }
    out
}
