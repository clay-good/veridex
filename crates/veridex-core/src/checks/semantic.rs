//! Semantic checks: language/annotation quality.
//!
//! Per the checks-catalog spec, Veridex **verifies** annotation quality without ever producing or
//! editing annotations. The first check here rates episode task strings: a present-but-empty or a
//! present-but-degenerate placeholder task carries little training signal for language-conditioned
//! policies.
//!
//! An **absent** task (`Episode::task == None`) is deliberately *not* flagged: it means the source
//! carried no resolvable task for that episode (e.g. a LeRobot dataset without `meta/tasks.jsonl`,
//! or a format with no task concept), which is "unresolved", not "the source has an empty task".
//! Flagging it would fire on every such episode and carry no signal. This check therefore judges
//! only tasks that are actually present.

use std::collections::{BTreeMap, BTreeSet};

use crate::cdm::{Dataset, Episode, TimestampNs};
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
    fn finding_codes(&self) -> &'static [&'static str] {
        &["SEMANTIC.EMPTY_TASK", "SEMANTIC.PLACEHOLDER_TASK"]
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

/// The episode's time span `[lo, hi]` on its clock: the **union** of the declared `[start_ts, end_ts]`
/// (when both are present and ordered) and the actual min/max frame timestamp across its streams.
/// `None` when the episode carries no timestamps at all (nothing to align against).
///
/// The union matters: a declared window that is *narrower* than the recorded frames (e.g. episode
/// metadata that doesn't encompass a late-joining stream) must not be treated as authoritative, or an
/// annotation on a genuinely recorded frame outside that window would be flagged `ANNOTATION_UNALIGNED`
/// — a false Error on real data. An annotation is aligned if it falls within any moment the episode
/// either declares or actually recorded.
fn episode_time_span(ep: &Episode) -> Option<(TimestampNs, TimestampNs)> {
    let mut span: Option<(TimestampNs, TimestampNs)> = None;
    for stream in &ep.streams {
        for f in &stream.frames {
            span = Some(match span {
                Some((lo, hi)) => (lo.min(f.ts), hi.max(f.ts)),
                None => (f.ts, f.ts),
            });
        }
    }
    if let (Some(s), Some(e)) = (ep.start_ts, ep.end_ts) {
        if e >= s {
            span = Some(match span {
                Some((lo, hi)) => (lo.min(s), hi.max(e)),
                None => (s, e),
            });
        }
    }
    span
}

/// Language-annotation integrity. Where a dataset carries timestamped language annotations (e.g. a
/// LeRobot mid-episode task change, surfaced as a `language` label with a timestamp), Veridex
/// **verifies** them without ever writing, generating, or altering one:
///
/// - `SEMANTIC.ANNOTATION_UNALIGNED` (error) — a timestamped annotation whose time falls outside its
///   episode's span, so it would attach to a moment the episode never recorded.
/// - `SEMANTIC.ANNOTATION_CONFLICT` (warning) — two annotations at the same timestamp with different
///   values: ambiguous supervision at one instant.
/// - `SEMANTIC.EMPTY_ANNOTATION` (warning) — an annotation present in form but empty in substance.
///
/// Only `language`-keyed labels are judged. Per-camera uniqueness is out of scope until an adapter
/// associates annotations with a specific stream.
pub struct AnnotationIntegrity;

impl Check for AnnotationIntegrity {
    fn id(&self) -> &'static str {
        "semantic.annotation-integrity"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "SEMANTIC.ANNOTATION_UNALIGNED",
            "SEMANTIC.ANNOTATION_CONFLICT",
            "SEMANTIC.EMPTY_ANNOTATION",
        ]
    }
    fn title(&self) -> &'static str {
        "Language-annotation integrity"
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
            let span = episode_time_span(ep);
            // Group timestamped language values by timestamp to detect same-instant conflicts.
            let mut by_ts: BTreeMap<TimestampNs, BTreeSet<&str>> = BTreeMap::new();
            for label in ep.labels.iter().filter(|l| l.key == "language") {
                let value = label.value.trim();

                // (1) Structural validity: an annotation present but empty carries no instruction.
                if value.is_empty() {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Semantic,
                            Severity::Warning,
                            Location::Episode { episode: ep.index },
                            "SEMANTIC.EMPTY_ANNOTATION",
                            format!("episode {} carries an empty language annotation", ep.index),
                        )
                        .with_risk(
                            "An empty annotation is present in form but supervises nothing, diluting \
                             language conditioning while counting as a labeled moment.",
                        )
                        .with_remedy("Fill in the annotation text, or drop the empty annotation."),
                    );
                }

                // (2) Timestamp alignment: a timed annotation must fall within the episode's span.
                if let (Some(ts), Some((lo, hi))) = (label.ts, span) {
                    if ts < lo || ts > hi {
                        findings.push(
                            Finding::new(
                                self.id(),
                                Category::Semantic,
                                Severity::Error,
                                Location::Episode { episode: ep.index },
                                "SEMANTIC.ANNOTATION_UNALIGNED",
                                format!(
                                    "episode {}: language annotation at ts {} falls outside the \
                                     episode span [{}, {}]",
                                    ep.index, ts, lo, hi
                                ),
                            )
                            .with_risk(
                                "An annotation outside the episode's timeline attaches to a moment \
                                 that was never recorded, so any per-frame language conditioning \
                                 built from it aligns to the wrong (or no) frame.",
                            )
                            .with_remedy(
                                "Re-derive the annotation timestamps against the episode's clock, or \
                                 drop annotations that reference frames outside the episode.",
                            ),
                        );
                    }
                }

                if let Some(ts) = label.ts {
                    by_ts.entry(ts).or_default().insert(value);
                }
            }

            // (3) Per-timestamp uniqueness: distinct values at one instant are ambiguous.
            for (ts, values) in by_ts.iter().filter(|(_, v)| v.len() > 1) {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Semantic,
                        Severity::Warning,
                        Location::Episode { episode: ep.index },
                        "SEMANTIC.ANNOTATION_CONFLICT",
                        format!(
                            "episode {}: {} conflicting language annotations at ts {} ({})",
                            ep.index,
                            values.len(),
                            ts,
                            list_a_few(values.iter().copied()),
                        ),
                    )
                    .with_risk(
                        "Two different instructions at the same instant give a language-conditioned \
                         policy contradictory supervision for the same frame.",
                    )
                    .with_remedy(
                        "Reconcile the annotations so at most one instruction applies per timestamp.",
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
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "SEMANTIC.DUPLICATE_STREAM_KEY",
            "SEMANTIC.AMBIGUOUS_STREAM_KEY",
        ]
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
        // A naming mistake is a property of the dataset's schema, not of an episode: the same
        // duplicated or ambiguous key repeats in every episode that carries those streams. Report each
        // distinct collision once, naming the first episode it appears in, so one mistake costs one
        // finding instead of one per episode (at 50 episodes a single case-collision otherwise
        // produced 100 warnings and drove the data score to zero).
        let mut reported_duplicate: BTreeSet<String> = BTreeSet::new();
        let mut reported_ambiguous: BTreeSet<String> = BTreeSet::new();
        for ep in &dataset.episodes {
            // (1) Exact-duplicate stream names — a violation of the CDM's "unique within its episode"
            // invariant. A repeated key is not a usable identifier at all, so this is the harder
            // fault (error) and is reported separately from a mere case/whitespace collision. One
            // finding per duplicated name (not per occurrence). BTreeMap keeps output deterministic.
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for stream in &ep.streams {
                *counts.entry(stream.name.as_str()).or_default() += 1;
            }
            for (name, count) in counts.iter().filter(|(_, c)| **c > 1) {
                if !reported_duplicate.insert((*name).to_string()) {
                    continue;
                }
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Semantic,
                        Severity::Error,
                        Location::Stream {
                            episode: ep.index,
                            stream: (*name).to_string(),
                        },
                        "SEMANTIC.DUPLICATE_STREAM_KEY",
                        format!(
                            "stream key `{}` appears {} times in episode {} — stream names must be \
                             unique within an episode",
                            name, count, ep.index
                        ),
                    )
                    .with_risk(
                        "A repeated stream key is not a usable identifier: code keyed on it silently \
                         picks one arbitrary stream, so a policy or check can train on or validate \
                         the wrong sensor.",
                    )
                    .with_remedy(
                        "Give each stream a unique name, or merge the duplicates if they are the \
                         same stream.",
                    ),
                );
            }

            // (2) Ambiguous collisions among *distinct* names (case/whitespace only). Group the
            // distinct names by a normalized (trimmed + lowercased) key; more than one distinct name
            // under a key is an ambiguous collision. Using a set of distinct names keeps exact
            // duplicates (handled above) out of this path, so the "others" list is never empty.
            let mut groups: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
            for stream in &ep.streams {
                let norm = stream.name.trim().to_ascii_lowercase();
                groups.entry(norm).or_default().insert(&stream.name);
            }
            for (norm, names) in groups.iter().filter(|(_, n)| n.len() > 1) {
                if !reported_ambiguous.insert(norm.clone()) {
                    continue;
                }
                for name in names {
                    // `names` is a sorted set of distinct names, so `others` is non-empty and stable.
                    let others: Vec<&str> = names.iter().copied().filter(|n| n != name).collect();
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
                                list_a_few(others.iter().copied()),
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

/// Name a few of a list and count the rest, each name quoted.
///
/// Nothing caps how many streams a file declares or how many labels it carries, so a list joined
/// verbatim here is sized by the input rather than by the catalog. That is not a cosmetic limit: a
/// finding message reaches the terminal, the JSON report, the SARIF and the HTML, and
/// `SEMANTIC.AMBIGUOUS_STREAM_KEY` emits one finding per member of a colliding group while listing
/// every *other* member — quadratic in the bytes. 2,000 streams that normalize alike produced 263 KB
/// per message and roughly half a gigabyte of report from a legal file. (A certificate carries only
/// counts, never message text, so it is the one output shape this could not reach.)
///
/// The count is always stated in full by the callers; only the enumeration is abbreviated, so
/// nothing a reader needs to size the problem is lost. (`statistical.rs`, `structural.rs`,
/// `temporal.rs` and `autonomy.rs` each carry their own copy of this four-and-the-rest shape.)
fn list_a_few<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut shown: Vec<String> = Vec::new();
    let mut rest = 0usize;
    for name in names {
        if shown.len() < 4 {
            shown.push(format!("`{name}`"));
        } else {
            rest += 1;
        }
    }
    let shown = shown.join(", ");
    match rest {
        0 => shown,
        rest => format!("{shown} and {rest} more"),
    }
}
