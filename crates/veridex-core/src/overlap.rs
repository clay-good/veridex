//! Episodes two datasets both contain: the one question about *pooling* that checking a dataset
//! cannot answer.
//!
//! Every check in the catalog looks inside one dataset. A corpus is assembled from several, and the
//! failure that costs a team a training run is not inside any of them: the same recording, exported
//! twice, arrives through two sources and is trained on twice. Nothing in either dataset is wrong,
//! each one's report is clean, each one's certificate verifies — and the pooled corpus is a smaller
//! dataset than its episode count says, weighted toward whatever was duplicated.
//!
//! This answers it from the fingerprints Veridex already takes. An episode's content signature
//! (`structural.duplicate-episode`'s own, so the two agree by construction) excludes the episode's
//! index and the dataset it sits in, and is `None` for an episode that cannot be fingerprinted at
//! all — so two episodes share a signature when they are the same recording, whichever dataset each
//! arrived in.
//!
//! Two questions, because a signature answers only the stronger one.
//!
//! **Exact only, and it says so.** A signature covers a stream's declared shape and its stored
//! statistics as well as every frame's content hash, so a re-export that recomputed its statistics,
//! re-encoded its video, or trimmed a tail is *not* reported here. Within one dataset
//! `structural.near-duplicate-episode` answers that weaker question by comparing frame hashes
//! directly, and so does the second half of this report: **shared frames**, counted over the
//! fingerprints that identify a particular recording rather than over every frame. A re-export is
//! then visible as the share of one dataset's frames that already occur in another, even when no
//! episode of either is byte-for-byte the other's.
//!
//! A fingerprint that occurs in more than one episode of its own dataset identifies no particular
//! recording — a padded frame, a frozen camera, a zeroed feature — so it is not evidence of
//! sharing and is left out of both sides of that count. Without that rule a flat frame two unrelated
//! datasets both happen to contain would read as a shared recording.

use std::collections::BTreeMap;

use serde_json::json;

use crate::cdm::Dataset;
use crate::checks::structural::DuplicateEpisode;

/// One dataset to compare, under the name a report should call it by.
pub struct Source<'a> {
    /// What to call this dataset in the report — the path the caller named it by.
    pub name: String,
    pub dataset: &'a Dataset,
}

/// One episode, as a member of a shared group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Member {
    pub dataset: String,
    pub episode: u64,
}

/// A group of episodes that are the same recording, spanning at least two datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    /// Every episode in the group, sorted — so the group reads the same way on every run.
    pub members: Vec<Member>,
    /// Streams that were left out of the comparison for these episodes, by name. Folded into the
    /// signature itself, so a group is only ever formed from episodes that excluded the same ones.
    pub uncompared_streams: Vec<String>,
}

/// One episode of one dataset that shares most of its frames with one episode of another — the
/// shape a **re-export** takes, where neither episode is byte-for-byte the other's because the
/// export recomputed its statistics or re-encoded its video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearShared {
    pub from: Member,
    pub to: Member,
    /// Fingerprints the two episodes have in common, counted over the fingerprints that identify
    /// one episode of their own dataset.
    pub shared: u64,
    /// The smaller of the two episodes' identifying-fingerprint counts — so an episode contained
    /// whole inside a longer one counts as a full match, as `near-duplicate-episode` has it.
    pub compared: u64,
}

impl NearShared {
    /// The share of the smaller episode's identifying fingerprints the two have in common.
    pub fn fraction(&self) -> f64 {
        if self.compared == 0 {
            return 0.0;
        }
        self.shared as f64 / self.compared as f64
    }
}

/// What each dataset contributed to the comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contribution {
    pub name: String,
    /// Episodes the dataset holds.
    pub episodes: u64,
    /// How many of those could be fingerprinted, and so compared at all.
    pub comparable: u64,
}

