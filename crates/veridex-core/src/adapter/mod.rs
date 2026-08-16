//! The adapter contract: how a source format becomes a [`Dataset`](crate::cdm::Dataset).
//!
//! An adapter's *only* job is to populate the CDM faithfully (design D2). Adding a format later is
//! "write an [`Adapter`]," never "touch the engine or the checks." Two rules from the ingestion
//! spec are baked into this contract:
//!
//! - **Self-declaration / fidelity.** Every ingest returns an [`IngestReport`] recording which
//!   source fields were mapped, which existed but the CDM cannot represent (`unmapped`), and which
//!   the source omitted — so reporting can disclose the certificate's coverage limits. Ingestion
//!   never silently drops information that could affect a verdict.
//! - **Clear rejection.** An [`AdapterRegistry`] that recognizes no adapter for a source returns
//!   [`IngestError::UnsupportedFormat`], which lists the formats that *are* supported, rather than
//!   partially parsing.

pub mod candbc;
pub(crate) mod cdr;
pub mod lerobot;
pub mod mcap;
pub mod mdf4;

use std::path::PathBuf;

use thiserror::Error;

use crate::cdm::Dataset;

/// Where a dataset lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A local filesystem path (a file or a dataset directory).
    Local(PathBuf),
    /// A remote dataset identifier (e.g. a Hugging Face Hub repo id), used with
    /// [`IngestOptions::metadata_only`] for structural checks without a full download.
    Remote(String),
}

/// How much of the dataset to ingest.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Sample {
    /// The whole dataset.
    #[default]
    All,
    /// The first N episodes (by episode index).
    FirstEpisodes(u64),
    /// A deterministic pseudo-random subset. `fraction` in `(0.0, 1.0]`; `seed` fixes the draw so
    /// the same request always selects the same episodes.
    Fraction { fraction: f64, seed: u64 },
}

/// The default ceiling on frames a single ingest will materialize.
///
/// Every adapter builds *streams × samples* frames, and both factors come from the file: a CAN log's
/// signals-per-id against its frame count, an MF4 group's channels against its records, a LeRobot
/// `info.json`'s declared features against its rows. That product is quadratic in attacker-controlled
/// input — 344 KB of crafted CAN measured 6.4M frames and 900 MB — so ingestion refuses past a budget
/// rather than being OOM-killed inside someone's CI gate. Chosen well above real datasets: a one-hour
/// ten-sensor rig at 100 Hz is 3.6M frames.
pub const DEFAULT_MAX_FRAMES: u64 = 20_000_000;

/// Options controlling an ingest.
#[derive(Debug, Clone)]
pub struct IngestOptions {
    /// Ingest structure/metadata only; do not fetch or index stream payloads. Required for the
    /// remote structural-check path.
    pub metadata_only: bool,
    /// Which episodes to ingest.
    pub sample: Sample,
    /// Ceiling on the frames this ingest may materialize; `None` removes the limit. Defaults to
    /// [`DEFAULT_MAX_FRAMES`].
    pub max_frames: Option<u64>,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            metadata_only: false,
            sample: Sample::default(),
            max_frames: Some(DEFAULT_MAX_FRAMES),
        }
    }
}

/// Tracks how many frames an ingest has committed to, refusing past the budget.
///
/// Adapters charge the budget *before* allocating, so a hostile file is rejected on the strength of
/// what it declares rather than after the memory is already gone.
#[derive(Debug)]
pub struct FrameBudget {
    limit: Option<u64>,
    used: u64,
}

impl FrameBudget {
    /// A budget for one ingest.
    pub fn new(options: &IngestOptions) -> Self {
        FrameBudget {
            limit: options.max_frames,
            used: 0,
        }
    }

    /// Charge `n` frames, or fail with a clear error naming the budget.
    pub fn take(&mut self, format_id: &'static str, n: u64) -> Result<(), IngestError> {
        self.used = self.used.saturating_add(n);
        match self.limit {
            Some(limit) if self.used > limit => Err(IngestError::FrameBudgetExceeded {
                format_id,
                limit,
                requested: self.used,
            }),
            _ => Ok(()),
        }
    }
}

