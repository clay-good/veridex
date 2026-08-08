//! The certificate document: the portable, content-bound statement about a dataset.
//!
//! A certificate is a *nutrition label*, not a seal of approval — it records what was checked, what
//! was found, the trust score, and provenance coverage, and binds to the exact CDM content hash so
//! it cannot be transplanted onto different data. It is signed separately (see
//! [`signing`](super::signing)); this module builds the unsigned content.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::certificate::coverage::ProvenanceCoverage;
use crate::certificate::score::TrustScore;
use crate::check::Category;
use crate::engine::{EffectiveConfig, ExecutedCheck, SeverityCounts, Status, Verdict};

/// The certificate schema id. Stable and additive within a major version.
pub const CERTIFICATE_SCHEMA_VERSION: &str = "veridex.certificate/1";

const ALL_CATEGORIES: &[Category] = &[
    Category::Structural,
    Category::Temporal,
    Category::Statistical,
    Category::Semantic,
    Category::Video,
    Category::Provenance,
];

/// Caller-supplied issuance metadata. Timestamps are caller-supplied so signing stays reproducible
/// and testable — the core never reads a wall clock (design D6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issuance {
    /// Issuer key identifier (e.g. the public key hex).
    pub key_id: String,
    /// Issuance timestamp, supplied by the caller (e.g. RFC 3339).
    pub timestamp: String,
}

/// Findings rolled up by severity and by category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsSummary {
    /// Counts by severity.
    pub by_severity: SeverityCounts,
    /// Counts by category (only categories with findings appear).
    pub by_category: BTreeMap<String, u64>,
}

/// The unsigned certificate content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Certificate {
    /// Schema id.
    pub schema: String,
    /// Dataset identity.
    pub dataset_id: String,
    /// The CDM content hash this certificate is bound to (hex).
    pub cdm_content_hash: String,
    /// The `veridex-core` version that produced the verdict.
    pub veridex_version: String,
    /// Overall verdict status.
    pub status: Status,
    /// Effective configuration used for the run.
    pub effective_config: EffectiveConfig,
    /// Checks executed, with versions.
    pub checks_run: Vec<ExecutedCheck>,
    /// Check categories that did not run (so a reader knows the coverage limits).
    pub categories_skipped: Vec<Category>,
    /// Findings rolled up.
    pub findings_summary: FindingsSummary,
    /// Trust score and grade.
    pub trust_score: TrustScore,
    /// Provenance coverage (known/asserted/unknown).
    pub provenance_coverage: ProvenanceCoverage,
    /// Caller-supplied issuance metadata.
    pub issuance: Issuance,
}

impl Certificate {
    /// Build a certificate from a verdict, its trust score, provenance coverage, and issuance
    /// metadata. The verdict's `cdm_content_hash` becomes the binding.
    pub fn build(
        dataset_id: impl Into<String>,
        verdict: &Verdict,
        trust_score: TrustScore,
        provenance_coverage: ProvenanceCoverage,
        issuance: Issuance,
    ) -> Certificate {
        let mut by_category: BTreeMap<String, u64> = BTreeMap::new();
        for f in &verdict.findings {
            *by_category
                .entry(category_tag(f.category).to_string())
                .or_insert(0) += 1;
        }

        let ran: std::collections::BTreeSet<Category> =
            verdict.checks_categories().into_iter().collect();
        let categories_skipped: Vec<Category> = ALL_CATEGORIES
            .iter()
            .copied()
            .filter(|c| !ran.contains(c))
            .collect();

        Certificate {
            schema: CERTIFICATE_SCHEMA_VERSION.to_string(),
            dataset_id: dataset_id.into(),
            cdm_content_hash: verdict.cdm_content_hash.clone(),
            veridex_version: verdict.veridex_version.clone(),
            status: verdict.status,
            effective_config: verdict.effective_config.clone(),
            checks_run: verdict.executed_checks.clone(),
            categories_skipped,
            findings_summary: FindingsSummary {
                by_severity: verdict.counts,
                by_category,
            },
            trust_score,
            provenance_coverage,
            issuance,
        }
    }
}

fn category_tag(c: Category) -> &'static str {
    match c {
        Category::Structural => "structural",
        Category::Temporal => "temporal",
        Category::Statistical => "statistical",
        Category::Semantic => "semantic",
        Category::Video => "video",
        Category::Provenance => "provenance",
    }
}
