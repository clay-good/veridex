//! Semantic checks: language/annotation quality.
//!
//! Per the checks-catalog spec, Veridex **verifies** annotation quality without ever producing or
//! editing annotations. The first check here rates episode task strings: a present-but-empty or a
//! present-but-degenerate placeholder task carries little training signal for language-conditioned
//! policies.
//!
//! An **absent** task (`Episode::task == None`) is deliberately *not* flagged: the v0.1 adapters do
//! not yet resolve task strings (LeRobot `task_index` → `meta/tasks` is a follow-up), so `None`
//! means "unresolved", not "the source has no task". Flagging it would fire on every episode and
//! carry no signal. This check therefore judges only tasks that are actually present.

use std::collections::BTreeMap;

use crate::cdm::Dataset;
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Degenerate placeholder task strings — matched case-insensitively against the trimmed task. These
/// are common low-information stand-ins that carry no real instruction.
const PLACEHOLDERS: &[&str] = &[
    "hold", "up", "down", "left", "right", "n/a", "na", "none", "null", "task", "test", "todo", "-",
];

/// Task-string quality: present-but-empty or present-but-placeholder episode tasks.
pub struct TaskQuality;

impl Check for TaskQuality {
    fn id(&self) -> &'static str {
        "semantic.task-quality"
    }
    fn title(&self) -> &'static str {
        "Task-string quality"
    }
    fn category(&self) -> Category {
        Category::Semantic
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
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
            // Only judge tasks that are actually present (see module docs on why `None` is skipped).
            let Some(task) = &ep.task else {
                continue;
            };
            let trimmed = task.trim();
            if trimmed.is_empty() {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Semantic,
                        Severity::Warning,
                        Location::Episode { episode: ep.index },
                        "SEMANTIC.EMPTY_TASK",
                        format!("episode {} has a present but empty task string", ep.index),
                    )
                    .with_risk(
                        "An empty task gives a language-conditioned policy nothing to condition on, \
                         yet still counts as a labeled episode.",
                    )
                    .with_remedy(
                        "Fill in the task/instruction for this episode, or drop the label if the \
                         episode is genuinely task-free.",
                    ),
                );
            } else if PLACEHOLDERS.contains(&trimmed.to_ascii_lowercase().as_str()) {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Semantic,
                        Severity::Info,
                        Location::Episode { episode: ep.index },
                        "SEMANTIC.PLACEHOLDER_TASK",
                        format!(
                            "episode {} has a low-information placeholder task `{}`",
                            ep.index, trimmed
                        ),
                    )
                    .with_risk(
                        "A placeholder task is present but carries no real instruction, diluting \
                         language supervision without being obviously missing.",
                    )
                    .with_remedy(
                        "Replace the placeholder with the actual instruction for this episode.",
                    ),
                );
            }
        }
        findings
    }
}

/// Stream-key clarity: camera/stream keys that are ambiguous within an episode.
///
/// The CDM requires stream names to be unique within an episode, but names that differ only by
/// letter case or surrounding whitespace (e.g. `observation.images.top` vs `observation.images.Top`)
/// are still ambiguous — humans and downstream tooling routinely confuse them, and a policy keyed on
/// the wrong one silently trains on the wrong camera. This check flags such near-collisions without
/// altering any key.
pub struct StreamKeyClarity;

impl Check for StreamKeyClarity {
    fn id(&self) -> &'static str {
        "semantic.stream-key-clarity"
    }
    fn title(&self) -> &'static str {
        "Stream-key clarity"
    }
    fn category(&self) -> Category {
        Category::Semantic
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
            // Group stream names by a normalized key (trimmed + lowercased). More than one distinct
            // name under the same key is an ambiguous collision. BTreeMap keeps output deterministic.
            let mut groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
            for stream in &ep.streams {
                let norm = stream.name.trim().to_ascii_lowercase();
                groups.entry(norm).or_default().push(&stream.name);
            }
            for (_, names) in groups.iter().filter(|(_, n)| n.len() > 1) {
                for name in names {
                    let mut others: Vec<&str> =
                        names.iter().copied().filter(|n| n != name).collect();
                    others.sort_unstable();
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Semantic,
                            Severity::Warning,
                            Location::Stream {
                                episode: ep.index,
                                stream: (*name).to_string(),
                            },
                            "SEMANTIC.AMBIGUOUS_STREAM_KEY",
                            format!(
                                "stream `{}` in episode {} is ambiguous with {} (differs only by case/whitespace)",
                                name,
                                ep.index,
                                others
                                    .iter()
                                    .map(|o| format!("`{o}`"))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            ),
                        )
                        .with_risk(
                            "Near-identical stream keys are easily confused; a policy or check keyed \
                             on the wrong one trains on or validates the wrong sensor.",
                        )
                        .with_remedy(
                            "Rename the keys so they are unambiguous, or consolidate them if they \
                             refer to the same stream.",
                        ),
                    );
                }
            }
        }
        findings
    }
}