/// What coverage an ingest actually achieved — recorded so verdicts and certificates can state
/// whether they saw the whole dataset or only a sample.
#[derive(Debug, Clone, PartialEq)]
pub enum Coverage {
    /// Every episode was ingested.
    Full,
    /// Only a sample was ingested.
    Sample {
        /// The sampling request that was honored.
        sample: Sample,
        /// Number of episodes actually ingested.
        episodes_ingested: u64,
    },
}

/// A source field that existed but the CDM cannot represent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedField {
    /// Where the field lived in the source (a dotted/pathy locator).
    pub source_path: String,
    /// Why it could not be mapped.
    pub note: String,
}

/// An adapter's honest account of what it did, per the ingestion fidelity requirement.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestReport {
    /// The adapter's format id (e.g. `lerobot`, `mcap`).
    pub format_id: &'static str,
    /// The concrete source version detected, if the format encodes one.
    pub source_version: Option<String>,
    /// Coverage achieved.
    pub coverage: Coverage,
    /// Source fields that were mapped into the CDM (dotted locators).
    pub mapped_fields: Vec<String>,
    /// Source fields that existed but the CDM cannot represent.
    pub unmapped_fields: Vec<UnmappedField>,
    /// Fields the source did not provide at all.
    pub omitted_fields: Vec<String>,
}

/// The result of a successful ingest: the CDM plus the fidelity report.
#[derive(Debug, Clone, PartialEq)]
pub struct Ingested {
    /// The populated Canonical Dataset Model.
    pub dataset: Dataset,
    /// How faithfully it was populated.
    pub report: IngestReport,
}

/// Whether an adapter recognizes a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// This adapter can ingest the source; `version` is the detected format version if known.
    Yes { version: Option<String> },
    /// This adapter does not handle the source.
    No,
}

/// Errors an ingest can produce.
#[derive(Debug, Error)]
pub enum IngestError {
    /// The local source path does not exist. Distinct from [`IngestError::UnsupportedFormat`] so a
    /// mistyped path is not misreported as an unrecognized format.
    #[error("no such file or directory: {0}")]
    SourceNotFound(PathBuf),

    /// No registered adapter recognized the source.
    #[error("unsupported format: no adapter recognized the source (supported: {})", .supported.join(", "))]
    UnsupportedFormat {
        /// The format ids the registry does support.
        supported: Vec<&'static str>,
    },

    /// The format was recognized but its concrete version is not supported by the adapter.
    #[error("{format_id}: unsupported version {version:?} (supported: {})", .supported.join(", "))]
    UnsupportedVersion {
        /// The recognizing adapter's format id.
        format_id: &'static str,
        /// The detected version.
        version: Option<String>,
        /// Versions the adapter supports.
        supported: &'static [&'static str],
    },

    /// The source was recognized but could not be parsed.
    #[error("{format_id}: parse error: {message}")]
    Parse {
        /// The recognizing adapter's format id.
        format_id: &'static str,
        /// A human-readable description.
        message: String,
    },

    /// More than one adapter recognized the source and no `--format` override was given.
    #[error("ambiguous format: source matches {} adapters ({}); specify one with --format", .candidates.len(), .candidates.join(", "))]
    AmbiguousFormat {
        /// The format ids that all recognized the source.
        candidates: Vec<&'static str>,
    },

    /// The source declares more frames than the ingest budget allows.
    #[error("{format_id}: dataset would materialize {requested} frames, over the {limit} budget — raise it with --max-frames if this dataset is genuinely this large")]
    FrameBudgetExceeded {
        /// The recognizing adapter's format id.
        format_id: &'static str,
        /// The budget in force.
        limit: u64,
        /// Frames the source would have produced.
        requested: u64,
    },

    /// An I/O error occurred while reading the source.
    #[error("io error: {0}")]
    Io(String),
}

