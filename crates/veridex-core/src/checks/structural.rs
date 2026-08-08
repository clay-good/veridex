//! Structural checks: episode/stream integrity.

use std::collections::HashMap;

use crate::cdm::Dataset;
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Episode-boundary integrity, covering the corrupted-cumulative-length class from
/// [lerobot#4143](https://github.com/huggingface/lerobot/issues/4143): when episode-length metadata
/// yields wrong cumulative boundaries, frames are silently misattributed to the wrong episode. In
/// the CDM that corruption surfaces as **duplicate episode indices** (two episodes claim the same
/// slot) or an **inverted boundary** (`start_ts > end_ts`).
pub struct EpisodeBoundary;

impl Check for EpisodeBoundary {
    fn id(&self) -> &'static str {
        "structural.episode-boundary"
    }
    fn title(&self) -> &'static str {
        "Episode boundary integrity"
    }
    fn category(&self) -> Category {
        Category::Structural
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Duplicate episode indices: the direct CDM signature of corrupted cumulative boundaries.
        let mut seen: HashMap<u64, u32> = HashMap::new();
        for ep in &dataset.episodes {
            *seen.entry(ep.index).or_insert(0) += 1;
        }
        for (index, count) in &seen {
            if *count > 1 {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Structural,
                        Severity::Error,
                        Location::Episode { episode: *index },
                        "STRUCTURAL.EPISODE_BOUNDARY",
                        format!(
                            "episode index {index} appears {count} times; cumulative episode \
                             boundaries are inconsistent"
                        ),
                    )
                    .with_risk(
                        "Frames are misattributed across episode boundaries, poisoning trajectory \
                         segmentation and any per-episode statistics.",
                    )
                    .with_remedy(
                        "Recompute the episode boundary/cumulative-length metadata from the source \
                         shards and re-export.",
                    ),
                );
            }
        }

        // Inverted boundaries.
        for ep in &dataset.episodes {
            if let (Some(start), Some(end)) = (ep.start_ts, ep.end_ts) {
                if start > end {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Structural,
                            Severity::Error,
                            Location::Episode { episode: ep.index },
                            "STRUCTURAL.EPISODE_BOUNDARY",
                            format!(
                                "episode {} has inverted bounds: start_ts {start} > end_ts {end}",
                                ep.index
                            ),
                        )
                        .with_risk("An inverted time window indicates corrupted episode metadata.")
                        .with_remedy("Re-derive episode start/end from the underlying frames."),
                    );
                }
            }
        }

        findings
    }
}

/// Cross-episode dtype/shape consistency. A stream name that keeps a different declared element
/// dtype or per-frame shape in different episodes cannot be stacked into a single training batch:
/// the loader will either error or silently truncate/pad. This surfaces that drift, which arises
/// when episodes recorded under different sensor configs (or merged from different sources) are
/// pooled under one dataset. Streams that declare no dtype/shape are skipped — Veridex never infers.
pub struct ShapeConsistency;

/// The first declared schema seen for a stream name, kept to compare later episodes against.
struct Baseline<'a> {
    dtype: &'a Option<String>,
    shape: &'a Option<Vec<u64>>,
    episode: u64,
}

