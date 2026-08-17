//! Certificate signing and offline verification (design D6).
//!
//! A certificate is signed with Ed25519 over the deterministic JSON of its content, mirroring the
//! signed-verdict pattern shared with Invariant. Verification is fully offline: it recomputes the
//! canonical bytes, checks the signature against the issuer public key, and confirms the bound CDM
//! content hash matches the presented dataset — rejecting tampering (signature mismatch) and
//! transplantation (content-hash mismatch) respectively.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

use super::document::Certificate;

const SIGN_DOMAIN: &[u8] = b"veridex.certificate.sig.v1\0";

/// Errors from signing or verification.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CertError {
    /// A key or signature was not valid lowercase hex of the expected length.
    #[error("malformed {what}: expected {expected} hex bytes")]
    Malformed {
        /// What was malformed (e.g. "public key").
        what: &'static str,
        /// Expected byte length.
        expected: usize,
    },
    /// The signature did not verify against the certificate content and public key (tampering).
    #[error("signature mismatch: the certificate was altered or signed by a different key")]
    SignatureMismatch,
    /// The certificate is bound to a different dataset than the one presented (transplant).
    #[error("content-hash mismatch: certificate is bound to {bound}, but the dataset hashes to {presented}{version_note}")]
    ContentHashMismatch {
        /// The hash the certificate is bound to.
        bound: String,
        /// The presented dataset's hash.
        presented: String,
        /// A trailing clause naming a version difference between the issuing and the verifying
        /// Veridex, when there is one. Empty otherwise. A content hash is only comparable within one
        /// canonical encoding, and that encoding changes between releases — so an unchanged dataset
        /// checked by a newer Veridex hashes differently, and without this the failure reads exactly
        /// like tampering.
        version_note: String,
    },
    /// The certificate was signed by a key other than the trusted issuer key provided.
    #[error("untrusted issuer: certificate key {found} does not match the expected issuer key")]
    UntrustedIssuer {
        /// The public key embedded in the certificate.
        found: String,
    },
    /// The key supplied as the trusted issuer is the issuer's *secret* key, not its public one.
    #[error(
        "the key given to --key is this certificate's issuer secret key, not its public key — \
         pass the `.pub` file `veridex keygen` wrote beside it"
    )]
    SecretKeyGiven,
    /// The certificate declares a signature algorithm this build cannot verify.
    #[error("unsupported signature algorithm `{found}` (expected `ed25519`)")]
    UnsupportedAlgorithm {
        /// The algorithm named in the certificate.
        found: String,
    },
    /// The certificate declares a schema this build does not know how to read.
    #[error("unsupported certificate schema {found}: this Veridex issues and verifies {expected}")]
    UnsupportedSchema {
        /// The schema named in the certificate.
        found: String,
        /// The schema this build knows.
        expected: &'static str,
    },
}

/// The only signature algorithm v0.1 issues and verifies.
const ALGORITHM: &str = "ed25519";

/// An Ed25519 signing keypair.
pub struct SigningKeypair {
    key: SigningKey,
}

impl SigningKeypair {
    /// Construct from a 32-byte seed (deterministic; useful for tests and reproducible issuance).
    pub fn from_seed(seed: [u8; 32]) -> Self {
        SigningKeypair {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// Construct from a 64-character hex secret seed, trimming surrounding whitespace. Returns
    /// `None` if the input is not exactly 32 hex-encoded bytes.
    pub fn from_secret_hex(hex: &str) -> Option<Self> {
        let seed: [u8; 32] = from_hex(hex.trim())?;
        Some(SigningKeypair::from_seed(seed))
    }

    /// Generate a fresh random keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("OS randomness");
        SigningKeypair::from_seed(seed)
    }

    /// The 32-byte secret seed, hex-encoded. Store this secret; never commit it.
    pub fn secret_hex(&self) -> String {
        to_hex(self.key.to_bytes().as_slice())
    }

    /// The public verifying key, hex-encoded. This is the issuer key id.
    pub fn public_hex(&self) -> String {
        to_hex(self.key.verifying_key().to_bytes().as_slice())
    }
}

/// A signed certificate: content plus its detached Ed25519 signature and the issuer public key.
/// (Not `Eq`: the certificate's effective config carries float tolerances.)
/// Unknown fields are rejected on every certificate type: the signature covers the *struct*, so a
/// field Veridex does not know about would ride along inside a document it just called authentic,
/// and any consumer reading the JSON directly would see attacker-authored data as verified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCertificate {
    /// The certificate content.
    pub certificate: Certificate,
    /// Signature algorithm (always `ed25519` in v0.1).
    pub algorithm: String,
    /// Issuer public key, hex-encoded (the key id).
    pub public_key: String,
    /// Ed25519 signature over the canonical certificate bytes, hex-encoded.
    pub signature: String,
}

/// What a successful verification reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// Issuer key id (public key hex).
    pub key_id: String,
    /// Issuance timestamp from the certificate.
    pub timestamp: String,
}

/// The exact bytes signed and verified: a domain tag plus the certificate's canonical JSON.
fn signing_message(certificate: &Certificate) -> Vec<u8> {
    let json = serde_json::to_vec(certificate).expect("certificate serializes");
    let mut msg = Vec::with_capacity(SIGN_DOMAIN.len() + json.len());
    msg.extend_from_slice(SIGN_DOMAIN);
    msg.extend_from_slice(&json);
    msg
}

