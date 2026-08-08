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
