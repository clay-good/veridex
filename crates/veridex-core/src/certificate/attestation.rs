//! Producer attestation: provenance a producer signs for, bound to the data it is about.
//!
//! Most of what provenance means is not in the file. A LeRobot Parquet does not record who operated
//! the robot, an MCAP does not record which calibration was in force, and no format records the
//! upstream dataset a merge drew from. Veridex will not infer any of it — an absent element stays
//! `unknown` — so until now the only way to raise provenance coverage was to change the recording
//! format, and the checks' own remedy ("attest this element") named a thing that did not exist.
//!
//! An attestation is the producer saying it, in a document that can be checked:
//!
//! - **Signed by the producer's key**, which is *not* the certificate issuer's key. A verifier
//!   learns which key asserted a value and can decide whether it trusts that key — the same
//!   structure as a certificate, one level down.
//! - **Bound to the dataset's CDM content hash**, so an attestation cannot be moved to other data.
//!   "This robot was operated by Dana" is a claim about *this* recording.
//! - **Carried beside the CDM, never folded into it.** The content hash describes the data; if
//!   claims about the data changed it, the hash an attestation binds to would change the moment the
//!   attestation was applied. Attested elements raise provenance *coverage* and appear in the
//!   certificate, and the dataset's hash means exactly what it meant before.
//!
//! What an attestation is not: evidence. It records that a named key asserted a value, which is why
//! attested elements count as `asserted` rather than `known` and score identically to a value the
//! format itself recorded — the trust rubric already treats the two the same, and the certificate
//! says which is which.

use serde::{Deserialize, Serialize};

use crate::cdm::{Dataset, ProvenanceClass};
use crate::certificate::signing::{to_hex, CertError, SigningKeypair, ALGORITHM};

/// Schema id for the attestation document.
pub const ATTESTATION_SCHEMA_VERSION: &str = "veridex.attestation/1";

/// Domain separation for the signature.
///
/// Distinct from the certificate's tag so an attestation can never be presented as a certificate, or
/// the reverse: the same key signs both, and without a domain tag a signature over one document's
/// bytes would verify over the other's if the bytes ever coincided.
const SIGN_DOMAIN: &[u8] = b"veridex.attestation.sig.v1\0";

/// One attested provenance element: a key a producer asserts a value for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestedElement {
    /// The provenance key (e.g. `license`, `annotator`, `calibration`).
    pub key: String,
    /// The value the producer asserts.
    pub value: String,
}

/// The unsigned attestation content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// Schema id.
    pub schema: String,
    /// The dataset this attestation is about, as Veridex identifies it.
    pub dataset_id: String,
    /// The CDM content hash it is bound to (hex). An attestation presented against other data is
    /// refused.
    pub cdm_content_hash: String,
    /// The elements the producer asserts, sorted by key so the document is deterministic.
    pub elements: Vec<AttestedElement>,
    /// Caller-supplied issuance timestamp (the core never reads a clock).
    pub timestamp: String,
}

/// An attestation with its Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedAttestation {
    /// The signed content.
    pub attestation: Attestation,
    /// Signature algorithm (`ed25519`).
    pub algorithm: String,
    /// The producer's public key (hex) — the key the signature verifies against.
    pub public_key: String,
    /// The signature (hex).
    pub signature: String,
}

impl Attestation {
    /// Build an attestation over `elements` for a dataset and its content hash.
    ///
    /// Elements are sorted by key and de-duplicated (last value for a repeated key wins), so the
    /// same request always produces the same document — an attestation whose bytes depended on
    /// argument order could not be compared, cached, or re-signed reproducibly.
    pub fn build(
        dataset_id: impl Into<String>,
        cdm_content_hash: impl Into<String>,
        elements: impl IntoIterator<Item = (String, String)>,
        timestamp: impl Into<String>,
    ) -> Attestation {
        let mut by_key: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (key, value) in elements {
            by_key.insert(key, value);
        }
        Attestation {
            schema: ATTESTATION_SCHEMA_VERSION.to_string(),
            dataset_id: dataset_id.into(),
            cdm_content_hash: cdm_content_hash.into(),
            elements: by_key
                .into_iter()
                .map(|(key, value)| AttestedElement { key, value })
                .collect(),
            timestamp: timestamp.into(),
        }
    }
}

/// The exact bytes signed and verified: a domain tag plus the attestation's JSON.
fn signing_message(attestation: &Attestation) -> Vec<u8> {
    let json = serde_json::to_vec(attestation).expect("attestation serializes");
    let mut message = Vec::with_capacity(SIGN_DOMAIN.len() + json.len());
    message.extend_from_slice(SIGN_DOMAIN);
    message.extend_from_slice(&json);
    message
}

