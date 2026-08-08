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
