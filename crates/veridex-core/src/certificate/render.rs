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
/// The certificate's own wording for a verdict status, shared so the issuing side, the verifying
/// side, and the terminal all say the same word about the same result.
pub fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pass => "pass",
        Status::PassWithWarnings => "pass (warnings)",
        Status::Fail => "fail",
    }
}

/// The ways a certificate's run departed from the declared catalog, as short human labels. Empty
/// when it ran the whole catalog at declared severities and default thresholds.
///
/// A certificate issued from a narrowed run scores what it measured, which can be far less than the
/// catalog. Signed and genuine, and still not a verdict on the dataset — so the limit is named
/// rather than left inside `effective_config` for a reader who thought to look. Shared by the
/// terminal and JSON renderers so the two cannot disagree about whether a run was narrowed.
pub(crate) fn narrowing_clauses(cfg: &crate::engine::EffectiveConfig) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(only) = &cfg.only_checks {
        out.push(match only.is_empty() {
            true => "only_checks = none".to_string(),
            false => format!("only_checks = {}", only.join(", ")),
        });
    }
    if let Some(cats) = &cfg.categories {
        let names: Vec<&str> = cats.iter().map(|c| c.tag()).collect();
        // `categories = []` selects nothing at all, and is the one case where saying so matters
        // most; it must not trail off as "categories = ".
        out.push(match names.is_empty() {
            true => "categories = none".to_string(),
            false => format!("categories = {}", names.join(", ")),
        });
    }
    if !cfg.disabled_checks.is_empty() {
        out.push(format!("disabled = {}", cfg.disabled_checks.join(", ")));
    }
    if !cfg.severity_overrides.is_empty() {
        let pairs: Vec<String> = cfg
            .severity_overrides
            .iter()
            .map(|(id, sev)| format!("{id} -> {}", sev.tag()))
            .collect();
        out.push(format!("severity overridden: {}", pairs.join(", ")));
    }
    // A *loosened* threshold narrows a run without deselecting anything — the check runs, measures
    // the defect, and passes it — so it belongs here beside the selections that remove checks
    // outright. A tightened one does not: it can only lower the score, so a reader warned about it
    // would be warned that the run was harder on the data than the catalog asks.
    let moved = crate::report::loosened_tolerances(&cfg.tolerances);
    if !moved.is_empty() {
        out.push(format!("thresholds loosened: {}", moved.join(", ")));
    }
    out
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
/// One line naming who asserted which provenance elements, when a certificate carries an
/// attestation.
///
/// The offline reader is the whole point of a certificate: they cannot re-run Veridex, and a trust
/// score raised by someone's signature rather than by the data is exactly what they need to see —
/// with the key, so they can decide whether they trust it.
fn attestation_line(cert: &crate::certificate::Certificate) -> Option<String> {
    let record = cert.attestation.as_ref()?;
    Some(format!(
        "  attested:   {} by producer key {} ({})",
        record.keys.join(", "),
        record.producer_key,
        record.timestamp
    ))
}

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
    let narrowed = narrowing_clauses(&cert.effective_config);
    if !narrowed.is_empty() {
        let _ = writeln!(
            out,
            "⚠ narrowed run: {} check(s) ran ({}) — this score was earned within that selection, \
             not over the full catalog",
            cert.checks_run.len(),
            narrowed.join("; ")
        );
    }
    // `checks_errored` was added to the document for one stated reason: the offline reader is the
    // whole point of it, and they cannot re-run Veridex to find out. It was added to the document
    // and never wired into the reader — neither renderer touched it — so a crashed check sat under
    // `checks_run` beside an all-zero severity count and the certificate read as a clean, complete
    // verdict over a check that measured nothing. `check`'s own terminal, JSON, and SARIF outputs
    // all surface errored checks; the certificate path was the one renderer that lost them.
    if !cert.checks_errored.is_empty() {
        let _ = writeln!(
            out,
            "⚠ {} check(s) errored and measured nothing: {} — their silence is not evidence about \
             the data",
            cert.checks_errored.len(),
            cert.checks_errored.join(", ")
        );
    }
    // The same gap, one axis over: a whole category that ran nothing. `verified_json` has carried
    // this since it was added; the terminal render never did, so a certificate whose config is
    // entirely default but whose `video` and `autonomy` categories ran nothing printed as a clean
    // grade-A pass with no warning at all.
    if !cert.categories_skipped.is_empty() {
        let names: Vec<&str> = cert.categories_skipped.iter().map(|c| c.tag()).collect();
        let _ = writeln!(
            out,
            "⚠ {} categor{} ran no checks: {}",
            cert.categories_skipped.len(),
            if cert.categories_skipped.len() == 1 {
                "y"
            } else {
                "ies"
            },
            names.join(", ")
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
    // `data` is the data-quality sub-score, the same number `check` prints. It used to be filled
    // with the verdict *status*, so this line read "[data pass · provenance 66%]" — the one
    // sub-score `docs/rubric-v1.md` promises the certificate always shows was never printed, and
    // the substitute flattered: a verdict with ten warnings and no errors is `pass (warnings)`
    // beside a data score of 60. The status has its own line above.
    let _ = writeln!(out, "  status:     {}", status_label(cert.status));
    let _ = writeln!(
        out,
        "  trust:      {} ({})  [data {} · provenance {}%]",
        cert.trust_score.grade.letter(),
        cert.trust_score.score,
        cert.trust_score.data_score,
        cert.trust_score.provenance_pct
    );
    if let Some(line) = attestation_line(cert) {
        let _ = writeln!(out, "{line}");
    }
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
    // Signed all along and, until the JSON render was fixed, compared against nothing and reported
    // nowhere. The terminal reader has the same need: a certificate from an older Veridex, whose
    // catalog lacked today's checks, otherwise reads as current.
    let _ = writeln!(out, "  issued by:  veridex {}", cert.veridex_version);
    // Findings by code, because the coarser rollups cannot say *what* a run could not measure.
    // `statistical: 1` beside "46 checks run, no categories skipped" is what a dataset whose streams
    // hold no summarizable values signed as — while all five statistical checks had nothing to
    // measure and seven cross-episode checks had nothing to compare. The abstention findings exist
    // so a pass cannot mean "nothing was asked", and every renderer of `check` surfaces them by
    // code; the certificate reader, who cannot re-run Veridex, was the one who got the rollup.
    //
    // Codes are declared by checks, so this line is bounded by the catalog. An older certificate
    // carries no code map and prints no line, rather than an empty one implying no findings.
    if !cert.findings_summary.by_code.is_empty() {
        let codes: Vec<String> = cert
            .findings_summary
            .by_code
            .iter()
            .map(|(code, n)| format!("{code} {n}"))
            .collect();
        let _ = writeln!(out, "  findings:   {}", codes.join(", "));
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
        // Signed since the document gained the field, and read by neither renderer until now: a
        // CI gate keying on `verified && status != "fail"` could not tell that a check crashed,
        // because a crash yields `pass (warnings)` — indistinguishable from ordinary warnings.
        "checks_errored": cert.checks_errored,
        "categories_skipped": cert.categories_skipped,
        "effective_config": cert.effective_config,
        // The terminal render warns "⚠ narrowed run" here; until now the machine render did not,
        // and a CI gate keying on `verified && status == "pass"` — the obvious gate to write — was
        // fully defeated by a `veridex.toml` in the working directory. `effective_config` carried
        // the facts, but only for a reader who already suspected and knew how to interpret them.
        // This is the same reason `dataset_checked` and `issuer_verified` are booleans here.
        "narrowed": !narrowing_clauses(&cert.effective_config).is_empty(),
        "narrowing": narrowing_clauses(&cert.effective_config),
        // Signed all along, compared against nothing and reported nowhere: a certificate from an
        // older Veridex whose catalog lacked today's checks read as current.
        "veridex_version": cert.veridex_version,
        // Who asserted what, when a producer attestation raised this certificate's provenance
        // coverage. Null for an ordinary certificate. A machine gate that trusts only its own
        // producers has no other way to see that a third of the score came from someone else's key.
        "attestation": cert.attestation,
        // Findings by code — what the run found, and equally what it could not measure. A gate
        // keying on `status` alone cannot tell a clean statistical family from one where every
        // statistical check had nothing to measure, which is the difference between evidence and
        // silence. Catalog-bounded, and empty for a certificate issued before the field existed.
        "findings_by_code": cert.findings_summary.by_code,
    });
    if let Some(readiness) = &cert.readiness {
        doc["readiness"] = serde_json::to_value(readiness).expect("readiness serializes");
    }
    serde_json::to_string_pretty(&doc).expect("verification summary serializes")
}
