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
    assert!(v.findings.iter().all(|f| f.check_id == "temporal.y"));
    assert_eq!(
        v.effective_config.categories,
        Some(vec![Category::Temporal])
    );
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
    assert!(v.findings.iter().all(|f| f.severity == Severity::Warning));
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