/// Sign a certificate, producing a portable [`SignedCertificate`].
pub fn sign(certificate: Certificate, keypair: &SigningKeypair) -> SignedCertificate {
    let signature: Signature = keypair.key.sign(&signing_message(&certificate));
    SignedCertificate {
        certificate,
        algorithm: ALGORITHM.to_string(),
        public_key: keypair.public_hex(),
        signature: to_hex(&signature.to_bytes()),
    }
}

/// Verify a signed certificate offline.
///
/// - `presented_cdm_hash`: if `Some`, the certificate must be bound to this hash (transplant check).
/// - `expected_issuer`: if `Some`, the certificate must be signed by this public key hex.
///
/// The returned [`Verified::key_id`] is the **actual signing key** (`signed.public_key`, the key the
/// signature verified against) — that is the authoritative issuer identity. The certificate's own
/// `issuance.key_id` is a self-asserted label the issuer chose and is not required to equal the
/// signing key; callers that need to trust a specific issuer must pass `expected_issuer` rather than
/// reading `issuance.key_id`.
pub fn verify(
    signed: &SignedCertificate,
    presented_cdm_hash: Option<&str>,
    expected_issuer: Option<&str>,
) -> Result<Verified, CertError> {
    // A signed certificate has exactly one byte form. Uppercasing the algorithm or the hex fields
    // leaves the same semantic document but a *different file*, and accepting both would mean two
    // distinct files verify identically — so a consumer that pins or de-duplicates certificates by
    // file digest could be handed either. Require the canonical spelling this crate writes.
    if signed.algorithm != ALGORITHM {
        return Err(CertError::UnsupportedAlgorithm {
            found: signed.algorithm.clone(),
        });
    }
    // Every other version mismatch in this file fails closed; this one did not. A document
    // declaring a future schema whose fields happen to parse under today's struct was verified as
    // though it were today's, so a field this build cannot see -- one a later schema gives meaning
    // to -- would be signed, present, and silently ignored. The signature makes it unforgeable, not
    // intelligible.
    if signed.certificate.schema != crate::certificate::CERTIFICATE_SCHEMA_VERSION {
        return Err(CertError::UnsupportedSchema {
            found: signed.certificate.schema.clone(),
            expected: crate::certificate::CERTIFICATE_SCHEMA_VERSION,
        });
    }
    if !is_canonical_hex(&signed.signature) {
        return Err(CertError::Malformed {
            what: "signature",
            expected: 64,
        });
    }
    if !is_canonical_hex(&signed.public_key) {
        return Err(CertError::Malformed {
            what: "public key",
            expected: 32,
        });
    }

    // Issuer identity, if a trusted key was supplied.
    if let Some(expected) = expected_issuer {
        if !expected.eq_ignore_ascii_case(&signed.public_key) {
            // `keygen` writes `issuer` and `issuer.pub` one letter apart, and a secret key is also
            // 64 hex characters — so pointing `--key` at the secret file parses, and the answer was
            // "untrusted issuer", which reads as an accusation about the certificate rather than a
            // mistyped path. Checked by deriving the public key the secret would produce.
            if crate::certificate::SigningKeypair::from_secret_hex(expected)
                .is_some_and(|kp| kp.public_hex().eq_ignore_ascii_case(&signed.public_key))
            {
                return Err(CertError::SecretKeyGiven);
            }
            return Err(CertError::UntrustedIssuer {
                found: signed.public_key.clone(),
            });
        }
    }

    // Signature over the canonical content (tamper check).
    let vk_bytes: [u8; 32] = from_hex(&signed.public_key).ok_or(CertError::Malformed {
        what: "public key",
        expected: 32,
    })?;
    let verifying_key = VerifyingKey::from_bytes(&vk_bytes).map_err(|_| CertError::Malformed {
        what: "public key",
        expected: 32,
    })?;
    let sig_bytes: [u8; 64] = from_hex(&signed.signature).ok_or(CertError::Malformed {
        what: "signature",
        expected: 64,
    })?;
    let signature = Signature::from_bytes(&sig_bytes);
    // `verify_strict` rejects non-canonical signatures and mixed-order/small-order keys, so a given
    // certificate has exactly one valid signature — no malleability.
    verifying_key
        .verify_strict(&signing_message(&signed.certificate), &signature)
        .map_err(|_| CertError::SignatureMismatch)?;

    // Binding to the presented dataset (transplant check).
    if let Some(presented) = presented_cdm_hash {
        if presented != signed.certificate.cdm_content_hash {
            let issued_by = &signed.certificate.veridex_version;
            return Err(CertError::ContentHashMismatch {
                bound: signed.certificate.cdm_content_hash.clone(),
                presented: presented.to_string(),
                version_note: if issued_by == crate::VERSION {
                    String::new()
                } else {
                    format!(
                        " — note this certificate was issued by veridex {issued_by} and you are \
                         verifying with {}; the canonical encoding can change between releases, \
                         which rehashes byte-identical data, so re-issue the certificate before \
                         reading this as tampering",
                        crate::VERSION
                    )
                },
            });
        }
    }

    Ok(Verified {
        key_id: signed.public_key.clone(),
        timestamp: signed.certificate.issuance.timestamp.clone(),
    })
}

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// Whether a string is hex in the one spelling this crate writes: lowercase digits only. Used on the
/// fields of a *signed document*, where more than one accepted spelling means more than one file that
/// verifies. A user-supplied key file stays tolerant — that is input, not a signed artifact.
fn is_canonical_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Decode lowercase/uppercase hex into a fixed-size array, or `None` on any malformed input.
fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    let bytes = s.as_bytes();
    for i in 0..N {
        let hi = (bytes[2 * i] as char).to_digit(16)?;
        let lo = (bytes[2 * i + 1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}
