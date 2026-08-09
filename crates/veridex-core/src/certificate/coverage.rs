//! Provenance coverage: how much of the expected provenance a dataset actually carries, split into
//! known / asserted / unknown (design D9). Kept separate from the check score so a clean data
//! verdict can never mask missing provenance.

use serde::{Deserialize, Serialize};

use crate::cdm::{Dataset, ProvenanceClass};

/// The provenance elements a trustworthy dataset is expected to carry. The coverage denominator.
pub const EXPECTED_PROVENANCE_KEYS: &[&str] = &[
    "license",
    "sensor",
    "clock",
    "calibration",
    "annotator",
    "upstream",
];

/// Coverage of the expected provenance set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceCoverage {
    /// Expected keys present as extracted (`known`).
    pub known: u32,
    /// Expected keys present as producer-attested (`asserted`).
    pub asserted: u32,
    /// Expected keys absent or only ever `unknown`.
    pub unknown: u32,
}

impl ProvenanceCoverage {
    /// Compute coverage of [`EXPECTED_PROVENANCE_KEYS`] over a dataset's provenance records. For each
    /// expected key, the strongest class found wins (`known` > `asserted` > absent/`unknown`).
    pub fn of(dataset: &Dataset) -> Self {
        let mut cov = ProvenanceCoverage {
            known: 0,
            asserted: 0,
            unknown: 0,
        };
        for key in EXPECTED_PROVENANCE_KEYS {
            let mut best: Option<ProvenanceClass> = None;
            for record in &dataset.provenance {
                for el in &record.elements {
                    // A placeholder value (`unknown`, `n/a`, …) is present in form but empty in
                    // substance, so it must not inflate coverage — treat it as if absent.
                    if el.key == *key && el.has_real_value() {
                        best = Some(match (best, el.class) {
                            (Some(ProvenanceClass::Known), _) | (_, ProvenanceClass::Known) => {
                                ProvenanceClass::Known
                            }
                            (Some(ProvenanceClass::Asserted), _)
                            | (_, ProvenanceClass::Asserted) => ProvenanceClass::Asserted,
                            _ => ProvenanceClass::Unknown,
                        });
                    }
                }
            }
            match best {
                Some(ProvenanceClass::Known) => cov.known += 1,
                Some(ProvenanceClass::Asserted) => cov.asserted += 1,
                _ => cov.unknown += 1,
            }
        }
        cov
    }

    /// Total expected elements considered (the denominator).
    pub fn total(&self) -> u32 {
        self.known + self.asserted + self.unknown
    }

    /// Percentage of expected elements that are known or asserted (0–100, integer). 100 when nothing
    /// is expected.
    pub fn covered_pct(&self) -> u32 {
        let total = self.total();
        if total == 0 {
            return 100;
        }
        (self.known + self.asserted) * 100 / total
    }
}
