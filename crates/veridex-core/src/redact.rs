//! Redaction for a report that leaves the building.
//!
//! A Veridex report is diagnostics, and diagnostics quote the dataset: stream keys, task strings,
//! annotator names, licenses, file paths. All of that is exactly what a team cannot hand to a
//! customer, a vendor, or a public issue tracker — while the thing they *want* to hand over, the
//! findings and the score, carries no such problem.
//!
//! So redaction is a rendering-time substitution, not a different run. Every identifier is replaced
//! by a stable placeholder (`stream#1`, `text#2`), stable within one report so a reader can still
//! tell that two findings concern the same stream, and meaningless outside it. The verdict, the
//! score, the exit code, and the CDM content hash are untouched: the report still says what is
//! wrong, how badly, and which dataset it is about — the hash is what lets the holder of the data
//! match the report to it.
//!
//! **What is removed:** the dataset identifier, stream names, task and label text, and provenance
//! element values, wherever they appear in a finding's message or location.
//!
//! **What is kept, deliberately:** episode indices, timestamps, frame counts, and every measured
//! quantity (a 210 ms drift, a 12σ outlier, a saturated fraction). Those are the finding. A report
//! that removed them would not be a redacted report, it would be an empty one — and this module
//! says so in the report itself rather than letting a reader assume otherwise.
//!
//! The disclosure rides as an `info` finding ([`REDACTION_CODE`]) rather than a header line,
//! because findings are what reach every renderer — terminal, JSON, HTML, SARIF, and `diff`. A
//! rendering-only banner would be invisible to exactly the machine consumer most likely to be
//! handed the redacted document.

use std::collections::BTreeSet;

use crate::cdm::Dataset;
use crate::check::{Category, Finding, Location, Severity};
use crate::engine::Verdict;

/// The `check_id` on the disclosure finding. Not a check: nothing ran to produce it.
pub const REDACTION_CHECK_ID: &str = "report.redaction";

/// The finding code disclosing that a report was redacted.
pub const REDACTION_CODE: &str = "REPORT.REDACTED";

/// The shortest identifier worth substituting.
///
/// A one- or two-character stream name (`x`, `q1`) collides with ordinary text far more often than
/// it hides anything, and over-substituting would corrupt the measurements the report exists to
/// convey. Such a name is left alone, and the disclosure says redaction is best-effort.
const MIN_IDENTIFIER: usize = 3;

/// A stable identifier → placeholder substitution built from one dataset.
pub struct Redactor {
    /// Longest first, so a stream named `arm` cannot chew a hole in `arm/gripper`.
    replacements: Vec<(String, String)>,
}

impl Redactor {
    /// Build the substitution for `dataset`.
    ///
    /// Placeholders are numbered over sorted identifiers, so the same dataset always redacts to the
    /// same report — a redacted report is still reproducible, and two runs of the same dataset are
    /// still comparable to each other.
    pub fn for_dataset(dataset: &Dataset) -> Redactor {
        let mut streams: BTreeSet<&str> = BTreeSet::new();
        let mut text: BTreeSet<&str> = BTreeSet::new();
        let mut values: BTreeSet<&str> = BTreeSet::new();
        for episode in &dataset.episodes {
            for stream in &episode.streams {
                streams.insert(stream.name.as_str());
            }
            if let Some(task) = &episode.task {
                text.insert(task.as_str());
            }
            for label in &episode.labels {
                text.insert(label.value.as_str());
            }
        }
        for provenance in &dataset.provenance {
            for element in &provenance.elements {
                if let Some(value) = &element.value {
                    values.insert(value.as_str());
                }
            }
        }

        let mut replacements: Vec<(String, String)> = Vec::new();
        let add = |set: BTreeSet<&str>, prefix: &str, out: &mut Vec<(String, String)>| {
            for (i, identifier) in set.into_iter().enumerate() {
                if identifier.chars().count() >= MIN_IDENTIFIER {
                    out.push((identifier.to_string(), format!("{prefix}#{}", i + 1)));
                }
            }
        };
        add(streams, "stream", &mut replacements);
        add(text, "text", &mut replacements);
        add(values, "value", &mut replacements);
        if dataset.id.chars().count() >= MIN_IDENTIFIER {
            replacements.push((dataset.id.clone(), "dataset".to_string()));
        }
        // Longest first: a substitution must never be applied inside a longer identifier it is a
        // prefix or substring of, or the longer one survives in pieces.
        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Redactor { replacements }
    }

