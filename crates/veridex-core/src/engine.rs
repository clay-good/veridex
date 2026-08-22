//! The validation engine: runs a registry of [`Check`]s over a CDM and produces a deterministic
//! [`Verdict`].
//!
//! The engine owns registration (with duplicate-id rejection), selection/config, fault isolation,
//! and result ordering. It knows nothing about specific checks. Determinism is guaranteed by
//! running checks and then sorting findings into a total order, so parallelizing execution later
//! cannot change the output (design D5).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::canonical::ContentHash;
use crate::cdm::Dataset;
use crate::check::{Category, Check, Finding, Scope, Severity};

/// Error raised while assembling the check registry.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Two checks share a check id.
    #[error("duplicate check id: {0}")]
    DuplicateId(&'static str),
}

/// Numeric tolerances for the checks that take one. Applied when the engine's checks are built, so
/// a run can loosen or tighten a threshold (e.g. a rig with a known 80 ms camera latency). Defaults
/// match each check's built-in default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tolerances {
    /// Max tolerated cross-stream duration drift for `TEMPORAL.CLOCK_SKEW`, in nanoseconds.
    pub clock_skew_ns: i64,
    /// Max tolerated shared-clock start offset for `TEMPORAL.START_OFFSET`, in nanoseconds.
    pub start_offset_ns: i64,
    /// Max tolerated shared-clock end offset for `TEMPORAL.END_OFFSET`, in nanoseconds.
    pub end_offset_ns: i64,
    /// Allowed relative rate deviation for `TEMPORAL.RATE` (0.10 = 10%).
    pub rate_deviation: f64,
    /// A `TEMPORAL.GAP` fires on an interval greater than this multiple of the expected interval.
    pub gap_factor: f64,
    /// `TEMPORAL.JITTER` fires when a stream's inter-frame intervals have a coefficient of variation
    /// (std / mean) above this — an irregular timeline whose mean rate can still look correct.
    pub jitter_cv: f64,
    /// `TEMPORAL.EPISODE_DURATION_OUTLIER` fires on an episode whose duration is more than this
    /// multiple away (either direction) from the dataset's median episode duration.
    pub episode_duration_factor: f64,
    /// `STATISTICAL.SATURATED` fires when at least this fraction of a stream's recomputed values sit
    /// exactly pinned at one extreme (0.50 = half the samples).
    pub saturation_fraction: f64,
    /// `STATISTICAL.SATURATED` abstains on streams with fewer than this many recomputed samples, where
    /// a pinned "fraction" isn't yet meaningful.
    pub saturation_min_samples: u64,
    /// `STATISTICAL.OUTLIER` fires on a value at least this many standard deviations from the mean.
    /// The Chebyshev tail bound is `1/z²`, so `z = 10` means the flagged value is ≤1% of samples.
    pub outlier_z: f64,
    /// `AUTONOMY.SEQUENCE_COMPLETE` fires when more than this fraction of the frames a rig sensor's
    /// own median cadence implies over its span are missing (0.05 = 5%).
    pub sequence_drop_fraction: f64,
    /// `AUTONOMY.EGO_POSE_CONTINUITY` fires on an ego-trajectory step implying a speed above this,
    /// in metres per second — a teleport rather than motion.
    pub ego_max_speed_mps: f64,
}

impl Tolerances {
    /// Replace any non-finite float field with its default. A `NaN`/`inf` tolerance would (a) serialize
    /// to JSON `null`, so a signed certificate embedding it could never be re-verified (the field is
    /// non-optional), and (b) silently disable the checks that guard on it (`x > NaN` is always false).
    /// The CLI/TOML config layer already rejects non-finite tolerances; this guards the direct
    /// library/Python path that constructs a [`Tolerances`] by hand.
    pub fn finite_or_default(self) -> Tolerances {
        let d = Tolerances::default();
        let fix = |v: f64, default: f64| if v.is_finite() { v } else { default };
        Tolerances {
            rate_deviation: fix(self.rate_deviation, d.rate_deviation),
            gap_factor: fix(self.gap_factor, d.gap_factor),
            jitter_cv: fix(self.jitter_cv, d.jitter_cv),
            episode_duration_factor: fix(self.episode_duration_factor, d.episode_duration_factor),
            saturation_fraction: fix(self.saturation_fraction, d.saturation_fraction),
            outlier_z: fix(self.outlier_z, d.outlier_z),
            sequence_drop_fraction: fix(self.sequence_drop_fraction, d.sequence_drop_fraction),
            ego_max_speed_mps: fix(self.ego_max_speed_mps, d.ego_max_speed_mps),
            ..self
        }
    }
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            clock_skew_ns: 50_000_000,   // 50 ms
            start_offset_ns: 50_000_000, // 50 ms
            end_offset_ns: 50_000_000,   // 50 ms
            rate_deviation: 0.10,        // 10%
            gap_factor: 3.0,
            jitter_cv: 0.5,
            episode_duration_factor: 10.0,
            saturation_fraction: 0.5,
            saturation_min_samples: 20,
            outlier_z: 10.0,
            sequence_drop_fraction: 0.05, // 5%
            ego_max_speed_mps: 100.0,
        }
    }
}

