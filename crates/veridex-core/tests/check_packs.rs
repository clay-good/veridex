//! Check-packs: third-party checks a verdict can account for.
//!
//! The plugin surface already existed — `Check` is public and object-safe, and `register` takes any
//! implementation — so a third party could add a check to a run and the verdict could not say it
//! happened. These tests hold the accountability that makes such a run mean something: the pack is
//! recorded, it moves the result hash, its findings name it, and it cannot be mistaken for the
//! built-in catalog.

use veridex_core::cdm::{Dataset, Episode};
use veridex_core::check::{Category, Check, Finding, Location, Scope, Severity};
use veridex_core::engine::{CheckPack, RegistryError};

/// A check a third party might write: it fires on every dataset, so its finding is easy to find.
struct AlwaysFires(&'static str);

impl Check for AlwaysFires {
    fn id(&self) -> &'static str {
        self.0
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &["MYLAB.HOUSE_RULE"]
    }
    fn title(&self) -> &'static str {
        "A rule this lab applies to its own data"
    }
    fn category(&self) -> Category {
        Category::Structural
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
    fn run(&self, _dataset: &Dataset) -> Vec<Finding> {
        vec![Finding::new(
            self.id(),
            Category::Structural,
            Severity::Info,
            Location::Dataset,
            "MYLAB.HOUSE_RULE",
            "this lab's own rule looked at the dataset".to_string(),
        )]
    }
}

fn dataset() -> Dataset {
    Dataset {
        id: "d".into(),
        metadata: Vec::new(),
        provenance: Vec::new(),
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(1),
            streams: Vec::new(),
            task: None,
            labels: Vec::new(),
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
        calibration: None,
    }
}

fn pack(name: &'static str, id: &'static str) -> Result<CheckPack, RegistryError> {
    CheckPack::new(name, "0.1.0", vec![Box::new(AlwaysFires(id))])
}

fn run_with(pack: Option<CheckPack>) -> veridex_core::engine::Verdict {
    let mut builder = veridex_core::engine::Engine::builder();
    if let Some(pack) = pack {
        builder = builder.register_pack(pack).expect("pack registers");
    }
    let engine = builder.build();
    let mut d = dataset();
    d.canonicalize_order();
    let hash = veridex_core::content_hash(&d);
    engine.run(&d, hash, &veridex_core::RunConfig::default())
}

/// The property the whole feature exists for: the same data checked under a larger catalog is a
/// different result, and says so. Before this, the two runs were indistinguishable.
#[test]
fn a_run_with_a_pack_differs_from_the_same_run_without_one() {
    let plain = run_with(None);
    let extended = run_with(Some(pack("mylab", "mylab/house-rule").unwrap()));

    assert!(plain.packs.is_empty());
    assert_eq!(extended.packs.len(), 1);
    assert_eq!(extended.packs[0].name, "mylab");
    assert_eq!(extended.packs[0].version, "0.1.0");
    assert_ne!(
        plain.result_content_hash, extended.result_content_hash,
        "a verdict must not hash identically to one produced by a different set of checks"
    );
}

/// And the direction that keeps every existing artifact valid: a run over the built-in catalog alone
/// hashes and serializes exactly as it did before packs existed.
#[test]
fn a_run_without_packs_is_unchanged_by_the_existence_of_packs() {
    let plain = run_with(None);
    let json = serde_json::to_string(&plain).expect("verdict serializes");
    assert!(
        !json.contains("packs"),
        "an empty pack list must not appear in the verdict at all: {json}"
    );
}

/// A finding from a pack names its pack, through the id every renderer already prints.
#[test]
fn a_packs_finding_is_attributable_to_it() {
    let extended = run_with(Some(pack("mylab", "mylab/house-rule").unwrap()));
    let mine = extended
        .findings
        .iter()
        .find(|f| f.code == "MYLAB.HOUSE_RULE")
        .expect("the pack's check ran");
    assert_eq!(mine.check_id, "mylab/house-rule");
    assert!(
        extended
            .executed_checks
            .iter()
            .any(|c| c.check_id == "mylab/house-rule"),
        "and it is listed among the checks the run executed"
    );
}

/// A pack's ids must sit under its own name. The registry refuses rather than rewriting them: the id
/// in a pack's source is the id in every report, so a reader tracing a finding back to the code that
/// raised it finds the same string.
#[test]
fn a_check_outside_its_packs_namespace_is_refused() {
    let err = pack("mylab", "house-rule").expect_err("an unnamespaced id must be refused");
    assert!(
        matches!(
            err,
            RegistryError::PackNamespace {
                pack: "mylab",
                check: "house-rule"
            }
        ),
        "{err:?}"
    );
    // And one that merely starts with the name is not under it either.
    assert!(pack("mylab", "mylabhouse-rule").is_err());
}

/// A pack may not take a built-in family's name. A built-in id carries no `/`, so
/// `structural/duplicate-episode` cannot *collide* with `structural.duplicate-episode` — it would
/// just sit in a report looking exactly like it, one character apart.
#[test]
fn a_pack_cannot_impersonate_the_built_in_catalog() {
    let err = pack("structural", "structural/duplicate-episode")
        .expect_err("a built-in family name must be refused");
    assert!(
        matches!(
            err,
            RegistryError::PackName {
                name: "structural",
                ..
            }
        ),
        "{err:?}"
    );
}

/// A name that cannot serve as a namespace is refused at the point the pack is defined.
#[test]
fn a_pack_name_that_is_not_a_namespace_is_refused() {
    assert!(pack("", "/x").is_err());
    assert!(
        pack("my/lab", "my/lab/x").is_err(),
        "a `/` would make the namespace ambiguous"
    );
    assert!(
        pack("MyLab", "MyLab/x").is_err(),
        "case is part of an id, so the name is lowercase"
    );
}

/// Two packs cannot shadow each other, and a pack cannot shadow the catalog — the same refusal
/// `register` already made for a duplicate id.
#[test]
fn two_packs_cannot_register_the_same_id() {
    let first = pack("mylab", "mylab/house-rule").unwrap();
    let second = pack("mylab", "mylab/house-rule").unwrap();
    let builder = veridex_core::engine::Engine::builder()
        .register_pack(first)
        .expect("the first registers");
    assert!(
        matches!(
            builder.register_pack(second),
            Err(RegistryError::DuplicateId("mylab/house-rule"))
        ),
        "a duplicate id must be refused however it arrives"
    );
}
