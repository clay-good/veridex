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
//! **What is removed:** the dataset identifier, stream names, task and label text, provenance
//! element values (including values a producer attested, which are not in the CDM at all), media and source *paths*, coordinate-frame names, and dataset metadata values —
//! wherever they appear in a finding's message or location. Paths get a second, pattern-based pass
//! on top of the enumerated ones, because a path is the string a dataset is most identifying in and
//! a check can quote one this module never saw.
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

use std::collections::{BTreeMap, BTreeSet};

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
    /// Path-shaped tokens met while redacting, and the placeholder each was given. Numbered in the
    /// order they are first seen, over findings that are already in the verdict's deterministic
    /// order, so the same report always redacts identically.
    paths: BTreeMap<String, String>,
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
        let mut paths: BTreeSet<&str> = BTreeSet::new();
        for episode in &dataset.episodes {
            for stream in &episode.streams {
                streams.insert(stream.name.as_str());
                // The video family names the media file it could not read or pair, and that path
                // usually carries the stream name and the operator's own directory naming.
                if let Some(media) = &stream.media {
                    paths.insert(media.uri.as_str());
                }
                // A coordinate frame names a sensor mount on a real rig (`acme_wrist_cam_link`).
                if let Some(frame) = &stream.frame_id {
                    values.insert(frame.as_str());
                }
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
        // Dataset metadata is where a source records the robot model, the site, the operator.
        for (_, value) in &dataset.metadata {
            values.insert(value.as_str());
        }
        // Calibration names every coordinate frame on the rig.
        if let Some(calibration) = &dataset.calibration {
            for transform in &calibration.transforms {
                values.insert(transform.parent_frame.as_str());
                values.insert(transform.child_frame.as_str());
            }
            for intrinsics in &calibration.intrinsics {
                streams.insert(intrinsics.stream.as_str());
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
        add(paths, "path", &mut replacements);
        if dataset.id.chars().count() >= MIN_IDENTIFIER {
            replacements.push((dataset.id.clone(), "dataset".to_string()));
        }
        // Longest first: a substitution must never be applied inside a longer identifier it is a
        // prefix or substring of, or the longer one survives in pieces.
        replacements.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        Redactor {
            replacements,
            paths: BTreeMap::new(),
        }
    }

    /// Also redact values a producer attested.
    ///
    /// Attested values are not in the CDM — that is the whole design — so a redactor built from the
    /// dataset alone does not know them, and the conflict finding quotes them verbatim. A producer
    /// who attests an operator's address or an internal licence term and then shares a redacted
    /// report would publish exactly the string redaction exists to remove.
    pub fn and_attested(mut self, values: impl IntoIterator<Item = String>) -> Redactor {
        let unique: BTreeSet<String> = values.into_iter().collect();
        for (i, value) in unique.into_iter().enumerate() {
            if value.chars().count() >= MIN_IDENTIFIER {
                self.replacements
                    .push((value, format!("attested#{}", i + 1)));
            }
        }
        self.replacements
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        self
    }

    /// Also redact the source files an ingest declined to read.
    ///
    /// Those paths reach the report through `COVERAGE.SOURCE_UNREAD`, and they are not in the CDM:
    /// the whole point of an unread source is that it did not become data. Without this the one
    /// finding whose entire content is a list of file paths would pass through untouched.
    pub fn and_unread_sources<'a>(
        mut self,
        sources: impl IntoIterator<Item = &'a str>,
    ) -> Redactor {
        let mut extra: Vec<(String, String)> = Vec::new();
        let unique: BTreeSet<&str> = sources.into_iter().collect();
        for (i, path) in unique.into_iter().enumerate() {
            if path.chars().count() >= MIN_IDENTIFIER {
                extra.push((path.to_string(), format!("unread#{}", i + 1)));
            }
        }
        self.replacements.extend(extra);
        self.replacements
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        self
    }

    /// Substitute every identifier in `text`.
    ///
    /// Substitution is plain and greedy, longest identifier first. It can over-redact — a stream
    /// literally named `state` takes the word "state" with it wherever a message uses it — which is
    /// the safe direction for a document being handed to someone else, and is why the disclosure
    /// calls this best-effort rather than a guarantee.
    pub fn redact_text(&mut self, text: &str) -> String {
        let mut out = text.to_string();
        for (identifier, placeholder) in &self.replacements {
            if out.contains(identifier.as_str()) {
                out = out.replace(identifier.as_str(), placeholder);
            }
        }
        self.redact_paths(&out)
    }

    /// Replace what is left that looks like a filesystem path.
    ///
    /// The enumerated identifiers cover what the CDM holds, and a check can quote a path the CDM
    /// never held — a directory it looked in, a file it failed to open. A path is also the single
    /// most identifying string a dataset has (`data/acme-warehouse-pilot/...`), so the backstop
    /// errs toward substituting: any surviving token carrying a `/` or `\` is replaced.
    fn redact_paths(&mut self, text: &str) -> String {
        if !text.contains('/') && !text.contains('\\') {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        // A "token" ends at whitespace or at the punctuation a message wraps identifiers in.
        let is_boundary = |c: char| c.is_whitespace() || matches!(c, '`' | '(' | ')' | ',' | ';');
        for part in text.split_inclusive(is_boundary) {
            let (token, tail) = match part.chars().last() {
                Some(last) if is_boundary(last) => {
                    (&part[..part.len() - last.len_utf8()], Some(last))
                }
                _ => (part, None),
            };
            let trimmed = token.trim_end_matches('.');
            if trimmed.contains('/') || trimmed.contains('\\') {
                let next = self.paths.len() + 1;
                let placeholder = self
                    .paths
                    .entry(trimmed.to_string())
                    .or_insert_with(|| format!("path#{next}"))
                    .clone();
                out.push_str(&placeholder);
                out.push_str(&token[trimmed.len()..]);
            } else {
                out.push_str(token);
            }
            if let Some(tail) = tail {
                out.push(tail);
            }
        }
        out
    }

    /// Substitute the stream name a location names, keeping every index and timestamp.
    pub fn redact_location(&mut self, location: &Location) -> Location {
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
    pub fn redact_verdict(&mut self, verdict: &Verdict) -> Verdict {
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
         label text, provenance values, coordinate-frame and metadata values, and file paths were \
         replaced with stable placeholders (`stream#1`, `text#2`, `value#3`, `path#4`), consistent \
         within this report and meaningless outside it",
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
