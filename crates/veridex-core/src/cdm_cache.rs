//! Validating a dataset again without reading it again.
//!
//! Nearly all of a run's cost is the ingest: a terabyte of shards becomes a few megabytes of CDM,
//! and then the check loop takes milliseconds. So re-running the catalog over a dataset — because
//! Veridex was upgraded, because a threshold moved, because a profile is being tried — paid for the
//! read every time, and on a large corpus that is the difference between a question a team asks and
//! one it does not.
//!
//! The cache is the **CDM itself**, saved beside the first full run. A later run loads it and
//! validates it, and the verdict is identical to a full run over those bytes *by construction*:
//! there is nothing left of a full run to differ, because the CDM is the whole of what a full run
//! computed from the dataset, and the verdict is a function of it plus the configuration. That is
//! the honest form of incremental validation here — not "skip the episodes that did not change",
//! which cannot be both cheaper than reading them and identical to having read them.
//!
//! Three things keep it from becoming a way to launder a verdict.
//!
//! The file carries the **encoding version** the hash was computed under, and a cache written under
//! a different one is refused rather than validated: a content hash only means something within one
//! encoding, and a verdict bound to a hash nobody can reproduce is worse than no verdict.
//!
//! The file carries its own **content hash**, and loading recomputes it from the CDM in the file. An
//! edited cache — an episode removed, a value softened — does not load. The hash is the same one the
//! verdict and any certificate are bound to, so this is the same check `verify` makes, made earlier.
//!
//! And a run from a cache **says so**, in a finding that reaches every renderer: `COVERAGE.CACHED_CDM`
//! names the version that built the CDM and the hash it is bound to. The verdict is a true statement
//! about that hash; whether that hash is the dataset on this disk is what `veridex verify` answers,
//! and nothing here pretends to have asked.

use serde::{Deserialize, Serialize};

use crate::adapter::{Coverage, IngestReport, Ingested, UnmappedField};
use crate::canonical::CANONICAL_VERSION;
use crate::cdm::Dataset;
use crate::check::{Category, Finding, Location, Severity};
use crate::content_hash;
use crate::engine::COVERAGE_CHECK_ID;

/// The schema this cache is written under.
pub const CDM_CACHE_SCHEMA_VERSION: &str = "veridex.cdm/1";

/// A dataset's Canonical Dataset Model, saved so a later run can validate it without re-reading the
/// dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CdmCache {
    /// Schema id.
    pub schema: String,
    /// The `veridex-core` version that built this CDM.
    pub veridex_version: String,
    /// The CDM canonical encoding version the hash below was computed under. A cache from another
    /// one is refused: a hash is only comparable within one encoding.
    pub cdm_encoding_version: u32,
    /// The CDM content hash, recomputed and compared on load.
    pub cdm_content_hash: String,
    /// The adapter that read the dataset, for the report.
    pub format_id: String,
    /// What the ingest covered, so a cached run is exactly as partial as the run that saved it —
    /// the adapter's own answer, not the verdict's rendering of it, so the round trip is lossless.
    pub coverage: Coverage,
    /// Source data the ingest declined to read, so `COVERAGE.SOURCE_UNREAD` survives the round trip.
    pub unread_sources: Vec<UnmappedField>,
    /// The CDM.
    pub dataset: Dataset,
}

/// Why a cache could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// The file is not a CDM cache.
    NotACache(String),
    /// The cache was written under a different canonical encoding, so its hash means something
    /// else.
    EncodingChanged {
        /// The encoding the file was written under.
        found: u32,
        /// The encoding this build computes.
        expected: u32,
    },
    /// The CDM in the file does not hash to the hash the file states.
    HashMismatch {
        /// The hash the file claims.
        claimed: String,
        /// The hash the CDM in it actually produces.
        computed: String,
    },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CacheError::NotACache(why) => write!(
                f,
                "this file is not a Veridex CDM cache ({why}) — write one with `veridex check \
                 <dataset> --emit-cdm <file>`"
            ),
            CacheError::EncodingChanged { found, expected } => write!(
                f,
                "this cache was written under CDM encoding version {found} and this build computes \
                 {expected}; a content hash is only comparable within one encoding, so the cache \
                 cannot be validated — re-read the dataset once to write a new one"
            ),
            CacheError::HashMismatch { claimed, computed } => write!(
                f,
                "this cache claims CDM hash {claimed} and the model in it hashes to {computed} — \
                 the file has been altered since it was written, and a verdict over it would be \
                 bound to a hash nothing produced"
            ),
        }
    }
}

