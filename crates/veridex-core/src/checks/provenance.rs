//! Provenance-completeness checks: surface missing or internally inconsistent provenance rather
//! than accepting gaps by default. These gaps flow into the certificate's `unknown` section.

use std::collections::HashMap;

use crate::cdm::{Dataset, ProvenanceClass};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

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

/// Presence and internal consistency of dataset provenance.
pub struct ProvenanceCompleteness;

impl Check for ProvenanceCompleteness {
    fn id(&self) -> &'static str {
        "provenance.completeness"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "PROVENANCE.INCONSISTENT",
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
        let mut findings = Vec::new();

        // Collect the best-known class per provenance key across all records, and check each
        // element's internal consistency as we go.
        let mut known_value: HashMap<&str, bool> = HashMap::new();
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

                let is_present = has_value && el.class != ProvenanceClass::Unknown;
                let entry = known_value.entry(el.key.as_str()).or_insert(false);
                *entry = *entry || is_present;
            }
        }

        // Surface each expected element that is absent or only ever `unknown`.
        for exp in EXPECTED {
            let present = known_value.get(exp.key).copied().unwrap_or(false);
            if !present {
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