    /// Substitute every identifier in `text`.
    ///
    /// Substitution is plain and greedy, longest identifier first. It can over-redact — a stream
    /// literally named `state` takes the word "state" with it wherever a message uses it — which is
    /// the safe direction for a document being handed to someone else, and is why the disclosure
    /// calls this best-effort rather than a guarantee.
    pub fn redact_text(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (identifier, placeholder) in &self.replacements {
            if out.contains(identifier.as_str()) {
                out = out.replace(identifier.as_str(), placeholder);
            }
        }
        out
    }

    /// Substitute the stream name a location names, keeping every index and timestamp.
    pub fn redact_location(&self, location: &Location) -> Location {
        match location {
            Location::Dataset => Location::Dataset,
            Location::Episode { episode } => Location::Episode { episode: *episode },
            Location::Stream { episode, stream } => Location::Stream {
                episode: *episode,
                stream: self.redact_text(stream),
            },
            Location::FrameRange {
                episode,
                stream,
                start_frame,
                end_frame,
            } => Location::FrameRange {
                episode: *episode,
                stream: self.redact_text(stream),
                start_frame: *start_frame,
                end_frame: *end_frame,
            },
            Location::TimeRange {
                episode,
                stream,
                start_ts,
                end_ts,
            } => Location::TimeRange {
                episode: *episode,
                stream: self.redact_text(stream),
                start_ts: *start_ts,
                end_ts: *end_ts,
            },
        }
    }

    /// A copy of `verdict` with every identifier substituted and the disclosure finding attached.
    ///
    /// The verdict's status, score inputs, coverage, effective config, and content hash are
    /// unchanged: redaction is about who can read the report, not about what the run concluded.
    /// Only the `info` count moves, by the one finding that says the document was redacted.
    pub fn redact_verdict(&self, verdict: &Verdict) -> Verdict {
        let mut out = verdict.clone();
        for finding in &mut out.findings {
            finding.message = self.redact_text(&finding.message);
            finding.location = self.redact_location(&finding.location);
        }
        out.findings.push(disclosure());
        out.findings.sort_by(crate::check::finding_order);
        out.counts.info += 1;
        out
    }
}

/// The finding that says the report was redacted, and precisely what that did and did not remove.
fn disclosure() -> Finding {
    Finding::new(
        REDACTION_CHECK_ID,
        Category::Structural,
        Severity::Info,
        Location::Dataset,
        REDACTION_CODE,
        "this report was redacted for sharing: the dataset identifier, stream names, task and \
         label text, and provenance values were replaced with stable placeholders (`stream#1`, \
         `text#2`, `value#3`), consistent within this report and meaningless outside it",
    )
    .with_risk(
        "Every measured quantity is still here — episode indices, timestamps, frame counts, \
         drifts, and outlier magnitudes are what a finding *is*, and a report without them would \
         say nothing. Redaction is also best-effort substitution over text: an identifier shorter \
         than three characters is left alone, and a name that is also an ordinary word may be \
         replaced where it was not an identifier. Read this as a report you may share, not as a \
         guarantee that nothing about the data can be inferred from it.",
    )
    .with_remedy(
        "Share this document rather than the unredacted report. The CDM content hash is retained \
         deliberately: it is what lets whoever holds the dataset match this report to it.",
    )
}
