//! A **static registry**: certificates published where anyone can look them up by content hash.
//!
//! A certificate verifies offline and always will — that is the whole design, and nothing here
//! changes it. What offline verification cannot answer is the question a *consumer* has when they
//! are handed a dataset and no document: has anyone certified these bytes, and is that certificate
//! still the issuer's current word on them? Those are lookups, and a lookup needs somewhere to look.
//!
//! This is that somewhere, in the smallest form that is not a service: a **directory**.
//!
//! ```text
//! registry/
//!   index.json                 every published record, newest first
//!   by-hash/<cdm hash>.json    the record for one dataset's bytes
//! ```
//!
//! Both files are plain JSON, so a registry is publishable by copying the directory to any static
//! host — GitHub Pages, an S3 bucket, a file share — and readable by `veridex registry lookup`
//! against a local path or by anything that can fetch a URL. No server, no database, no account.
//!
//! **What it does not do, deliberately.** It is not an authority. A record carries the certificate
//! the issuer published and whatever status the issuer later recorded, and
//! [`verify_record`] checks the certificate with exactly the function `veridex verify` uses — so a
//! registry cannot make an invalid certificate look valid, and a registry that lies is caught by
//! the signature it cannot forge. Nothing in `check`, `certify` or `verify` consults a registry;
//! publishing is an extra step a producer takes, and a consumer who never looks loses nothing a
//! certificate gave them.
//!
//! **Revocation is the issuer's own statement**, recorded beside the certificate rather than
//! derived: an offline certificate cannot be withdrawn (the bytes are out), so the honest thing a
//! registry can offer is the issuer saying "I no longer stand behind this" where a consumer can
//! read it. A lookup reports it, and the badge reflects it.

use serde::{Deserialize, Serialize};

use crate::certificate::{relate, verify, CertError, Relation, SignedCertificate, Verified};

/// The schema every record this build writes declares, and the only one it reads.
pub const REGISTRY_SCHEMA_VERSION: &str = "veridex.registry/1";

/// The file holding every published record, for browsing and for a static host's index page.
pub const INDEX_FILE: &str = "index.json";

/// The directory holding one record per dataset content hash.
pub const BY_HASH_DIR: &str = "by-hash";

/// What the issuer later said about a certificate they published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// The certificate stands as published.
    Current,
    /// A later certificate for the same dataset lineage replaces it, by content hash.
    Superseded {
        /// The CDM content hash the superseding certificate is bound to.
        by: String,
    },
    /// The issuer withdrew it. The certificate still verifies — bytes cannot be recalled — and this
    /// is the issuer saying they no longer stand behind what it attests.
    Revoked {
        /// Why, as the issuer wrote it. Empty when they gave no reason.
        reason: String,
        /// When, from the caller's clock (this crate reads none).
        at: String,
    },
}

impl Status {
    /// The word a report prints for this status.
    pub fn tag(&self) -> &'static str {
        match self {
            Status::Current => "current",
            Status::Superseded { .. } => "superseded",
            Status::Revoked { .. } => "revoked",
        }
    }

    /// Whether a gate should accept this record. A superseded or revoked certificate is still a
    /// true statement about the bytes it was issued over, and still not the issuer's current word.
    pub fn is_current(&self) -> bool {
        matches!(self, Status::Current)
    }
}

/// One published certificate and what the issuer has said about it since.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// The schema this record was written under.
    pub schema: String,
    /// The CDM content hash the certificate is bound to — the key a record is found by.
    pub cdm_content_hash: String,
    /// The dataset identity the certificate names.
    pub dataset_id: String,
    /// The issuer's public key, hex.
    pub issuer_key: String,
    /// The issuance timestamp the certificate records.
    pub issued_at: String,
    /// When this record was published, from the publisher's clock.
    pub published_at: String,
    /// What the issuer has said about it since.
    pub status: Status,
    /// The certificate itself, verbatim, so a lookup answers with the document and not a summary
    /// of one. A consumer verifies it offline exactly as if it had arrived by email.
    pub certificate: SignedCertificate,
}

