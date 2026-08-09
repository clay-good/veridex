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
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.EPISODE_BOUNDARY"]
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

/// Declared-vs-actual episode count. When a source manifest declares how many episodes the dataset
/// contains (e.g. LeRobot `meta/info.json` `total_episodes`), the number ingested must match. A
/// shortfall is the signature of a truncated or partially-downloaded export — training silently on
/// less data than the manifest promises. Datasets that declare no count are skipped.
pub struct DeclaredEpisodeCount;

impl Check for DeclaredEpisodeCount {
    fn id(&self) -> &'static str {
        "structural.declared-episode-count"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.EPISODE_COUNT_MISMATCH"]
    }
    fn title(&self) -> &'static str {
        "Declared episode count matches the data"
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
        let Some(declared) = dataset
            .metadata
            .iter()
            .find(|(k, _)| k == crate::cdm::META_DECLARED_EPISODES)
            .and_then(|(_, v)| v.parse::<u64>().ok())
        else {
            return Vec::new();
        };
        let actual = dataset.episodes.len() as u64;
        if declared == actual {
            return Vec::new();
        }
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Error,
            Location::Dataset,
            "STRUCTURAL.EPISODE_COUNT_MISMATCH",
            format!("manifest declares {declared} episodes but {actual} were ingested"),
        )
        .with_risk(
            "A mismatch between the manifest and the data means the export is truncated or corrupt; \
             training would use fewer (or more) episodes than intended.",
        )
        .with_remedy(
            "Re-download or re-export the dataset, or fix the manifest's total_episodes to match.",
        )]
    }
}

/// Declared-vs-actual frame count. When a source manifest declares a total frame count (e.g.
/// LeRobot `meta/info.json` `total_frames`), the frames ingested must match. This catches truncation
/// that leaves every episode present but some episodes short — which the episode-count check misses.
/// The actual count per episode is its longest stream (in a frame-aligned source, every stream in an
/// episode has the same length). Datasets that declare no frame count are skipped.
pub struct DeclaredFrameCount;

impl Check for DeclaredFrameCount {
    fn id(&self) -> &'static str {
        "structural.declared-frame-count"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.FRAME_COUNT_MISMATCH"]
    }
    fn title(&self) -> &'static str {
        "Declared frame count matches the data"
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
        let Some(declared) = dataset
            .metadata
            .iter()
            .find(|(k, _)| k == crate::cdm::META_DECLARED_FRAMES)
            .and_then(|(_, v)| v.parse::<u64>().ok())
        else {
            return Vec::new();
        };
        // Per episode, the longest stream is the episode length (streams are frame-aligned).
        let actual: u64 = dataset
            .episodes
            .iter()
            .map(|ep| {
                ep.streams
                    .iter()
                    .map(|s| s.frames.len() as u64)
                    .max()
                    .unwrap_or(0)
            })
            .sum();
        if declared == actual {
            return Vec::new();
        }
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Error,
            Location::Dataset,
            "STRUCTURAL.FRAME_COUNT_MISMATCH",
            format!("manifest declares {declared} frames but {actual} were ingested"),
        )
        .with_risk(
            "A frame-count mismatch means the export is truncated or corrupt; episodes may be cut \
             short, breaking trajectories even when every episode is present.",
        )
        .with_remedy(
            "Re-download or re-export the dataset, or fix the manifest's total_frames to match.",
        )]
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
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.SHAPE_MISMATCH"]
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
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "STRUCTURAL.EMPTY_DATASET",
            "STRUCTURAL.EMPTY_EPISODE",
            "STRUCTURAL.EMPTY_STREAM",
            "STRUCTURAL.SINGLE_FRAME_STREAM",
        ]
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
        // A dataset with no episodes at all is degenerate: nothing to train on. Without this guard
        // the per-episode loop below is empty and the dataset would silently pass every check.
        if dataset.episodes.is_empty() {
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Structural,
                    Severity::Error,
                    Location::Dataset,
                    "STRUCTURAL.EMPTY_DATASET",
                    "dataset has no episodes".to_string(),
                )
                .with_risk("A dataset with no episodes contains no data to train on or verify.")
                .with_remedy(
                    "Check the source path and the ingest: the export may be empty or unreadable.",
                ),
            );
            return findings;
        }
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

/// Episode-index continuity. Frame-aligned datasets number episodes contiguously; a gap in the
/// indices (e.g. `0, 1, 3` — episode 2 absent) means an episode was silently dropped between export
/// and here. Unlike the declared-count check this needs no manifest, and unlike the boundary check it
/// catches *absent* episodes rather than duplicated ones. A warning, since a few datasets legitimately
/// use non-contiguous ids.
pub struct EpisodeContinuity;

impl Check for EpisodeContinuity {
    fn id(&self) -> &'static str {
        "structural.episode-continuity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.EPISODE_INDEX_GAP"]
    }
    fn title(&self) -> &'static str {
        "Episode-index continuity"
    }
    fn category(&self) -> Category {
        Category::Structural
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // Distinct, sorted indices. (Duplicates are the episode-boundary check's concern.)
        let mut indices: Vec<u64> = dataset.episodes.iter().map(|ep| ep.index).collect();
        indices.sort_unstable();
        indices.dedup();
        if indices.len() < 2 {
            return Vec::new();
        }

        // Any index missing between the smallest and largest observed is a dropped episode.
        let (lo, hi) = (indices[0], indices[indices.len() - 1]);
        let present: std::collections::HashSet<u64> = indices.iter().copied().collect();
        let missing: Vec<u64> = (lo..=hi).filter(|i| !present.contains(i)).collect();
        if missing.is_empty() {
            return Vec::new();
        }

        // Summarize the gap compactly; list the first few missing indices.
        let shown: Vec<String> = missing.iter().take(8).map(|i| i.to_string()).collect();
        let more = if missing.len() > shown.len() {
            format!(", … ({} more)", missing.len() - shown.len())
        } else {
            String::new()
        };
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Warning,
            Location::Dataset,
            "STRUCTURAL.EPISODE_INDEX_GAP",
            format!(
                "episode indices span {lo}..={hi} but {} are missing: {}{more}",
                missing.len(),
                shown.join(", ")
            ),
        )
        .with_risk(
            "A gap in episode indices means an episode was dropped between export and ingest; you \
             train on less data than the numbering implies, and per-episode joins can misalign.",
        )
        .with_remedy(
            "Recover the missing episode(s), or re-export so the surviving episodes are renumbered \
             contiguously.",
        )]
    }
}
