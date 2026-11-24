//! An append-only, hash-chained record of what this tool certified and verified.
//!
//! A certificate says "these checks, at these versions and thresholds, produced these findings on
//! data with this content hash". It does not say how many times that was tried. An issuer who runs
//! `certify` twenty times and keeps the twentieth has told nobody about the nineteen, and a
//! certificate is a portable document: once it leaves the machine that made it, the history of its
//! making is gone. This log is that history, kept where the issuer can publish it and a verifier can
//! check it.
//!
//! Each line is one JSON entry. Every entry names the hash of the entry before it, so the file is a
//! chain: altering an entry changes its own hash, which the next entry recorded, and every link from
//! there on fails. [`verify_log`] walks it and names the first break, so "this log was edited" is a
//! statement with a line number behind it rather than a suspicion.
//!
//! ## What this does and does not prove
//!
//! Stated here because an "audit log" sounds like more than any single file can be.
//!
//! **Altering a past entry is detected.** That is the chain, and it holds even if the editor rewrote
//! every `prev` field afterwards: an entry written by `certify` carries a **signature** over its own
//! hash from the same issuer key that signed the certificate, so re-chaining a forged history means
//! re-signing it, which means the issuer's secret key. Without the key an attacker can corrupt the
//! log but cannot rewrite it into a different consistent history.
//!
//! **Dropping entries from the end is not**, by this file alone. A truncated chain is a valid chain,
//! one entry shorter. Detecting it needs a value kept somewhere the log's holder does not control —
//! so [`verify_log`] reports the chain **head**, which a caller can pin (`veridex audit --expect-head`),
//! publish, or commit; and this limit is printed with the result rather than left for the reader to
//! work out.
//!
//! **Nothing here is a timestamp authority.** The time in an entry is the one the issuing machine
//! supplied, exactly as in a certificate's issuance block, and a machine that lies about its clock
//! writes a lie into the log. The *order* is the chain's, which no clock can change.
//!
//! This crate reads no clock and opens no file to decide what to write: the caller supplies the
//! time, as it does for issuance.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::certificate::signing::SigningKeypair;

/// The schema every entry this build writes declares, and the only one it reads.
pub const AUDIT_SCHEMA_VERSION: &str = "veridex.audit/1";

/// The domain tag over which an entry's hash — and its signature — is computed. Domain separation so
/// an entry's bytes can never be replayed as a certificate's, or the reverse.
const AUDIT_DOMAIN: &[u8] = b"veridex.audit.entry.v1\0";

/// What the first entry in a chain names as its predecessor. A literal, not an empty string, so a
/// genesis entry and an entry whose `prev` field was emptied by hand are different documents.
pub const GENESIS: &str = "genesis";

/// What an entry records happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// A certificate was issued over a dataset.
    Issued,
    /// A certificate was checked against a dataset (`veridex verify`), with the outcome recorded.
    Verified,
}

impl Action {
    /// The word this action is written and printed as.
    pub fn tag(self) -> &'static str {
        match self {
            Action::Issued => "issued",
            Action::Verified => "verified",
        }
    }
}

/// One line of the log: what happened, to which dataset, under which key — and the link that makes
/// the file a chain.
///
/// `#[serde(deny_unknown_fields)]` for the same reason the signed certificate carries it: a field
/// this build does not know would ride along inside a document it just called intact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The schema this entry was written under.
    pub schema: String,
    /// Position in the chain, from 0.
    pub seq: u64,
    /// The `entry_hash` of the entry before this one, or [`GENESIS`] for the first.
    pub prev: String,
    /// What happened.
    pub action: Action,
    /// The dataset's CDM id.
    pub dataset_id: String,
    /// The CDM content hash the certificate is bound to.
    pub cdm_hash: String,
    /// The canonical encoding version that hash was computed under — a hash means nothing without
    /// it, and a log outlives an encoding bump.
    pub cdm_encoding_version: u32,
    /// The verdict status at issuance, or the verification outcome. Free-form on purpose: this is a
    /// record of what was reported, not a field anything dispatches on.
    pub outcome: String,
    /// The issuing (or verifying) key id, hex — empty when the action needed no key.
    pub key_id: String,
    /// SHA-256 over the certificate document's bytes, so an entry names the exact document and not
    /// merely the dataset it was about.
    pub certificate_hash: String,
    /// The time the writer supplied. Never read from a clock in this crate.
    pub timestamp: String,
    /// The Veridex version that wrote the entry.
    pub veridex_version: String,
    /// SHA-256 over the module's domain tag plus this entry's canonical body (every field above). The link
    /// the next entry records.
    pub entry_hash: String,
    /// Ed25519 over the same message `entry_hash` digests, hex, when a key signed this entry.
    /// Absent for an entry written without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// The fields a caller supplies; the chain fields are computed by [`append`].
