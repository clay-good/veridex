//! Provenance-completeness checks: surface missing or internally inconsistent provenance rather
//! than accepting gaps by default. These gaps flow into the certificate's `unknown` section.

use std::collections::{BTreeSet, HashMap};

use crate::cdm::{Dataset, Modality, ProvenanceClass, ProvenanceScope};
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

/// **Simulated time.** A recording whose timeline came from a simulator, which nothing said.
///
/// A ROS node started with `use_sim_time` takes its clock from a `rosgraph_msgs/Clock` publisher
/// instead of the system clock, and every other node in that graph does the same. So a recording
/// that carries a clock channel was made on a simulator's clock — and it looks, in every other
/// respect, exactly like a drive: the topics are a rig's topics, the cadences are a rig's cadences,
/// the transform tree is a rig's tree, and the trust score is a rig's score.
///
/// Two things follow, and the report said neither. Every temporal result — rate, gap, jitter, span,
/// cross-sensor sync — is about the simulator's clock rather than about hardware, so "this rig is
/// synchronized to 2 ms" is a statement about a simulation loop. And the data itself is, in all
/// likelihood, not from physical sensors: pooling it with real recordings puts the sim-to-real gap
/// inside the training set, where no verdict in this tool would show it.
///
/// A warning rather than an error, and deliberately not a judgement about the data: simulated data
/// is legitimate training data, and world models are routinely built from it. What is not legitimate
/// is a corpus that cannot tell which of its recordings were simulated.
///
/// Read from the recording's own topic inventory (`META_ROS_TIME_SOURCE`), in the positive direction
/// only: a recording without a clock channel says nothing about where its time came from, and this
/// check says nothing about it either. No other format carries the question.
pub struct TimeSource;

impl Check for TimeSource {
    fn id(&self) -> &'static str {
        "provenance.time-source"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["PROVENANCE.SIMULATED_CLOCK"]
    }
    fn title(&self) -> &'static str {
        "Recording time source"
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
        let simulated = dataset.metadata.iter().any(|(k, v)| {
            k == crate::cdm::META_ROS_TIME_SOURCE && v == crate::cdm::ROS_TIME_SOURCE_SIMULATED
        });
        if !simulated {
            return Vec::new();
        }
        vec![Finding::new(
            self.id(),
            Category::Provenance,
            Severity::Warning,
            Location::Dataset,
            "PROVENANCE.SIMULATED_CLOCK",
            "this recording carries a ROS clock channel, so the graph that produced it ran on \
             simulated time rather than on a clock on a rig"
                .to_string(),
        )
        .with_risk(
            "Every temporal result in this report — rate, gap, jitter, span, cross-sensor sync — is \
             about the simulator's clock rather than about hardware, and the data itself is in all \
             likelihood not from physical sensors. Nothing else here distinguishes this recording \
             from a drive: the topics, the cadences, the transform tree and the trust score are a \
             rig's. Pooled with real recordings, it puts the sim-to-real gap inside the training \
             set where no verdict would show it.",
        )
        .with_remedy(
            "Keep simulated recordings labelled as such wherever this dataset is pooled or \
             published. Simulated data is legitimate training data — a corpus that cannot tell \
             which of its recordings were simulated is the problem.",
        )]
    }
}

/// Patterns that make a recorded string **likely** personal data, and the word for each kind.
///
/// Deliberately few and deliberately conservative. Every one of them is a shape that cannot plausibly
/// be a sensor name, a license id or a file of measurements — and nothing here reaches for the shapes
/// that only look specific: a nine-digit run is not a social-security number, a date of birth is not
/// distinguishable from a calibration date, and a flagged dataset that is clean is worse than an
/// unflagged one that is not, because a reader learns to ignore the finding.
///
/// Matched on text Veridex **already reads** — the dataset's metadata values, its provenance
/// element values, its task and label strings. No pixel is decoded here or anywhere, and the
/// imagery half of the question is answered by `PROVENANCE.IMAGERY_WITHOUT_CONSENT` below rather
/// than guessed at.
type PersonalPattern = (&'static str, fn(&str) -> bool);

const PERSONAL_PATTERNS: &[PersonalPattern] = &[
    ("an email address", looks_like_email),
    ("a telephone number", looks_like_phone),
    ("a user's home directory", looks_like_home_directory),
];

/// `name@host.tld`: an `@` with a label either side and a dotted host with a plausible suffix.
fn looks_like_email(text: &str) -> bool {
    text.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '@' && c != '.');
        let Some((local, host)) = word.split_once('@') else {
            return false;
        };
        if local.is_empty() || host.len() < 4 || !host.contains('.') {
            return false;
        }
        let suffix = host.rsplit('.').next().unwrap_or("");
        (2..=24).contains(&suffix.len())
            && suffix.chars().all(|c| c.is_ascii_alphabetic())
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
    })
}

