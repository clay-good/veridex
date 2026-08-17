//! Behavior tests for the validation engine.

use veridex_core::cdm::{Dataset, Episode};
use veridex_core::check::{Category, Check, Finding, Location, Scope, Severity};
use veridex_core::engine::{Engine, RegistryError, RunConfig, Status};
use veridex_core::{content_hash, ContentHash};

/// A check that flags every episode as empty (for deterministic, controllable findings).
struct FlagEpisodes {
    id: &'static str,
    category: Category,
    severity: Severity,
}

impl Check for FlagEpisodes {
    fn id(&self) -> &'static str {
        self.id
    }
    fn title(&self) -> &'static str {
        "flag every episode"
    }
    fn category(&self) -> Category {
        self.category
    }
    fn default_severity(&self) -> Severity {
        self.severity
    }
    fn scope(&self) -> Scope {
        Scope::Episode
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["EPISODE.FLAGGED"]
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        dataset
            .episodes
            .iter()
            .map(|ep| {
                Finding::new(
                    self.id,
                    self.category,
                    self.severity,
                    Location::Episode { episode: ep.index },
                    "EPISODE.FLAGGED",
                    format!("episode {} flagged", ep.index),
                )
            })
            .collect()
    }
}

/// A check that always panics, to exercise fault isolation.
struct Crasher;
impl Check for Crasher {
    fn id(&self) -> &'static str {
        "test.crasher"
    }
    fn title(&self) -> &'static str {
        "always panics"
    }
    fn category(&self) -> Category {
        Category::Structural
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Dataset
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["TEST.CRASH"]
    }
    fn run(&self, _dataset: &Dataset) -> Vec<Finding> {
        panic!("boom");
    }
}

fn ds(n: u64) -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes: (0..n)
            .map(|i| Episode {
                index: i,
                start_ts: None,
                end_ts: None,
                streams: vec![],
                task: None,
                labels: vec![],
                ego_poses: None,
                declared_frame_count: None,
            })
            .collect(),
    }
}

fn hash(d: &Dataset) -> ContentHash {
    content_hash(d)
}

#[test]
fn duplicate_check_id_is_rejected() {
    let result = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "dup",
            category: Category::Structural,
            severity: Severity::Info,
        }))
        .unwrap()
        .register(Box::new(FlagEpisodes {
            id: "dup",
            category: Category::Temporal,
            severity: Severity::Info,
        }));
    match result {
        Err(e) => assert_eq!(e, RegistryError::DuplicateId("dup")),
        Ok(_) => panic!("expected a duplicate-id error"),
    }
}

#[test]
fn same_cdm_yields_byte_identical_verdict_and_shared_hash() {
    let d = ds(3);
    let engine = || {
        Engine::builder()
            .register(Box::new(FlagEpisodes {
                id: "structural.flag",
                category: Category::Structural,
                severity: Severity::Warning,
            }))
            .unwrap()
            .build()
    };
    let cfg = RunConfig::default();
    let v1 = engine().run(&d, hash(&d), &cfg);
    let v2 = engine().run(&d, hash(&d), &cfg);
    assert_eq!(v1, v2);
    assert_eq!(v1.result_content_hash, v2.result_content_hash);
    // JSON serialization is byte-identical too.
    assert_eq!(
        serde_json::to_string(&v1).unwrap(),
        serde_json::to_string(&v2).unwrap()
    );
}

#[test]
fn error_finding_drives_fail_status() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "e",
            category: Category::Structural,
            severity: Severity::Error,
        }))
        .unwrap()
        .build();
    let v = engine.run(&d, hash(&d), &RunConfig::default());
    assert_eq!(v.status, Status::Fail);
    assert_eq!(v.counts.error, 1);
}

#[test]
fn warning_only_yields_pass_with_warnings_and_info_yields_pass() {
    let d = ds(2);
    let warn = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "w",
            category: Category::Structural,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build()
        .run(&d, hash(&d), &RunConfig::default());
    assert_eq!(warn.status, Status::PassWithWarnings);

    let info = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "i",
            category: Category::Structural,
            severity: Severity::Info,
        }))
        .unwrap()
        .build()
        .run(&d, hash(&d), &RunConfig::default());
    assert_eq!(info.status, Status::Pass);
}