/// The result of comparing two or more datasets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlapReport {
    pub datasets: Vec<Contribution>,
    /// Groups spanning more than one dataset, largest first and then in sorted order.
    pub shared: Vec<Shared>,
    /// Episodes that appear in more than one dataset, counted once per dataset.
    pub shared_episodes: u64,
    /// Episode pairs across datasets that share most of their frames without being identical,
    /// largest share first. The re-export an exact signature cannot see.
    pub near: Vec<NearShared>,
    /// Datasets whose fingerprints were too many to hold at once, by name. Their rows are absent
    /// from `frames` rather than reported as zero, because "too many to compare" and "nothing
    /// shared" are opposite answers.
    pub frames_uncounted: Vec<String>,
}

impl OverlapReport {
    /// Whether any episode appears in more than one of the datasets compared.
    pub fn overlaps(&self) -> bool {
        !self.shared.is_empty()
    }

    /// Whether these datasets share anything at all — a whole episode or most of an episode's
    /// frames. The one question a pipeline gating a new source against its corpus asks.
    pub fn shares_anything(&self) -> bool {
        self.overlaps() || self.shares_frames()
    }

    /// Whether any dataset's frames occur in another, which a re-export shows as while sharing no
    /// whole episode.
    pub fn shares_frames(&self) -> bool {
        !self.near.is_empty()
    }

    /// Episodes that could not be compared at all, across every dataset.
    pub fn uncomparable(&self) -> u64 {
        self.datasets
            .iter()
            .map(|d| d.episodes - d.comparable)
            .sum()
    }
}

/// Compare two or more datasets and report the episodes they share.
///
/// Groups within a single dataset are deliberately not reported: that is
/// `structural.duplicate-episode`'s question, every `veridex check` already answers it, and
/// repeating it here would mean a corpus report in which a dataset's internal duplication reads as
/// pooling. A group is reported when its members span at least two of the datasets given — and all
/// of its members are listed, including any that sit in the same dataset, because that is the shape
/// of the group the reader has to act on.
pub fn overlap(sources: &[Source<'_>], min_fraction: f64) -> OverlapReport {
    let mut groups: BTreeMap<[u8; 32], (Vec<Member>, Vec<String>)> = BTreeMap::new();
    let datasets: Vec<Contribution> = sources
        .iter()
        .map(|s| {
            let mut comparable = 0u64;
            for ep in &s.dataset.episodes {
                if let Some(print) = DuplicateEpisode::fingerprint(ep) {
                    comparable += 1;
                    let entry = groups
                        .entry(print.digest)
                        .or_insert_with(|| (Vec::new(), print.uncompared.clone()));
                    entry.0.push(Member {
                        dataset: s.name.clone(),
                        episode: ep.index,
                    });
                }
            }
            Contribution {
                name: s.name.clone(),
                episodes: s.dataset.episodes.len() as u64,
                comparable,
            }
        })
        .collect();

    let mut shared: Vec<Shared> = groups
        .into_values()
        .filter(|(members, _)| {
            // Spanning two datasets is the question; two episodes of one dataset are not pooling.
            members
                .iter()
                .map(|m| m.dataset.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1
        })
        .map(|(mut members, uncompared)| {
            members.sort();
            Shared {
                members,
                uncompared_streams: uncompared,
            }
        })
        .collect();
    // Largest groups first — a recording in four sources matters more than one in two — then in
    // sorted order so the report is stable.
    shared.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.members.cmp(&b.members))
    });
    let shared_episodes = shared.iter().map(|g| g.members.len() as u64).sum();
    let (near, frames_uncounted) = near_matches(sources, min_fraction, &shared);
    OverlapReport {
        datasets,
        shared,
        shared_episodes,
        near,
        frames_uncounted,
    }
}

