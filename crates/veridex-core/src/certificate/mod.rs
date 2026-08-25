//! The trust certificate: deterministic scoring, a content-bound document, and Ed25519 signing.
//!
//! - [`score()`] and [`ProvenanceCoverage`] compute the trust score (design D7).
//! - [`Certificate`] is the portable, content-bound statement (design D9).
//! - [`sign`]/[`verify`] provide Ed25519 signing and offline verification with tamper and
//!   transplant rejection (design D6).

pub mod coverage;
pub mod document;
pub mod render;
pub mod score;
pub mod signing;

pub use coverage::{ProvenanceCoverage, EXPECTED_PROVENANCE_KEYS};
pub use document::{
    Certificate, CriterionResult, FindingsSummary, Issuance, ReadinessReport,
    CERTIFICATE_SCHEMA_VERSION,
};
pub use render::{
    readiness_verdict, render_readiness, render_verified, status_label, verified_json,
};
pub use score::{score, Grade, TrustScore, RUBRIC_VERSION};
pub use signing::{
    sign, verify, CertError, SignedCertificate, SigningKeypair, Verified, Zeroizing,
};