/// A telephone number as a person writes one: an international prefix or a grouped run of digits
/// with separators — never a bare run, which is every counter, id and checksum in a dataset.
fn looks_like_phone(text: &str) -> bool {
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).count();
    text.split_whitespace()
        .collect::<Vec<_>>()
        .windows(3)
        .map(|w| w.join(" "))
        .chain(text.split_whitespace().map(str::to_string))
        .any(|candidate| {
            let separated = candidate.contains('-') || candidate.contains(' ');
            let prefixed = candidate.trim_start().starts_with('+');
            if !(separated || prefixed) {
                return false;
            }
            // Only digits and the punctuation a phone number is written with, and a count in the
            // range the E.164 plan allows.
            let shaped = candidate
                .chars()
                .all(|c| c.is_ascii_digit() || "+-() ".contains(c));
            shaped && (9..=15).contains(&digits(&candidate))
        })
}

/// `/home/<user>/`, `/Users/<user>/`, `C:\Users\<user>\` — a path that names the person who ran the
/// recorder. The commonest personal datum in a robot dataset, and the one nobody means to publish.
fn looks_like_home_directory(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    for prefix in ["/home/", "/users/", "\\users\\", "c:\\users\\"] {
        if let Some(rest) = lower.split(prefix).nth(1) {
            let user = rest
                .split(['/', '\\', ' ', '"', '\''])
                .next()
                .unwrap_or_default();
            // A home directory names somebody; the root of one names nobody.
            if !user.is_empty() && user != "shared" && user != "public" {
                return true;
            }
        }
    }
    false
}

/// **Personal data and imagery consent.** What a dataset records *about people*, and what nobody can
/// tell from the outside.
///
/// Two findings, and the line between them is the architectural commitment this tool is built on:
/// Veridex reads structure and metadata and fingerprints bytes, and never interprets them.
///
/// `PROVENANCE.PERSONAL_DATA` flags a recorded value that looks like personal data — an email
/// address, a telephone number, a path naming the user who ran the recorder. These are matched on the
/// text Veridex already reads: dataset metadata values, provenance element values, task and label
/// strings. The commonest of them is the home-directory path, which arrives in a `source_file` or a
/// `derived_from` and is nobody's intention to publish. The finding names **where** and **what
/// kind** and never the value itself, because a finding travels into every report, and a report that
/// quoted the email address would publish exactly what it was warning about.
///
/// `PROVENANCE.IMAGERY_WITHOUT_CONSENT` reports a dataset that carries camera streams and records no
/// consent status. It is **not** a claim that a person appears in them: Veridex does not decode a
/// pixel, so it cannot know, and a tool that implied otherwise would be worse than silent. What it
/// says is that the dataset carries imagery and no record of the permission under which it was
/// taken, which is the thing a producer can actually act on before publishing — and the thing a
/// consumer cannot discover from any other field.
///
/// Both are **flags**: nothing here is redacted, blurred or removed, and the dataset is never
/// modified. Both are probabilistic by construction, which the risk text says in those words. The
/// pixel half of the catalog's privacy requirement stays refused by design, and
/// [docs/checks.md](../../../docs/checks.md) says so where a reader will look for it.
pub struct PersonalData;

