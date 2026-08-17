//! Rendering a certificate's substance — for the terminal and as JSON.
//!
//! A certificate is only useful offline if a reader can *see* what it attests without parsing the
//! document by hand: what it is bound to, the trust score, and — for a readiness certificate — each
//! criterion's verdict. These helpers are shared by the CLI and the Python bindings so both surfaces
//! report the identical thing (design D1).
//!
//! Everything rendered here comes from the signed document, so it is exactly what the signature
//! covers: a tampered readiness block does not verify at all, and there is nothing to render.

use serde_json::json;

use crate::certificate::document::ReadinessReport;
use crate::certificate::signing::{SignedCertificate, Verified};
use crate::engine::Status;

/// The readiness verdict as a short label: `READY`, `NOT READY`, or `N/A` when the profile does not
/// apply to this dataset (an autonomy profile against a non-rig), which is never a vacuous pass.
pub fn readiness_verdict(report: &ReadinessReport) -> &'static str {
    if !report.applicable {
        "N/A (profile does not apply)"
    } else if report.ready {
        "READY"
    } else {
        "NOT READY"
    }
}

/// Render a readiness report as an indented terminal block: the profile verdict, then each criterion
/// with the guarantee it attests. `indent` is the leading whitespace for the profile line.
pub fn render_readiness(report: &ReadinessReport, indent: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{indent}{} profile: {}",
        report.profile,
        readiness_verdict(report)
    );
    for c in &report.criteria {
        // A criterion whose check never ran is neither a pass nor a failure of the data — it is a
        // gap in what was examined, and saying so is the whole point of reporting per criterion.
        let (mark, suffix) = match (c.ran, c.passed) {
            (false, _) => ("?", " [check did not run]"),
            (true, true) => ("✓", ""),
            (true, false) => ("✗", ""),
        };
        let _ = writeln!(
            out,
            "{indent}  {mark} {} — {}{suffix}",
            c.check_id, c.threshold
        );
    }
    out
}

/// The status label used in certificate rendering.
fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pass => "pass",
        Status::PassWithWarnings => "pass (warnings)",
        Status::Fail => "fail",
    }
}

