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
pub struct Tolerances {
    /// Max tolerated cross-stream duration drift for `TEMPORAL.CLOCK_SKEW`, in nanoseconds.
    pub clock_skew_ns: i64,
    /// Max tolerated shared-clock start offset for `TEMPORAL.START_OFFSET`, in nanoseconds.
    pub start_offset_ns: i64,
    /// Allowed relative rate deviation for `TEMPORAL.RATE` (0.10 = 10%).
    pub rate_deviation: f64,
    /// A `TEMPORAL.GAP` fires on an interval greater than this multiple of the expected interval.
    pub gap_factor: f64,
    /// `TEMPORAL.JITTER` fires when a stream's inter-frame intervals have a coefficient of variation
    /// (std / mean) above this — an irregular timeline whose mean rate can still look correct.
    pub jitter_cv: f64,
}

impl Default for Tolerances {
    fn default() -> Self {
        Tolerances {
            clock_skew_ns: 50_000_000,   // 50 ms
            start_offset_ns: 50_000_000, // 50 ms
            rate_deviation: 0.10,        // 10%
            gap_factor: 3.0,
            jitter_cv: 0.5,
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
            tolerances: c.tolerances,
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

/// The full result of a run. (Not `Eq`: the effective config's tolerances carry floats.)
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
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
    /// The distinct categories of the checks that ran (from `executed_checks`).
    pub fn checks_categories(&self) -> Vec<Category> {
        let mut cats: Vec<Category> = self.executed_checks.iter().map(|c| c.category).collect();
        cats.sort();
        cats.dedup();
        cats
    }
}

/// A subset view of the verdict used to compute [`Verdict::result_content_hash`] — every field
/// except the hash itself, in a fixed order.
#[derive(Serialize)]
struct DigestView<'a> {
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

    /// Validate `dataset` (whose content hash is `cdm_hash`) under `config`.
    pub fn run(&self, dataset: &Dataset, cdm_hash: ContentHash, config: &RunConfig) -> Verdict {
        let mut findings: Vec<Finding> = Vec::new();
        let mut errored_checks: Vec<ErroredCheck> = Vec::new();
        let mut executed_checks: Vec<ExecutedCheck> = Vec::new();

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
            let result = catch_unwind(AssertUnwindSafe(|| check.run(dataset)));
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

        // Stable total order, independent of execution order.
        findings.sort_by(|a, b| {
            a.check_id
                .cmp(b.check_id)
                .then_with(|| a.location.sort_key().cmp(&b.location.sort_key()))
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.message.cmp(&b.message))
                .then_with(|| a.severity.cmp(&b.severity))
        });
        executed_checks.sort_by(|a, b| a.check_id.cmp(&b.check_id));
        errored_checks.sort_by(|a, b| a.check_id.cmp(b.check_id));

        let counts = count_severities(&findings);
        let status = status_from(&counts);
        let effective_config = EffectiveConfig::from(config);

        let digest = DigestView {
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

fn status_from(counts: &SeverityCounts) -> Status {
    if counts.error > 0 {
        Status::Fail
    } else if counts.warning > 0 {
        Status::PassWithWarnings
    } else {
        Status::Pass
    }
}

fn digest_hex<T: Serialize>(value: &T) -> String {
    // serde_json serializes struct fields in declaration order and BTreeMaps sorted, so the bytes
    // are deterministic for our verdict shape (no floats involved).
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
