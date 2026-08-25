//! Structural checks: episode/stream integrity.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::cdm::{Dataset, Modality};
use crate::check::{Category, Check, CheckContext, Finding, Location, Scope, Severity};

/// Episode-boundary integrity, covering the corrupted-cumulative-length class from
/// [lerobot#4143](https://github.com/huggingface/lerobot/issues/4143): when episode-length metadata
/// yields wrong cumulative boundaries, frames are silently misattributed to the wrong episode. In
/// the CDM that corruption surfaces as a **declared-vs-actual length mismatch** (the manifest's
/// per-episode `length` disagrees with the frames ingested), **duplicate episode indices** (two
/// episodes claim the same slot), or an **inverted boundary** (`start_ts > end_ts`).
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
        self.run_with(dataset, true)
    }
    fn run_in(&self, dataset: &Dataset, context: &CheckContext) -> Vec<Finding> {
        self.run_with(dataset, context.frames_read)
    }
}

impl EpisodeBoundary {
    /// `frames_read` is false under a metadata-only ingest, where no episode has frames by request.
    /// The duplicate-index and inverted-boundary arms still apply — both read the manifest — but
    /// declared-vs-actual length cannot: "declares 120, ingested 0" would be true of every sound
    /// dataset checked that way, turning the lerobot#4143 detector into a detector of the request.
    fn run_with(&self, dataset: &Dataset, frames_read: bool) -> Vec<Finding> {
        let mut findings = self.run_manifest_arms(dataset);
        if frames_read {
            findings.extend(self.run_declared_length_arm(dataset));
        }
        findings
    }

