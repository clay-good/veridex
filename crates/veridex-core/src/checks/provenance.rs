//! Provenance-completeness checks: surface missing or internally inconsistent provenance rather
//! than accepting gaps by default. These gaps flow into the certificate's `unknown` section.

use std::collections::HashMap;

use crate::cdm::{Dataset, ProvenanceClass};
use crate::check::{Category, Check, CheckContext, Finding, Location, Scope, Severity};

/// A provenance element Veridex expects a trustworthy dataset to carry, with the severity of its
/// absence.
struct Expected {
    key: &'static str,
    severity: Severity,
    code: &'static str,
    risk: &'static str,
}

const EXPECTED: &[Expected] = &[
    Expected {
        key: "license",
        severity: Severity::Warning,
        code: "PROVENANCE.MISSING_LICENSE",
        risk: "Without a license the data's usage terms are unknown; training on it may be legally unsafe.",
    },
    Expected {
        key: "sensor",
        severity: Severity::Info,
        code: "PROVENANCE.MISSING_SENSOR",
        risk: "Unknown sensor/device means calibration and domain gaps can't be reasoned about.",
    },
    Expected {
        key: "clock",
        severity: Severity::Info,
        code: "PROVENANCE.MISSING_CLOCK",
        risk: "Unknown clock source undermines cross-stream synchronization guarantees.",
    },
    Expected {
        key: "calibration",
        severity: Severity::Info,
        code: "PROVENANCE.MISSING_CALIBRATION",
        risk: "Missing calibration blocks spatial and multi-camera reasoning.",
    },
    Expected {
        key: "annotator",
        severity: Severity::Info,
        code: "PROVENANCE.MISSING_ANNOTATOR",
        risk: "Unknown annotator/operator identity weakens label accountability.",
    },
    Expected {
        key: "upstream",
        severity: Severity::Info,
        code: "PROVENANCE.MISSING_UPSTREAM",
        risk: "Missing upstream lineage hides where the data came from and what it derives from.",
    },
];

/// The expected elements an adapter can only get from a stream payload.
///
/// `calibration` is decoded from ROS message bodies (a `CameraInfo`, a `TFMessage`), and `upstream`
/// from a per-episode field inside a TFRecord. A run that opens no payload has neither, and it has
/// neither *by request* — so their absence there is a fact about the run, not about the dataset.
/// Every other expected element is read from a manifest, a header or a dataset card, all of which a
/// metadata-only run does read; their absence means the same thing in either mode.
const PAYLOAD_DERIVED: &[&str] = &["calibration", "upstream"];

/// Presence and internal consistency of dataset provenance.
pub struct ProvenanceCompleteness;

impl Check for ProvenanceCompleteness {
    fn id(&self) -> &'static str {
        "provenance.completeness"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "PROVENANCE.INCONSISTENT",
            "PROVENANCE.PLACEHOLDER_VALUE",
            "PROVENANCE.MISSING_LICENSE",
            "PROVENANCE.MISSING_SENSOR",
            "PROVENANCE.MISSING_CLOCK",
            "PROVENANCE.MISSING_CALIBRATION",
            "PROVENANCE.MISSING_ANNOTATOR",
            "PROVENANCE.MISSING_UPSTREAM",
        ]
    }
    fn title(&self) -> &'static str {
        "Provenance completeness and consistency"
    }
    fn category(&self) -> Category {
        Category::Provenance
    }
    fn default_severity(&self) -> Severity {
        Severity::Info
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        self.evaluate(dataset, true)
    }

    /// A run that opened no stream payload cannot tell a dataset with no calibration or lineage from
    /// one whose calibration and lineage live in the payloads it declined to read. Reporting them
    /// missing there measures the request rather than the data — the same defect
    /// `autonomy.calibration-completeness` was fixed for, arriving through provenance instead.
    ///
    /// Not visible from the CDM: a metadata-only rig and an uncalibrated one carry the same absence,
    /// so the ingest's own answer is the one that decides. The narrowing itself is disclosed by
    /// `COVERAGE.METADATA_ONLY`, so this silence is not the reader's only signal.
    fn run_in(&self, dataset: &Dataset, context: &CheckContext) -> Vec<Finding> {
        self.evaluate(dataset, !context.frames_read)
    }
}

impl ProvenanceCompleteness {
    fn evaluate(&self, dataset: &Dataset, skip_payload_derived: bool) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Collect the best-known class per provenance key across all records, and check each
        // element's internal consistency as we go.
        let mut known_value: HashMap<&str, bool> = HashMap::new();
        let mut placeholder_seen: HashMap<&str, bool> = HashMap::new();
        for record in &dataset.provenance {
            for el in &record.elements {
                let has_value = el.value.is_some();
                // Internal consistency: known/asserted must carry a value; unknown must not.
                let inconsistent = match el.class {
                    ProvenanceClass::Known | ProvenanceClass::Asserted => !has_value,
                    ProvenanceClass::Unknown => has_value,
                };
                if inconsistent {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Provenance,
                            Severity::Warning,
                            Location::Dataset,
                            "PROVENANCE.INCONSISTENT",
                            format!(
                                "provenance element `{}` is classified `{}` but {}",
                                el.key,
                                el.class.tag(),
                                if has_value {
                                    "carries a value"
                                } else {
                                    "has no value"
                                }
                            ),
                        )
                        .with_risk("Inconsistent provenance can't be trusted or attested.")
                        .with_remedy("Fix the extractor so class and value agree."),
                    );
                }

                // A known/asserted element whose value is a placeholder ("unknown", "n/a", …) is
                // present in form but empty in substance — it would otherwise silently satisfy the
                // presence check below. Flag it, and don't count it as real provenance.
                let placeholder =
                    has_value && el.class != ProvenanceClass::Unknown && !el.has_real_value();
                // One finding per key even if the placeholder recurs across records.
                let first_placeholder = placeholder
                    && !placeholder_seen
                        .insert(el.key.as_str(), true)
                        .unwrap_or(false);
                if first_placeholder {
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Provenance,
                            Severity::Info,
                            Location::Dataset,
                            "PROVENANCE.PLACEHOLDER_VALUE",
                            format!(
                                "provenance element `{}` is `{}` but its value `{}` is a placeholder",
                                el.key,
                                el.class.tag(),
                                el.value.as_deref().unwrap_or_default()
                            ),
                        )
                        .with_risk(
                            "A placeholder value looks like provenance but records nothing; it can \
                             mask that the real origin is unknown.",
                        )
                        .with_remedy("Record the actual value, or classify the element as `unknown`."),
                    );
                }

                let is_present = has_value && el.class != ProvenanceClass::Unknown && !placeholder;
                let entry = known_value.entry(el.key.as_str()).or_insert(false);
                *entry = *entry || is_present;
            }
        }

        // Surface each expected element that is absent or only ever `unknown`.
        for exp in EXPECTED {
            let present = known_value.get(exp.key).copied().unwrap_or(false);
            // An element only a stream payload could have supplied is not *missing* on a run that
            // opened no payload; it was not looked for. See `PAYLOAD_DERIVED` and `run_in`.
            if !present && !(skip_payload_derived && PAYLOAD_DERIVED.contains(&exp.key)) {
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Provenance,
                        exp.severity,
                        Location::Dataset,
                        exp.code,
                        format!("provenance is missing `{}`", exp.key),
                    )
                    .with_risk(exp.risk)
                    .with_remedy(
                        "Attest this element with `veridex certify` inputs, or extract it from a \
                         richer source.",
                    ),
                );
            }
        }

        findings
    }
}