/// The most distinct frame fingerprints this comparison holds for one dataset.
///
/// A frame count is a product of numbers the files choose, and the sets below are built in memory on
/// top of the CDMs already there. Past this, the dataset's row is withheld and named rather than
/// reported as zero: "too many to compare" and "nothing shared" are opposite answers. Two million
/// hashes is 64 MB per dataset, which is a corpus far larger than any this is reached from.
const MAX_FINGERPRINTS_PER_DATASET: usize = 2_000_000;

/// Episode pairs across datasets that share most of their frames.
///
/// Counted over the fingerprints that **identify** one episode of their own dataset. A fingerprint
/// occurring in more than one of its own episodes names no particular recording — a padded frame, a
/// frozen camera, a zeroed feature — and counting it would make two unrelated datasets that both
/// contain a flat frame look like they shared one.
///
/// The threshold is the same one `structural.near-duplicate-episode` applies within a dataset, over
/// the same denominator (the smaller episode, so containment counts as a full match), so the two
/// answers mean the same thing on either side of a dataset boundary. A single coincidental
/// fingerprint — two datasets whose first frame happens to hold the same value — is a few percent of
/// an episode and is not reported, which is the whole reason this is counted per episode rather than
/// per dataset.
fn near_matches(
    sources: &[Source<'_>],
    min_fraction: f64,
    exact: &[Shared],
) -> (Vec<NearShared>, Vec<String>) {
    use std::collections::HashMap;

    // Per dataset: each identifying fingerprint and the episode it identifies, plus how many each
    // episode has. Built once, then intersected through an inverted index rather than pairwise.
    struct Prints {
        /// Fingerprint -> the one episode of this dataset holding it.
        owner: HashMap<[u8; 32], u64>,
        /// Episode -> how many identifying fingerprints it holds.
        counts: HashMap<u64, u64>,
    }
    let mut per_dataset: Vec<Option<Prints>> = Vec::new();
    let mut uncounted: Vec<String> = Vec::new();
    for s in sources {
        let mut holders: HashMap<[u8; 32], (u64, u32)> = HashMap::new();
        let mut over = false;
        'dataset: for ep in &s.dataset.episodes {
            for stream in &ep.streams {
                for frame in &stream.frames {
                    let Some(hash) = frame.value_ref.content_hash else {
                        continue;
                    };
                    match holders.get_mut(&hash) {
                        Some((owner, holders_count)) => {
                            if *owner != ep.index {
                                *owner = ep.index;
                                *holders_count += 1;
                            }
                        }
                        None => {
                            // `>=` or `>` differ by one fingerprint at a ceiling of two million, and either way the
                            // overflow is disclosed rather than dropped — a fixture that could see the difference would
                            // have to build two million fingerprints. A mutation sweep flagged it; it is untestable at
                            // any cost worth paying, and the behavior either side of the ceiling is what the tests cover.
                            if holders.len() >= MAX_FINGERPRINTS_PER_DATASET {
                                over = true;
                                break 'dataset;
                            }
                            holders.insert(hash, (ep.index, 1));
                        }
                    }
                }
            }
        }
        if over {
            uncounted.push(s.name.clone());
            per_dataset.push(None);
            continue;
        }
        let mut owner: HashMap<[u8; 32], u64> = HashMap::new();
        let mut counts: HashMap<u64, u64> = HashMap::new();
        for (hash, (episode, holders_count)) in holders {
            // Held by one episode of this dataset, so it names a particular recording.
            if holders_count == 1 {
                owner.insert(hash, episode);
                *counts.entry(episode).or_insert(0) += 1;
            }
        }
        per_dataset.push(Some(Prints { owner, counts }));
    }

    // Shared counts per (from episode, into dataset, into episode). Each fingerprint contributes at
    // most one increment per dataset pair, so this is linear in fingerprints rather than quadratic
    // in episodes.
    let mut shared: HashMap<(usize, u64, usize, u64), u64> = HashMap::new();
    for (i, from) in per_dataset.iter().enumerate() {
        let Some(from) = from else { continue };
        for (j, into) in per_dataset.iter().enumerate() {
            // Unordered: the count and the denominator are both symmetric, so reporting a pair
            // twice would say one thing twice.
            if j <= i {
                continue;
            }
            let Some(into) = into else { continue };
            for (hash, from_ep) in &from.owner {
                if let Some(into_ep) = into.owner.get(hash) {
                    *shared.entry((i, *from_ep, j, *into_ep)).or_insert(0) += 1;
                }
            }
        }
    }

    let mut rows: Vec<NearShared> = Vec::new();
    for ((i, from_ep, j, into_ep), count) in shared {
        let from = per_dataset[i].as_ref().expect("counted");
        let into = per_dataset[j].as_ref().expect("counted");
        let compared = from
            .counts
            .get(&from_ep)
            .copied()
            .unwrap_or(0)
            .min(into.counts.get(&into_ep).copied().unwrap_or(0));
        if compared == 0 {
            continue;
        }
        let row = NearShared {
            from: Member {
                dataset: sources[i].name.clone(),
                episode: from_ep,
            },
            to: Member {
                dataset: sources[j].name.clone(),
                episode: into_ep,
            },
            shared: count,
            compared,
        };
        // Pairs an exact group already speaks for are left to it, so the two halves of this report
        // never say the same thing twice — and the suppression is exactly as wide as what the exact
        // half reports, never wider.
        let already = exact
            .iter()
            .any(|g| g.members.contains(&row.from) && g.members.contains(&row.to));
        if !already && row.fraction() >= min_fraction {
            rows.push(row);
        }
    }
    // Largest share first, then in sorted order so the report is stable.
    rows.sort_by(|a, b| {
        b.fraction()
            .total_cmp(&a.fraction())
            .then_with(|| (&a.from, &a.to).cmp(&(&b.from, &b.to)))
    });
    (rows, uncounted)
}