    /// The arms that read only the manifest: duplicate episode indices and inverted bounds. Both
    /// apply whether or not any frame was read.
    fn run_manifest_arms(&self, dataset: &Dataset) -> Vec<Finding> {
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

    /// Declared-vs-actual per-episode length: the direct lerobot#4143 signature. When the source
    /// manifest records a frame count for an episode (LeRobot `meta/episodes.jsonl` `length`) that
    /// disagrees with the frames actually ingested, the cumulative boundaries LeRobot derives from
    /// those lengths are wrong, and frames load under the wrong episode during training. The actual
    /// count is the largest per-stream frame count (streams are frame-aligned; the max is robust to
    /// a stream that abstains from frames).
    ///
    /// Only meaningful when frames were read — see [`EpisodeBoundary::run_with`].
    fn run_declared_length_arm(&self, dataset: &Dataset) -> Vec<Finding> {
        let mut findings = Vec::new();
        for ep in &dataset.episodes {
            if let Some(declared) = ep.declared_frame_count {
                let actual = ep
                    .streams
                    .iter()
                    .map(|s| s.frames.len() as u64)
                    .max()
                    .unwrap_or(0);
                if declared != actual {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Structural,
                            Severity::Error,
                            Location::Episode { episode: ep.index },
                            "STRUCTURAL.EPISODE_BOUNDARY",
                            format!(
                                "episode {} declares {declared} frames but {actual} were ingested; \
                                 cumulative episode boundaries are inconsistent",
                                ep.index
                            ),
                        )
                        .with_risk(
                            "Wrong per-episode lengths make cumulative boundaries misplace frames, so \
                             frames load under the wrong episode during training — corrupting \
                             trajectory segmentation and per-episode statistics.",
                        )
                        .with_remedy(
                            "Recompute the per-episode length metadata (meta/episodes.jsonl) from the \
                             source shards and re-export.",
                        ),
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
    /// Abstains entirely under a metadata-only ingest: the declared total is a claim about frames,
    /// and no frame was read to compare it against.
    fn run_in(&self, dataset: &Dataset, context: &CheckContext) -> Vec<Finding> {
        if !context.frames_read {
            return Vec::new();
        }
        self.run(dataset)
    }
}

/// Cross-episode dtype/shape consistency. A stream name that keeps a different declared element
/// dtype or per-frame shape in different episodes cannot be stacked into a single training batch:
/// the loader will either error or silently truncate/pad. This surfaces that drift, which arises
/// when episodes recorded under different sensor configs (or merged from different sources) are
/// pooled under one dataset. Streams that declare no dtype/shape are skipped — Veridex never infers.
pub struct ShapeConsistency;

/// The first declared dtype and the first declared shape seen for a stream name, each kept with the
/// episode it came from.
///
/// The two are tracked **independently**. A single baseline captured from the first episode that
/// declared *either* one, and was never enriched afterwards, so a stream whose first episode stated
/// a dtype but no shape had `shape: None` frozen in as its baseline — and the comparison requires
/// both sides declared, so shape drift for that stream could never be reported again, however many
/// later episodes declared conflicting shapes. HDF5 and Zarr both write `shape: None` for a 1-D
/// dataset, which makes this the ordinary case rather than a corner: an `/action` that is `(N,)` in
/// one episode file and `(N,7)` in another is exactly the un-collatable drift this check exists for.
#[derive(Default)]
struct Baseline<'a> {
    dtype: Option<(&'a String, u64)>,
    shape: Option<(&'a Vec<u64>, u64)>,
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
                let base = baseline.entry(&stream.name).or_default();

                // Compare against whichever axis already has a baseline, then fill in the ones that
                // do not — so a stream declaring only a dtype in one episode and only a shape in
                // another still acquires a baseline for both.
                let conflict = match (&stream.dtype, base.dtype) {
                    (Some(d), Some((b, at))) if d != b => Some((
                        at,
                        describe(&Some(b.clone()), &None),
                        describe(&Some(d.clone()), &None),
                    )),
                    _ => match (&stream.shape, base.shape) {
                        (Some(sh), Some((b, at))) if sh != b => Some((
                            at,
                            describe(&None, &Some(b.clone())),
                            describe(&None, &Some(sh.clone())),
                        )),
                        _ => None,
                    },
                };
                if base.dtype.is_none() {
                    if let Some(d) = &stream.dtype {
                        base.dtype = Some((d, ep.index));
                    }
                }
                if base.shape.is_none() {
                    if let Some(sh) = &stream.shape {
                        base.shape = Some((sh, ep.index));
                    }
                }

                let Some((base_episode, base_desc, here_desc)) = conflict else {
                    continue;
                };
                // One finding per stream name: a schema that drifts across many episodes is one
                // defect, and the first pair that shows it is the pair worth naming.
                if !reported.insert(stream.name.as_str()) {
                    continue;
                }
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
                            "stream `{}` declares {base_desc} in episode {base_episode} but \
                             {here_desc} in episode {}",
                            stream.name, ep.index,
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

/// Cross-episode stream presence. A stream key that appears in some episodes but is absent from
/// others yields a heterogeneous feature set: whether a sensor dropped out mid-collection or two
/// exports with different feature sets were pooled, the loader either errors on the missing feature
/// or silently fills it. Unlike [`ShapeConsistency`] (which compares the *schema* of streams that are
/// present), this catches streams that are *missing* entirely from some episodes. A warning, since a
/// few datasets legitimately record different streams per episode.
///
/// Episodes with no streams are excluded from the comparison — an empty episode is the
/// [`DegenerateEpisode`] check's concern, and counting it here would make every stream look
/// inconsistent.
pub struct StreamPresence;

impl Check for StreamPresence {
    fn id(&self) -> &'static str {
        "structural.stream-presence"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.STREAM_PRESENCE_INCONSISTENT"]
    }
    fn title(&self) -> &'static str {
        "Cross-episode stream presence"
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
        // Consider only episodes that actually carry streams (empty ones are DegenerateEpisode's
        // concern). The comparison needs at least two such episodes to mean anything.
        let episodes: Vec<&crate::cdm::Episode> = dataset
            .episodes
            .iter()
            .filter(|ep| !ep.streams.is_empty())
            .collect();
        if episodes.len() < 2 {
            return Vec::new();
        }
        let total = episodes.len();

        // For each stream name, the distinct episode indices that carry it. BTreeMap/BTreeSet keep
        // the output deterministic regardless of episode/stream ordering.
        let mut presence: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
        for ep in &episodes {
            for stream in &ep.streams {
                presence
                    .entry(stream.name.as_str())
                    .or_default()
                    .insert(ep.index);
            }
        }

        let mut findings = Vec::new();
        for (name, present_in) in &presence {
            if present_in.len() >= total {
                continue; // present in every episode — consistent.
            }
            // Which episodes lack it. Compact the list like the episode-continuity check does.
            // Dedup by index: duplicate episode indices (an `EpisodeBoundary` corruption) would
            // otherwise list the same missing index twice and inflate the reported count.
            let mut missing: Vec<u64> = episodes
                .iter()
                .map(|ep| ep.index)
                .filter(|idx| !present_in.contains(idx))
                .collect();
            missing.sort_unstable();
            missing.dedup();
            // `total` counts raw episodes; `present_in` holds distinct indices. Duplicate episode
            // indices (a corruption `EpisodeBoundary` flags) can make `present_in.len() < total` while
            // the stream is in fact present in every distinct episode — an empty `missing`. Don't emit
            // a malformed "missing from " finding for that; the duplicate index is already reported.
            if missing.is_empty() {
                continue;
            }
            let shown: Vec<String> = missing.iter().take(8).map(|i| i.to_string()).collect();
            let more = if missing.len() > shown.len() {
                format!(", … ({} more)", missing.len() - shown.len())
            } else {
                String::new()
            };
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Structural,
                    Severity::Warning,
                    Location::Dataset,
                    "STRUCTURAL.STREAM_PRESENCE_INCONSISTENT",
                    format!(
                        "stream `{name}` is present in {} of {total} episodes; missing from {}{more}",
                        present_in.len(),
                        shown.join(", "),
                    ),
                )
                .with_risk(
                    "A stream present in only some episodes yields a heterogeneous feature set: \
                     batched training errors on the missing feature or silently fills it, and a \
                     sensor that drops out mid-collection often signals a hardware or export fault.",
                )
                .with_remedy(
                    "Confirm whether the stream is meant to be present everywhere; if so, recover or \
                     re-export the episodes missing it, otherwise document the intentional variation.",
                ),
            );
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
        self.run_with(dataset, true)
    }
    /// Under a metadata-only ingest the frame-count arms abstain: every stream carries no frames by
    /// request, so `EMPTY_STREAM` would fire on every stream of every sound dataset. The
    /// empty-dataset and empty-stream*set* arms still hold — both are manifest facts.
    fn run_in(&self, dataset: &Dataset, context: &CheckContext) -> Vec<Finding> {
        self.run_with(dataset, context.frames_read)
    }
}

impl DegenerateEpisode {
    fn run_with(&self, dataset: &Dataset, frames_read: bool) -> Vec<Finding> {
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
            if !frames_read {
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

        // Any index missing between the smallest and largest observed is a dropped episode. Both
        // bounds come from the file, so the span between them is attacker-controlled and enormous:
        // two lines of a LeRobot `meta/episodes.jsonl` declaring index 0 and index u64::MAX made
        // this walk `0..=u64::MAX` and collect the misses into a `Vec` — not a slow check, a process
        // that never returns and allocates until it is killed. The gap count is the difference of
        // the bounds and the number present; it never needs the misses enumerated to be computed.
        let (lo, hi) = (indices[0], indices[indices.len() - 1]);
        // Written as `(hi - lo) - (n - 1)` rather than `(hi - lo + 1) - n`: the indices are sorted,
        // distinct, and at least two, so `hi - lo >= n - 1` and neither subtraction can wrap — while
        // the `+ 1` form overflows on exactly the input that motivated this, `lo = 0, hi = u64::MAX`.
        let missing_count = (hi - lo) - (indices.len() as u64 - 1);
        if missing_count == 0 {
            return Vec::new();
        }

        // List the first few missing indices for the message, walking only far enough to find them
        // rather than materializing the whole gap.
        let present: std::collections::HashSet<u64> = indices.iter().copied().collect();
        let shown: Vec<String> = (lo..=hi)
            .filter(|i| !present.contains(i))
            .take(8)
            .map(|i| i.to_string())
            .collect();
        let more = if missing_count > shown.len() as u64 {
            format!(", … ({} more)", missing_count - shown.len() as u64)
        } else {
            String::new()
        };
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Warning,
            Location::Dataset,
            "STRUCTURAL.EPISODE_INDEX_GAP",
            format!("episode indices span {lo}..={hi} but {missing_count} are missing: {}{more}", shown.join(", ")),
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

/// Exact-duplicate episodes. A dataset that carries the *same episode more than once* — a re-upload,
/// a bad merge of two exports, or a copy-paste in a manifest — over-weights those trajectories and
/// inflates the apparent dataset size, biasing training toward the repeated data. This groups
/// episodes by an exact **content** signature and flags any group holding more than one episode.
///
/// Soundness note: a duplicate claim requires proof that the *frame contents* are identical, so the
/// signature is built only from episodes whose every frame carries a `content_hash` — and the hash is
/// part of the signature. An episode with any hashless frame is **not fingerprintable** and is
/// excluded, because timestamps + schema + stored stats alone do not distinguish two genuinely
/// different same-length episodes (in LeRobot, for instance, every episode shares one relative time
/// base and dataset-global stats). This keeps the check from false-flagging normal datasets: it fires
/// only once adapters populate per-frame content hashes, never on shape-only coincidence.
///
/// Scope note: this catches *exact* duplicates. *Near*-duplicate detection (the same trajectory
/// re-recorded with small differences) needs frame-payload similarity, which the MVP design does not
/// decode, so it is deliberately out of scope here.
pub struct DuplicateEpisode;

impl DuplicateEpisode {
    /// A deterministic, exact signature of an episode's content, excluding its index. Returns `None`
    /// when the episode is not fingerprintable — any stream has no frames, or any frame lacks a
    /// `content_hash` — because without proven-identical content a duplicate cannot be claimed. Two
    /// episodes with equal `Some` signatures are byte-for-byte equivalent in every field a duplicate
    /// shares, frame contents included. Floats are captured by their bit pattern so equality is exact
    /// (and `NaN`-stable); the modality enum is captured by its `Debug` form.
    pub(crate) fn signature(ep: &crate::cdm::Episode) -> Option<String> {
        use std::fmt::Write as _;
        // An episode with no streams carries no content to compare — leave it to DegenerateEpisode.
        if ep.streams.is_empty() {
            return None;
        }
        let mut sig = String::new();
        // task and labels (labels sorted for order-insensitivity).
        let _ = write!(sig, "task={:?};", ep.task);
        let mut labels: Vec<&crate::cdm::Label> = ep.labels.iter().collect();
        labels.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.value.cmp(&b.value)));
        for l in labels {
            let _ = write!(sig, "L[{}={}];", l.key, l.value);
        }
        // streams, sorted by name (a duplicate has the same set regardless of listing order).
        let mut streams: Vec<&crate::cdm::Stream> = ep.streams.iter().collect();
        streams.sort_by(|a, b| a.name.cmp(&b.name));
        for s in streams {
            // A stream with no frames can't establish content identity.
            if s.frames.is_empty() {
                return None;
            }
            let _ = write!(
                sig,
                "S[name={};mod={:?};rate={:?};clock={};dtype={:?};shape={:?};frames=",
                s.name,
                s.modality,
                s.declared_rate_hz.map(f64::to_bits),
                s.clock_id,
                s.dtype,
                s.shape,
            );
            for f in &s.frames {
                // The content hash is what proves duplication; without it the episode is not
                // fingerprintable and the whole check must abstain for it.
                let hash = f.value_ref.content_hash?;
                let _ = write!(sig, "{}@{},", f.ts, hex32(&hash));
            }
            if let Some(st) = s.stats {
                let _ = write!(
                    sig,
                    ";stats={},{},{},{}",
                    st.min.to_bits(),
                    st.max.to_bits(),
                    st.mean.to_bits(),
                    st.std.to_bits()
                );
            }
            sig.push_str("];");
        }
        Some(sig)
    }
}

/// Lowercase hex of a 32-byte content hash, for the episode signature.
fn hex32(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

impl Check for DuplicateEpisode {
    fn id(&self) -> &'static str {
        "structural.duplicate-episode"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.DUPLICATE_EPISODE"]
    }
    fn title(&self) -> &'static str {
        "Duplicate episodes"
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
        // signature -> episode indices, in first-seen order within each group. Episodes that are not
        // fingerprintable (no proven-identical content) return `None` and are skipped, so a duplicate
        // is never claimed from shape/timing coincidence alone.
        let mut groups: HashMap<String, Vec<u64>> = HashMap::new();
        for ep in &dataset.episodes {
            if let Some(sig) = Self::signature(ep) {
                groups.entry(sig).or_default().push(ep.index);
            }
        }
        // Keep only groups with more than one episode; sort each group's indices, then order the
        // groups by their smallest index so the report is deterministic.
        let mut dup_groups: Vec<Vec<u64>> = groups
            .into_values()
            .filter(|idxs| idxs.len() > 1)
            .map(|mut idxs| {
                idxs.sort_unstable();
                idxs
            })
            .collect();
        dup_groups.sort_by_key(|idxs| idxs[0]);

        dup_groups
            .into_iter()
            .map(|idxs| {
                let list = idxs
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                Finding::new(
                    self.id(),
                    Category::Structural,
                    Severity::Warning,
                    Location::Dataset,
                    "STRUCTURAL.DUPLICATE_EPISODE",
                    format!(
                        "episodes {list} are exact duplicates (identical streams, timestamps, and \
                         stored statistics)"
                    ),
                )
                .with_risk(
                    "Duplicate episodes over-weight their trajectories and inflate the apparent \
                     dataset size — a re-upload or a bad merge — biasing training toward the \
                     repeated data.",
                )
                .with_remedy("De-duplicate the dataset so each episode appears once.")
            })
            .collect()
    }
}

/// The streams of one episode that can evidence duplication: stream name → its distinct frame
/// hashes. See [`NearDuplicateEpisode::evidence`] for what qualifies.
type EvidenceStreams<'a> = BTreeMap<&'a str, BTreeSet<[u8; 32]>>;

/// Near-duplicate episodes: two episodes built largely from the **same frames**.
///
/// [`DuplicateEpisode`] proves *exact* duplication — same streams, same timestamps, same stored
/// statistics, same frame bytes. The catalog also owes the case one step to the side, which is the
/// common one in a real corpus: an episode re-uploaded with its tail trimmed, a merge that pulled
/// the same recording in twice under different indices, one episode wholly contained in a longer
/// one. Every frame of the overlap is byte-identical; the episodes are not, so the exact check is
/// silent and the redundancy trains twice.
///
/// The evidence is set overlap over frame `content_hash`es — no payload is decoded, so nothing here
/// depends on understanding the data. What that buys and what it does not, stated plainly: this
/// catches re-uploads and partial copies, and it does **not** catch a re-encoded or perturbed copy,
/// whose bytes differ in every frame. That half needs payload similarity and remains out of scope.
///
/// Three guards keep it from firing on honest data, which is the whole difficulty of a
/// similarity check:
///
/// - **Only distinctive streams are evidence.** A stream is evidence only if every frame carries a
///   hash, it has at least [`NearDuplicateEpisode::MIN_FRAMES`] frames, and at least
///   [`NearDuplicateEpisode::MIN_DISTINCT_FRACTION`] of those frames are distinct from one another.
///   An arm at rest, a locked joint, or a quantized channel repeats a handful of values across every
///   episode of a dataset; overlap there is a fact about the sensor, not about duplication.
/// - **Every shared stream must agree.** A pair's overlap is the *minimum* across the streams both
///   episodes carry as evidence, so one coincidentally-similar channel cannot carry the claim while
///   the camera disagrees.
/// - **A frame that everyone has is not evidence.** A hash appearing in more than
///   [`NearDuplicateEpisode::MAX_EPISODES_PER_HASH`] episodes is boilerplate — a calibration frame,
///   a home position — and is skipped. The ceiling is deliberately far above any *duplication*
///   group: set near the size of a plausible re-upload it would defeat the case the check is for,
///   since a recording ingested forty times shares every frame with thirty-nine others. What bounds
///   the pathological dataset is [`NearDuplicateEpisode::MAX_TRACKED_PAIRS`], which abstains
///   loudly instead of quietly dropping evidence.
///
/// Pairs that [`DuplicateEpisode`] reports are suppressed here, using *its* signature function, so
/// the suppression can never be broader than what the other check actually says (a narrower
/// suppression would report twice; a broader one would report zero times, which is the dangerous
/// direction).
pub struct NearDuplicateEpisode {
    /// Minimum shared fraction of frames — over `min(|a|, |b|)`, so containment counts as full
    /// overlap — at which a pair is reported. Configurable as `near_duplicate_fraction`.
    pub min_overlap: f64,
}

impl NearDuplicateEpisode {
    /// Fewest frames a stream needs before its overlap means anything.
    pub const MIN_FRAMES: usize = 8;
    /// Fraction of a stream's frames that must be distinct from one another for it to be evidence.
    pub const MIN_DISTINCT_FRACTION: f64 = 0.8;
    /// A hash in more episodes than this is boilerplate, not evidence.
    ///
    /// High on purpose: a duplication group is evidence and must not be mistaken for boilerplate.
    /// At 512 the worst a single hash can cost is ~131k pair increments, and a dataset with enough
    /// such hashes trips [`Self::MAX_TRACKED_PAIRS`], which says so rather than going quiet.
    pub const MAX_EPISODES_PER_HASH: usize = 512;
    /// Ceiling on candidate pairs held at once. Past it the check abstains, loudly.
    pub const MAX_TRACKED_PAIRS: usize = 200_000;

    /// The evidence streams of one episode: stream name → its distinct frame hashes.
    fn evidence(ep: &crate::cdm::Episode) -> EvidenceStreams<'_> {
        let mut out = BTreeMap::new();
        for stream in &ep.streams {
            if stream.frames.len() < Self::MIN_FRAMES {
                continue;
            }
            let mut hashes = BTreeSet::new();
            let mut all_hashed = true;
            for frame in &stream.frames {
                match frame.value_ref.content_hash {
                    Some(h) => {
                        hashes.insert(h);
                    }
                    None => {
                        all_hashed = false;
                        break;
                    }
                }
            }
            if !all_hashed {
                continue;
            }
            let distinct = hashes.len() as f64 / stream.frames.len() as f64;
            if distinct < Self::MIN_DISTINCT_FRACTION {
                continue;
            }
            out.insert(stream.name.as_str(), hashes);
        }
        out
    }
}

impl Check for NearDuplicateEpisode {
    fn id(&self) -> &'static str {
        "structural.near-duplicate-episode"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "STRUCTURAL.NEAR_DUPLICATE_EPISODE",
            "STRUCTURAL.NEAR_DUPLICATE_UNCHECKED",
        ]
    }
    fn title(&self) -> &'static str {
        "Near-duplicate episodes"
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
        // Evidence per episode, keyed by the episode's position so pairs are cheap to name.
        let evidence: Vec<(u64, EvidenceStreams<'_>)> = dataset
            .episodes
            .iter()
            .map(|ep| (ep.index, Self::evidence(ep)))
            .filter(|(_, streams)| !streams.is_empty())
            .collect();
        if evidence.len() < 2 {
            return Vec::new();
        }

        // Shared-hash counts per (pair, stream). Built through an inverted index so the cost is
        // linear in frames rather than quadratic in episodes.
        let mut shared: BTreeMap<(usize, usize), BTreeMap<&str, usize>> = BTreeMap::new();
        let mut stream_names: BTreeSet<&str> = BTreeSet::new();
        for (_, streams) in &evidence {
            stream_names.extend(streams.keys().copied());
        }
        let mut overflowed = false;
        // Episodes at least one of whose frames was actually compared. An episode every one of
        // whose hashes was skipped as boilerplate was not examined at all, and saying nothing about
        // it would be indistinguishable from finding nothing — see the abstention below.
        let mut examined: BTreeSet<usize> = BTreeSet::new();
        for name in &stream_names {
            let mut index: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
            for (position, (_, streams)) in evidence.iter().enumerate() {
                if let Some(hashes) = streams.get(name) {
                    for h in hashes {
                        index.entry(*h).or_default().push(position);
                    }
                }
            }
            for (_, holders) in index {
                if holders.len() > Self::MAX_EPISODES_PER_HASH {
                    continue;
                }
                // A hash only one episode holds is evidence of nothing shared — but the episode
                // holding it *was* compared, which is the difference this set records.
                examined.extend(holders.iter().copied());
                if holders.len() < 2 {
                    continue;
                }
                for (i, a) in holders.iter().enumerate() {
                    for b in &holders[i + 1..] {
                        if shared.len() >= Self::MAX_TRACKED_PAIRS
                            && !shared.contains_key(&(*a, *b))
                        {
                            overflowed = true;
                            continue;
                        }
                        *shared.entry((*a, *b)).or_default().entry(name).or_insert(0) += 1;
                    }
                }
            }
        }

        let unexamined = evidence.len() - examined.len();
        if overflowed || unexamined > 0 {
            // Abstention is a finding: a check that ran out of room and said nothing is
            // indistinguishable from a check that looked and found nothing.
            return vec![Finding::new(
                self.id(),
                Category::Structural,
                Severity::Info,
                Location::Dataset,
                "STRUCTURAL.NEAR_DUPLICATE_UNCHECKED",
                if overflowed {
                    format!(
                        "near-duplicate detection abstained: more than {} episode pairs share \
                         frames, which is past what this check will hold at once",
                        Self::MAX_TRACKED_PAIRS
                    )
                } else {
                    format!(
                        "near-duplicate detection abstained on {unexamined} episode(s): every one \
                         of their frames is held by more than {} episodes, which this check treats \
                         as boilerplate rather than evidence",
                        Self::MAX_EPISODES_PER_HASH
                    )
                },
            )
            .with_risk(
                "Those episodes were not checked for near-duplication. A re-uploaded or partially \
                 copied episode among them is not absent from this report, it was never looked \
                 for.",
            )
            .with_remedy(
                "Check the dataset in parts (each shard or split on its own), where the pair count \
                 is within reach.",
            )];
        }

        // Pairs the exact check already speaks for. Computed with its own signature so this
        // suppression is exactly as wide as what it reports — never wider.
        //
        // Built in one pass keyed by episode index: looking each one up by scanning the episode list
        // is quadratic in the episode count, and a signature is itself linear in the episode's
        // frames, so the naive form costs a large dataset dearly for a suppression list.
        let mut signature_of: BTreeMap<u64, Option<String>> = BTreeMap::new();
        for episode in &dataset.episodes {
            signature_of
                .entry(episode.index)
                .or_insert_with(|| DuplicateEpisode::signature(episode));
        }
        let signatures: Vec<Option<&String>> = evidence
            .iter()
            .map(|(index, _)| signature_of.get(index).and_then(Option::as_ref))
            .collect();

        // Flag the pairs whose weakest shared stream still clears the threshold.
        let mut flagged: Vec<(usize, usize, f64)> = Vec::new();
        for ((a, b), per_stream) in &shared {
            let (index_a, streams_a) = &evidence[*a];
            let (index_b, streams_b) = &evidence[*b];
            let _ = (index_a, index_b);
            // Every stream both carry as evidence must agree, including one with no shared frames
            // at all — which is why the iteration is over the shared *names*, not the counted ones.
            let common: Vec<&str> = streams_a
                .keys()
                .filter(|n| streams_b.contains_key(*n))
                .copied()
                .collect();
            if common.is_empty() {
                continue;
            }
            let mut weakest = f64::INFINITY;
            for name in common {
                let count = per_stream.get(name).copied().unwrap_or(0) as f64;
                let denominator = streams_a[name].len().min(streams_b[name].len()) as f64;
                weakest = weakest.min(count / denominator);
            }
            if weakest >= self.min_overlap
                && !(signatures[*a].is_some() && signatures[*a] == signatures[*b])
            {
                flagged.push((*a, *b, weakest));
            }
        }
        if flagged.is_empty() {
            return Vec::new();
        }

        // Cluster the flagged pairs, so a group of three near-identical episodes is one finding
        // rather than three: the score deducts per finding, and this is one root cause.
        let mut group_of: Vec<usize> = (0..evidence.len()).collect();
        fn root(group_of: &mut [usize], mut x: usize) -> usize {
            while group_of[x] != x {
                group_of[x] = group_of[group_of[x]];
                x = group_of[x];
            }
            x
        }
        for (a, b, _) in &flagged {
            let (ra, rb) = (root(&mut group_of, *a), root(&mut group_of, *b));
            if ra != rb {
                group_of[ra.max(rb)] = ra.min(rb);
            }
        }
        let mut groups: BTreeMap<usize, (BTreeSet<u64>, f64)> = BTreeMap::new();
        for (a, b, overlap) in &flagged {
            let entry = groups
                .entry(root(&mut group_of, *a))
                .or_insert_with(|| (BTreeSet::new(), 1.0));
            entry.0.insert(evidence[*a].0);
            entry.0.insert(evidence[*b].0);
            entry.1 = entry.1.min(*overlap);
        }

        groups
            .into_values()
            .map(|(indices, overlap)| {
                let list = indices
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                Finding::new(
                    self.id(),
                    Category::Structural,
                    Severity::Warning,
                    Location::Dataset,
                    "STRUCTURAL.NEAR_DUPLICATE_EPISODE",
                    format!(
                        "episodes {list} are near-duplicates: they share at least {:.0}% of their \
                         frames byte-for-byte in every stream compared, without being exact copies",
                        overlap * 100.0
                    ),
                )
                .with_risk(
                    "A re-uploaded or partially copied episode trains its trajectory twice, \
                     over-weighting it and inflating the apparent size of the dataset — and unlike \
                     an exact duplicate it survives de-duplication by content hash.",
                )
                .with_remedy(
                    "Compare these episodes at the source: keep the longest or the original \
                     recording and drop the copies, or record why the overlap is intentional.",
                )
            })
            .collect()
    }
}