#[derive(Debug, Clone)]
pub struct Record {
    /// What happened.
    pub action: Action,
    /// The dataset's CDM id.
    pub dataset_id: String,
    /// The CDM content hash.
    pub cdm_hash: String,
    /// The verdict status or verification outcome, as reported.
    pub outcome: String,
    /// The key id involved, or empty.
    pub key_id: String,
    /// SHA-256 of the certificate document.
    pub certificate_hash: String,
    /// The caller's timestamp.
    pub timestamp: String,
}

/// The body of an entry, in the order it is hashed. A struct rather than a hand-rolled string so a
/// field added to [`Entry`] is a compile error here rather than a field silently outside the hash.
#[derive(Serialize)]
struct Body<'a> {
    schema: &'a str,
    seq: u64,
    prev: &'a str,
    action: Action,
    dataset_id: &'a str,
    cdm_hash: &'a str,
    cdm_encoding_version: u32,
    outcome: &'a str,
    key_id: &'a str,
    certificate_hash: &'a str,
    timestamp: &'a str,
    veridex_version: &'a str,
}

/// The exact bytes an entry's hash and signature are computed over.
fn message(body: &Body<'_>) -> Vec<u8> {
    let json = serde_json::to_vec(body).expect("an audit entry body serializes");
    let mut msg = Vec::with_capacity(AUDIT_DOMAIN.len() + json.len());
    msg.extend_from_slice(AUDIT_DOMAIN);
    msg.extend_from_slice(&json);
    msg
}

fn digest(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 of a document's bytes, hex — what [`Record::certificate_hash`] holds.
pub fn document_hash(bytes: &[u8]) -> String {
    digest(bytes)
}

/// Errors reading or extending a log.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuditError {
    /// A line is not an entry this build can read.
    #[error("line {line}: not a Veridex audit entry: {detail}")]
    NotAnEntry {
        /// 1-based line number.
        line: usize,
        /// What the parser said.
        detail: String,
    },
    /// A line declares a schema this build does not know.
    #[error("line {line}: audit schema `{found}`, expected `{expected}`")]
    UnknownSchema {
        /// 1-based line number.
        line: usize,
        /// The schema found.
        found: String,
        /// The schema this build writes.
        expected: &'static str,
    },
}

/// Why a chain is not intact, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Break {
    /// An entry's recorded hash is not the hash of its own contents: the entry was altered.
    EntryAltered {
        /// 1-based line number.
        line: usize,
        /// The entry's own `seq`.
        seq: u64,
    },
    /// An entry's `prev` is not the previous entry's hash: an entry was removed, inserted or
    /// reordered.
    LinkBroken {
        /// 1-based line number.
        line: usize,
        /// What this entry names as its predecessor.
        expected: String,
        /// What the previous line actually hashes to.
        found: String,
    },
    /// The sequence numbers do not run 0, 1, 2 …
    SequenceJumped {
        /// 1-based line number.
        line: usize,
        /// The `seq` this line should carry.
        expected: u64,
        /// The `seq` it does carry.
        found: u64,
    },
    /// An entry carries a signature that does not verify against the key it names.
    SignatureMismatch {
        /// 1-based line number.
        line: usize,
        /// The key id the entry names.
        key_id: String,
    },
}

impl std::fmt::Display for Break {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Break::EntryAltered { line, seq } => write!(
                f,
                "line {line} (seq {seq}) does not hash to the hash it records: that entry was \
                 altered after it was written"
            ),
            Break::LinkBroken {
                line,
                expected,
                found,
            } => write!(
                f,
                "line {line} names predecessor {}… but the line before it hashes to {}…: an entry \
                 was removed, inserted or reordered",
                short(expected),
                short(found)
            ),
            Break::SequenceJumped {
                line,
                expected,
                found,
            } => write!(
                f,
                "line {line} is numbered {found} where the chain is at {expected}"
            ),
            Break::SignatureMismatch { line, key_id } => write!(
                f,
                "line {line} carries a signature that does not verify against key {}…: the entry \
                 was rewritten by someone without that key, or signed by a different one",
                short(key_id)
            ),
        }
    }
}

fn short(hex: &str) -> &str {
    &hex[..hex.len().min(8)]
}

/// What walking a log found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditReport {
    /// The entries read, in file order.
    pub entries: Vec<Entry>,
    /// The first break found, or `None` when the chain is intact.
    pub broken: Option<Break>,
    /// The last entry's hash — the chain head, and the value to pin or publish, because a log
    /// shortened from the end is otherwise a valid log.
    pub head: String,
    /// How many entries carried a signature **and verified it**, up to where the walk stopped. An
    /// unsigned entry is still chained; it is just not evidence against whoever holds the file. On a
    /// broken chain this counts only the entries before the break, because nothing after it was
    /// checked.
    pub signed: usize,
}

