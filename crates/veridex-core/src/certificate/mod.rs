//! The trust certificate: deterministic scoring, a content-bound document, and Ed25519 signing.
//!
//! - [`score`] and [`ProvenanceCoverage`] compute the trust score (design D7).
//! - [`Certificate`] is the portable, content-bound statement (design D9).
//! - [`sign`]/[`verify`] provide Ed25519 signing and offline verification with tamper and
//!   transplant rejection (design D6).

pub mod coverage;
pub mod document;
pub mod score;
pub mod signing;

pub use coverage::{ProvenanceCoverage, EXPECTED_PROVENANCE_KEYS};
pub use document::{Certificate, FindingsSummary, Issuance, CERTIFICATE_SCHEMA_VERSION};
pub use score::{score, Grade, TrustScore, RUBRIC_VERSION};
pub use signing::{sign, verify, CertError, SignedCertificate, SigningKeypair, Verified};