/// Errors a registry operation can report.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// The registry directory could not be read or written.
    #[error("registry at {path}: {detail}")]
    Io {
        /// The path involved.
        path: String,
        /// What the filesystem said.
        detail: String,
    },
    /// A file in the registry is not a record this build can read.
    #[error("{path} is not a Veridex registry record: {detail}")]
    NotARecord {
        /// The file.
        path: String,
        /// What the parser said.
        detail: String,
    },
    /// The certificate offered for publication does not verify, so there is nothing to publish.
    ///
    /// Checked at publication rather than at lookup: a registry that accepts an unverifiable
    /// document becomes a place where one looks verified, and the whole value of a lookup is that
    /// what comes back is the issuer's signed word.
    #[error("the certificate does not verify, so it was not published: {0}")]
    DoesNotVerify(#[from] CertError),
    /// No record for the thing asked about.
    #[error("no certificate published for {0}")]
    NotFound(String),
}

fn io_err(path: &std::path::Path, e: std::io::Error) -> RegistryError {
    RegistryError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    }
}

/// Where a dataset's record lives inside the registry.
fn record_path(registry: &std::path::Path, cdm_content_hash: &str) -> std::path::PathBuf {
    registry
        .join(BY_HASH_DIR)
        .join(format!("{cdm_content_hash}.json"))
}

/// Publish `signed` into the registry at `registry`, returning the record written.
///
/// The certificate is **verified first**, with the same function `veridex verify` calls: a registry
/// that accepts an unverifiable document is a place where one looks verified. The issuer key is
/// taken from the certificate itself — a registry does not decide whom to trust, it records who
/// signed — and the trust decision stays with whoever reads the record.
///
/// Publishing a certificate for a dataset that already has one **supersedes** rather than
/// overwrites, where the two are related that way: the older record is marked
/// [`Status::Superseded`] and kept, because a registry whose history can be edited is a registry
/// whose claims cannot be checked. The relation is computed by [`relate`], the same code
/// `veridex supersedes` uses, so the two can never disagree.
pub fn publish(
    registry: &std::path::Path,
    signed: &SignedCertificate,
    published_at: &str,
) -> Result<Record, RegistryError> {
    verify(signed, None, None)?;
    let hash = signed.certificate.cdm_content_hash.clone();

    // Everything already published for this dataset identity, so the new certificate can be
    // related to it rather than silently replacing it.
    let existing = read_all(registry)?;
    for mut older in existing
        .into_iter()
        .filter(|r| r.dataset_id == signed.certificate.dataset_id && r.cdm_content_hash != hash)
        .filter(|r| r.status.is_current())
    {
        let relation = relate(
            &older.certificate.certificate,
            &older.certificate.public_key,
            &signed.certificate,
            &signed.public_key,
        );
        if relation.relation == Relation::Supersedes {
            older.status = Status::Superseded { by: hash.clone() };
            write_record(registry, &older)?;
        }
    }

    let record = Record {
        schema: REGISTRY_SCHEMA_VERSION.to_string(),
        cdm_content_hash: hash,
        dataset_id: signed.certificate.dataset_id.clone(),
        issuer_key: signed.public_key.clone(),
        issued_at: signed.certificate.issuance.timestamp.clone(),
        published_at: published_at.to_string(),
        status: Status::Current,
        certificate: signed.clone(),
    };
    write_record(registry, &record)?;
    write_index(registry)?;
    Ok(record)
}

/// Record the issuer's withdrawal of a published certificate.
///
/// The certificate still verifies afterwards, and says so: bytes that have left cannot be recalled,
/// and pretending otherwise would be the one thing a trust tool must not do. What this changes is
/// the issuer's current word, which is what a lookup reports and a badge shows.
pub fn revoke(
    registry: &std::path::Path,
    cdm_content_hash: &str,
    reason: &str,
    at: &str,
) -> Result<Record, RegistryError> {
    let mut record = lookup_by_hash(registry, cdm_content_hash)?;
    record.status = Status::Revoked {
        reason: reason.to_string(),
        at: at.to_string(),
    };
    write_record(registry, &record)?;
    write_index(registry)?;
    Ok(record)
}