impl std::error::Error for CacheError {}

/// Save an ingested dataset's CDM so a later run can validate it without re-reading the dataset.
pub fn save(ingested: &Ingested) -> CdmCache {
    // Canonicalized before hashing, exactly as the pipeline does before validating, so the hash in
    // the file is the one a full run over the same bytes carries.
    let mut dataset = ingested.dataset.clone();
    dataset.canonicalize_order();
    CdmCache {
        schema: CDM_CACHE_SCHEMA_VERSION.to_string(),
        veridex_version: crate::VERSION.to_string(),
        cdm_encoding_version: CANONICAL_VERSION,
        cdm_content_hash: content_hash(&dataset).to_hex(),
        format_id: ingested.report.format_id.to_string(),
        coverage: ingested.report.coverage.clone(),
        unread_sources: ingested.report.unread_sources.clone(),
        dataset,
    }
}

/// Render a cache as the JSON written to disk.
pub fn render(cache: &CdmCache) -> String {
    serde_json::to_string_pretty(cache).unwrap_or_else(|_| "{}".to_string())
}

/// Read a cache back, refusing one this build cannot honestly validate.
pub fn load(text: &str) -> Result<CdmCache, CacheError> {
    let cache: CdmCache =
        serde_json::from_str(text).map_err(|e| CacheError::NotACache(e.to_string()))?;
    if cache.schema != CDM_CACHE_SCHEMA_VERSION {
        return Err(CacheError::NotACache(format!(
            "schema `{}`, expected `{CDM_CACHE_SCHEMA_VERSION}`",
            cache.schema
        )));
    }
    if cache.cdm_encoding_version != CANONICAL_VERSION {
        return Err(CacheError::EncodingChanged {
            found: cache.cdm_encoding_version,
            expected: CANONICAL_VERSION,
        });
    }
    // The one check that makes the file trustworthy as input: the model in it hashes to the hash it
    // states. An edited cache does not load.
    let computed = content_hash(&cache.dataset).to_hex();
    if !computed.eq_ignore_ascii_case(&cache.cdm_content_hash) {
        return Err(CacheError::HashMismatch {
            claimed: cache.cdm_content_hash.clone(),
            computed,
        });
    }
    Ok(cache)
}

/// Turn a loaded cache back into an [`Ingested`], so the ordinary pipeline validates it.
///
/// The verdict is then produced by the same code a full run uses, over the same CDM, with the same
/// coverage and the same unread disclosures — which is why it is identical rather than merely
/// similar.
pub fn as_ingested(cache: CdmCache) -> Ingested {
    Ingested {
        dataset: cache.dataset,
        report: IngestReport {
            // Leaked for the life of the process: a cache names its format at runtime and the
            // report holds a `&'static str`, and one small leak per run is cheaper than making
            // every adapter's format id an owned string.
            format_id: Box::leak(cache.format_id.into_boxed_str()),
            source_version: None,
            coverage: cache.coverage,
            mapped_fields: vec!["cached CDM -> dataset (no source was read)".into()],
            unmapped_fields: Vec::new(),
            unread_sources: cache.unread_sources,
            omitted_fields: vec![
                "everything a source read would add: this run validated a saved CDM and opened no \
                 dataset"
                    .into(),
            ],
        },
    }
}

/// The finding a run from a cache discloses about itself.
///
/// Info, not a warning: for the hash it names, this verdict is what a full run produces — the
/// reasoning in this module's header. What it cannot say is whether that hash is the dataset on this
/// disk, and the finding points at the command that answers that.
pub fn cached_finding(cache_version: &str, cdm_hash: &str) -> Finding {
    Finding::new(
        COVERAGE_CHECK_ID,
        Category::Structural,
        Severity::Info,
        Location::Dataset,
        "COVERAGE.CACHED_CDM",
        format!(
            "this run validated a saved CDM (written by veridex {cache_version}, bound to \
             {cdm_hash}) and opened no dataset"
        ),
    )
    .with_risk(
        "For that content hash this verdict is what a full read produces — the model is the whole \
         of what a read computes, and the verdict is a function of it. What this run did not do is \
         establish that the hash is the dataset you have now: a cache is a statement about bytes \
         that were read once, somewhere.",
    )
    .with_remedy(
        "Where that matters, re-read the dataset (`veridex check <dataset>`), or bind a certificate \
         to it and let `veridex verify <dataset> --certificate …` compare the hashes for you.",
    )
}
