//! The trust certificate: scoring now, signed content to follow.
//!
//! This module currently provides the deterministic v1 [`score`]ing rubric and [`ProvenanceCoverage`].
//! The signed, content-bound certificate document (COSE/JWS, offline `verify`) builds on top of
//! these in a later change.

pub mod coverage;
pub mod score;

pub use coverage::{ProvenanceCoverage, EXPECTED_PROVENANCE_KEYS};
pub use score::{score, Grade, TrustScore, RUBRIC_VERSION};
