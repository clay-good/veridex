//! The nutrition label: a certificate rendered for a dataset card.
//!
//! A certificate is a document a machine verifies. A **label** is the same facts in the form a
//! person meets them — pasted into a Hugging Face dataset card, a README, a PR description — which
//! is how a certificate actually travels. The two must not diverge, so the label is rendered from
//! the signed certificate alone: no re-run, no dataset, nothing that could describe a different
//! verdict than the one that was signed.
//!
//! Markdown, because the destination is a dataset card. It stays legible as plain text when it is
//! read in a terminal, which is the other place it is looked at.
//!
//! Two things the label states rather than implies, both required of it:
//!
//! - **A certificate is a statement of fact, not an endorsement.** It records what was checked and
//!   what was found. A label that read as a seal of approval would be the single most misleading
//!   artifact this project could produce.
//! - **Whether the issuer was verified.** A label rendered without a trusted key is internally
//!   consistent and says nothing about *who* made the claim, and it says so in the label itself —
//!   the caveat has to travel with the pasted text, not stay in the terminal that produced it.

use std::fmt::Write as _;

use crate::certificate::document::Certificate;
use crate::certificate::render::{narrowing_clauses, readiness_verdict, status_label};
use crate::certificate::signing::SignedCertificate;

/// Render a signed certificate as a Markdown nutrition label.
///
/// `issuer_verified` is the caller's trust decision, carried into the text: a label produced without
/// a trusted issuer key says so, because that caveat has to survive being pasted somewhere else.
pub fn render_label(signed: &SignedCertificate, issuer_verified: bool) -> String {
    let cert: &Certificate = &signed.certificate;
    let score = &cert.trust_score;
    let counts = &cert.findings_summary.by_severity;
    let coverage = &cert.provenance_coverage;
    let mut out = String::new();

    let _ = writeln!(out, "## Veridex trust label");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "**Grade {} — {}/100** · data {} · provenance {}% · {}",
        score.grade.letter(),
        score.score,
        score.data_score,
        score.provenance_pct,
        status_label(cert.status)
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "| | |");
    let _ = writeln!(out, "| --- | --- |");
    let _ = writeln!(out, "| Dataset | `{}` |", cert.dataset_id);
    let _ = writeln!(
        out,
        "| Content hash | `{}` |",
        abbreviate(&cert.cdm_content_hash)
    );
    let _ = writeln!(
        out,
        "| Findings | {} error · {} warning · {} info |",
        counts.error, counts.warning, counts.info
    );
    let _ = writeln!(
        out,
        "| Provenance | {} known · {} attested · {} unknown |",
        coverage.known, coverage.asserted, coverage.unknown
    );
    if !cert.findings_summary.by_category.is_empty() {
        let families: Vec<String> = cert
            .findings_summary
            .by_category
            .iter()
            .map(|(family, count)| format!("{family} {count}"))
            .collect();
        let _ = writeln!(out, "| By family | {} |", families.join(" · "));
    }
    // A check that crashed produced no findings, which is not the same as finding nothing — and the
    // label is exactly where that difference would otherwise be lost between a clean count and a
    // clean dataset.
    if !cert.checks_errored.is_empty() {
        let _ = writeln!(
            out,
            "| Checks that failed to run | {} |",
            cert.checks_errored.join(", ")
        );
    }
    if !cert.categories_skipped.is_empty() {
        let skipped: Vec<&str> = cert.categories_skipped.iter().map(|c| c.tag()).collect();
        let _ = writeln!(out, "| Families not run | {} |", skipped.join(", "));
    }
    if let Some(record) = &cert.attestation {
        let _ = writeln!(
            out,
            "| Attested | {} — by producer key `{}` |",
            record.keys.join(", "),
            abbreviate(&record.producer_key)
        );
    }
    if let Some(readiness) = &cert.readiness {
        let met = readiness
            .criteria
            .iter()
            .filter(|criterion| criterion.passed)
            .count();
        let _ = writeln!(
            out,
            "| Readiness (`{}`) | {} — {} of {} criteria met |",
            readiness.profile,
            readiness_verdict(readiness),
            met,
            readiness.criteria.len()
        );
    }
    let _ = writeln!(
        out,
        "| Checked by | veridex {} · rubric {} |",
        cert.veridex_version, score.rubric_version
    );
    let _ = writeln!(
        out,
        "| Issued | {} by key `{}` |",
        cert.issuance.timestamp,
        abbreviate(&cert.issuance.key_id)
    );
    let _ = writeln!(out);

    // Everything that qualifies the numbers above, in the same document as the numbers.
    let narrowed = narrowing_clauses(&cert.effective_config);
    if !narrowed.is_empty() {
        let _ = writeln!(
            out,
            "> **Narrowed run.** This score was earned within a reduced selection, not over the \
             full catalog at its default thresholds ({}).",
            narrowed.join("; ")
        );
        let _ = writeln!(out);
    }
    if !issuer_verified {
        let _ = writeln!(
            out,
            "> **Issuer not verified.** This certificate is internally consistent, but nothing here \
             establishes who issued it. Verify it against a trusted public key before relying on it."
        );
        let _ = writeln!(out);
    }

    let _ = writeln!(
        out,
        "Verify offline: `veridex verify <dataset> --certificate <certificate.json> --key \
         <issuer.pub>`"
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "*A Veridex certificate records what was checked and what was found. It is a statement of \
         fact about a specific dataset, not an endorsement of it.*"
    );
    out
}

/// First and last eight characters of a long hex string, so a label stays readable while still
/// pinning enough of the value to compare by eye.
fn abbreviate(hex: &str) -> String {
    if hex.len() <= 20 {
        return hex.to_string();
    }
    format!("{}…{}", &hex[..8], &hex[hex.len() - 8..])
}