/// The contract every format adapter implements.
///
/// Object-safe: adapters are stored as `Box<dyn Adapter>` in an [`AdapterRegistry`].
pub trait Adapter: Send + Sync {
    /// A stable, lowercase format identifier (e.g. `lerobot`, `mcap`).
    fn format_id(&self) -> &'static str;

    /// The concrete source versions this adapter supports (e.g. `["3.0"]`).
    fn supported_versions(&self) -> &'static [&'static str];

    /// Cheaply decide whether this adapter handles `source`, without a full parse.
    fn detect(&self, source: &Source) -> Detection;

    /// Ingest `source` into the CDM, honoring `options` and recording fidelity.
    ///
    /// Called only after [`Adapter::detect`] returned [`Detection::Yes`]. The adapter is
    /// responsible for rejecting a recognized-but-unsupported version with
    /// [`IngestError::UnsupportedVersion`].
    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError>;
}

/// A set of adapters, tried in registration order.
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: Vec<Box<dyn Adapter>>,
}

impl AdapterRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an adapter. Adapters are consulted in registration order.
    pub fn register(&mut self, adapter: Box<dyn Adapter>) -> &mut Self {
        self.adapters.push(adapter);
        self
    }

    /// The format ids this registry can ingest.
    pub fn supported_formats(&self) -> Vec<&'static str> {
        self.adapters.iter().map(|a| a.format_id()).collect()
    }

    /// Ingest `source` by autodetection.
    ///
    /// Returns [`IngestError::UnsupportedFormat`] (listing supported formats) when nothing
    /// recognizes the source, and [`IngestError::AmbiguousFormat`] when more than one adapter does —
    /// never silently guessing. Use [`AdapterRegistry::ingest_as`] to force a format.
    pub fn ingest(
        &self,
        source: &Source,
        options: &IngestOptions,
    ) -> Result<Ingested, IngestError> {
        check_source_exists(source)?;
        let matches: Vec<&dyn Adapter> = self
            .adapters
            .iter()
            .filter(|a| matches!(a.detect(source), Detection::Yes { .. }))
            .map(|a| a.as_ref())
            .collect();
        match matches.as_slice() {
            [] => Err(IngestError::UnsupportedFormat {
                supported: self.supported_formats(),
            }),
            [only] => only.ingest(source, options),
            many => Err(IngestError::AmbiguousFormat {
                candidates: many.iter().map(|a| a.format_id()).collect(),
            }),
        }
    }

    /// Ingest `source` with the adapter whose `format_id` matches `format`, bypassing autodetection.
    ///
    /// Returns [`IngestError::UnsupportedFormat`] when no registered adapter has that id.
    pub fn ingest_as(
        &self,
        format: &str,
        source: &Source,
        options: &IngestOptions,
    ) -> Result<Ingested, IngestError> {
        check_source_exists(source)?;
        match self.adapters.iter().find(|a| a.format_id() == format) {
            Some(adapter) => adapter.ingest(source, options),
            None => Err(IngestError::UnsupportedFormat {
                supported: self.supported_formats(),
            }),
        }
    }
}

/// Reject a local source whose path does not exist before format detection, so a mistyped path
/// yields a clear [`IngestError::SourceNotFound`] instead of a misleading "unsupported format".
/// Remote sources are not filesystem paths and are left to the adapter.
fn check_source_exists(source: &Source) -> Result<(), IngestError> {
    if let Source::Local(path) = source {
        if !path.exists() {
            return Err(IngestError::SourceNotFound(path.clone()));
        }
    }
    Ok(())
}

/// A registry preloaded with the standard adapters: LeRobot v3, MCAP, CAN+DBC, and ASAM MF4.
pub fn default_registry() -> AdapterRegistry {
    let mut reg = AdapterRegistry::new();
    reg.register(Box::new(lerobot::LeRobotAdapter));
    reg.register(Box::new(mcap::McapAdapter));
    reg.register(Box::new(candbc::CanDbcAdapter));
    reg.register(Box::new(mdf4::Mdf4Adapter));
    reg
}