/// Caller configuration for a run. All collections are ordered for determinism.
#[derive(Debug, Clone, Default)]
pub struct RunConfig {
    /// If set, only checks in these categories run. `None` means all categories.
    pub categories: Option<BTreeSet<Category>>,
    /// If set, only checks with these ids run. `None` means all ids.
    pub only_checks: Option<BTreeSet<String>>,
    /// Checks disabled by id (applied after selection).
    pub disabled_checks: BTreeSet<String>,
    /// Per-check severity overrides, applied to every finding of that check.
    pub severity_overrides: BTreeMap<String, Severity>,
    /// Numeric tolerances for the checks that take one. The engine builds its checks with these.
    pub tolerances: Tolerances,
}

/// The effective configuration, snapshotted into the verdict so a run is reproducible from it.
/// (Not `Eq`: [`Tolerances`] carries floats.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveConfig {
    /// Selected categories, sorted; `None` means all.
    pub categories: Option<Vec<Category>>,
    /// Selected check ids, sorted; `None` means all.
    pub only_checks: Option<Vec<String>>,
    /// Disabled check ids, sorted.
    pub disabled_checks: Vec<String>,
    /// Severity overrides, id → severity.
    pub severity_overrides: BTreeMap<String, Severity>,
    /// The numeric tolerances the checks ran with.
    pub tolerances: Tolerances,
}

impl From<&RunConfig> for EffectiveConfig {
    fn from(c: &RunConfig) -> Self {
        EffectiveConfig {
            categories: c.categories.as_ref().map(|s| s.iter().copied().collect()),
            only_checks: c.only_checks.as_ref().map(|s| s.iter().cloned().collect()),
            disabled_checks: c.disabled_checks.iter().cloned().collect(),
            severity_overrides: c.severity_overrides.clone(),
            // Sanitize so the serialized snapshot is always finite and the certificate round-trips,
            // even if a direct library caller built the config with a non-finite tolerance.
            tolerances: c.tolerances.finite_or_default(),
        }
    }
}

/// Overall verdict status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// No findings, or only `info`.
    Pass,
    /// Worst finding is a `warning`.
    PassWithWarnings,
    /// At least one `error` finding.
    Fail,
}

/// Counts of findings by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeverityCounts {
    /// Number of `error` findings.
    pub error: u64,
    /// Number of `warning` findings.
    pub warning: u64,
    /// Number of `info` findings.
    pub info: u64,
}

/// A check that was selected to run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutedCheck {
    /// Check id.
    pub check_id: String,
    /// Check version.
    pub version: String,
    /// Check category.
    pub category: Category,
}

/// A check that raised internally instead of producing findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErroredCheck {
    /// Check id.
    pub check_id: &'static str,
    /// Check version.
    pub version: &'static str,
    /// Captured error/panic message.
    pub message: String,
}

/// How much of the source dataset the run actually saw.
///
/// Carried in the verdict, digested into `result_content_hash`, and therefore covered by a
/// certificate's signature. A partial run is a different verdict from a full one and must not be
/// readable as the same thing — this is the field that makes the difference visible to anyone
/// holding only the report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverageNote {
    /// Every episode of the source was ingested.
    #[default]
    Full,
    /// Only a sample was ingested; the verdict speaks for that sample alone.
    Sample {
        /// The sampling request that was honored, in human-readable form.
        request: String,
        /// Number of episodes actually ingested.
        episodes_ingested: u64,
    },
    /// Only the source's manifest was read: every episode is present but none has frames. The
    /// verdict speaks for the declared structure, the stored statistics, and the provenance — and
    /// for nothing that requires the data itself.
    MetadataOnly {
        /// Number of episodes the manifest declared.
        episodes_declared: u64,
    },
}

