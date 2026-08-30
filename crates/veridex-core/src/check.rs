//! Check types: the vocabulary the [validation engine](crate::engine) runs and aggregates.
//!
//! The engine knows nothing about specific checks; a [`Check`] declares its identity and scope and
//! inspects a [`Dataset`], emitting [`Finding`]s. Concrete checks live in
//! `checks-catalog` (implemented incrementally).

use serde::{Deserialize, Serialize};

use crate::cdm::Dataset;

/// Finding severity. Ordered `Info < Warning < Error`; the maximum severity present drives the
/// verdict [`Status`](crate::engine::Status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Informational; never fails a run.
    Info,
    /// A problem worth surfacing but not disqualifying.
    Warning,
    /// A disqualifying problem.
    Error,
}

impl Severity {
    /// The stable canonical tag for this severity.
    pub fn tag(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }
}

/// The category a check belongs to (exactly one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Episode/stream/shape integrity.
    Structural,
    /// Timestamps, rates, cross-stream synchronization.
    Temporal,
    /// Distributions, ranges, saturation.
    Statistical,
    /// Language/annotation semantics.
    Semantic,
    /// Video-specific checks.
    Video,
    /// Provenance presence and consistency.
    Provenance,
    /// Autonomy sensor-rig checks (rig-wide sync, spatial calibration, ego-pose, sequence).
    Autonomy,
}

impl Category {
    /// The stable canonical tag for this category.
    pub fn tag(self) -> &'static str {
        match self {
            Category::Structural => "structural",
            Category::Temporal => "temporal",
            Category::Statistical => "statistical",
            Category::Semantic => "semantic",
            Category::Video => "video",
            Category::Provenance => "provenance",
            Category::Autonomy => "autonomy",
        }
    }
}

/// The CDM scope a check applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// The whole dataset.
    Dataset,
    /// Each episode.
    Episode,
    /// Each stream.
    Stream,
}

impl Scope {
    /// The stable canonical tag for this scope.
    pub fn tag(self) -> &'static str {
        match self {
            Scope::Dataset => "dataset",
            Scope::Episode => "episode",
            Scope::Stream => "stream",
        }
    }
}

/// The precise CDM location a finding concerns. Precise enough to navigate to without rerunning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Location {
    /// The dataset as a whole.
    Dataset,
    /// A single episode.
    Episode {
        /// Episode index.
        episode: u64,
    },
    /// A single stream within an episode.
    Stream {
        /// Episode index.
        episode: u64,
        /// Stream name.
        stream: String,
    },
    /// A contiguous frame range within a stream.
    FrameRange {
        /// Episode index.
        episode: u64,
        /// Stream name.
        stream: String,
        /// Inclusive start frame index.
        start_frame: u64,
        /// Inclusive end frame index.
        end_frame: u64,
    },
    /// A time range within a stream (nanoseconds on that stream's clock).
    TimeRange {
        /// Episode index.
        episode: u64,
        /// Stream name.
        stream: String,
        /// Inclusive start timestamp.
        start_ts: i64,
        /// Inclusive end timestamp.
        end_ts: i64,
    },
}

impl Location {
    /// The episode index this location concerns, if any (dataset-scope locations have none). Used
    /// by reporting to roll findings up per episode.
    pub fn episode(&self) -> Option<u64> {
        match self {
            Location::Dataset => None,
            Location::Episode { episode }
            | Location::Stream { episode, .. }
            | Location::FrameRange { episode, .. }
            | Location::TimeRange { episode, .. } => Some(*episode),
        }
    }

    /// A total-order sort key so findings order deterministically regardless of execution order.
    /// Tuple shape: (variant rank, episode, stream, a, b).
    pub(crate) fn sort_key(&self) -> (u8, u64, &str, i64, i64) {
        match self {
            Location::Dataset => (0, 0, "", 0, 0),
            Location::Episode { episode } => (1, *episode, "", 0, 0),
            Location::Stream { episode, stream } => (2, *episode, stream.as_str(), 0, 0),
            Location::FrameRange {
                episode,
                stream,
                start_frame,
                end_frame,
            } => (
                3,
                *episode,
                stream.as_str(),
                *start_frame as i64,
                *end_frame as i64,
            ),
            Location::TimeRange {
                episode,
                stream,
                start_ts,
                end_ts,
            } => (4, *episode, stream.as_str(), *start_ts, *end_ts),
        }
    }
}

/// A single issue found by a check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The check that produced it.
    pub check_id: &'static str,
    /// Effective severity (after any configured override).
    pub severity: Severity,
    /// The check's category.
    pub category: Category,
    /// Precise CDM location.
    pub location: Location,
    /// Stable machine-readable code (e.g. `TEMPORAL.CLOCK_SKEW`).
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Why this matters for training (the risk if ignored). Empty if the check declares none.
    pub risk: String,
    /// Suggested remedy. Empty if the check declares none. (Veridex never mutates the dataset;
    /// this is advice, not an action.)
    pub remedy: String,
}