/// Frozen/stuck video stream. A camera whose feed freezes keeps emitting **byte-identical** frames
/// while its timestamps keep advancing — so every timestamp-based temporal check (monotonicity, rate,
/// gaps, skew) passes, yet the observations are stale garbage. Real camera frames are never
/// byte-identical (sensor noise alone guarantees it), so a run of frames sharing one `content_hash`
/// on a `Video` stream is a genuine freeze. This is scoped to `Video` because a constant *scalar*
/// stream (an arm at rest) is legitimate — that case is `STATISTICAL.DEGENERATE`'s concern, not this.
///
/// Only frames that carry a `content_hash` are compared (MCAP image messages are fingerprinted;
/// LeRobot video features live outside the Parquet and are unhashed, so the check honestly abstains
/// for them). A stream must repeat one frame for at least [`StuckStream::STUCK_RUN`] consecutive
/// frames to be flagged, so an isolated duplicated frame (an encoder hiccup) doesn't trip it.
pub struct StuckStream;

impl StuckStream {
    /// Minimum run of consecutive byte-identical frames that counts as a freeze rather than a hiccup.
    const STUCK_RUN: usize = 5;
}

impl Check for StuckStream {
    fn id(&self) -> &'static str {
        "structural.stuck-stream"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.STUCK_STREAM"]
    }
    fn title(&self) -> &'static str {
        "Frozen/stuck video stream"
    }
    fn category(&self) -> Category {
        Category::Structural
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
        for ep in &dataset.episodes {
            for stream in &ep.streams {
                if stream.modality != Modality::Video {
                    continue;
                }
                // Longest run of consecutive frames sharing one content hash. A frame without a hash
                // breaks the run (we can't claim it repeats the prior frame's content).
                let mut longest = 0usize;
                let mut run = 0usize;
                let mut prev: Option<[u8; 32]> = None;
                for frame in &stream.frames {
                    match frame.value_ref.content_hash {
                        Some(h) if Some(h) == prev => run += 1,
                        Some(h) => {
                            run = 1;
                            prev = Some(h);
                        }
                        None => {
                            run = 0;
                            prev = None;
                        }
                    }
                    longest = longest.max(run);
                }
                if longest >= Self::STUCK_RUN {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Structural,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: stream.name.clone(),
                            },
                            "STRUCTURAL.STUCK_STREAM",
                            format!(
                                "video stream `{}` in episode {} repeats one identical frame for {} \
                                 consecutive frames — a frozen or stuck feed",
                                stream.name, ep.index, longest
                            ),
                        )
                        .with_risk(
                            "A frozen camera repeats the same frame while timestamps keep advancing, \
                             so the policy trains on stale observations that the timestamp-based \
                             temporal checks cannot detect.",
                        )
                        .with_remedy(
                            "Check the capture pipeline for a stuck encoder or sensor; drop or \
                             re-record the frozen segment.",
                        ),
                    );
                }
            }
        }
        findings
    }
}