impl CoverageNote {
    /// Whether the run saw less than the whole dataset.
    pub fn is_partial(&self) -> bool {
        !matches!(self, CoverageNote::Full)
    }

    /// Whether stream payloads were read at all.
    ///
    /// `false` only under [`CoverageNote::MetadataOnly`]. The checks that reason about frames read
    /// this and abstain: with no frames read, "0 frames ingested" is not a finding about the
    /// dataset, it is a description of the request — and reporting it as a defect would fail every
    /// sound dataset checked this way.
    pub fn frames_read(&self) -> bool {
        !matches!(self, CoverageNote::MetadataOnly { .. })
    }
}

/// The full result of a run. (Not `Eq`: the effective config's tolerances carry floats.)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    /// How much of the source dataset this verdict covers.
    pub coverage: CoverageNote,
    /// The `veridex-core` version that produced this verdict.
    pub veridex_version: String,
    /// The CDM content hash the run was performed over (hex).
    pub cdm_content_hash: String,
    /// Overall status.
    pub status: Status,
    /// Finding counts by severity.
    pub counts: SeverityCounts,
    /// Findings, in a stable total order independent of execution order.
    pub findings: Vec<Finding>,
    /// Checks that errored internally, listed separately from data findings.
    pub errored_checks: Vec<ErroredCheck>,
    /// Checks that were selected and executed, sorted by id.
    pub executed_checks: Vec<ExecutedCheck>,
    /// The effective configuration used.
    pub effective_config: EffectiveConfig,
    /// SHA-256 over the canonical JSON of every other field (hex). Two byte-identical runs share it.
    pub result_content_hash: String,
}

impl Verdict {
    /// The distinct categories of the checks that ran **and finished**.
    ///
    /// `executed_checks` records invocation, not success, so a category whose every check crashed
    /// used to count as covered — and the certificate derives `categories_skipped` from this, so
    /// nothing anywhere said the category went unmeasured.
    pub fn checks_categories(&self) -> Vec<Category> {
        let mut cats: Vec<Category> = self
            .executed_checks
            .iter()
            .filter(|c| !self.errored_checks.iter().any(|e| e.check_id == c.check_id))
            .map(|c| c.category)
            .collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// Whether this run departed from the declared catalog in a way that could have *raised* its
    /// score — checks deselected, a severity overridden, or a threshold loosened.
    ///
    /// A threshold moved to be *stricter* is not a narrowing: it measures the data harder than the
    /// catalog asks. `--profile world-model-ready` does exactly that, so treating any moved
    /// threshold as a narrowing made every readiness run report itself as one.
    ///
    /// Reads the [`SCOPE_CHECK_ID`] finding rather than re-deriving the condition, so there is one
    /// answer to "was this run narrowed" and every consumer gets the same one.
    pub fn scope_narrowed(&self) -> bool {
        self.findings.iter().any(|f| f.check_id == SCOPE_CHECK_ID)
    }
}

/// A subset view of the verdict used to compute [`Verdict::result_content_hash`] — every field
/// except the hash itself, in a fixed order.
#[derive(Serialize)]
struct DigestView<'a> {
    coverage: &'a CoverageNote,
    veridex_version: &'a str,
    cdm_content_hash: &'a str,
    status: Status,
    counts: SeverityCounts,
    findings: &'a [Finding],
    errored_checks: &'a [ErroredCheck],
    executed_checks: &'a [ExecutedCheck],
    effective_config: &'a EffectiveConfig,
}

/// Builds an [`Engine`], rejecting duplicate check ids.
#[derive(Default)]
pub struct EngineBuilder {
    checks: Vec<Box<dyn Check>>,
    seen: BTreeSet<&'static str>,
}