/// The record for one dataset's content hash.
pub fn lookup_by_hash(
    registry: &std::path::Path,
    cdm_content_hash: &str,
) -> Result<Record, RegistryError> {
    let path = record_path(registry, cdm_content_hash);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RegistryError::NotFound(cdm_content_hash.to_string())
        } else {
            io_err(&path, e)
        }
    })?;
    parse_record(&path, &text)
}

/// Every record published for a dataset identity, newest issuance first.
///
/// A dataset id is not unique over time — re-recording produces different bytes under the same name
/// — so this is the form a lookup by *name* has to take: every certificate anyone published for
/// that name, with their statuses, rather than a single answer that hides the others.
pub fn lookup_by_dataset(
    registry: &std::path::Path,
    dataset_id: &str,
) -> Result<Vec<Record>, RegistryError> {
    let mut out: Vec<Record> = read_all(registry)?
        .into_iter()
        .filter(|r| r.dataset_id == dataset_id)
        .collect();
    if out.is_empty() {
        return Err(RegistryError::NotFound(dataset_id.to_string()));
    }
    out.sort_by(|a, b| {
        b.issued_at
            .cmp(&a.issued_at)
            .then_with(|| a.cdm_content_hash.cmp(&b.cdm_content_hash))
    });
    Ok(out)
}

/// Verify a record's certificate, with the same function offline verification uses.
///
/// This is the whole of what a "hosted verification endpoint" can honestly be: the registry holds
/// the document, and verifying it is the same computation wherever it happens. A caller may pin the
/// issuer (`expected_issuer`) and the dataset's own hash (`presented_cdm_hash`) exactly as
/// `veridex verify` does.
pub fn verify_record(
    record: &Record,
    presented_cdm_hash: Option<&str>,
    expected_issuer: Option<&str>,
) -> Result<Verified, CertError> {
    verify(&record.certificate, presented_cdm_hash, expected_issuer)
}

/// Every record in the registry, in no particular order.
pub fn read_all(registry: &std::path::Path) -> Result<Vec<Record>, RegistryError> {
    let dir = registry.join(BY_HASH_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(&dir, e)),
    };
    let mut out = Vec::new();
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| io_err(&dir, e))?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            paths.push(path);
        }
    }
    // Sorted so a listing is deterministic whatever order the filesystem hands back.
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).map_err(|e| io_err(&path, e))?;
        out.push(parse_record(&path, &text)?);
    }
    Ok(out)
}

