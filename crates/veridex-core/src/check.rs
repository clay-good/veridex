//! Check types: the vocabulary the [validation engine](crate::engine) runs and aggregates.
//!
//! The engine knows nothing about specific checks; a [`Check`] declares its identity and scope and
//! inspects a [`Dataset`](crate::cdm::Dataset), emitting [`Finding`]s. Concrete checks live in
//! `checks-catalog` (implemented incrementally).

use serde::Serialize;

use crate::cdm::Dataset;

/// Finding severity. Ordered `Info < Warning < Error`; the maximum severity present drives the
/// verdict [`Status`](crate::engine::Status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Informational; never fails a run.
    Info,
    /// A problem worth surfacing but not disqualifying.
    Warning,
    /// A disqualifying problem.
    Error,
}

/// The category a check belongs to (exactly one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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
    /// Stable machine-readable code (e.g. `EPISODE.EMPTY`).
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

impl Finding {
    /// Convenience constructor; severity defaults are set by the check and may be overridden by the
    /// engine before the finding lands in the verdict.
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
        }
    }
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
    /// Inspect the dataset and emit findings. Findings should use [`Check::default_severity`]
    /// unless the check intentionally varies severity by finding.
    fn run(&self, dataset: &Dataset) -> Vec<Finding>;
}