impl EngineBuilder {
    /// Register a check. Fails if its id duplicates an already-registered one.
    pub fn register(mut self, check: Box<dyn Check>) -> Result<Self, RegistryError> {
        let id = check.id();
        if !self.seen.insert(id) {
            return Err(RegistryError::DuplicateId(id));
        }
        self.checks.push(check);
        Ok(self)
    }

    /// Finish building.
    pub fn build(self) -> Engine {
        Engine {
            checks: self.checks,
        }
    }
}

/// Static, run-independent metadata describing one registered check. Emitted by
/// [`Engine::catalog`] for discoverability (`veridex checks`).
#[derive(Debug, Clone, Serialize)]
pub struct CheckInfo {
    /// The check's stable id (e.g. `structural.shape-consistency`).
    pub id: &'static str,
    /// One-line human title.
    pub title: &'static str,
    /// The family this check belongs to.
    pub category: Category,
    /// The severity the check emits by default (before any config override).
    pub default_severity: Severity,
    /// The CDM scope the check reasons over.
    pub scope: Scope,
    /// The check's version.
    pub version: &'static str,
    /// Every finding `code` this check can emit.
    pub finding_codes: &'static [&'static str],
}

/// A registry of checks that can validate a CDM.
pub struct Engine {
    checks: Vec<Box<dyn Check>>,
}

impl Engine {
    /// Start building an engine.
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    /// Ids of every registered check, in registration order.
    pub fn check_ids(&self) -> Vec<&'static str> {
        self.checks.iter().map(|c| c.id()).collect()
    }

    /// Static metadata for every registered check, in registration order, without running any of
    /// them. Powers `veridex checks` (catalog discoverability).
    pub fn catalog(&self) -> Vec<CheckInfo> {
        self.checks
            .iter()
            .map(|c| CheckInfo {
                id: c.id(),
                title: c.title(),
                category: c.category(),
                default_severity: c.default_severity(),
                scope: c.scope(),
                version: c.version(),
                finding_codes: c.finding_codes(),
            })
            .collect()
    }

    /// Validate `dataset` (whose content hash is `cdm_hash`) under `config`, over the whole source.
    ///
    /// Use [`Engine::run_over`] when the dataset in hand is only a sample of its source, so the
    /// verdict says so.
    pub fn run(&self, dataset: &Dataset, cdm_hash: ContentHash, config: &RunConfig) -> Verdict {
        self.run_over(dataset, cdm_hash, config, CoverageNote::Full)
    }

    /// Like [`Engine::run`], but records that the dataset covers only part of its source.
    pub fn run_over(
        &self,
        dataset: &Dataset,
        cdm_hash: ContentHash,
        config: &RunConfig,
        coverage: CoverageNote,
    ) -> Verdict {
        let mut findings: Vec<Finding> = Vec::new();
        let mut errored_checks: Vec<ErroredCheck> = Vec::new();
        let mut executed_checks: Vec<ExecutedCheck> = Vec::new();

        // What the checks are allowed to conclude from absence, derived from what the ingest read.
        let context = crate::check::CheckContext {
            frames_read: coverage.frames_read(),
        };

        // A partial run says so as a *finding*, not only as the verdict's `coverage` field.
        //
        // The field is rendered by the terminal report, the JSON, and the HTML — and by nothing
        // else. SARIF carries no coverage marker, so a CI job gating on a code-scanning upload sees
        // a metadata-only or sampled run as a clean scan of the whole dataset; `diff` never reads
        // coverage either, so swapping a partial report in for a full one reads as findings
        // *resolved*. Findings are the one channel that reaches every renderer and the certificate's
        // own summary — the same reasoning `temporal.clock-measurability` is built on, and the same
        // reason SARIF synthesizes a result for a check that errored rather than letting its silence
        // pass. Info severity: this is a statement about the run's scope, not a defect in the data,
        // and it must not change the status of a dataset that is genuinely sound.
        if let Some(finding) = coverage_finding(&coverage) {
            findings.push(finding);
        }

        for check in &self.checks {
            if !check_selected(check.as_ref(), config) {
                continue;
            }
            executed_checks.push(ExecutedCheck {
                check_id: check.id().to_string(),
                version: check.version().to_string(),
                category: check.category(),
            });

            // Fault isolation: a panicking check is recorded, not fatal.
            let result = catch_unwind(AssertUnwindSafe(|| check.run_in(dataset, &context)));
            match result {
                Ok(mut produced) => {
                    let override_sev = config.severity_overrides.get(check.id()).copied();
                    for f in &mut produced {
                        if let Some(sev) = override_sev {
                            f.severity = sev;
                        }
                    }
                    findings.extend(produced);
                }
                Err(payload) => {
                    errored_checks.push(ErroredCheck {
                        check_id: check.id(),
                        version: check.version(),
                        message: panic_message(payload),
                    });
                }
            }
        }

        // A narrowed run says so as a finding too, for the same reason a partial ingest does. Counted
        // from what actually happened (checks executed vs. registered) rather than from the config's
        // wording, so a `only_checks` that names the whole catalog is correctly silent.
        let overridden = self
            .checks
            .iter()
            .filter(|c| {
                check_selected(c.as_ref(), config) && config.severity_overrides.contains_key(c.id())
            })
            .count();
        if let Some(finding) =
            scope_finding(config, executed_checks.len(), self.checks.len(), overridden)
        {
            findings.push(finding);
        }

        // Total order over the finding's full content, independent of execution order. Every field is
        // in the key: a sort that ties on non-identical content falls through to `Vec` order, which
        // is execution order — and `result_content_hash` is computed over this sequence, so the same
        // two findings emitted in either order would have to hash alike and would not.
        findings.sort_by(|a, b| {
            a.check_id
                .cmp(b.check_id)
                .then_with(|| a.location.sort_key().cmp(&b.location.sort_key()))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.message.cmp(&b.message))
                .then_with(|| a.severity.cmp(&b.severity))
                .then_with(|| a.category.cmp(&b.category))
                .then_with(|| a.risk.cmp(&b.risk))
                .then_with(|| a.remedy.cmp(&b.remedy))
        });
        executed_checks.sort_by(|a, b| a.check_id.cmp(&b.check_id));
        errored_checks.sort_by(|a, b| a.check_id.cmp(b.check_id));

        let counts = count_severities(&findings);
        let status = status_from(&counts, &errored_checks);
        let effective_config = EffectiveConfig::from(config);

        let digest = DigestView {
            coverage: &coverage,
            veridex_version: crate::VERSION,
            cdm_content_hash: &cdm_hash.to_hex(),
            status,
            counts,
            findings: &findings,
            errored_checks: &errored_checks,
            executed_checks: &executed_checks,
            effective_config: &effective_config,
        };
        let result_content_hash = digest_hex(&digest);

        Verdict {
            coverage,
            veridex_version: crate::VERSION.to_string(),
            cdm_content_hash: cdm_hash.to_hex(),
            status,
            counts,
            findings,
            errored_checks,
            executed_checks,
            effective_config,
            result_content_hash,
        }
    }
}