fn parse_record(path: &std::path::Path, text: &str) -> Result<Record, RegistryError> {
    let record: Record = serde_json::from_str(text).map_err(|e| RegistryError::NotARecord {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    if record.schema != REGISTRY_SCHEMA_VERSION {
        return Err(RegistryError::NotARecord {
            path: path.display().to_string(),
            detail: format!(
                "schema `{}`, expected `{REGISTRY_SCHEMA_VERSION}`",
                record.schema
            ),
        });
    }
    Ok(record)
}

fn write_record(registry: &std::path::Path, record: &Record) -> Result<(), RegistryError> {
    let dir = registry.join(BY_HASH_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;
    let path = record_path(registry, &record.cdm_content_hash);
    let json = serde_json::to_string_pretty(record).expect("a registry record serializes");
    std::fs::write(&path, json).map_err(|e| io_err(&path, e))
}

/// What the index holds: the record list without the certificates, so a browser or a static site
/// can list a registry without downloading every document in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// The dataset's content hash — the key to fetch the full record by.
    pub cdm_content_hash: String,
    /// The dataset identity.
    pub dataset_id: String,
    /// The issuer's public key, hex.
    pub issuer_key: String,
    /// The issuance timestamp.
    pub issued_at: String,
    /// The status word (`current`, `superseded`, `revoked`).
    pub status: String,
}

fn write_index(registry: &std::path::Path) -> Result<(), RegistryError> {
    let mut entries: Vec<IndexEntry> = read_all(registry)?
        .into_iter()
        .map(|r| IndexEntry {
            cdm_content_hash: r.cdm_content_hash,
            dataset_id: r.dataset_id,
            issuer_key: r.issuer_key,
            issued_at: r.issued_at,
            status: r.status.tag().to_string(),
        })
        .collect();
    entries.sort_by(|a, b| {
        b.issued_at
            .cmp(&a.issued_at)
            .then_with(|| a.cdm_content_hash.cmp(&b.cdm_content_hash))
    });
    let path = registry.join(INDEX_FILE);
    let doc = serde_json::json!({
        "schema": REGISTRY_SCHEMA_VERSION,
        "entries": entries,
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&doc).expect("the index serializes"),
    )
    .map_err(|e| io_err(&path, e))
}

/// A status badge for a record, as an SVG a README or dataset card can embed.
///
/// Written by hand rather than fetched from a badge service: a badge that is an image request to
/// somebody else's host tells that host who is reading which dataset's page, and a trust tool
/// should not need a third party to say what it already knows. The colours are the conventional
/// ones (green pass, yellow warnings, red fail), with grey for a certificate the issuer no longer
/// stands behind — a revoked `A` is not a green badge.
pub fn badge(record: &Record) -> String {
    let grade = record.certificate.certificate.trust_score.grade.letter();
    let score = record.certificate.certificate.trust_score.score;
    let (label, colour) = match &record.status {
        Status::Revoked { .. } => ("revoked".to_string(), "#9f9f9f"),
        Status::Superseded { .. } => ("superseded".to_string(), "#9f9f9f"),
        Status::Current => (
            format!("{grade} {score}"),
            match record.certificate.certificate.status {
                crate::Status::Pass => "#2da44e",
                crate::Status::PassWithWarnings => "#d4a72c",
                crate::Status::Fail => "#cf222e",
            },
        ),
    };
    // Sized from the text so a long label is not clipped: 7 px per character is the width of the
    // 11 px sans-serif digits and letters this uses, plus padding.
    let left = 54u32;
    let right = 14 + 7 * label.chars().count() as u32;
    let total = left + right;
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{total}\" height=\"20\" \
         role=\"img\" aria-label=\"veridex: {label}\">\
         <title>veridex: {label}</title>\
         <rect width=\"{left}\" height=\"20\" fill=\"#555\"/>\
         <rect x=\"{left}\" width=\"{right}\" height=\"20\" fill=\"{colour}\"/>\
         <g fill=\"#fff\" text-anchor=\"middle\" \
         font-family=\"DejaVu Sans,Verdana,Geneva,sans-serif\" font-size=\"11\">\
         <text x=\"{}\" y=\"14\">veridex</text>\
         <text x=\"{}\" y=\"14\">{label}</text></g></svg>",
        left / 2,
        left + right / 2
    )
}

/// The human rendering of a lookup.
pub fn render_record(record: &Record) -> String {
    let cert = &record.certificate.certificate;
    let mut out = String::new();
    out.push_str(&format!("Veridex registry — {}\n\n", record.dataset_id));
    out.push_str(&format!("  status      {}\n", record.status.tag()));
    if let Status::Revoked { reason, at } = &record.status {
        out.push_str(&format!("  revoked     {at}"));
        if !reason.is_empty() {
            out.push_str(&format!(" — {reason}"));
        }
        out.push('\n');
    }
    if let Status::Superseded { by } = &record.status {
        out.push_str(&format!("  superseded  by {}…\n", &by[..by.len().min(16)]));
    }
    out.push_str(&format!(
        "  grade       {} ({})\n",
        cert.trust_score.grade.letter(),
        cert.trust_score.score
    ));
    out.push_str(&format!(
        "  verdict     {}\n",
        crate::status_label(cert.status)
    ));
    out.push_str(&format!(
        "  bound to    {}\n",
        &record.cdm_content_hash[..record.cdm_content_hash.len().min(16)]
    ));
    out.push_str(&format!(
        "  issuer      {}…\n",
        &record.issuer_key[..record.issuer_key.len().min(16)]
    ));
    out.push_str(&format!("  issued      {}\n", record.issued_at));
    out.push_str(&format!("  published   {}\n", record.published_at));
    out.push('\n');
    out.push_str("  The certificate in this record verifies offline, by itself, with the key it\n");
    out.push_str("  names — a registry cannot make one valid and cannot withdraw one. What it\n");
    out.push_str("  records is the issuer's current word about it.\n");
    out
}

/// The machine rendering of a lookup.
pub fn render_record_json(record: &Record) -> String {
    serde_json::to_string_pretty(record).expect("a registry record serializes")
}