/// Sign an attestation with the producer's key.
pub fn sign_attestation(attestation: Attestation, keypair: &SigningKeypair) -> SignedAttestation {
    let signature = keypair.sign_bytes(&signing_message(&attestation));
    SignedAttestation {
        attestation,
        algorithm: ALGORITHM.to_string(),
        public_key: keypair.public_hex(),
        signature: to_hex(&signature),
    }
}

/// Why an attestation was refused.
///
/// Its own type rather than [`CertError`], because the certificate's messages name a certificate:
/// "the certificate was altered" is the wrong sentence to print while refusing an attestation, and a
/// message that names the nearest thing instead of the actual one sends the reader to the wrong file.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttestError {
    /// The signature did not verify against the content and the producer key.
    #[error("signature mismatch: the attestation was altered, or signed by a different key")]
    SignatureMismatch,
    /// The attestation is bound to a different dataset than the one presented.
    #[error(
        "this attestation is about a different dataset: it is bound to {bound}, and this one \
         hashes to {presented}"
    )]
    NotThisDataset {
        /// The hash the attestation is bound to.
        bound: String,
        /// The presented dataset's hash.
        presented: String,
    },
    /// Signed by a key other than the one the caller trusts.
    #[error("untrusted producer: this attestation is signed by {found}, not the expected key")]
    UntrustedProducer {
        /// The public key the document carries.
        found: String,
    },
    /// A malformed key or signature, an unknown algorithm, or an unknown schema.
    #[error("{0}")]
    Malformed(String),
}

impl From<CertError> for AttestError {
    fn from(e: CertError) -> Self {
        match e {
            CertError::SignatureMismatch => AttestError::SignatureMismatch,
            other => AttestError::Malformed(other.to_string()),
        }
    }
}

/// Verify an attestation offline.
///
/// - `presented_cdm_hash`: the attestation must be bound to this hash, so it cannot be moved to
///   other data.
/// - `expected_producer`: if `Some`, the attestation must be signed by this public key.
///
/// Returns the producer key the signature actually verified against — the authoritative identity.
pub fn verify_attestation(
    signed: &SignedAttestation,
    presented_cdm_hash: &str,
    expected_producer: Option<&str>,
) -> Result<String, AttestError> {
    if signed.algorithm != ALGORITHM {
        return Err(AttestError::Malformed(format!(
            "unsupported signature algorithm `{}` (expected `{ALGORITHM}`)",
            signed.algorithm
        )));
    }
    if signed.attestation.schema != ATTESTATION_SCHEMA_VERSION {
        return Err(AttestError::Malformed(format!(
            "unsupported attestation schema `{}` (expected `{ATTESTATION_SCHEMA_VERSION}`)",
            signed.attestation.schema
        )));
    }
    crate::certificate::signing::verify_detached(
        &signing_message(&signed.attestation),
        &signed.public_key,
        &signed.signature,
    )?;
    if signed.attestation.cdm_content_hash != presented_cdm_hash {
        return Err(AttestError::NotThisDataset {
            bound: signed.attestation.cdm_content_hash.clone(),
            presented: presented_cdm_hash.to_string(),
        });
    }
    if let Some(expected) = expected_producer {
        if expected != signed.public_key {
            return Err(AttestError::UntrustedProducer {
                found: signed.public_key.clone(),
            });
        }
    }
    Ok(signed.public_key.clone())
}

/// An attested value that contradicts what the dataset itself records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationConflict {
    /// The provenance key both claim.
    pub key: String,
    /// What the dataset records.
    pub recorded: String,
    /// What the producer attests.
    pub attested: String,
}

/// Attested keys that contradict a value the dataset itself records as `known`.
///
/// An attestation that *adds* what the format does not carry is the point of the feature. One that
/// *overrides* what the format does carry is a different thing: either the recording is wrong or the
/// claim is, and Veridex reports the disagreement rather than silently preferring one. The extracted
/// value keeps its place in the CDM either way — an attestation never rewrites what was read.
pub fn conflicts(dataset: &Dataset, attestation: &Attestation) -> Vec<AttestationConflict> {
    let mut out = Vec::new();
    for element in &attestation.elements {
        for record in &dataset.provenance {
            for recorded in &record.elements {
                if recorded.key != element.key || recorded.class != ProvenanceClass::Known {
                    continue;
                }
                if let Some(value) = &recorded.value {
                    if value != &element.value {
                        out.push(AttestationConflict {
                            key: element.key.clone(),
                            recorded: value.clone(),
                            attested: element.value.clone(),
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out.dedup();
    out
}