/// Streams whose frames carry no content fingerprint — and therefore what the content-based checks
/// could not compare.
///
/// The third of the family that
/// [`ClockMeasurability`](crate::checks::temporal::ClockMeasurability) started. Three checks in this
/// module prove things about frame *content*: `structural.duplicate-episode` (two episodes holding
/// byte-identical frames — a re-upload or a bad merge), `structural.near-duplicate-episode` (two
/// episodes built largely from the same frames — a partial copy), and `structural.stuck-stream` (a
/// camera repeating a byte-identical frame while timestamps advance — a freeze no timestamp check
/// can see). All three are sound-only by design: they compare `content_hash`, and abstain on any
/// frame without one.
///
/// That abstention was silent, and it is not a rare corner. A LeRobot video feature's pixels live in
/// `.mp4` files outside the Parquet, so its frames carry no hash — and `duplicate-episode` aborts the
/// whole episode signature if *any* frame of *any* stream lacks one, so a single video feature makes
/// two byte-identical episodes undetectable. That is the ordinary layout of a real LeRobot dataset.
/// `stuck-stream` only ever looks at `Video` streams, which on LeRobot are exactly the hashless ones,
/// so the frozen-camera check never ran there at all. Both reported nothing, and nothing read as
/// clean.
///
/// Informational: the dataset is not worse for storing its pixels beside its table. What changes is
/// that "no duplicate episodes found" and "no stuck camera found" were not findings about the data.
pub struct ContentMeasurability;