impl Check for PersonalData {
    fn id(&self) -> &'static str {
        "provenance.personal-data"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "PROVENANCE.PERSONAL_DATA",
            "PROVENANCE.IMAGERY_WITHOUT_CONSENT",
        ]
    }
    fn title(&self) -> &'static str {
        "Personal data and imagery consent"
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
        let mut findings = Vec::new();

        // Every string the CDM carries that a producer wrote, with the field it came from — the
        // same places the redactor substitutes, for the same reason: these are where a dataset is
        // identifying.
        let mut text: Vec<(String, &str)> = Vec::new();
        for (key, value) in &dataset.metadata {
            text.push((format!("metadata `{key}`"), value.as_str()));
        }
        for record in &dataset.provenance {
            for element in &record.elements {
                if let Some(value) = &element.value {
                    text.push((format!("provenance `{}`", element.key), value.as_str()));
                }
            }
        }
        // Episode text is labelled by the *field*, not by the episode — a task string written once
        // and copied into a thousand episodes is one fact about the dataset, and a finding per
        // episode would bury every other finding in the report under it.
        for episode in &dataset.episodes {
            if let Some(task) = &episode.task {
                text.push(("an episode task".to_string(), task.as_str()));
            }
            for label in &episode.labels {
                text.push((
                    format!("an episode `{}` label", label.key),
                    label.value.as_str(),
                ));
            }
        }

        // One finding per (kind, field), sorted, so a value repeated across every episode is one
        // finding rather than a thousand.
        let mut found: BTreeSet<(&str, String)> = BTreeSet::new();
        for (field, value) in &text {
            for (kind, matches) in PERSONAL_PATTERNS {
                if matches(value) {
                    found.insert((kind, field.clone()));
                }
            }
        }
        for (kind, field) in found {
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Provenance,
                    Severity::Warning,
                    Location::Dataset,
                    "PROVENANCE.PERSONAL_DATA",
                    // The value is deliberately not quoted: this finding travels into every report,
                    // and one that printed the email address would publish what it is warning about.
                    format!(
                        "{field} holds what looks like {kind} — the value is not quoted here, \
                         because this finding travels wherever the report does"
                    ),
                )
                .with_risk(
                    "A dataset published with a person's address, number or user name in its \
                     metadata publishes it to everyone who pulls the dataset, and it is in the \
                     content hash, so it cannot be quietly removed afterwards without changing \
                     every certificate issued over it. This is a pattern match on recorded text, \
                     not a judgement: it can be wrong in both directions.",
                )
                .with_remedy(
                    "Read the field and decide. Where it should not ship, re-export without it — \
                     Veridex only ever flags, and never edits your data. `veridex check --redact` \
                     keeps the value out of a report you are sharing, which is a different problem \
                     from keeping it out of the dataset.",
                ),
            );
        }

        // The imagery half, which is not a claim about pixels.
        let cameras: BTreeSet<&str> = dataset
            .episodes
            .iter()
            .flat_map(|e| e.streams.iter())
            .filter(|s| s.modality == Modality::Video)
            .map(|s| s.name.as_str())
            .collect();
        let consent_recorded = dataset
            .provenance
            .iter()
            .flat_map(|p| p.elements.iter())
            .any(|e| {
                // The CDM's own keys, which `provenance_key_for` maps a producer's `consent_status`,
                // `data_consent`, `redacted` or `pii_redaction` onto. Either answers the question:
                // consent says the imagery was taken with permission, redaction says what was
                // removed from it before publishing.
                (e.key == "consent" || e.key == "redaction")
                    && e.class != ProvenanceClass::Unknown
                    && e.has_real_value()
            });
        if !cameras.is_empty() && !consent_recorded {
            let shown: Vec<&str> = cameras.iter().take(4).copied().collect();
            let listed = match cameras.len().saturating_sub(4) {
                0 => shown.join(", "),
                rest => format!("{} and {rest} more", shown.join(", ")),
            };
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Provenance,
                    Severity::Info,
                    Location::Dataset,
                    "PROVENANCE.IMAGERY_WITHOUT_CONSENT",
                    format!(
                        "{} camera stream{} recorded ({listed}) and the dataset records no \
                         consent status for the imagery",
                        cameras.len(),
                        if cameras.len() == 1 { " is" } else { "s are" }
                    ),
                )
                .with_risk(
                    "Veridex never decodes a pixel, so it cannot tell you whether a person appears \
                     in this imagery — and neither can anything else in this report. What it can \
                     tell you is that the dataset carries imagery and no record of the permission \
                     it was taken under, which is the one thing a consumer cannot discover from any \
                     other field and a producer can still fix.",
                )
                .with_remedy(
                    "Record the consent the recording was made under, or the redaction applied to \
                     it before publishing — a `consent_status` or `redaction_status` key in an MCAP \
                     `Metadata` record, a rosbag2 `custom_data` entry, an HDF5 root attribute or a \
                     Zarr `.zattrs` entry, which Veridex reads as the `consent` and `redaction` \
                     provenance elements. Then the dataset carries its own answer. Veridex reads \
                     that answer and reports it; it does not look at your images.",
                ),
            );
        }
        findings
    }
}