/// Render a successful verification for the terminal: who signed it, when, what data it is bound to,
/// what it scored, and — when present — the per-criterion readiness verdict.
///
/// `issuer_verified` says whether the signature was checked against a **trusted** issuer key. A valid
/// signature alone only proves the document is self-consistent: anyone can sign a certificate that
/// says whatever they like about data they hold, so an unverified issuer is called out, not implied.
///
/// `dataset_checked` says whether the certificate's bound content hash was compared against an actual
/// dataset. The same reasoning applies: without a dataset to compare, the `bound to:` line is an echo
/// of the certificate's own claim rather than a confirmation of anything, and a certificate issued
/// for one dataset presented alongside another verifies exactly like a genuine pairing.
pub fn render_verified(
    signed: &SignedCertificate,
    verified: &Verified,
    issuer_verified: bool,
    dataset_checked: bool,
) -> String {
    use std::fmt::Write;
    let cert = &signed.certificate;
    let mut out = String::new();
    let _ = writeln!(out, "✓ certificate verified");
    if !issuer_verified {
        let _ = writeln!(
            out,
            "⚠ issuer NOT verified: this certificate is internally consistent, but anyone could \
             have issued it — re-run with --key <trusted-public-key> to check who did"
        );
    }
    if !dataset_checked {
        let _ = writeln!(
            out,
            "⚠ dataset NOT checked: no dataset was given, so nothing confirms this certificate \
             describes the data you hold — pass the dataset path to compare its content hash"
        );
    }
    // A certificate issued from a narrowed run scores what it measured, which can be far less than
    // the catalog. Signed and genuine, and still not a verdict on the dataset — so the limit is
    // named here rather than left inside `effective_config` for a reader who thought to look.
    let cfg = &cert.effective_config;
    let mut narrowed: Vec<String> = Vec::new();
    if let Some(only) = &cfg.only_checks {
        narrowed.push(format!("only_checks = {}", only.join(", ")));
    }
    if let Some(cats) = &cfg.categories {
        let names: Vec<&str> = cats.iter().map(|c| c.tag()).collect();
        narrowed.push(format!("categories = {}", names.join(", ")));
    }
    if !cfg.disabled_checks.is_empty() {
        narrowed.push(format!("disabled = {}", cfg.disabled_checks.join(", ")));
    }
    if !cfg.severity_overrides.is_empty() {
        let pairs: Vec<String> = cfg
            .severity_overrides
            .iter()
            .map(|(id, sev)| format!("{id} -> {}", sev.tag()))
            .collect();
        narrowed.push(format!("severity overridden: {}", pairs.join(", ")));
    }
    if !narrowed.is_empty() {
        let _ = writeln!(
            out,
            "⚠ narrowed run: {} check(s) ran ({}) — this score was earned within that selection, \
             not over the full catalog",
            cert.checks_run.len(),
            narrowed.join("; ")
        );
    }
    let _ = writeln!(out, "  issuer key: {}", verified.key_id);
    let _ = writeln!(out, "  issued at:  {}", verified.timestamp);
    let _ = writeln!(out, "  dataset:    {}", cert.dataset_id);
    // The hash prefix is what binds the certificate to specific bytes; showing it lets a reader
    // match the certificate to a dataset by eye.
    let bound = cert
        .cdm_content_hash
        .get(..16)
        .unwrap_or(&cert.cdm_content_hash);
    let _ = writeln!(out, "  bound to:   {bound}…");
    let _ = writeln!(
        out,
        "  trust:      {} ({})  [data {} · provenance {}%]",
        cert.trust_score.grade.letter(),
        cert.trust_score.score,
        status_label(cert.status),
        cert.trust_score.provenance_pct
    );
    // A score means something only within the rubric that produced it — the rubric is versioned for
    // exactly that reason, and `verify` accepts a certificate scored under any of them (a newer
    // issuer's document is still perfectly readable; refusing it would be the wrong trade). What
    // must not happen is a reader comparing a v2 number against v1 expectations without being told,
    // so a rubric this build does not use is named right beside the score it produced.
    if cert.trust_score.rubric_version != crate::certificate::RUBRIC_VERSION {
        let _ = writeln!(
            out,
            "⚠ scored under rubric {} (this build uses {}) — the number above is not comparable to \
             scores from this version",
            cert.trust_score.rubric_version,
            crate::certificate::RUBRIC_VERSION
        );
    }
    if let Some(readiness) = &cert.readiness {
        out.push_str(&render_readiness(readiness, "  "));
    }
    out
}

/// The machine-readable summary of a successful verification, as pretty JSON. Carries the same facts
/// as [`render_verified`], including the signed readiness block verbatim when present, and whether
/// the issuer was checked against a trusted key.
pub fn verified_json(
    signed: &SignedCertificate,
    verified: &Verified,
    issuer_verified: bool,
    dataset_checked: bool,
) -> String {
    let cert = &signed.certificate;
    let mut doc = json!({
        "verified": true,
        "issuer_verified": issuer_verified,
        // Whether the bound content hash was compared against a real dataset. A CI script keying on
        // `verified: true` alone would accept a certificate issued for entirely different data.
        "dataset_checked": dataset_checked,
        "key_id": verified.key_id,
        "timestamp": verified.timestamp,
        "dataset_id": cert.dataset_id,
        "cdm_content_hash": cert.cdm_content_hash,
        "status": cert.status,
        "trust_score": cert.trust_score,
        // The scope the score was earned within, so a machine reader can tell a full run from a
        // one-check run without parsing the signed document itself.
        "checks_run": cert.checks_run.len(),
        "categories_skipped": cert.categories_skipped,
        "effective_config": cert.effective_config,
    });
    if let Some(readiness) = &cert.readiness {
        doc["readiness"] = serde_json::to_value(readiness).expect("readiness serializes");
    }
    serde_json::to_string_pretty(&doc).expect("verification summary serializes")
}
