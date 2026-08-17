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
pub(crate) mod chunks;
pub mod hdf5;
pub mod lerobot;
pub mod mcap;
pub mod mdf4;
pub mod rlds;
pub(crate) mod stats;
pub mod zarr;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::Digest;
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
    Fraction {
        /// Portion of the dataset's episodes to draw, in `(0.0, 1.0]`.
        fraction: f64,
        /// Fixes the draw: the same seed over the same episode set always selects the same episodes.
        seed: u64,
    },
}

impl Sample {
    /// Whether this request asks for less than the whole dataset.
    pub fn is_partial(&self) -> bool {
        !matches!(self, Sample::All)
    }

    /// Reject a request that cannot select a meaningful subset, before any file is read.
    ///
    /// A zero-episode or non-positive-fraction sample would ingest nothing and then report a clean
    /// verdict over it — silence read as a pass, which is exactly what this codebase refuses to do.
    pub fn validate(&self) -> Result<(), IngestError> {
        match self {
            Sample::All => Ok(()),
            Sample::FirstEpisodes(0) => Err(IngestError::InvalidSample {
                reason: "a sample of 0 episodes ingests nothing; ask for at least 1".into(),
            }),
            Sample::FirstEpisodes(_) => Ok(()),
            Sample::Fraction { fraction, .. } if !(fraction.is_finite() && *fraction > 0.0) => {
                Err(IngestError::InvalidSample {
                    reason: format!("sample fraction {fraction} is not a positive finite number"),
                })
            }
            Sample::Fraction { fraction, .. } if *fraction > 1.0 => {
                Err(IngestError::InvalidSample {
                    reason: format!("sample fraction {fraction} is above 1.0 (the whole dataset)"),
                })
            }
            Sample::Fraction { .. } => Ok(()),
        }
    }

    /// A short human-readable description of the request, for reports and certificates.
    pub fn describe(&self) -> String {
        match self {
            Sample::All => "full dataset".into(),
            Sample::FirstEpisodes(n) => format!("first {n} episode(s) by index"),
            Sample::Fraction { fraction, seed } => {
                // Trimmed rather than raw: `0.29 * 100.0` is 28.999999999999996 in binary floating
                // point, and this string is the partial-coverage banner every report prints — and is
                // bound into the verdict hash.
                let pct = fraction * 100.0;
                let pct = if (pct - pct.round()).abs() < 1e-9 {
                    format!("{}", pct.round())
                } else {
                    format!("{pct:.4}")
                        .trim_end_matches('0')
                        .trim_end_matches('.')
                        .to_string()
                };
                format!("{pct}% of episodes (seed {seed})")
            }
        }
    }

    /// Which of `available` episode indices this request selects.
    ///
    /// Deterministic in both arms and independent of the order `available` is discovered in:
    /// `FirstEpisodes` takes the numerically lowest indices, and `Fraction` orders the indices by
    /// `SHA-256(seed, index)` and takes the first `ceil(fraction × n)` — so the same seed over the
    /// same episode set always draws the same episodes, and a positive fraction never draws none.
    pub fn select(&self, available: &BTreeSet<u64>) -> BTreeSet<u64> {
        match self {
            Sample::All => available.clone(),
            Sample::FirstEpisodes(n) => available.iter().copied().take(*n as usize).collect(),
            Sample::Fraction { fraction, seed } => {
                let n = available.len();
                if n == 0 {
                    return BTreeSet::new();
                }
                let want = (((n as f64) * fraction).ceil() as usize).clamp(1, n);
                let mut ordered: Vec<(u64, u64)> = available
                    .iter()
                    .map(|&idx| (draw_key(*seed, idx), idx))
                    .collect();
                // Sort by the draw key, breaking ties on the index so the order is total.
                ordered.sort_unstable();
                ordered.into_iter().take(want).map(|(_, idx)| idx).collect()
            }
        }
    }
}