/// The check id under which a partial run discloses its own scope.
///
/// Not a registered [`Check`] — coverage is a property of the ingest, which no check can read from
/// the CDM. The engine emits it directly, the way SARIF synthesizes a result for a check that
/// errored. It is deliberately not in the catalog, so `veridex checks` still lists exactly the
/// checks that run, and configuration cannot disable the disclosure.
pub const COVERAGE_CHECK_ID: &str = "veridex.coverage";

/// The finding a partial run discloses about itself, or `None` for a full run.
fn coverage_finding(coverage: &CoverageNote) -> Option<Finding> {
    let (code, message, risk, remedy) = match coverage {
        CoverageNote::Full => return None,
        CoverageNote::Sample {
            request,
            episodes_ingested,
        } => (
            "COVERAGE.SAMPLE",
            format!(
                "this run read a sample of the dataset ({request}; {episodes_ingested} episode(s) \
                 ingested), so every result below speaks for those episodes alone"
            ),
            "The episodes this run never opened are exactly where an undetected problem would be. \
             A clean result over a sample is not a clean result over the dataset, and read as one \
             it waves through the data it never looked at.",
            "Re-run without the sampling flags before treating this as a verdict on the dataset. \
             `veridex certify` refuses a sampled run for the same reason.",
        ),
        CoverageNote::MetadataOnly { episodes_declared } => (
            "COVERAGE.METADATA_ONLY",
            format!(
                "this run read only the dataset's manifest ({episodes_declared} episode(s) \
                 declared); no stream payload was opened, so no timestamp, value, content hash, or \
                 media header was examined"
            ),
            "The checks that need the data — every temporal check, every recomputed statistic, the \
             duplicate, stuck-stream, and video checks — had nothing to measure and produced \
             nothing. Their silence is the shape of the request, not evidence about the dataset.",
            "Re-run without --metadata-only before treating this as a verdict on the data. \
             `veridex certify` refuses a metadata-only run for the same reason.",
        ),
    };
    Some(
        Finding::new(
            COVERAGE_CHECK_ID,
            Category::Structural,
            Severity::Info,
            crate::check::Location::Dataset,
            code,
            message,
        )
        .with_risk(risk)
        .with_remedy(remedy),
    )
}