impl Check for ContentMeasurability {
    fn id(&self) -> &'static str {
        "structural.content-measurability"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["STRUCTURAL.UNFINGERPRINTED_CONTENT"]
    }
    fn title(&self) -> &'static str {
        "Frame content was fingerprinted"
    }
    fn category(&self) -> Category {
        Category::Structural
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
        // Reported once for the dataset: whether a stream's payload is hashable is a property of the
        // source layout, so one finding per episode would repeat the same fact for every episode.
        let mut unhashed: BTreeSet<&str> = BTreeSet::new();
        // Whether any episode was fingerprintable at all — the duplicate check needs a whole episode.
        let mut any_episode_complete = false;
        let mut any_frames = false;
        for ep in &dataset.episodes {
            let mut complete = !ep.streams.is_empty();
            for s in &ep.streams {
                if s.frames.is_empty() {
                    continue;
                }
                any_frames = true;
                if s.frames.iter().any(|f| f.value_ref.content_hash.is_none()) {
                    unhashed.insert(s.name.as_str());
                    complete = false;
                }
            }
            any_episode_complete |= complete;
        }
        if unhashed.is_empty() || !any_frames {
            return Vec::new();
        }

        let names: Vec<&str> = unhashed.into_iter().collect();
        let shown = names.iter().take(4).copied().collect::<Vec<_>>().join(", ");
        let listed = match names.len().saturating_sub(4) {
            0 => shown,
            rest => format!("{shown} and {rest} more"),
        };
        // The duplicate check needs *every* stream of an episode hashed, so one hashless feature
        // disables it for the whole dataset — a much larger consequence than the per-stream one, and
        // worth stating separately rather than leaving the reader to infer it.
        let duplicate_note = if any_episode_complete {
            "the duplicate-episode check still applies to the episodes that were fully fingerprinted"
        } else {
            "no episode was fully fingerprinted, so the duplicate-episode check could not run on \
             this dataset at all"
        };
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Info,
            Location::Dataset,
            "STRUCTURAL.UNFINGERPRINTED_CONTENT",
            format!(
                "{} stream(s) carry frames with no content fingerprint, so the stuck-stream and \
                 near-duplicate checks could not inspect them ({listed}); {duplicate_note}",
                names.len(),
            ),
        )
        .with_risk(
            "The content-based checks prove things by comparing frame bytes. Where there are no \
             bytes to compare, they produce nothing — so a re-uploaded episode or a frozen camera \
             in these streams is not absent from this report, it was never looked for.",
        )
        .with_remedy(
            "Treat duplicate-episode, near-duplicate-episode and stuck-stream as unverified for \
             these streams. For a LeRobot dataset this is the video features, whose pixels live \
             outside the Parquet.",
        )]
    }
}
