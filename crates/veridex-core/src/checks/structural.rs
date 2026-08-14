//! Structural checks: episode/stream integrity.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::cdm::{Dataset, Modality};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

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

        // Declared-vs-actual per-episode length: the direct lerobot#4143 signature. When the source
        // manifest records a frame count for an episode (LeRobot `meta/episodes.jsonl` `length`) that
        // disagrees with the frames actually ingested, the cumulative boundaries LeRobot derives from
        // those lengths are wrong, and frames load under the wrong episode during training. The actual
        // count is the largest per-stream frame count (streams are frame-aligned; the max is robust to
        // a stream that abstains from frames).
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
    fn signature(ep: &crate::cdm::Episode) -> Option<String> {
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
