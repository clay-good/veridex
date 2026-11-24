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
//! **Exact only, and it says so.** A signature covers a stream's declared shape and its stored
//! statistics as well as every frame's content hash, so a re-export that recomputed its statistics,
//! re-encoded its video, or trimmed a tail is *not* reported here. Within one dataset
//! `structural.near-duplicate-episode` answers that weaker question by comparing frame hashes
//! directly; across datasets this reports what it can prove — byte-for-byte the same recording —
//! and names what it could not compare.

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
}

impl OverlapReport {
    /// Whether any episode appears in more than one of the datasets compared.
    pub fn overlaps(&self) -> bool {
        !self.shared.is_empty()
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
pub fn overlap(sources: &[Source<'_>]) -> OverlapReport {
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
    OverlapReport {
        datasets,
        shared,
        shared_episodes,
    }
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
        if report.overlaps() {
            "SHARED EPISODES"
        } else {
            "NO SHARED EPISODES"
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
    if report.uncomparable() > 0 {
        out.push_str(&format!(
            "  {} episode(s) could not be compared at all: no stream in them carries a per-frame \
             content fingerprint, so nothing here is evidence about those episodes either way.\n",
            report.uncomparable()
        ));
    }
    out.push_str(
        "  Exact matches only: a re-export that recomputed its statistics or re-encoded its video \
         is the same recording and is not reported here.\n",
    );
    out
}

/// Render an overlap report as machine output.
pub fn render_overlap_json(report: &OverlapReport) -> String {
    let doc = json!({
        "schema": "veridex.overlap/1",
        "overlaps": report.overlaps(),
        "shared_episodes": report.shared_episodes,
        "uncomparable_episodes": report.uncomparable(),
        "datasets": report.datasets.iter().map(|d| json!({
            "name": d.name,
            "episodes": d.episodes,
            "comparable_episodes": d.comparable,
        })).collect::<Vec<_>>(),
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
        overlap(&[
            Source {
                name: "a".into(),
                dataset: a,
            },
            Source {
                name: "b".into(),
                dataset: b,
            },
        ])
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
        let r = overlap(&[
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
        ]);
        assert_eq!(r.shared.len(), 2);
        assert_eq!(
            r.shared[0].members.len(),
            3,
            "a recording in three sources matters more than one in two: {:?}",
            r.shared
        );
        assert_eq!(r.shared_episodes, 5);
    }
}