impl Finding {
    /// Convenience constructor. `risk`/`remedy` default empty; set them with [`Finding::with_risk`]
    /// and [`Finding::with_remedy`]. Severities may be overridden by the engine before the finding
    /// lands in the verdict.
    pub fn new(
        check_id: &'static str,
        category: Category,
        severity: Severity,
        location: Location,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Finding {
            check_id,
            severity,
            category,
            location,
            code: code.into(),
            message: message.into(),
            risk: String::new(),
            remedy: String::new(),
        }
    }

    /// Attach the training risk this finding represents.
    pub fn with_risk(mut self, risk: impl Into<String>) -> Self {
        self.risk = risk.into();
        self
    }

    /// Attach a suggested remedy.
    pub fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = remedy.into();
        self
    }
}

/// The total order findings are reported in, independent of the order the checks emitted them.
///
/// Every field is in the key on purpose: a sort that ties on non-identical content falls through to
/// `Vec` order, which is execution order — and `result_content_hash` is computed over this
/// sequence, so the same two findings emitted in either order would have to hash alike and would
/// not. Shared by the engine and by [`crate::redact`], which re-sorts after adding its disclosure,
/// so the two orders cannot drift apart.
pub(crate) fn finding_order(a: &Finding, b: &Finding) -> core::cmp::Ordering {
    a.check_id
        .cmp(b.check_id)
        .then_with(|| a.location.sort_key().cmp(&b.location.sort_key()))
        .then_with(|| a.code.cmp(&b.code))
        .then_with(|| a.message.cmp(&b.message))
        .then_with(|| a.severity.cmp(&b.severity))
        .then_with(|| a.category.cmp(&b.category))
        .then_with(|| a.risk.cmp(&b.risk))
        .then_with(|| a.remedy.cmp(&b.remedy))
}

/// A registered check. The engine treats it as an opaque unit of work over the CDM.
pub trait Check: Send + Sync {
    /// Stable, unique check id (e.g. `structural.episode-boundary`).
    fn id(&self) -> &'static str;
    /// Human-readable title.
    fn title(&self) -> &'static str;
    /// The category this check belongs to.
    fn category(&self) -> Category;
    /// Default severity for this check's findings.
    fn default_severity(&self) -> Severity;
    /// The CDM scope this check applies to.
    fn scope(&self) -> Scope;
    /// The check's version, recorded in the verdict for reproducibility.
    fn version(&self) -> &'static str;
    /// Every finding `code` this check can emit (e.g. `TEMPORAL.CLOCK_SKEW`). Declaring the codes
    /// makes the catalog self-describing — `veridex checks` lists them and tests guard them against
    /// the doc catalog — so a code can't silently drift from its documentation. Every code a check's
    /// [`Check::run`] emits must appear here.
    fn finding_codes(&self) -> &'static [&'static str];
    /// Inspect the dataset and emit findings. Findings should use [`Check::default_severity`]
    /// unless the check intentionally varies severity by finding.
    fn run(&self, dataset: &Dataset) -> Vec<Finding>;

    /// Like [`Check::run`], but told what the ingest actually covered.
    ///
    /// The default ignores the context, which is right for every check whose conclusions depend only
    /// on what is in the CDM. Override it in a check that would otherwise read *absence* as a
    /// defect — under a metadata-only ingest an episode legitimately carries no frames, and a check
    /// that compares a declared frame count against the frames present would fail every sound
    /// dataset. The engine always calls this.
    fn run_in(&self, dataset: &Dataset, _context: &CheckContext) -> Vec<Finding> {
        self.run(dataset)
    }
}

/// What a check needs to know about the run beyond the CDM itself.
///
/// Deliberately minimal: a check reads the dataset, not the environment. The one thing the CDM
/// cannot express is the difference between "this stream has no frames" and "this run did not read
/// any frames," and those must not produce the same findings.
#[derive(Debug, Clone)]
pub struct CheckContext {
    /// Whether the ingest read stream payloads at all. `false` under a metadata-only ingest.
    pub frames_read: bool,
    /// Provenance keys a verified producer attestation supplied, in the order the attestation lists
    /// them.
    ///
    /// The trust score already counts these as covered — that is what an attestation is for — so a
    /// check that reports one *missing* contradicts the score computed from the same document, in
    /// the same report. The element is not missing; it is claimed by a signed producer rather than
    /// extracted, which `PROVENANCE.ATTESTED` discloses along with the key that signed it.
    pub attested_keys: Vec<String>,
}

impl Default for CheckContext {
    /// A full ingest: frames were read, and nothing was attested.
    fn default() -> Self {
        CheckContext {
            frames_read: true,
            attested_keys: Vec::new(),
        }
    }
}