/// A fraction as a whole-number percentage, for a line a person reads.
fn percent(fraction: f64) -> String {
    format!("{:.0}%", fraction * 100.0)
}

/// How many groups a terminal report lists before summarizing the rest. A corpus can share
/// thousands of episodes, and a report nobody can read is one nobody acts on.
const SHOWN_GROUPS: usize = 20;

/// Render an overlap report for a terminal.
pub fn render_overlap(report: &OverlapReport) -> String {
    let mut out = String::new();
    out.push_str("Veridex overlap\n");
    out.push_str(&format!(
        "  Verdict:  {}\n",
        match (report.overlaps(), report.shares_frames()) {
            (true, _) => "SHARED EPISODES",
            // A re-export shares no whole episode and is the same recording all the same, so it
            // must not read as "nothing shared".
            (false, true) => "SHARED FRAMES",
            (false, false) => "NOTHING SHARED",
        }
    ));
    for d in &report.datasets {
        let note = if d.comparable == d.episodes {
            String::new()
        } else {
            format!(
                " ({} comparable — the rest carry no content fingerprint)",
                d.comparable
            )
        };
        out.push_str(&format!("  {}: {} episode(s){note}\n", d.name, d.episodes));
    }
    if report.overlaps() {
        out.push_str(&format!(
            "  {} episode(s) in {} group(s) are the same recording in more than one dataset:\n",
            report.shared_episodes,
            report.shared.len()
        ));
        for group in report.shared.iter().take(SHOWN_GROUPS) {
            let members: Vec<String> = group
                .members
                .iter()
                .map(|m| format!("{} episode {}", m.dataset, m.episode))
                .collect();
            out.push_str(&format!("    {}\n", members.join(" = ")));
            if !group.uncompared_streams.is_empty() {
                out.push_str(&format!(
                    "      ({} stream(s) carry no content fingerprint and were not compared: {})\n",
                    group.uncompared_streams.len(),
                    group.uncompared_streams.join(", ")
                ));
            }
        }
        if let Some(rest) = report.shared.len().checked_sub(SHOWN_GROUPS) {
            if rest > 0 {
                out.push_str(&format!("    …and {rest} more group(s)\n"));
            }
        }
        out.push_str(
            "  Risk: a pooled corpus is a smaller dataset than its episode count says, weighted \
             toward whatever was duplicated. Training on it re-weights those recordings without \
             anything in either dataset's own report being wrong.\n",
        );
    }
    if report.shares_frames() {
        out.push_str("  Episodes that share most of their frames without being identical:\n");
        for row in report.near.iter().take(SHOWN_GROUPS) {
            out.push_str(&format!(
                "    {} episode {} and {} episode {} share {} of their frames ({} of {})\n",
                row.from.dataset,
                row.from.episode,
                row.to.dataset,
                row.to.episode,
                percent(row.fraction()),
                row.shared,
                row.compared
            ));
        }
        out.push_str(
            "  A re-export that recomputed its statistics shares no whole episode and shows up \
             here instead.\n",
        );
    }
    for name in &report.frames_uncounted {
        out.push_str(&format!(
            "  {name} holds more frame fingerprints than this comparison will hold at once, so its \
             frames were not counted — which is not the same as sharing none.\n"
        ));
    }
    if report.uncomparable() > 0 {
        out.push_str(&format!(
            "  {} episode(s) could not be compared at all: no stream in them carries a per-frame \
             content fingerprint, so nothing here is evidence about those episodes either way.\n",
            report.uncomparable()
        ));
    }
    out.push_str(
        "  Episode groups are exact matches only: a re-export that recomputed its statistics or \
         re-encoded its video is the same recording and appears in the frame counts rather than as \
         a group.\n",
    );
    out
}

