//! The remote manifest read, end to end, against a fake Hub.
//!
//! No test here touches a network, and that is the design rather than a compromise: everything the
//! remote path *decides* — which files to ask for, what to do when one is missing, how the dataset is
//! identified, what coverage the run carries, what a caller is then refused — sits above the socket
//! behind [`FetchFile`]. A test that needs a network is a test that does not run in CI, and these
//! are the decisions worth pinning.
//!
//! What is deliberately not covered: TLS, redirects, and HTTP status handling live in `HubFetcher`
//! and are exercised by using the tool. The host allowlist that guards them is unit-tested in
//! `src/remote.rs`, where it needs no socket either.
#![cfg(feature = "remote")]

use std::collections::BTreeMap;

use veridex_core::adapter::{
    default_registry, Coverage, IngestError, IngestOptions, Sample, Source,
};
use veridex_core::remote::FetchFile;

/// A Hub that answers from a fixed map.
struct FakeHub(BTreeMap<String, Vec<u8>>);

impl FetchFile for FakeHub {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, IngestError> {
        match self.0.iter().find(|(k, _)| url.ends_with(k.as_str())) {
            Some((_, body)) if body.len() as u64 > max_bytes => Err(IngestError::Parse {
                format_id: "hub",
                message: "over the cap".into(),
            }),
            Some((_, body)) => Ok(Some(body.clone())),
            None => Ok(None),
        }
    }
}

/// A LeRobot v3 manifest as the Hub would serve it: two features, three episodes with declared
/// lengths, and a dataset card carrying the licence.
fn lerobot_manifest() -> FakeHub {
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10.0,
        "robot_type": "so100",
        "total_episodes": 3,
        "total_frames": 30,
        "features": {
            "observation.state": { "dtype": "float32", "shape": [6] },
            "action": { "dtype": "float32", "shape": [6] },
        },
    });
    let episodes = "{\"episode_index\": 0, \"length\": 10}\n\
                    {\"episode_index\": 1, \"length\": 10}\n\
                    {\"episode_index\": 2, \"length\": 10}\n";
    FakeHub(
        [
            ("meta/info.json", serde_json::to_string(&info).unwrap()),
            ("meta/episodes.jsonl", episodes.to_string()),
            (
                "README.md",
                "---\nlicense: apache-2.0\n---\n\n# A dataset\n".to_string(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_bytes()))
        .collect(),
    )
}

fn metadata_only() -> IngestOptions {
    IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    }
}

#[test]
fn a_hub_dataset_is_checked_from_its_manifest_alone() {
    let out = default_registry()
        .ingest_remote_with(
            "hf://lerobot/pickplace",
            &metadata_only(),
            &lerobot_manifest(),
        )
        .expect("a manifest-only remote ingest");

    // The dataset is the repository, not the temporary directory the manifest was staged in.
    assert_eq!(out.dataset.id, "lerobot/pickplace");
    assert_eq!(
        out.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: 3
        }
    );
    assert_eq!(out.dataset.episodes.len(), 3);
    assert!(out.dataset.episodes.iter().all(|e| e.streams.len() == 2));
    // Declared, not read: every episode has the manifest's length and no frames.
    assert_eq!(out.dataset.episodes[0].declared_frame_count, Some(10));
    assert!(out
        .dataset
        .episodes
        .iter()
        .all(|e| e.streams.iter().all(|s| s.frames.is_empty())));
    // The licence off the dataset card is real provenance and survives the round trip.
    assert!(out.dataset.provenance.iter().any(|p| p
        .elements
        .iter()
        .any(|e| e.key == "license" && e.value.as_deref() == Some("apache-2.0"))));
    // Where it came from is recorded rather than left implicit.
    assert!(out
        .dataset
        .metadata
        .iter()
        .any(|(k, v)| k == "hub_repo" && v == "lerobot/pickplace"));
    assert!(out
        .dataset
        .metadata
        .iter()
        .any(|(k, v)| k == "hub_revision" && v == "main"));
}

