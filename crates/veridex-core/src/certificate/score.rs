//! The v1 trust-scoring rubric (design D7): a documented, versioned, deterministic function from a
//! [`Verdict`] and [`ProvenanceCoverage`] to a 0–100 score and an A–F grade.
//!
//! Deliberately boring and integer-only so it is exactly reproducible. The full rubric text ships
//! as `docs/rubric-v1.md`. Scores are only comparable **within the same `rubric_version`**.

use serde::Serialize;

use crate::check::{Category, Severity};
use crate::engine::Verdict;

use super::coverage::ProvenanceCoverage;

/// The rubric version. Bump on any scoring change; scores across versions are not comparable.
pub const RUBRIC_VERSION: &str = "v1";

// Per-finding deductions from the data-quality sub-score (integer points).
const ERROR_PENALTY: i32 = 15;
const WARNING_PENALTY: i32 = 4;
/// A check that failed to run is a coverage gap, not a clean result.
const ERRORED_CHECK_PENALTY: i32 = 10;

// Weighting of the two axes (out of 10): data quality 70%, provenance coverage 30%. This caps a
// dataset with zero provenance at 70 no matter how clean its data — provenance can't be masked.
const DATA_WEIGHT: u32 = 7;
const PROVENANCE_WEIGHT: u32 = 3;

/// A letter grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Grade {
    /// 90–100.
    A,
    /// 80–89.
    B,
    /// 70–79.
    C,
    /// 60–69.
    D,
    /// below 60.
    F,
}

impl Grade {
    /// The band for an overall score.
    pub fn from_score(score: u8) -> Grade {
        match score {
            90..=100 => Grade::A,
            80..=89 => Grade::B,
            70..=79 => Grade::C,
            60..=69 => Grade::D,
            _ => Grade::F,
        }
    }

    /// The single-letter label.
    pub fn letter(self) -> char {
        match self {
            Grade::A => 'A',
            Grade::B => 'B',
            Grade::C => 'C',
            Grade::D => 'D',
            Grade::F => 'F',
        }
    }
}

/// The computed trust score, with both sub-scores exposed so the certificate can show its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TrustScore {
    /// Overall 0–100 score.
    pub score: u8,
    /// Letter grade for `score`.
    pub grade: Grade,
    /// The rubric version used.
    pub rubric_version: &'static str,
    /// Data-quality sub-score (0–100), before provenance weighting.
    pub data_score: u8,
    /// Provenance coverage percentage (0–100).
    pub provenance_pct: u8,
}

/// Compute the v1 trust score from a verdict and provenance coverage.
///
/// Data quality: start at 100, deduct per non-provenance finding by severity and per errored check,
/// floor at 0. Provenance: the percentage of expected elements that are known or asserted. Overall:
/// a 70/30 weighting of the two, rounded down.
pub fn score(verdict: &Verdict, coverage: &ProvenanceCoverage) -> TrustScore {
    let mut errors = 0i32;
    let mut warnings = 0i32;
    for f in &verdict.findings {
        // Provenance coverage is scored on its own axis, not as data-quality deductions.
        if f.category == Category::Provenance {
            continue;
        }
        match f.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            Severity::Info => {}
        }
    }
    let errored = verdict.errored_checks.len() as i32;

    let data_score = (100
        - errors * ERROR_PENALTY
        - warnings * WARNING_PENALTY
        - errored * ERRORED_CHECK_PENALTY)
        .clamp(0, 100) as u32;

    let provenance_pct = coverage.covered_pct();

    let overall = (data_score * DATA_WEIGHT + provenance_pct * PROVENANCE_WEIGHT)
        / (DATA_WEIGHT + PROVENANCE_WEIGHT);
    let score = overall as u8;

    TrustScore {
        score,
        grade: Grade::from_score(score),
        rubric_version: RUBRIC_VERSION,
        data_score: data_score as u8,
        provenance_pct: provenance_pct as u8,
    }
}