impl AuditReport {
    /// Whether the chain is intact as read. Says nothing about entries that were never in the file,
    /// which is what [`AuditReport::head`] is for.
    pub fn intact(&self) -> bool {
        self.broken.is_none()
    }
}

/// Parse a log and verify its chain, stopping at the first break.
///
/// Every entry is checked three ways: its own hash against its contents, its `prev` against the
/// previous entry's hash, and its signature (when it carries one) against the key it names. A line
/// that is not an entry at all is an error rather than a break — the file is not a log, which is a
/// different thing from a log that was edited.
pub fn verify_log(text: &str) -> Result<AuditReport, AuditError> {
    let mut entries: Vec<Entry> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let entry: Entry = serde_json::from_str(raw).map_err(|e| AuditError::NotAnEntry {
            line,
            detail: e.to_string(),
        })?;
        if entry.schema != AUDIT_SCHEMA_VERSION {
            return Err(AuditError::UnknownSchema {
                line,
                found: entry.schema,
                expected: AUDIT_SCHEMA_VERSION,
            });
        }
        entries.push(entry);
    }

    let mut broken = None;
    let mut prev_hash = GENESIS.to_string();
    let mut signed = 0;
    for (i, entry) in entries.iter().enumerate() {
        let line = i + 1;
        let body = body_of(entry);
        let msg = message(&body);
        if entry.seq != i as u64 {
            broken = Some(Break::SequenceJumped {
                line,
                expected: i as u64,
                found: entry.seq,
            });
            break;
        }
        if digest(&msg) != entry.entry_hash {
            broken = Some(Break::EntryAltered {
                line,
                seq: entry.seq,
            });
            break;
        }
        if entry.prev != prev_hash {
            broken = Some(Break::LinkBroken {
                line,
                expected: entry.prev.clone(),
                found: prev_hash.clone(),
            });
            break;
        }
        if let Some(sig) = &entry.signature {
            signed += 1;
            if crate::certificate::signing::verify_detached(&msg, &entry.key_id, sig).is_err() {
                broken = Some(Break::SignatureMismatch {
                    line,
                    key_id: entry.key_id.clone(),
                });
                break;
            }
        }
        prev_hash = entry.entry_hash.clone();
    }

    Ok(AuditReport {
        head: entries
            .last()
            .map(|e| e.entry_hash.clone())
            .unwrap_or_else(|| GENESIS.to_string()),
        entries,
        broken,
        signed,
    })
}

fn body_of(entry: &Entry) -> Body<'_> {
    Body {
        schema: &entry.schema,
        seq: entry.seq,
        prev: &entry.prev,
        action: entry.action,
        dataset_id: &entry.dataset_id,
        cdm_hash: &entry.cdm_hash,
        cdm_encoding_version: entry.cdm_encoding_version,
        outcome: &entry.outcome,
        key_id: &entry.key_id,
        certificate_hash: &entry.certificate_hash,
        timestamp: &entry.timestamp,
        veridex_version: &entry.veridex_version,
    }
}

/// Build the next entry of a chain whose last entry is `last`, signing it when a key is given.
///
/// Pure: no file, no clock. [`append`] is the thin shell that reads the tail and writes the line.
pub fn next_entry(
    last: Option<&Entry>,
    record: &Record,
    keypair: Option<&SigningKeypair>,
) -> Entry {
    let seq = last.map(|e| e.seq + 1).unwrap_or(0);
    let prev = last
        .map(|e| e.entry_hash.clone())
        .unwrap_or_else(|| GENESIS.to_string());
    let version = crate::VERSION.to_string();
    let body = Body {
        schema: AUDIT_SCHEMA_VERSION,
        seq,
        prev: &prev,
        action: record.action,
        dataset_id: &record.dataset_id,
        cdm_hash: &record.cdm_hash,
        cdm_encoding_version: crate::CANONICAL_VERSION,
        outcome: &record.outcome,
        key_id: &record.key_id,
        certificate_hash: &record.certificate_hash,
        timestamp: &record.timestamp,
        veridex_version: &version,
    };
    let msg = message(&body);
    Entry {
        schema: AUDIT_SCHEMA_VERSION.to_string(),
        seq,
        prev,
        action: record.action,
        dataset_id: record.dataset_id.clone(),
        cdm_hash: record.cdm_hash.clone(),
        cdm_encoding_version: crate::CANONICAL_VERSION,
        outcome: record.outcome.clone(),
        key_id: record.key_id.clone(),
        certificate_hash: record.certificate_hash.clone(),
        timestamp: record.timestamp.clone(),
        veridex_version: version,
        entry_hash: digest(&msg),
        signature: keypair.map(|k| k.sign_hex(&msg)),
    }
}

