//! Relating two certificates for one dataset: which of them describes it now.
//!
//! A certificate is a statement about particular bytes — it binds a CDM content hash — so
//! re-recording a dataset, fixing the fault a certificate reported, or re-exporting a shard produces
//! a *different* dataset by the only identity that matters to a verifier. Both certificates stay
//! independently valid offline forever, which is the point of signing them, and that is exactly what
//! leaves a reader holding two valid documents about "the same dataset" with nothing to say which
//! one is current.
//!
//! This module answers that from what the two documents already carry, with no clock and no network:
//! the signing key each one verified against, the dataset identity, the content hash each is bound
//! to, and the issuance timestamp each records. Nothing here revokes anything — a supersession is a
//! statement about which document describes the dataset now, and the superseded one remains a true
//! statement about the bytes it was issued over.

use serde_json::json;

use super::document::Certificate;
use super::render::status_label;

/// How a later certificate relates to an earlier one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Same issuer, same dataset, different bytes, and the later one was issued later: it
    /// supersedes.
    Supersedes,
    /// Same issuer, same dataset, **same** bytes: the dataset was certified twice, which is a
    /// re-issue rather than a supersession. The two may still differ in what they claim (a
    /// different profile, a narrowed catalog), and nothing about the data changed.
    Reissue,
    /// Everything matches except the order: the document presented as the later one was issued
    /// first.
    OutOfOrder,
    /// Same issuer and dataset, and at least one timestamp is not an instant this crate can order.
    Unordered,
    /// The two were signed by different keys. A certificate cannot supersede one issued by somebody
    /// else, whatever its timestamp says.
    DifferentIssuer,
    /// Different datasets. Nothing relates them.
    Unrelated,
}

impl Relation {
    /// The stable tag used in machine output.
    pub fn tag(self) -> &'static str {
        match self {
            Relation::Supersedes => "supersedes",
            Relation::Reissue => "reissue",
            Relation::OutOfOrder => "out-of-order",
            Relation::Unordered => "unordered",
            Relation::DifferentIssuer => "different-issuer",
            Relation::Unrelated => "unrelated",
        }
    }

    /// Whether the later certificate supersedes the earlier one. The one question a caller gating
    /// on this needs answered, and the CLI's exit code.
    pub fn is_supersession(self) -> bool {
        self == Relation::Supersedes
    }
}

/// What one certificate says about itself, for the side-by-side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// The key the signature verified against — the authoritative issuer identity, not the
    /// self-asserted `issuance.key_id`.
    pub key: String,
    /// The dataset identity the certificate carries.
    pub dataset_id: String,
    /// The CDM content hash it is bound to.
    pub cdm_content_hash: String,
    /// The issuance timestamp it records, as recorded.
    pub timestamp: String,
    /// Its trust score and status, so a reader learns which way the dataset moved.
    pub score: u8,
    pub grade: String,
    pub status: &'static str,
}

impl Side {
    fn of(certificate: &Certificate, key: &str) -> Side {
        Side {
            key: key.to_ascii_lowercase(),
            dataset_id: certificate.dataset_id.clone(),
            cdm_content_hash: certificate.cdm_content_hash.clone(),
            timestamp: certificate.issuance.timestamp.clone(),
            score: certificate.trust_score.score,
            grade: certificate.trust_score.grade.letter().to_string(),
            status: status_label(certificate.status),
        }
    }
}

/// The relation between two certificates, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supersession {
    pub relation: Relation,
    /// One sentence a reader can act on. Never an accusation: every outcome here is an ordinary
    /// thing for two certificates to be.
    pub reason: String,
    pub earlier: Side,
    pub later: Side,
    /// Trust-score movement from the earlier to the later certificate, where the relation makes the
    /// comparison meaningful at all.
    pub score_delta: Option<i16>,
}