/// The check id under which a narrowed run discloses its own scope.
///
/// Like [`COVERAGE_CHECK_ID`], deliberately not a registered [`Check`]: it is a statement about the
/// selection itself, so configuration must not be able to switch off the disclosure that
/// configuration was narrowed.
pub const SCOPE_CHECK_ID: &str = "veridex.scope";

/// The finding a narrowed run discloses about itself, or `None` when the full catalog ran at its
/// declared severities.
///
/// Coverage answers "how much of the dataset did we read"; this answers "how much of the catalog did
/// we run", and until now nothing answered it outside the JSON envelope. A `veridex.toml` carrying
/// one `only_checks` line turned a 12-finding FAIL into a clean PASS on the terminal, in the HTML
/// report that is built to travel, in SARIF, and in a signed certificate — and `diff
/// --fail-on-regression` read the five vanished findings as *resolved* and the score's 13-point
/// climb as an improvement. That is the same failure shape `CoverageNote` exists to prevent, one
/// axis over, and it takes the same remedy: a finding, because findings are the only channel that
/// reaches every renderer, the diff, and the certificate's own summary.
///
/// A *loosened* threshold is the third way, and it hid in the gap between the first two. Deselecting
/// `temporal.clock-skew` fires this finding; setting `clock_skew_ms = 10000` does not deselect
/// anything — the check runs, measures a real 210ms drift, and passes it — so the run looked
/// complete on every count this function was watching. The terminal and HTML reports named the
/// departure in a "Tolerances (non-default)" line, but SARIF and `diff` never saw it, and those are
/// the two consumers that gate CI: one `veridex.toml` line took a run from exit 20 with
/// `TEMPORAL.CLOCK_SKEW` and `TEMPORAL.END_OFFSET` to exit 0 with neither, and `diff
/// --fail-on-regression` read the score's 13-point climb as an improvement. Same shape, same
/// remedy.
///
/// The direction is the whole test: this finding exists because narrowing a run *raises* the score,
/// so only a threshold moved to be **looser** qualifies. A tightened one lowers the score, and
/// disclosing it as a narrowing tells a reader the opposite of what happened. That is not a corner
/// case — `--profile world-model-ready` tightens cross-sensor sync to 20 ms by construction, so a
/// direction-blind test made the product's own readiness profile self-incriminating: every profile
/// run emitted `SCOPE.NARROWED`, every profile certificate verified with a narrowing warning, and
/// `--min-score` refused to gate the profile runs it is meant for. The reports still print every
/// departure in either direction; only the disclosure is directional.
///
/// Info severity, for the reason coverage is: the run's scope is not a defect in the data, and
/// saying so must not fail a dataset that is genuinely sound.
fn scope_finding(
    config: &RunConfig,
    executed: usize,
    registered: usize,
    overridden: usize,
) -> Option<Finding> {
    let loosened = crate::report::loosened_tolerances(&config.tolerances);
    if executed == registered && overridden == 0 && loosened.is_empty() {
        return None;
    }

    let mut clauses: Vec<String> = Vec::new();
    if executed < registered {
        clauses.push(format!("{executed} of {registered} catalog checks ran"));
        if let Some(cats) = &config.categories {
            let mut names: Vec<&str> = cats.iter().map(|c| c.tag()).collect();
            names.sort_unstable();
            // `categories = []` selects nothing and is the most extreme narrowing there is, so the
            // one line reporting it must not trail off as "categories limited to )".
            clauses.push(match names.is_empty() {
                true => "categories limited to none".to_string(),
                false => format!("categories limited to {}", names.join(", ")),
            });
        }
        if let Some(only) = &config.only_checks {
            let mut ids: Vec<&str> = only.iter().map(String::as_str).collect();
            ids.sort_unstable();
            clauses.push(format!("only_checks = {}", ids.join(", ")));
        }
        if !config.disabled_checks.is_empty() {
            let mut ids: Vec<&str> = config.disabled_checks.iter().map(String::as_str).collect();
            ids.sort_unstable();
            clauses.push(format!("disabled = {}", ids.join(", ")));
        }
    }
    if overridden > 0 {
        let mut pairs: Vec<String> = config
            .severity_overrides
            .iter()
            .map(|(id, sev)| format!("{id} -> {}", sev.tag()))
            .collect();
        pairs.sort();
        clauses.push(format!("severity overridden: {}", pairs.join(", ")));
    }
    if !loosened.is_empty() {
        clauses.push(format!("thresholds loosened: {}", loosened.join(", ")));
    }

    Some(
        Finding::new(
            SCOPE_CHECK_ID,
            Category::Structural,
            Severity::Info,
            crate::check::Location::Dataset,
            "SCOPE.NARROWED",
            format!(
                "this run did not apply the full check catalog at its declared thresholds ({}), so \
                 every result below speaks for the narrowed scope alone",
                clauses.join("; ")
            ),
        )
        .with_risk(
            "The checks that did not run produced nothing, a check whose severity was lowered \
             produced something quieter than its author intended, and a check measured against a \
             moved threshold passed data the default would have failed. None of those silences is \
             evidence about the data. A clean result here is a clean result within this selection, \
             and read as a verdict on the dataset it waves through everything outside it.",
        )
        .with_remedy(
            "Re-run with the default catalog before treating this as a verdict on the dataset, or \
             keep the narrowing and read the result as scoped to it. Compare only against runs made \
             under the same selection — `veridex diff --fail-on-regression` now treats a change in \
             scope as a regression for this reason.",
        ),
    )
}