#[test]
fn category_selection_scopes_the_run() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "structural.x",
            category: Category::Structural,
            severity: Severity::Warning,
        }))
        .unwrap()
        .register(Box::new(FlagEpisodes {
            id: "temporal.y",
            category: Category::Temporal,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build();

    let cfg = RunConfig {
        categories: Some([Category::Temporal].into_iter().collect()),
        ..Default::default()
    };
    let v = engine.run(&d, hash(&d), &cfg);

    assert_eq!(v.executed_checks.len(), 1);
    assert_eq!(v.executed_checks[0].check_id, "temporal.y");
    // The structural check did not run, so nothing may be reported under it...
    assert!(v.findings.iter().all(|f| f.check_id != "structural.x"));
    // ...and the run must say out loud that it did not apply the whole catalog. Without this, a
    // category filter turns a failing dataset into a clean PASS on every human-facing surface.
    let scope: Vec<_> = v
        .findings
        .iter()
        .filter(|f| f.check_id == veridex_core::engine::SCOPE_CHECK_ID)
        .collect();
    assert_eq!(scope.len(), 1);
    assert_eq!(scope[0].code, "SCOPE.NARROWED");
    assert_eq!(scope[0].severity, Severity::Info);
    assert!(scope[0].message.contains("1 of 2 catalog checks ran"));
    assert!(scope[0].message.contains("categories limited to temporal"));
    assert_eq!(
        v.effective_config.categories,
        Some(vec![Category::Temporal])
    );
}

/// The full catalog at declared severities is the silent case: no scope finding at all, so an
/// ordinary run's output and its content hash are unchanged by this disclosure existing.
#[test]
fn an_unnarrowed_run_says_nothing_about_scope() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "structural.x",
            category: Category::Structural,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build();

    let v = engine.run(&d, hash(&d), &RunConfig::default());
    assert!(v
        .findings
        .iter()
        .all(|f| f.check_id != veridex_core::engine::SCOPE_CHECK_ID));
}

/// A severity override narrows what the run reports without removing a check, so it is disclosed
/// too — a check downgraded to `info` produces something quieter than its author intended.
#[test]
fn a_severity_override_is_disclosed_as_a_narrowed_scope() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "structural.x",
            category: Category::Structural,
            severity: Severity::Error,
        }))
        .unwrap()
        .build();

    let cfg = RunConfig {
        severity_overrides: [("structural.x".to_string(), Severity::Info)]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let v = engine.run(&d, hash(&d), &cfg);

    let scope = v
        .findings
        .iter()
        .find(|f| f.check_id == veridex_core::engine::SCOPE_CHECK_ID)
        .expect("a severity override is a narrowed scope");
    assert!(scope.message.contains("structural.x -> info"));
    // Every catalog check still ran, so the count clause must not appear.
    assert!(!scope.message.contains("catalog checks ran"));
}

/// A moved threshold narrows the run without deselecting anything: the check runs, measures a real
/// defect, and passes it. The terminal and HTML reports named the departure; SARIF and `diff` — the
/// two consumers that gate CI — did not, so one `veridex.toml` line took a run from exit 20 to
/// exit 0 with no trace and `diff --fail-on-regression` read the score's climb as an improvement.
#[test]
fn a_moved_threshold_is_disclosed_as_a_narrowed_scope() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "structural.x",
            category: Category::Structural,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build();

    let cfg = RunConfig {
        tolerances: veridex_core::Tolerances {
            clock_skew_ns: 10_000_000_000,
            ..Default::default()
        },
        ..Default::default()
    };
    let v = engine.run(&d, hash(&d), &cfg);

    let scope = v
        .findings
        .iter()
        .find(|f| f.check_id == veridex_core::engine::SCOPE_CHECK_ID)
        .expect("a moved tolerance is a narrowed scope");
    assert_eq!(scope.code, "SCOPE.NARROWED");
    assert_eq!(scope.severity, Severity::Info);
    assert!(
        scope
            .message
            .contains("thresholds moved: clock-skew 10000ms"),
        "the disclosure must name which threshold moved and to what, got: {}",
        scope.message
    );
    // Nothing was deselected and no severity was changed, so neither of the other two clauses
    // belongs here.
    assert!(!scope.message.contains("catalog checks ran"));
    assert!(!scope.message.contains("severity overridden"));
}