#[test]
fn the_owner_is_part_of_the_identity_the_hash_binds() {
    // Two owners publishing a dataset of the same name are two datasets. If the id dropped the
    // owner, their CDMs would hash identically and a certificate for one would verify the other.
    let a = default_registry()
        .ingest_remote_with(
            "hf://lerobot/pickplace",
            &metadata_only(),
            &lerobot_manifest(),
        )
        .unwrap()
        .dataset;
    let b = default_registry()
        .ingest_remote_with("hf://acme/pickplace", &metadata_only(), &lerobot_manifest())
        .unwrap()
        .dataset;
    assert_ne!(a.id, b.id);
    assert_ne!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b)
    );
}

#[test]
fn the_same_manifest_read_twice_yields_the_same_content_hash() {
    // The staging directory is temporary and differently named on every run, so it must not reach
    // the CDM. If it did, no two remote checks of one dataset would agree.
    let once = default_registry()
        .ingest_remote_with(
            "hf://lerobot/pickplace",
            &metadata_only(),
            &lerobot_manifest(),
        )
        .unwrap()
        .dataset;
    let twice = default_registry()
        .ingest_remote_with(
            "hf://lerobot/pickplace",
            &metadata_only(),
            &lerobot_manifest(),
        )
        .unwrap()
        .dataset;
    assert_eq!(
        veridex_core::content_hash(&once),
        veridex_core::content_hash(&twice)
    );
}

#[test]
fn a_revision_is_part_of_what_was_read() {
    let out = default_registry()
        .ingest_remote_with(
            "hf://lerobot/pickplace@v2.1",
            &metadata_only(),
            &lerobot_manifest(),
        )
        .unwrap();
    assert!(out
        .dataset
        .metadata
        .iter()
        .any(|(k, v)| k == "hub_revision" && v == "v2.1"));
}

#[test]
fn a_remote_run_carries_every_refusal_a_metadata_only_run_does() {
    // This is what makes the feature safe to offer: a run that read no data cannot become a signed
    // claim about data, and cannot pass a score gate. Both hang off the coverage, which is asserted
    // above; here the *ingest* refusals are pinned.
    let registry = default_registry();

    // A full remote check would mean downloading the dataset. Veridex is not a downloader.
    match registry.ingest(
        &Source::Remote("hf://lerobot/pickplace".into()),
        &IngestOptions::default(),
    ) {
        Err(IngestError::NotImplemented { what, hint }) => {
            assert!(what.contains("remote dataset's data"), "{what}");
            assert!(hint.contains("--metadata-only"), "{hint}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }

    // Sampling a manifest-only read would describe a second, different partial coverage.
    let sampled = IngestOptions {
        sample: Sample::FirstEpisodes(1),
        ..metadata_only()
    };
    match registry.ingest(&Source::Remote("hf://lerobot/pickplace".into()), &sampled) {
        Err(IngestError::InvalidSample { .. }) => {}
        other => panic!("expected a sampling refusal, got {other:?}"),
    }
}

#[test]
fn a_repository_that_is_not_a_lerobot_dataset_is_refused_not_read_as_empty() {
    let empty = FakeHub(BTreeMap::new());
    match default_registry().ingest_remote_with("hf://someone/notes", &metadata_only(), &empty) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("meta/info.json"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_manifest_that_contradicts_itself_is_still_checked_as_one() {
    // The whole point of reading a manifest is that it can be wrong. This one declares four episodes
    // and lists three, which is a real defect a remote check must still catch — the run is narrow,
    // not credulous.
    let mut hub = lerobot_manifest();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10.0,
        "total_episodes": 4,
        "total_frames": 40,
        "features": { "action": { "dtype": "float32", "shape": [6] } },
    });
    hub.0.insert(
        "meta/info.json".into(),
        serde_json::to_string(&info).unwrap().into_bytes(),
    );

    let out = default_registry()
        .ingest_remote_with("hf://lerobot/pickplace", &metadata_only(), &hub)
        .expect("the ingest succeeds; the disagreement is a finding, not a parse error");
    let engine = veridex_core::checks::default_engine().unwrap();
    let verdict = engine.run_over(
        &out.dataset,
        veridex_core::content_hash(&out.dataset),
        &veridex_core::RunConfig::default(),
        veridex_core::engine::CoverageNote::MetadataOnly {
            episodes_declared: 4,
        },
    );
    assert!(
        verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "{:?}",
        verdict.findings.iter().map(|f| &f.code).collect::<Vec<_>>()
    );
}
