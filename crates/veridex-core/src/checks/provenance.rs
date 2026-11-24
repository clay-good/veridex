//! Provenance-completeness checks: surface missing or internally inconsistent provenance rather
//! than accepting gaps by default. These gaps flow into the certificate's `unknown` section.

use std::collections::{BTreeSet, HashMap};

use crate::cdm::{Dataset, ProvenanceClass, ProvenanceScope};
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
            "PROVENANCE.PARTIAL",
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
        self.evaluate(dataset, true, &[])
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
        self.evaluate(dataset, !context.frames_read, &context.attested_keys)
    }
}

impl ProvenanceCompleteness {
    fn evaluate(
        &self,
        dataset: &Dataset,
        skip_payload_derived: bool,
        attested_keys: &[String],
    ) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Collect the best-known class per provenance key across all records, and check each
        // element's internal consistency as we go.
        let mut known_value: HashMap<&str, bool> = HashMap::new();
        let mut placeholder_seen: HashMap<&str, bool> = HashMap::new();
        // Which expected keys a dataset-scoped record supplies, and for the rest, which episodes
        // each key is actually recorded for.
        let mut dataset_scoped: BTreeSet<&str> = BTreeSet::new();
        let mut episodes_covering: HashMap<&str, BTreeSet<u64>> = HashMap::new();
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