/// The deterministic draw key for one episode index under one seed: the leading 8 bytes of
/// `SHA-256(seed_le || index_le)`. A hash rather than a linear congruence so adjacent episode
/// indices do not land adjacent in the draw order.
fn draw_key(seed: u64, index: u64) -> u64 {
    let mut hasher = sha2::Sha256::new();
    hasher.update(seed.to_le_bytes());
    hasher.update(index.to_le_bytes());
    let digest = hasher.finalize();
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 yields 32 bytes"))
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

/// The default ceiling on how far a compressed container may expand, as a multiple of the file's own
/// size.
///
/// The frame budget bounds how many frames a file produces; it does not bound how many *bytes* a
/// compressed container unpacks into on the way there. A single MCAP chunk can declare a gigabyte of
/// uncompressed records inside a hundred kilobytes on disk, and one oversized message inside it is
/// one frame — cheap by the frame budget, ruinous in memory. Expansion is bounded relative to the
/// source instead, so the limit scales with genuinely large logs while a decompression bomb (ratios
/// in the thousands) is refused. Real sensor payloads — already-compressed images and point clouds —
/// rarely clear 10x.
pub const DEFAULT_MAX_DECOMPRESSION_RATIO: u64 = 100;

/// The floor under a decompression budget, so a small file still gets a workable allowance.
const DECOMPRESSION_BUDGET_FLOOR_BYTES: u64 = 64 * 1024 * 1024;

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
    /// Ceiling on how far a compressed container may expand, as a multiple of the source's size;
    /// `None` removes the limit. Defaults to [`DEFAULT_MAX_DECOMPRESSION_RATIO`].
    pub max_decompression_ratio: Option<u64>,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            metadata_only: false,
            sample: Sample::default(),
            max_frames: Some(DEFAULT_MAX_FRAMES),
            max_decompression_ratio: Some(DEFAULT_MAX_DECOMPRESSION_RATIO),
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

/// Tracks how many decompressed bytes an ingest has committed to, refusing past the budget.
///
/// The budget is derived from the source's own size (see [`DEFAULT_MAX_DECOMPRESSION_RATIO`]), so it
/// scales with genuinely large logs. Like [`FrameBudget`], adapters charge it on what the file
/// *declares* before unpacking anything, and again on what actually arrives — a header that
/// understates its expansion buys nothing.
#[derive(Debug)]
pub struct DecompressionBudget {
    limit: Option<u64>,
    used: u64,
}

impl DecompressionBudget {
    /// A budget for one ingest of a source of `source_bytes` bytes.
    pub fn new(options: &IngestOptions, source_bytes: u64) -> Self {
        DecompressionBudget {
            limit: options.max_decompression_ratio.map(|ratio| {
                source_bytes
                    .saturating_mul(ratio)
                    .max(DECOMPRESSION_BUDGET_FLOOR_BYTES)
            }),
            used: 0,
        }
    }

    /// Bytes still allowed, or `None` when this budget is unlimited.
    ///
    /// For callers that must bound a *read* as well as charge for it: charging says the expansion
    /// was too large after the fact, and a decompressor pointed at a corrupt stream has already
    /// spent the time and memory by then.
    pub fn remaining(&self) -> Option<u64> {
        self.limit.map(|limit| limit.saturating_sub(self.used))
    }

    /// Charge `n` decompressed bytes, or fail with a clear error naming the budget.
    pub fn take(&mut self, format_id: &'static str, n: u64) -> Result<(), IngestError> {
        self.used = self.used.saturating_add(n);
        match self.limit {
            Some(limit) if self.used > limit => Err(IngestError::DecompressionBudgetExceeded {
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
    /// Only the source's manifest was read: every episode is present, but no stream payload was
    /// touched, so no episode has frames. Structure, declared types, stored statistics, and
    /// provenance are covered; anything that needs the data itself is not.
    MetadataOnly {
        /// Number of episodes the manifest declared and the CDM carries.
        episodes_declared: u64,
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

    /// An explicit `--format` named a format no registered adapter provides.
    #[error("unknown format `{requested}` (supported: {})", .supported.join(", "))]
    UnknownFormat {
        /// The format id the caller asked for.
        requested: String,
        /// The format ids the registry does support.
        supported: Vec<&'static str>,
    },

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

    /// The source's compressed data would expand past the ingest budget.
    #[error("{format_id}: compressed data would expand to {requested} bytes, over the {limit}-byte budget — raise it with --max-decompression-ratio if this file is genuinely this compressible")]
    DecompressionBudgetExceeded {
        /// The recognizing adapter's format id.
        format_id: &'static str,
        /// The budget in force, in bytes.
        limit: u64,
        /// Decompressed bytes the source would have produced.
        requested: u64,
    },

    /// The sampling request itself is meaningless (zero episodes, a non-positive fraction).
    #[error("invalid sample: {reason}")]
    InvalidSample {
        /// What is wrong with the request.
        reason: String,
    },

    /// A sample was requested of a source that cannot honor it.
    #[error("{format_id}: cannot sample this source: {reason}")]
    SamplingUnsupported {
        /// The recognizing adapter's format id.
        format_id: &'static str,
        /// Why the sample cannot be honored.
        reason: String,
    },

    /// An [`IngestOptions`] / [`Source`] combination the implementation does not yet support.
    ///
    /// Refused rather than ignored: an option that silently does nothing is worse than one that is
    /// absent, because the caller believes it took effect.
    #[error("{what} is not implemented — {hint}")]
    NotImplemented {
        /// The option or source kind that was asked for.
        what: &'static str,
        /// What to do instead.
        hint: &'static str,
    },

    /// An I/O error occurred while reading the source.
    #[error("io error: {0}")]
    Io(String),
}

/// Reject an ingest whose options this implementation cannot honor, before any adapter runs.
///
/// [`Source::Remote`] is part of the ingestion spec but not yet built. Accepting it and quietly
/// reading a local file instead would hand back a verdict the caller would read as something it is
/// not. A metadata-only *sample* is refused for a different reason: the two requests describe
/// different partial coverages, and one verdict cannot carry both without one of them being lost.
fn check_options_supported(source: &Source, options: &IngestOptions) -> Result<(), IngestError> {
    if options.metadata_only && options.sample.is_partial() {
        return Err(IngestError::InvalidSample {
            reason: "a metadata-only ingest already reads no stream payloads; combining it with a \
                     sample would report one partial coverage and hide the other — ask for one or \
                     the other"
                .into(),
        });
    }
    if matches!(source, Source::Remote(_)) {
        return Err(IngestError::NotImplemented {
            what: "remote ingestion",
            hint: "fetch the dataset locally and check the path",
        });
    }
    options.sample.validate()
}

/// Refuse a sampling request an adapter cannot honor, naming the format.
///
/// Formats that ingest a recording as a single episode (MCAP, CAN+DBC, MF4) have no episode axis to
/// sample along. Returning the whole recording labelled [`Coverage::Full`] would be correct output
/// for a request that was never honored; saying so is not.
pub fn reject_sampling(
    format_id: &'static str,
    options: &IngestOptions,
) -> Result<(), IngestError> {
    if options.sample.is_partial() {
        return Err(IngestError::SamplingUnsupported {
            format_id,
            reason: "this format ingests a recording as a single episode, so there is no episode \
                     axis to sample along"
                .into(),
        });
    }
    Ok(())
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

    /// Whether this adapter can populate the CDM from the source's manifest alone, without reading
    /// any stream payload ([`IngestOptions::metadata_only`]).
    ///
    /// Defaults to `false`, and the registry refuses the request on that answer — so a format whose
    /// structure lives *inside* its container (MCAP, HDF5, Zarr, RLDS, CAN, MF4) is refused by name
    /// rather than reading everything anyway and labelling it metadata-only. A new adapter is
    /// therefore safe by default: it has to claim the capability to be handed the option.
    fn supports_metadata_only(&self) -> bool {
        false
    }

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
        check_options_supported(source, options)?;
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
            [only] => {
                check_adapter_supports_options(*only, options)?;
                only.ingest(source, options)
            }
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
        check_options_supported(source, options)?;
        match self.adapters.iter().find(|a| a.format_id() == format) {
            Some(adapter) => {
                check_adapter_supports_options(adapter.as_ref(), options)?;
                adapter.ingest(source, options)
            }
            // Names the value that was wrong. `UnsupportedFormat` says "no adapter recognized the
            // source", which is a statement about the file — and the file is usually fine; it is
            // `--format nope` that is not.
            None => Err(IngestError::UnknownFormat {
                requested: format.to_string(),
                supported: self.supported_formats(),
            }),
        }
    }
}

/// Refuse an option the *recognizing* adapter cannot honor, before it is handed the source.
///
/// Enforced here rather than inside each adapter so the refusal cannot be forgotten: an adapter that
/// does not claim [`Adapter::supports_metadata_only`] never sees the option set.
fn check_adapter_supports_options(
    adapter: &dyn Adapter,
    options: &IngestOptions,
) -> Result<(), IngestError> {
    if options.metadata_only && !adapter.supports_metadata_only() {
        return Err(IngestError::NotImplemented {
            what: "metadata-only ingestion for this format",
            hint:
                "this format keeps its structure inside the container, so there is no manifest to \
                   read on its own — drop --metadata-only, or sample the dataset instead",
        });
    }
    Ok(())
}

/// Reject a local source whose path does not exist before format detection, so a mistyped path
/// yields a clear [`IngestError::SourceNotFound`] instead of a misleading "unsupported format".
/// Remote sources are not filesystem paths and are left to the adapter.
fn check_source_exists(source: &Source) -> Result<(), IngestError> {
    if let Source::Local(path) = source {
        if !path.exists() {
            // A URL reaching this function is a *remote* source the front-end wrapped as a local
            // path, and "no such file or directory: https://…" reads as a typo in the URL rather
            // than as the feature not existing. Both front-ends build `Source::Local` from whatever
            // string they were handed, so the distinction is drawn here.
            if let Some(text) = path.to_str() {
                if looks_remote(text) {
                    return Err(IngestError::NotImplemented {
                        what: "remote ingestion",
                        hint: "fetch the dataset locally and check the path",
                    });
                }
            }
            return Err(IngestError::SourceNotFound(path.clone()));
        }
    }
    Ok(())
}

/// The dataset id to take from a source path: the name of the directory or file it really names.
///
/// [`Path::file_name`] answers about the path *as written*, and returns `None` for one ending in a
/// `.` or `..` component. So `veridex check .` run from inside a dataset fell through to the
/// adapter's fallback string — `"lerobot"`, `"zarr"` — while `veridex check /path/to/that/dataset`
/// took the directory name. The id is bound into the CDM content hash, so the same bytes hashed two
/// different ways depending on how the user spelled the path, and a `verify` run from inside the
/// dataset failed a genuine certificate with a content-hash mismatch. Resolving the path first makes
/// the id a property of what is on disk rather than of what was typed, which is what the
/// determinism contract promises.
pub(crate) fn dataset_id_from_path(path: &Path, fallback: &str) -> String {
    let resolved = path.canonicalize().ok();
    resolved
        .as_deref()
        .and_then(Path::file_name)
        // A path that cannot be resolved (it was deleted between detection and ingest, or the
        // process cannot traverse to it) still answers for itself where it can.
        .or_else(|| path.file_name())
        .and_then(|s| s.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| fallback.to_owned())
}

/// Whether a source string names somewhere other than this filesystem.
fn looks_remote(text: &str) -> bool {
    const REMOTE_SCHEMES: &[&str] = &["http://", "https://", "s3://", "gs://", "hf://"];
    REMOTE_SCHEMES.iter().any(|s| text.starts_with(s))
}

/// A registry preloaded with the standard adapters: LeRobot v3, MCAP, CAN+DBC, ASAM MF4, RLDS/TFDS,
/// HDF5, and Zarr.
pub fn default_registry() -> AdapterRegistry {
    let mut reg = AdapterRegistry::new();
    reg.register(Box::new(lerobot::LeRobotAdapter));
    reg.register(Box::new(mcap::McapAdapter));
    reg.register(Box::new(candbc::CanDbcAdapter));
    reg.register(Box::new(mdf4::Mdf4Adapter));
    reg.register(Box::new(rlds::RldsAdapter));
    reg.register(Box::new(hdf5::Hdf5Adapter));
    reg.register(Box::new(zarr::ZarrAdapter));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(ratio: Option<u64>) -> IngestOptions {
        IngestOptions {
            max_decompression_ratio: ratio,
            ..IngestOptions::default()
        }
    }

    #[test]
    fn the_decompression_budget_scales_with_the_source() {
        // A 1 GB log gets a proportionate allowance: 100x its own size.
        let big = 1_000_000_000u64;
        let mut budget = DecompressionBudget::new(&options(Some(100)), big);
        assert!(budget.take("mcap", big * 100).is_ok());
        assert!(budget.take("mcap", 1).is_err());
    }

    #[test]
    fn a_small_file_still_gets_the_floor() {
        // 100 KB x 100 is under the floor, so the floor is what applies — a small honest file is
        // never squeezed, and a small hostile one is still bounded.
        let mut budget = DecompressionBudget::new(&options(Some(100)), 100_000);
        assert!(budget
            .take("mcap", DECOMPRESSION_BUDGET_FLOOR_BYTES)
            .is_ok());
        assert!(budget.take("mcap", 1).is_err());
    }

    #[test]
    fn charges_accumulate_and_name_the_budget() {
        let mut budget = DecompressionBudget::new(&options(Some(1)), 1_000_000_000);
        assert!(budget.take("mcap", 600_000_000).is_ok());
        match budget.take("mcap", 600_000_000) {
            Err(IngestError::DecompressionBudgetExceeded {
                format_id,
                limit,
                requested,
            }) => {
                assert_eq!(format_id, "mcap");
                assert_eq!(limit, 1_000_000_000);
                assert_eq!(requested, 1_200_000_000, "charges accumulate across calls");
            }
            other => panic!("expected a decompression-budget error, got {other:?}"),
        }
    }

    #[test]
    fn no_ratio_means_no_ceiling() {
        let mut budget = DecompressionBudget::new(&options(None), 1);
        assert!(budget.take("mcap", u64::MAX).is_ok());
    }

    #[test]
    fn an_enormous_ratio_saturates_rather_than_wrapping() {
        // A ratio that would overflow the multiply must not wrap to a tiny limit.
        let mut budget = DecompressionBudget::new(&options(Some(u64::MAX)), u64::MAX);
        assert!(budget.take("mcap", u64::MAX - 1).is_ok());
    }
}