/// One entry, as the line it is written as (no trailing newline).
pub fn render_entry(entry: &Entry) -> String {
    serde_json::to_string(entry).expect("an audit entry serializes")
}

/// Append a record to the log at `path`, creating it if it does not exist.
///
/// Reads the existing file to find the chain's tail, builds the next entry from it, and appends one
/// line. The read is of the whole file because a log of issuances is kilobytes, and parsing the tail
/// is how `prev` is known — but the chain is **not** re-verified here: appending to a log that was
/// already edited is not a failure to report at issuance time, it is what `veridex audit` is for.
///
/// Returns the entry written.
pub fn append(
    path: &std::path::Path,
    record: &Record,
    keypair: Option<&SigningKeypair>,
) -> std::io::Result<Entry> {
    let existing = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let last = existing
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .and_then(|l| serde_json::from_str::<Entry>(l).ok());
    let entry = next_entry(last.as_ref(), record, keypair);
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{}", render_entry(&entry))?;
    file.flush()?;
    Ok(entry)
}

/// The chain head a caller pinned, when it is *not* the head the log actually has.
///
/// Separate from [`verify_log`] because the log cannot know what was pinned, and because a mismatch
/// is the one failure that comes from outside the file: every entry still present may chain
/// perfectly and the log still be short of entries that were in it.
pub fn head_mismatch<'a>(report: &AuditReport, expected: Option<&'a str>) -> Option<&'a str> {
    expected
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .filter(|e| *e != report.head)
}

/// The machine-readable result of walking a log — the shape `veridex audit --json` prints and the
/// Python binding returns, rendered once here so the two cannot drift.
pub fn render_log_json(report: &AuditReport, expect_head: Option<&str>) -> String {
    let mismatch = head_mismatch(report, expect_head);
    serde_json::json!({
        "intact": report.intact() && mismatch.is_none(),
        "entries": report.entries.len(),
        "signed": report.signed,
        "head": report.head,
        "broken": report.broken.as_ref().map(|b| b.to_string()),
        "expected_head": mismatch,
    })
    .to_string()
}

/// What a log says about one certificate document.
///
/// Three answers, and the difference between the second and third is the point of having the type:
/// a log that records *an* issuance for a dataset is not a log that records *this document*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateRecord {
    /// The log holds an entry whose certificate hash is this document's: this exact document was
    /// recorded.
    ThisDocument {
        /// The matching entries, in chain order (an issuance, and any verifications of it).
        seqs: Vec<u64>,
    },
    /// No entry names this document, but the log does record issuances bound to the same dataset
    /// content hash — a certificate re-issued over the same bytes, or this document re-serialized.
    SameDatasetOnly {
        /// The entries bound to that content hash.
        seqs: Vec<u64>,
    },
    /// The log says nothing about this dataset at all.
    Absent,
}

/// What `log` records about the certificate document `certificate_bytes`, which must be the bytes as
/// the issuer wrote them.
///
/// Matched on the hash of the document, not on what the document claims: a log entry names a
/// certificate by digest, so "this document was recorded" is a statement about bytes. A
/// re-serialized certificate — same content, different whitespace — is therefore *not* this
/// document, and [`CertificateRecord::SameDatasetOnly`] is how that is reported rather than as a
/// match or as an absence. Nothing here verifies a signature; that is `verify`'s job, and a log can
/// only say what it was told.
pub fn certificate_in_log(report: &AuditReport, certificate_bytes: &[u8]) -> CertificateRecord {
    let document = document_hash(certificate_bytes);
    let this: Vec<u64> = report
        .entries
        .iter()
        .filter(|e| e.certificate_hash == document)
        .map(|e| e.seq)
        .collect();
    if !this.is_empty() {
        return CertificateRecord::ThisDocument { seqs: this };
    }
    // Fall back to the dataset the document is bound to, read from the document itself.
    let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(certificate_bytes) else {
        return CertificateRecord::Absent;
    };
    let bound = parsed
        .get("certificate")
        .and_then(|c| c.get("cdm_content_hash"))
        .and_then(|h| h.as_str())
        .unwrap_or_default();
    if bound.is_empty() {
        return CertificateRecord::Absent;
    }
    let same: Vec<u64> = report
        .entries
        .iter()
        .filter(|e| e.cdm_hash == bound)
        .map(|e| e.seq)
        .collect();
    if same.is_empty() {
        CertificateRecord::Absent
    } else {
        CertificateRecord::SameDatasetOnly { seqs: same }
    }
}