/// Relate two certificates, given the key each one's signature verified against.
///
/// Callers verify both documents first and pass `signed.public_key` for each. That key, not the
/// certificate's own `issuance.key_id`, is the issuer: the label inside the document is chosen by
/// whoever wrote it and is not required to equal the signing key, so relating on it would let a
/// document claim an issuer it cannot sign for.
pub fn relate(
    earlier: &Certificate,
    earlier_key: &str,
    later: &Certificate,
    later_key: &str,
) -> Supersession {
    let a = Side::of(earlier, earlier_key);
    let b = Side::of(later, later_key);
    let delta = i16::from(b.score) - i16::from(a.score);

    let (relation, reason) = if !a.key.eq_ignore_ascii_case(&b.key) {
        (
            Relation::DifferentIssuer,
            format!(
                "these certificates were signed by different keys ({} and {}), so neither \
                 supersedes the other — a supersession is an issuer restating what it certified, \
                 and no issuer speaks for another's documents",
                short(&a.key),
                short(&b.key)
            ),
        )
    } else if a.dataset_id != b.dataset_id {
        (
            Relation::Unrelated,
            format!(
                "these certificates are about different datasets (`{}` and `{}`), so neither \
                 supersedes the other",
                a.dataset_id, b.dataset_id
            ),
        )
    } else if a.cdm_content_hash.eq_ignore_ascii_case(&b.cdm_content_hash) {
        (
            Relation::Reissue,
            format!(
                "both certificates are bound to the same content hash ({}), so this is the same \
                 dataset certified twice rather than a supersession — what differs is what the two \
                 runs claimed about it, not the bytes",
                short(&a.cdm_content_hash)
            ),
        )
    } else {
        match order(&a.timestamp, &b.timestamp) {
            None => (
                Relation::Unordered,
                format!(
                    "the two certificates cannot be ordered: an issuance timestamp has to be an \
                     RFC 3339 UTC instant for this comparison, and `{}` or `{}` is not one. Both \
                     documents remain valid; which is current is not something they state",
                    a.timestamp, b.timestamp
                ),
            ),
            Some(std::cmp::Ordering::Less) => (
                Relation::Supersedes,
                format!(
                    "the later certificate supersedes the earlier one: same issuer, same dataset \
                     `{}`, different bytes, and issued afterwards. The earlier certificate is not \
                     revoked — it remains a true statement about the bytes it was issued over",
                    b.dataset_id
                ),
            ),
            Some(_) => (
                Relation::OutOfOrder,
                "the certificate given as the later one was issued first or at the same instant, \
                 so it cannot supersede the other — pass them the other way round if that is what \
                 you meant"
                    .to_string(),
            ),
        }
    };

    // A score movement only means something when the two describe the same dataset. Across
    // datasets, or across issuers, it is two unrelated numbers subtracted.
    let score_delta = matches!(
        relation,
        Relation::Supersedes | Relation::Reissue | Relation::OutOfOrder | Relation::Unordered
    )
    .then_some(delta);

    Supersession {
        relation,
        reason,
        earlier: a,
        later: b,
        score_delta,
    }
}

/// The first eight characters of a hex identity, for a line a person reads.
fn short(hex: &str) -> String {
    hex.chars().take(8).collect::<String>() + "…"
}

/// Order two RFC 3339 **UTC** instants, or `None` when either is not one.
///
/// Written out rather than pulled in, for the reason the issuance formatter is: this crate reads no
/// clock and depends on no date library. Deliberately strict — exactly `YYYY-MM-DDTHH:MM:SS`
/// followed by an optional fractional part and a `Z` (lower case `t`/`z` accepted, as RFC 3339
/// allows) — because the alternative is guessing at a local offset, and an ordering guessed wrong
/// would name the wrong certificate as current. Anything else is reported unordered, which is a
/// fact about the documents rather than a fault in them.
fn order(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(utc_instant(a)?.cmp(&utc_instant(b)?))
}

/// An RFC 3339 UTC instant as its comparable parts: `(y, m, d, h, min, s, nanos)`.
fn utc_instant(text: &str) -> Option<(i64, u32, u32, u32, u32, u32, u64)> {
    let body = text
        .strip_suffix('Z')
        .or_else(|| text.strip_suffix('z'))
        .or_else(|| text.strip_suffix("+00:00"))?;
    let (date, time) = body
        .split_once('T')
        .or_else(|| body.split_once('t'))
        .or_else(|| body.split_once(' '))?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = two_digits(d.next()?)?;
    let day: u32 = two_digits(d.next()?)?;
    if d.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // A fractional second is optional, and compared as nanoseconds so `.5` and `.50` are one
    // instant rather than two strings.
    let (clock, nanos) = match time.split_once('.') {
        Some((clock, frac)) => {
            if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let digits: String = frac.chars().take(9).collect();
            let scale = 10u64.pow(9 - digits.len() as u32);
            (clock, digits.parse::<u64>().ok()? * scale)
        }
        None => (time, 0),
    };
    let mut t = clock.split(':');
    let hour: u32 = two_digits(t.next()?)?;
    let minute: u32 = two_digits(t.next()?)?;
    let second: u32 = two_digits(t.next()?)?;
    if t.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some((year, month, day, hour, minute, second, nanos))
}