impl Check for ShapeConsistency {
    fn id(&self) -> &'static str {
        "structural.shape-consistency"
    }
    fn title(&self) -> &'static str {
        "Cross-episode dtype/shape consistency"
    }
    fn category(&self) -> Category {
        Category::Structural
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // First declared schema seen for each stream name, and the episode it came from.
        let mut baseline: HashMap<&str, Baseline<'_>> = HashMap::new();
        // Names already reported, so a stream that drifts across many episodes yields one finding.
        let mut reported: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut findings = Vec::new();

        for ep in &dataset.episodes {
            for stream in &ep.streams {
                // Nothing declared → nothing to compare against.
                if stream.dtype.is_none() && stream.shape.is_none() {
                    continue;
                }
                match baseline.get(stream.name.as_str()) {
                    None => {
                        baseline.insert(
                            &stream.name,
                            Baseline {
                                dtype: &stream.dtype,
                                shape: &stream.shape,
                                episode: ep.index,
                            },
                        );
                    }
                    Some(base) => {
                        let dtype_differs = stream.dtype.is_some()
                            && base.dtype.is_some()
                            && &stream.dtype != base.dtype;
                        let shape_differs = stream.shape.is_some()
                            && base.shape.is_some()
                            && &stream.shape != base.shape;
                        if (dtype_differs || shape_differs) && reported.insert(stream.name.as_str())
                        {
                            findings.push(
                                Finding::new(
                                    self.id(),
                                    Category::Structural,
                                    Severity::Error,
                                    Location::Stream {
                                        episode: ep.index,
                                        stream: stream.name.clone(),
                                    },
                                    "STRUCTURAL.SHAPE_MISMATCH",
                                    format!(
                                        "stream `{}` declares {} in episode {} but {} in episode {}",
                                        stream.name,
                                        describe(base.dtype, base.shape),
                                        base.episode,
                                        describe(&stream.dtype, &stream.shape),
                                        ep.index,
                                    ),
                                )
                                .with_risk(
                                    "Inconsistent tensor dtype/shape across episodes breaks batched \
                                     collation: the data loader errors or silently pads/truncates, \
                                     corrupting inputs.",
                                )
                                .with_remedy(
                                    "Re-export the affected episodes with a consistent feature \
                                     schema, or drop the episodes that diverge.",
                                ),
                            );
                        }
                    }
                }
            }
        }
        findings
    }
}

/// Render a `dtype`/`shape` pair for a finding message, e.g. `float32 shape [6]` or `shape [3,480]`.
fn describe(dtype: &Option<String>, shape: &Option<Vec<u64>>) -> String {
    let mut parts = Vec::new();
    if let Some(d) = dtype {
        parts.push(d.clone());
    }
    if let Some(s) = shape {
        let dims: Vec<String> = s.iter().map(|d| d.to_string()).collect();
        parts.push(format!("shape [{}]", dims.join(",")));
    }
    if parts.is_empty() {
        "an undeclared schema".into()
    } else {
        parts.join(" ")
    }
}

/// Degenerate episodes and streams: an episode with no streams, a stream with no frames (error), or
/// a stream with a single frame (warning) carries no usable trajectory.
pub struct DegenerateEpisode;

impl Check for DegenerateEpisode {
    fn id(&self) -> &'static str {
        "structural.degenerate-episode"
    }
    fn title(&self) -> &'static str {
        "Degenerate episodes and streams"
    }
    fn category(&self) -> Category {
        Category::Structural
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Episode
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            if ep.streams.is_empty() {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Structural,
                        Severity::Error,
                        Location::Episode { episode: ep.index },
                        "STRUCTURAL.EMPTY_EPISODE",
                        format!("episode {} has no streams", ep.index),
                    )
                    .with_risk("An episode with no data contributes nothing and skews counts.")
                    .with_remedy("Drop the empty episode or fix the export that produced it."),
                );
                continue;
            }
            for stream in &ep.streams {
                match stream.frames.len() {
                    0 => findings.push(
                        Finding::new(
                            self.id(),
                            Category::Structural,
                            Severity::Error,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "STRUCTURAL.EMPTY_STREAM",
                            format!(
                                "stream `{}` in episode {} has no frames",
                                stream.name, ep.index
                            ),
                        )
                        .with_risk("A stream with no frames breaks cross-stream alignment.")
                        .with_remedy("Remove the stream or repair the missing shard."),
                    ),
                    1 => findings.push(
                        Finding::new(
                            self.id(),
                            Category::Structural,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "STRUCTURAL.SINGLE_FRAME_STREAM",
                            format!(
                                "stream `{}` in episode {} has a single frame",
                                stream.name, ep.index
                            ),
                        )
                        .with_risk("A single-frame stream carries no temporal signal.")
                        .with_remedy("Confirm the recording captured the full trajectory."),
                    ),
                    _ => {}
                }
            }
        }
        findings
    }
}