                // Where the element is present *from* — a dataset-scoped record speaks for the whole
                // dataset, an episode- or stream-scoped one speaks for that episode only.
                if is_present {
                    match &record.scope {
                        ProvenanceScope::Dataset => {
                            dataset_scoped.insert(el.key.as_str());
                        }
                        ProvenanceScope::Episode(index) => {
                            episodes_covering
                                .entry(el.key.as_str())
                                .or_default()
                                .insert(*index);
                        }
                        ProvenanceScope::Stream { episode, .. } => {
                            episodes_covering
                                .entry(el.key.as_str())
                                .or_default()
                                .insert(*episode);
                        }
                    }
                }
            }
        }

        // An element recorded for *some* episodes and no others is present for the dataset and
        // absent for most of it. `PROVENANCE.MISSING_*` cannot say so — the element is not missing —
        // and neither can the coverage percentage, which counts the strongest class found anywhere
        // and so reads a lineage recorded on one episode of a thousand as lineage for the dataset.
        // An OXE conversion where only part of the shards carried `episode_metadata/file_path`
        // produces exactly that: `upstream` known, 999 episodes with no origin, and nothing saying
        // which is which.
        //
        // The denominator is the episodes **in this run**. A sampled run holds only the episodes it
        // read, so this says nothing about the ones it did not — that narrowing is disclosed by the
        // run's own coverage note, and folding it in here would report a partiality created by the
        // request rather than found in the data.
        let total_episodes = dataset.episodes.len();
        for exp in EXPECTED {
            // A verified producer attestation is a claim about the whole dataset — that is what
            // signing for an element means, and what makes the trust score count it as covered. So
            // it settles the scope question the way a dataset-scoped record does, and reporting the
            // element partial beside it would have the same report say both: exactly the
            // contradiction `PROVENANCE.MISSING_*` was fixed for when an attested element was also
            // reported missing. What the attestation *is* stays disclosed by `PROVENANCE.ATTESTED`,
            // which names the element and the key that signed for it.
            if dataset_scoped.contains(exp.key)
                || attested_keys.iter().any(|k| k == exp.key)
                || total_episodes == 0
            {
                continue;
            }
            let Some(covered) = episodes_covering.get(exp.key) else {
                continue; // absent everywhere: that is `PROVENANCE.MISSING_*`, reported below
            };
            if covered.len() >= total_episodes {
                continue; // every episode carries it
            }
            if skip_payload_derived && PAYLOAD_DERIVED.contains(&exp.key) {
                continue;
            }
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Provenance,
                    Severity::Info,
                    Location::Dataset,
                    "PROVENANCE.PARTIAL",
                    format!(
                        "provenance element `{}` is recorded for {} of {} episode(s); the rest \
                         carry none",
                        exp.key,
                        covered.len(),
                        total_episodes
                    ),
                )
                .with_risk(
                    "The element counts as present for the dataset — in the coverage percentage \
                     and in the certificate — while most episodes have no such record. Any \
                     decision made per episode (which data to pool, what to filter, what a \
                     license permits) is then made on provenance the episode does not have.",
                )
                .with_remedy(
                    "Record the element for every episode, or record it once at dataset scope if \
                     it genuinely describes the whole dataset.",
                ),
            );
        }

        // Surface each expected element that is absent or only ever `unknown`.
        for exp in EXPECTED {
            let present = known_value.get(exp.key).copied().unwrap_or(false);
            // An element only a stream payload could have supplied is not *missing* on a run that
            // opened no payload; it was not looked for. See `PAYLOAD_DERIVED` and `run_in`.
            let unjudgeable = skip_payload_derived && PAYLOAD_DERIVED.contains(&exp.key);
            // An element a verified producer attestation supplied is not missing: it is claimed by
            // a signed producer rather than extracted from the data. The trust score counts it as
            // covered, so reporting it missing had the same report say both — and `remedy: attest
            // this element` was advice the reader had already taken. What it *is* stays disclosed
            // by `PROVENANCE.ATTESTED`, which names both the element and the key that signed it.
            let attested = attested_keys.iter().any(|k| k == exp.key);
            if !present && !unjudgeable && !attested {
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

/// What a license permits a derived dataset to do — the only question Veridex asks of a license
/// string, and it asks it mechanically.
///
/// Training data is a derived work: a pooled corpus, a filtered subset, a re-encoded shard. The
/// three classes below are the terms that make that derivation conditional or impossible, and they
/// are what an SPDX identifier or a dataset card's license slug states plainly enough to read
/// without interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LicenseTerms {
    /// Derivation and redistribution permitted with attribution at most (MIT, Apache-2.0, BSD,
    /// CC-BY, CC0, ODC-BY).
    Permissive,
    /// Derivation permitted only under the same terms (CC-BY-SA, the GPL family, ODbL, MPL, EPL).
    ShareAlike,
    /// Commercial use not permitted (any `-nc`).
    NonCommercial,
    /// Derivative works not permitted at all (any `-nd`).
    NoDerivatives,
}

impl LicenseTerms {
    /// How restrictive this is, so a pool's governing terms are the maximum over its sources.
    fn rank(self) -> u8 {
        match self {
            LicenseTerms::Permissive => 0,
            LicenseTerms::ShareAlike => 1,
            LicenseTerms::NonCommercial => 2,
            LicenseTerms::NoDerivatives => 3,
        }
    }
}

/// Read a license identifier's terms, or `None` for a string this table does not recognize.
///
/// Matched on the normalized identifier rather than on a fixed list of versions, because the
/// condition lives in the identifier by construction: SPDX and the Hugging Face card vocabulary
/// both spell non-commercial `-nc`, no-derivatives `-nd` and share-alike `-sa`, and a version suffix
/// never changes which of those applies. A string that states no such term and is not one of the
/// known permissive or copyleft families is returned unrecognized rather than guessed — a license
/// Veridex read as permissive when it is not would be the one mistake this check must never make.
fn license_terms(value: &str) -> Option<LicenseTerms> {
    let norm = value.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    // The conditions, most restrictive first. `-nd` and `-nc` are matched as whole segments so a
    // name that merely contains those letters cannot trip them.
    let has_segment = |needle: &str| {
        norm.split('-').any(|seg| seg == needle)
            || norm.starts_with(&format!("{needle}-"))
            || norm.ends_with(&format!("-{needle}"))
    };
    if has_segment("nd") || norm.contains("noderiv") {
        return Some(LicenseTerms::NoDerivatives);
    }
    if has_segment("nc") || norm.contains("noncommercial") || norm.contains("non-commercial") {
        return Some(LicenseTerms::NonCommercial);
    }
    if has_segment("sa") || norm.contains("sharealike") {
        return Some(LicenseTerms::ShareAlike);
    }
    const COPYLEFT: &[&str] = &["gpl", "agpl", "lgpl", "odbl", "mpl", "epl", "cddl", "osl"];
    const PERMISSIVE: &[&str] = &[
        "mit",
        "apache",
        "bsd",
        "cc0",
        "cc-by",
        "ccby",
        "odc-by",
        "pddl",
        "isc",
        "unlicense",
        "zlib",
        "wtfpl",
        "public-domain",
    ];
    // Prefix-anchored, so `gpl-3.0` and `lgpl-2.1` resolve and a string that merely mentions a
    // family in prose does not.
    if COPYLEFT
        .iter()
        .any(|f| norm == *f || norm.starts_with(&format!("{f}-")))
    {
        return Some(LicenseTerms::ShareAlike);
    }
    if PERMISSIVE
        .iter()
        .any(|f| norm == *f || norm.starts_with(&format!("{f}-")))
    {
        return Some(LicenseTerms::Permissive);
    }
    None
}

/// **License compatibility across a dataset's lineage.** What the recorded licenses permit a derived
/// dataset to do, and whether they can all be satisfied at once.
///
/// `provenance.completeness` asks only whether a license is *recorded*. So a `cc-by-nc-4.0` dataset
/// and an `apache-2.0` one scored identically — one sixth of provenance coverage each — and nothing
/// in the report said that training a commercial policy on the first is not permitted. Pooling makes
/// it worse: training data is a derived work, so one non-commercial source governs the whole pool,
/// and the obligation is invisible in every other output.
///
/// `PROVENANCE.LICENSE_NONCOMMERCIAL`, `PROVENANCE.LICENSE_NO_DERIVATIVES` and
/// `PROVENANCE.LICENSE_SHARE_ALIKE` name the term and the license that carries it.
/// `PROVENANCE.LICENSE_CONFLICT` fires where two or more recorded licenses cannot all be satisfied
/// by one derived dataset — a no-derivatives source in a pool at all, a non-commercial source beside
/// a permissive one (the pool is non-commercial, which the permissive source's users will not
/// expect), or two different share-alike families, which require the derived work to carry two
/// incompatible licenses at once.
///
/// Read from every provenance scope, so a dataset whose episodes came from different sources is
/// judged on all of them rather than on the dataset-level record alone.
///
/// The three term codes are **informational**, deliberately. A license is a fact about the data, not
/// a defect in it: a `cc-by-nc-4.0` recording is a perfectly sound dataset, and whether its terms
/// permit the use someone has in mind is a question about that use, which Veridex does not know. A
/// warning there would accuse an honest dataset and would move its trust score for something that is
/// not a data-quality fault. `PROVENANCE.LICENSE_CONFLICT` *is* a warning, because terms that cannot
/// all be satisfied at once are unsatisfiable whatever the intended use — that is a fault in how the
/// dataset was assembled.
///
/// **Informational, and not legal advice.** The classification is mechanical: it reads the term out
/// of an SPDX identifier or a dataset card's license slug and says what that term is. A license
/// string it does not recognize is reported as unevaluated (`PROVENANCE.LICENSE_UNRECOGNIZED`)
/// rather than assumed permissive, which is the one mistake this check must never make. Silent where
/// no license is recorded at all: `PROVENANCE.MISSING_LICENSE` already reports that absence as a
/// warning, so the report cannot be read as "the licensing is fine".
pub struct LicenseCompatibility;

impl Check for LicenseCompatibility {
    fn id(&self) -> &'static str {
        "provenance.license-compatibility"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "PROVENANCE.LICENSE_CONFLICT",
            "PROVENANCE.LICENSE_NONCOMMERCIAL",
            "PROVENANCE.LICENSE_NO_DERIVATIVES",
            "PROVENANCE.LICENSE_SHARE_ALIKE",
            "PROVENANCE.LICENSE_UNRECOGNIZED",
        ]
    }
    fn abstention_codes(&self) -> &'static [&'static str] {
        &["PROVENANCE.LICENSE_UNRECOGNIZED"]
    }
    fn title(&self) -> &'static str {
        "License compatibility"
    }
    fn category(&self) -> Category {
        Category::Provenance
    }
    fn default_severity(&self) -> Severity {
        Severity::Warning
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // Every license the dataset records, in canonical order, deduplicated on the identifier as
        // written. A pooled dataset records one per source; most datasets record one in all.
        let mut licenses: BTreeSet<&str> = BTreeSet::new();
        for record in &dataset.provenance {
            for el in &record.elements {
                if el.key == "license"
                    && el.class != ProvenanceClass::Unknown
                    && el.has_real_value()
                {
                    if let Some(v) = el.value.as_deref() {
                        licenses.insert(v.trim());
                    }
                }
            }
        }
        if licenses.is_empty() {
            return Vec::new();
        }

        let mut findings = Vec::new();
        let mut read: Vec<(&str, LicenseTerms)> = Vec::new();
        let mut unrecognized: Vec<&str> = Vec::new();
        for value in &licenses {
            match license_terms(value) {
                Some(terms) => read.push((value, terms)),
                None => unrecognized.push(value),
            }
        }

        // The terms themselves, one finding per license that carries a condition.
        for (value, terms) in &read {
            let (code, severity, what, risk) = match terms {
                LicenseTerms::Permissive => continue,
                LicenseTerms::ShareAlike => (
                    "PROVENANCE.LICENSE_SHARE_ALIKE",
                    Severity::Info,
                    "requires a derived work to carry the same license",
                    "A model or dataset derived from this data inherits the obligation. Teams \
                     routinely discover it after publishing, when the derived artifact is already \
                     out under different terms.",
                ),
                LicenseTerms::NonCommercial => (
                    "PROVENANCE.LICENSE_NONCOMMERCIAL",
                    Severity::Info,
                    "does not permit commercial use",
                    "Training data is a derived work, so this term reaches the policy trained on \
                     it. Nothing else in a Veridex report distinguishes this from a permissively \
                     licensed dataset: both record a license, and both score the same provenance \
                     coverage.",
                ),
                LicenseTerms::NoDerivatives => (
                    "PROVENANCE.LICENSE_NO_DERIVATIVES",
                    Severity::Info,
                    "does not permit derivative works",
                    "A pooled corpus, a filtered subset and a re-encoded shard are all derivative \
                     works, so this data cannot lawfully enter one at all — and a Veridex pass over \
                     it says nothing about that today.",
                ),
            };
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Provenance,
                    severity,
                    Location::Dataset,
                    code,
                    format!("recorded license `{value}` {what}"),
                )
                .with_risk(risk)
                .with_remedy(
                    "Confirm the intended use against the license itself before training or \
                     redistributing. Veridex reads the term out of the identifier and reports it; \
                     this is information, not legal advice.",
                ),
            );
        }

        // Whether they can all be satisfied at once. Only a dataset that records more than one
        // license has a compatibility question to answer.
        if read.len() > 1 {
            let worst = read.iter().map(|(_, t)| *t).max_by_key(|t| t.rank());
            let share_alike: Vec<&str> = read
                .iter()
                .filter(|(_, t)| *t == LicenseTerms::ShareAlike)
                .map(|(v, _)| *v)
                .collect();
            let reason = if read.iter().any(|(_, t)| *t == LicenseTerms::NoDerivatives) {
                Some(
                    "a no-derivatives source cannot enter a derived dataset at all, so no terms \
                     exist under which the pool as a whole may be used"
                        .to_string(),
                )
            } else if share_alike.len() > 1 {
                Some(format!(
                    "`{}` each require the derived work to carry their own license, and it cannot \
                     carry both",
                    share_alike.join("` and `")
                ))
            } else if worst == Some(LicenseTerms::NonCommercial) {
                Some(
                    "a non-commercial source governs the whole pool, which the users of the \
                     permissively licensed sources beside it will not expect"
                        .to_string(),
                )
            } else if worst == Some(LicenseTerms::ShareAlike) {
                Some(format!(
                    "`{}` requires the derived work to carry its own license, which the \
                     permissively licensed sources beside it do not state",
                    share_alike.join("`, `")
                ))
            } else {
                None
            };
            if let Some(reason) = reason {
                let all: Vec<&str> = read.iter().map(|(v, _)| *v).collect();
                findings.push(
                    Finding::new(
                        self.id(),
                        Category::Provenance,
                        Severity::Warning,
                        Location::Dataset,
                        "PROVENANCE.LICENSE_CONFLICT",
                        format!(
                            "this dataset records {} different licenses (`{}`) whose terms cannot \
                             all be satisfied by one derived dataset: {reason}",
                            read.len(),
                            all.join("`, `")
                        ),
                    )
                    .with_risk(
                        "A pooled dataset is a derived work of every source in it, so the strictest \
                         source's terms govern the pool — and nothing else in a report says which \
                         sources those were. This is how a corpus assembled from several public \
                         datasets becomes unusable for the purpose it was assembled for.",
                    )
                    .with_remedy(
                        "Separate the sources whose terms disagree, or use the pool only under the \
                         strictest of them. Veridex reports the terms it read from the identifiers; \
                         this is information, not legal advice.",
                    ),
                );
            }
        }

        // And what was not evaluated. A license string this table does not recognize must not read
        // as a permissive one: the silence of this check is otherwise indistinguishable from a pass.
        if !unrecognized.is_empty() {
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Provenance,
                    Severity::Info,
                    Location::Dataset,
                    "PROVENANCE.LICENSE_UNRECOGNIZED",
                    format!(
                        "{} recorded license identifier(s) state no term this check recognizes, so \
                         what they permit a derived dataset to do was not evaluated (`{}`)",
                        unrecognized.len(),
                        unrecognized.join("`, `")
                    ),
                )
                .with_risk(
                    "A clean licensing result over these is the absence of a reading, not a \
                     judgement that the terms are permissive — which is the assumption the silence \
                     would otherwise invite.",
                )
                .with_remedy(
                    "Record the license as an SPDX identifier (`apache-2.0`, `cc-by-nc-4.0`) where \
                     one applies, and read the terms yourself where none does.",
                ),
            );
        }
        findings
    }
}