/// A strictly two-digit field, so `2026-3-01` is refused rather than read as March.
fn two_digits(text: &str) -> Option<u32> {
    (text.len() == 2 && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// Render a supersession for a terminal.
pub fn render_supersession(s: &Supersession) -> String {
    let mut out = String::new();
    out.push_str("Certificate supersession\n");
    out.push_str(&format!(
        "  Verdict:  {}\n",
        if s.relation.is_supersession() {
            "SUPERSEDES"
        } else {
            "DOES NOT SUPERSEDE"
        }
    ));
    out.push_str(&format!("  Relation: {}\n", s.relation.tag()));
    if s.earlier.dataset_id == s.later.dataset_id {
        out.push_str(&format!("  Dataset:  {}\n", s.earlier.dataset_id));
    }
    if s.earlier.key.eq_ignore_ascii_case(&s.later.key) {
        out.push_str(&format!("  Issuer:   {} (both)\n", short(&s.earlier.key)));
    }
    for (role, side) in [("Earlier", &s.earlier), ("Later  ", &s.later)] {
        out.push_str(&format!(
            "  {role}: {}  hash {}  trust {} ({})  {}\n",
            side.timestamp,
            short(&side.cdm_content_hash),
            side.score,
            side.grade,
            side.status
        ));
    }
    if let Some(delta) = s.score_delta {
        if s.earlier.status != s.later.status {
            out.push_str(&format!(
                "  Movement: trust {delta:+}, status {} -> {}\n",
                s.earlier.status, s.later.status
            ));
        } else {
            out.push_str(&format!("  Movement: trust {delta:+}\n"));
        }
    }
    out.push_str(&format!("  {}\n", s.reason));
    out
}

/// Render a supersession as machine output.
pub fn render_supersession_json(s: &Supersession) -> String {
    let side = |x: &Side| {
        json!({
            "issuer_key": x.key,
            "dataset_id": x.dataset_id,
            "cdm_content_hash": x.cdm_content_hash,
            "timestamp": x.timestamp,
            "trust_score": x.score,
            "grade": x.grade,
            "status": x.status,
        })
    };
    let doc = json!({
        "relation": s.relation.tag(),
        "supersedes": s.relation.is_supersession(),
        "reason": s.reason,
        "earlier": side(&s.earlier),
        "later": side(&s.later),
        "score_delta": s.score_delta,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_utc_instants_order_by_instant_not_by_string() {
        use std::cmp::Ordering;
        assert_eq!(
            order("2026-03-01T10:00:00Z", "2026-04-02T09:30:00Z"),
            Some(Ordering::Less)
        );
        // A fractional second compares as a quantity: `.5` and `.50` are one instant, and `.9` is
        // after `.10`, which a string comparison gets backwards.
        assert_eq!(
            order("2026-03-01T10:00:00.5Z", "2026-03-01T10:00:00.50Z"),
            Some(Ordering::Equal)
        );
        assert_eq!(
            order("2026-03-01T10:00:00.10Z", "2026-03-01T10:00:00.9Z"),
            Some(Ordering::Less)
        );
        // `+00:00` is the same instant as `Z`.
        assert_eq!(
            order("2026-03-01T10:00:00+00:00", "2026-03-01T10:00:00Z"),
            Some(Ordering::Equal)
        );
    }

    #[test]
    fn anything_not_a_utc_instant_is_refused_rather_than_guessed() {
        // Each of these could be ordered by guessing, and a guess here names the wrong certificate
        // as the current one.
        for text in [
            "1700000000",                // the epoch seconds the parity tests sign with
            "2026-03-01T10:00:00+02:00", // a local offset
            "2026-3-01T10:00:00Z",       // not two digits
            "2026-03-01",                // a date with no time
            "yesterday",
            "",
        ] {
            assert!(
                utc_instant(text).is_none(),
                "`{text}` must not parse as a UTC instant"
            );
        }
    }
}