/// Render an overlap report as machine output.
pub fn render_overlap_json(report: &OverlapReport) -> String {
    let doc = json!({
        "schema": "veridex.overlap/1",
        "shares_anything": report.shares_anything(),
        "overlaps": report.overlaps(),
        "shares_frames": report.shares_frames(),
        "shared_episodes": report.shared_episodes,
        "uncomparable_episodes": report.uncomparable(),
        "datasets": report.datasets.iter().map(|d| json!({
            "name": d.name,
            "episodes": d.episodes,
            "comparable_episodes": d.comparable,
        })).collect::<Vec<_>>(),
        "near": report.near.iter().map(|r| json!({
            "from": {"dataset": r.from.dataset, "episode": r.from.episode},
            "to": {"dataset": r.to.dataset, "episode": r.to.episode},
            "shared_fingerprints": r.shared,
            "compared_fingerprints": r.compared,
            "fraction": r.fraction(),
        })).collect::<Vec<_>>(),
        "frames_uncounted": report.frames_uncounted,
        "shared": report.shared.iter().map(|g| json!({
            "members": g.members.iter().map(|m| json!({
                "dataset": m.dataset,
                "episode": m.episode,
            })).collect::<Vec<_>>(),
            "uncompared_streams": g.uncompared_streams,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdm::{ClockKind, Dataset, Episode, Frame, Modality, Stream, TimestampNs, ValueRef};

    fn frame(ts: TimestampNs, hash: Option<u8>) -> Frame {
        Frame {
            ts,
            value_ref: ValueRef {
                uri: "data.parquet".into(),
                byte_offset: None,
                byte_len: None,
                content_hash: hash.map(|b| [b; 32]),
            },
        }
    }

    /// One episode whose two frames carry the given content hashes — the only thing that makes two
    /// episodes provably the same recording.
    fn episode(index: u64, hashes: Option<(u8, u8)>) -> Episode {
        let frames = match hashes {
            Some((a, b)) => vec![frame(0, Some(a)), frame(1_000, Some(b))],
            None => vec![frame(0, None), frame(1_000, None)],
        };
        Episode {
            index,
            start_ts: Some(0),
            end_ts: Some(1_000),
            task: None,
            labels: vec![],
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
            streams: vec![Stream {
                name: "action".into(),
                modality: Modality::Action,
                declared_rate_hz: None,
                clock_id: "c".into(),
                clock_kind: ClockKind::Measured,
                dtype: None,
                shape: None,
                dim_names: None,
                stats: None,
                dim_stats: None,
                observed_stats: None,
                observed_saturation: None,
                observed_non_finite: None,
                observed_dim_stats: None,
                latched: None,
                declared_range: None,
                point_fields: None,
                observed_diagnostics: None,
                observed_point_counts: None,
                observed_image_dims: None,
                observed_body_decodes: None,
                observed_header_stamps: None,
                observed_sequence: None,
                observed_fix_availability: None,
                media: None,
                frame_id: None,
                frames,
            }],
        }
    }

    fn dataset(episodes: Vec<Episode>) -> Dataset {
        Dataset {
            id: "d".into(),
            calibration: None,
            metadata: vec![],
            provenance: vec![],
            episodes,
        }
    }

    fn report(a: &Dataset, b: &Dataset) -> OverlapReport {
        overlap_at(a, b, 0.8)
    }

    fn overlap_at(a: &Dataset, b: &Dataset, min_fraction: f64) -> OverlapReport {
        overlap(
            &[
                Source {
                    name: "a".into(),
                    dataset: a,
                },
                Source {
                    name: "b".into(),
                    dataset: b,
                },
            ],
            min_fraction,
        )
    }

    #[test]
    fn the_same_recording_in_two_datasets_is_reported_once_with_both_members() {
        let a = dataset(vec![episode(0, Some((1, 2))), episode(1, Some((3, 4)))]);
        let b = dataset(vec![episode(7, Some((3, 4)))]);
        let r = report(&a, &b);
        assert!(r.overlaps());
        assert_eq!(r.shared.len(), 1);
        assert_eq!(
            r.shared[0].members,
            vec![
                Member {
                    dataset: "a".into(),
                    episode: 1
                },
                Member {
                    dataset: "b".into(),
                    episode: 7
                }
            ],
            "the group names both sides, by the dataset the caller named and the episode's own index"
        );
        assert_eq!(r.shared_episodes, 2);
    }

    #[test]
    fn two_datasets_sharing_nothing_report_nothing() {
        let a = dataset(vec![episode(0, Some((1, 2)))]);
        let b = dataset(vec![episode(0, Some((3, 4)))]);
        let r = report(&a, &b);
        assert!(!r.overlaps());
        assert_eq!(r.shared_episodes, 0);
        assert_eq!(r.uncomparable(), 0);
    }

    #[test]
    fn duplication_inside_one_dataset_is_not_pooling() {
        // `structural.duplicate-episode` answers this, on every `veridex check`. Repeating it here
        // would mean a corpus report in which a dataset's internal duplication reads as overlap
        // with another dataset.
        let a = dataset(vec![episode(0, Some((1, 2))), episode(1, Some((1, 2)))]);
        let b = dataset(vec![episode(0, Some((9, 9)))]);
        assert!(!report(&a, &b).overlaps());

        // …but when the group *does* span two datasets, every member is listed, including the two
        // that sit in the same one: that is the shape of the group a reader has to act on.
        let b = dataset(vec![episode(5, Some((1, 2)))]);
        let r = report(&a, &b);
        assert_eq!(r.shared.len(), 1);
        assert_eq!(r.shared[0].members.len(), 3);
    }

    #[test]
    fn an_episode_with_no_fingerprint_is_excluded_and_counted_rather_than_matched() {
        // Two episodes nobody can fingerprint are not thereby the same recording — the defect this
        // whole comparison would otherwise invent on any format that stores its pixels out of band.
        let a = dataset(vec![episode(0, None)]);
        let b = dataset(vec![episode(0, None)]);
        let r = report(&a, &b);
        assert!(!r.overlaps());
        assert_eq!(r.uncomparable(), 2);
        assert_eq!(r.datasets[0].comparable, 0);
        assert_eq!(r.datasets[0].episodes, 1);
        // And the report says so rather than leaving the silence to be read as a clean result.
        assert!(
            render_overlap(&r).contains("could not be compared"),
            "{}",
            render_overlap(&r)
        );
    }

    #[test]
    fn a_recording_in_three_datasets_sorts_before_one_in_two() {
        let a = dataset(vec![episode(0, Some((1, 1))), episode(1, Some((2, 2)))]);
        let b = dataset(vec![episode(0, Some((1, 1))), episode(1, Some((2, 2)))]);
        let c = dataset(vec![episode(0, Some((1, 1)))]);
        let r = overlap(
            &[
                Source {
                    name: "a".into(),
                    dataset: &a,
                },
                Source {
                    name: "b".into(),
                    dataset: &b,
                },
                Source {
                    name: "c".into(),
                    dataset: &c,
                },
            ],
            0.8,
        );
        assert_eq!(r.shared.len(), 2);
        assert_eq!(
            r.shared[0].members.len(),
            3,
            "a recording in three sources matters more than one in two: {:?}",
            r.shared
        );
        assert_eq!(r.shared_episodes, 5);
    }
    /// An episode whose frames carry `hashes`, one frame each.
    fn episode_of(index: u64, hashes: &[u8]) -> Episode {
        let mut ep = episode(index, Some((0, 0)));
        ep.streams[0].frames = hashes
            .iter()
            .enumerate()
            .map(|(i, h)| frame(i as i64 * 1_000, Some(*h)))
            .collect();
        ep
    }

    #[test]
    fn a_re_export_is_found_by_its_frames_even_though_no_episode_matches() {
        // The realistic pooling case, and the one an exact signature cannot see: the same recording
        // re-exported, so its summary statistics differ and no episode is byte-for-byte the other's
        // — while nine of its ten frames are the same bytes.
        let mut a = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])]);
        let mut b = dataset(vec![episode_of(4, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 99])]);
        // A difference an export introduces and a signature notices: the stored statistics.
        a.episodes[0].streams[0].stats = Some(crate::cdm::StreamStats {
            min: 0.0,
            max: 1.0,
            mean: 0.5,
            std: 0.1,
        });
        b.episodes[0].streams[0].stats = None;

        let r = report(&a, &b);
        assert!(
            !r.overlaps(),
            "no episode is byte-for-byte the other's, so there is no exact group: {:?}",
            r.shared
        );
        assert!(r.shares_frames(), "{r:?}");
        let row = &r.near[0];
        assert_eq!(row.from.episode, 0);
        assert_eq!(row.to.episode, 4);
        assert_eq!(row.shared, 9);
        assert_eq!(row.compared, 10);
        assert!(
            render_overlap(&r).contains("share 90% of their frames"),
            "{}",
            render_overlap(&r)
        );
    }

    #[test]
    fn one_coincidental_frame_between_unrelated_datasets_is_not_a_match() {
        // The reason this is counted per episode rather than per dataset: two unrelated datasets
        // whose first frame happens to hold the same value share one fingerprint, which is a few
        // percent of an episode and no evidence at all. Counted over the datasets as wholes it
        // would have read as a double-digit percentage.
        let a = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 8])]);
        let b = dataset(vec![episode_of(0, &[1, 20, 30, 40, 50, 60, 70, 80])]);
        let r = report(&a, &b);
        assert!(!r.shares_frames(), "{:?}", r.near);
    }

    #[test]
    fn a_fingerprint_more_than_one_of_its_own_episodes_holds_is_not_evidence() {
        // A padded frame, a frozen camera, a zeroed feature: held by every episode of its own
        // dataset, so it names no particular recording. Counting it would make two datasets that
        // both contain a flat frame look like they shared one.
        let a = dataset(vec![
            episode_of(0, &[7, 7, 7, 7, 7, 7, 7, 7]),
            episode_of(1, &[7, 7, 7, 7, 7, 7, 7, 7]),
        ]);
        let b = dataset(vec![episode_of(0, &[7, 7, 7, 7, 7, 7, 7, 7])]);
        let r = report(&a, &b);
        assert!(!r.shares_frames(), "{:?}", r.near);
        // The exact signature still speaks for these, which is the right place for it: they really
        // are identical episodes.
        assert!(r.overlaps());
    }

    #[test]
    fn an_episode_contained_in_a_longer_one_counts_as_a_full_match() {
        // The denominator is the smaller episode, as `near-duplicate-episode` has it: a trimmed
        // re-export is contained in the original rather than similar to it.
        let a = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])]);
        let b = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5])]);
        let r = report(&a, &b);
        assert!(r.shares_frames());
        assert!(
            (r.near[0].fraction() - 1.0).abs() < f64::EPSILON,
            "{:?}",
            r.near[0]
        );
    }
    // Boundary pins, written after a mutation sweep over this module: three of its comparisons
    // could be flipped with no test noticing, and each one is a promise the report makes.

    #[test]
    fn a_pair_exactly_at_the_threshold_is_reported() {
        // The threshold is the one `near-duplicate-episode` applies within a dataset, and that one
        // reports a pair whose weakest shared stream *reaches* it. Either side of the boundary must
        // behave as it says: eight of ten at 0.8 is a match, seven is not.
        let a = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])]);
        let eight = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 8, 91, 92])]);
        let r = overlap_at(&a, &eight, 0.8);
        assert!(
            r.shares_frames(),
            "exactly at the threshold is a match: {r:?}"
        );
        let seven = dataset(vec![episode_of(0, &[1, 2, 3, 4, 5, 6, 7, 81, 82, 83])]);
        assert!(!overlap_at(&a, &seven, 0.8).shares_frames());
    }

    #[test]
    fn the_report_says_nothing_about_episodes_it_could_compare() {
        // The "could not be compared" sentence is for the episodes that carry no fingerprint. A
        // clean pair has none, and printing "0 episode(s) could not be compared" would be a
        // sentence about nothing in a report whose whole value is that its silences mean something.
        let a = dataset(vec![episode_of(0, &[1, 2, 3])]);
        let b = dataset(vec![episode_of(0, &[4, 5, 6])]);
        let text = render_overlap(&report(&a, &b));
        assert!(!text.contains("could not be compared"), "{text}");
    }

    #[test]
    fn a_long_list_of_groups_is_summarized_and_a_short_one_is_not() {
        // The tail line exists so a corpus sharing thousands of episodes stays readable. It must
        // appear only when something was actually left out.
        let many: Vec<Episode> = (0..SHOWN_GROUPS as u64 + 1)
            .map(|i| episode_of(i, &[i as u8 + 1, i as u8 + 1]))
            .collect();
        let r = report(&dataset(many.clone()), &dataset(many));
        let text = render_overlap(&r);
        assert!(r.shared.len() > SHOWN_GROUPS, "{} groups", r.shared.len());
        assert!(text.contains("and 1 more group(s)"), "{text}");

        let few = vec![episode_of(0, &[7, 7])];
        let short = render_overlap(&report(&dataset(few.clone()), &dataset(few)));
        assert!(!short.contains("more group(s)"), "{short}");

        // And exactly at the cap, where the subtraction yields zero rather than nothing: "and 0
        // more" is a sentence about nothing.
        let exact: Vec<Episode> = (0..SHOWN_GROUPS as u64)
            .map(|i| episode_of(i, &[i as u8 + 1, i as u8 + 1]))
            .collect();
        let at_cap = render_overlap(&report(&dataset(exact.clone()), &dataset(exact)));
        assert!(!at_cap.contains("more group(s)"), "{at_cap}");
    }
}