/// Whether a check runs under the given config: category filter, id allow-list, then disable-list.
fn check_selected(check: &dyn Check, config: &RunConfig) -> bool {
    if let Some(cats) = &config.categories {
        if !cats.contains(&check.category()) {
            return false;
        }
    }
    if let Some(only) = &config.only_checks {
        if !only.contains(check.id()) {
            return false;
        }
    }
    !config.disabled_checks.contains(check.id())
}

fn count_severities(findings: &[Finding]) -> SeverityCounts {
    let mut c = SeverityCounts::default();
    for f in findings {
        match f.severity {
            Severity::Error => c.error += 1,
            Severity::Warning => c.warning += 1,
            Severity::Info => c.info += 1,
        }
    }
    c
}

/// The run's overall status.
///
/// `errored` is not decoration. A check that panicked produced no findings, and reading no findings
/// as a pass is the failure mode this project names most often — here at its worst, because the
/// silence comes from a crash rather than from clean data. With every check crashing, the verdict
/// read `Pass`, `veridex check` exited 0, and CI went green over a dataset on which nothing was
/// measured at all. A crash is not evidence the data is bad, so it is not `Fail`; it is evidence the
/// verdict is incomplete, which is exactly what `PassWithWarnings` is for.
fn status_from(counts: &SeverityCounts, errored: &[ErroredCheck]) -> Status {
    if counts.error > 0 {
        Status::Fail
    } else if counts.warning > 0 || !errored.is_empty() {
        Status::PassWithWarnings
    } else {
        Status::Pass
    }
}

fn digest_hex<T: Serialize>(value: &T) -> String {
    // serde_json serializes struct fields in declaration order and BTreeMaps sorted, so the bytes
    // are deterministic for our verdict shape. The effective-config tolerances are f64s; serde_json
    // formats them with ryu (shortest round-trip, platform-independent), so they stay stable — the
    // tolerances are always finite (config parsing rejects non-finite), so none serialize to `null`.
    let bytes = serde_json::to_vec(value).expect("verdict serializes");
    let mut hasher = Sha256::new();
    hasher.update(b"veridex.verdict.v1\0");
    hasher.update(&bytes);
    let out: [u8; 32] = hasher.finalize().into();
    let mut s = String::with_capacity(64);
    for b in out {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "check panicked".to_string()
    }
}