#[test]
fn severity_override_is_applied_and_recorded() {
    let d = ds(1);
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "ovr",
            category: Category::Structural,
            severity: Severity::Error,
        }))
        .unwrap()
        .build();

    let cfg = RunConfig {
        severity_overrides: [("ovr".to_string(), Severity::Warning)]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let v = engine.run(&d, hash(&d), &cfg);

    assert_eq!(v.status, Status::PassWithWarnings);
    // The override applies to the overridden check's own findings. The run also discloses that a
    // severity was overridden at all, which is an `info` finding and deliberately not subject to
    // the override it is reporting.
    assert!(v
        .findings
        .iter()
        .filter(|f| f.check_id == "ovr")
        .all(|f| f.severity == Severity::Warning));
    assert_eq!(
        v.effective_config.severity_overrides.get("ovr"),
        Some(&Severity::Warning)
    );
}

#[test]
fn crashing_check_is_isolated_and_reported_separately() {
    let d = ds(1);
    // Silence the default panic hook so the intentional panic doesn't clutter test output.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let engine = Engine::builder()
        .register(Box::new(Crasher))
        .unwrap()
        .register(Box::new(FlagEpisodes {
            id: "ok",
            category: Category::Structural,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build();
    let v = engine.run(&d, hash(&d), &RunConfig::default());
    std::panic::set_hook(prev);

    // The crasher is recorded as errored, separately from data findings.
    assert_eq!(v.errored_checks.len(), 1);
    assert_eq!(v.errored_checks[0].check_id, "test.crasher");
    assert!(v.errored_checks[0].message.contains("boom"));
    // The other check still ran.
    assert!(v.findings.iter().any(|f| f.check_id == "ok"));
    // Reproducibility metadata is present.
    assert_eq!(v.cdm_content_hash, hash(&d).to_hex());
    assert!(!v.veridex_version.is_empty());
}

/// A crash produces no findings, and no findings used to read as a pass — here at its worst, since
/// the silence comes from a check that did not run rather than from clean data. With every check
/// crashing the verdict said `Pass` and the CLI exited 0.
#[test]
fn a_run_in_which_every_check_crashed_is_not_a_pass() {
    let d = ds(1);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let engine = Engine::builder()
        .register(Box::new(Crasher))
        .unwrap()
        .build();
    let v = engine.run(&d, hash(&d), &RunConfig::default());
    std::panic::set_hook(prev);

    assert!(v.findings.is_empty(), "a crash produces no findings");
    assert_eq!(v.counts.error + v.counts.warning + v.counts.info, 0);
    assert_eq!(
        v.status,
        Status::PassWithWarnings,
        "nothing was measured; that is an incomplete verdict, not a clean one"
    );
}

/// `executed_checks` records invocation, not success, so a category whose only check crashed used to
/// count as covered — and the certificate derives `categories_skipped` from exactly this.
#[test]
fn a_category_whose_check_crashed_does_not_count_as_covered() {
    let d = ds(1);
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let engine = Engine::builder()
        .register(Box::new(Crasher))
        .unwrap()
        // A different category, so `Structural` is represented only by the check that crashed.
        .register(Box::new(FlagEpisodes {
            id: "ok",
            category: Category::Temporal,
            severity: Severity::Warning,
        }))
        .unwrap()
        .build();
    let v = engine.run(&d, hash(&d), &RunConfig::default());
    std::panic::set_hook(prev);

    let covered = v.checks_categories();
    assert!(
        covered.contains(&Category::Temporal),
        "the check that finished still counts"
    );
    assert!(
        !covered.contains(&Category::Structural),
        "a category is not covered by a check that crashed in it: {covered:?}"
    );
}

#[test]
fn findings_are_sorted_deterministically_regardless_of_registration_order() {
    let d = ds(3);
    // Register temporal before structural; findings must still sort by (check_id, location, ...).
    let engine = Engine::builder()
        .register(Box::new(FlagEpisodes {
            id: "zzz.last",
            category: Category::Temporal,
            severity: Severity::Info,
        }))
        .unwrap()
        .register(Box::new(FlagEpisodes {
            id: "aaa.first",
            category: Category::Structural,
            severity: Severity::Info,
        }))
        .unwrap()
        .build();
    let v = engine.run(&d, hash(&d), &RunConfig::default());

    let ids: Vec<&str> = v.findings.iter().map(|f| f.check_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "findings must be in a stable total order");
}
