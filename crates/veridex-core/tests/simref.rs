//! Scenario / map / simulation references: what the MCAP adapter extracts, how versions are
//! resolved, and how the result is reported and emitted.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{Adapter, IngestOptions, Source};
use veridex_core::cdm::{Dataset, ProvenanceClass};
use veridex_core::simref::{self, SimRefKind};

/// Write a one-channel MCAP carrying `meta` as a producer Metadata record, at `path`.
fn write_mcap_with_metadata(path: &Path, meta: BTreeMap<String, String>) {
    let mut out = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut out)).expect("writer");
        let schema = w
            .add_schema("sensor_msgs/msg/PointCloud2", "ros2msg", b"")
            .expect("schema");
        let channel = w
            .add_channel(schema, "/lidar/points", "cdr", &BTreeMap::new())
            .expect("channel");
        for (seq, t) in [1_000_000_000u64, 1_100_000_000, 1_200_000_000]
            .into_iter()
            .enumerate()
        {
            w.write_to_known_channel(
                &mcap::records::MessageHeader {
                    channel_id: channel,
                    sequence: seq as u32,
                    log_time: t,
                    publish_time: t,
                },
                b"payload",
            )
            .expect("write");
        }
        w.write_metadata(&mcap::records::Metadata {
            name: "recording_info".to_string(),
            metadata: meta,
        })
        .expect("metadata");
        w.finish().expect("finish");
    }
    std::fs::write(path, &out).expect("write mcap");
}

fn ingest(path: &Path) -> Dataset {
    McapAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset
}

fn element<'a>(dataset: &'a Dataset, key: &str) -> Option<&'a str> {
    dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == key && e.class != ProvenanceClass::Unknown)
        .and_then(|e| e.value.as_deref())
}

#[test]
fn sim_references_and_their_declared_versions_reach_provenance_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A real OpenDRIVE sidecar next to the log: its header revision is the map version.
    std::fs::create_dir_all(dir.path().join("maps")).expect("mkdir");
    std::fs::write(
        dir.path().join("maps/demo_town.xodr"),
        "<?xml version=\"1.0\"?>\n<OpenDRIVE>\n  <header revMajor=\"1\" revMinor=\"7\" name=\"demo_town\"/>\n</OpenDRIVE>\n",
    )
    .expect("write xodr");

    let mut meta = BTreeMap::new();
    meta.insert("scenario".into(), "OpenSCENARIO 1.2".to_string());
    meta.insert("opendrive".into(), "maps/demo_town.xodr".to_string());
    meta.insert("osi_version".into(), "3.5.0".to_string());
    meta.insert("simulator".into(), "carla-0.9.15".to_string());
    let mcap_path = dir.path().join("run.mcap");
    write_mcap_with_metadata(&mcap_path, meta);

    let d = ingest(&mcap_path);
    assert_eq!(element(&d, "scenario_ref"), Some("OpenSCENARIO 1.2"));
    // No sidecar for the scenario, so the version comes from the recorded value itself.
    assert_eq!(element(&d, "scenario_version"), Some("1.2"));
    assert_eq!(element(&d, "map_ref"), Some("maps/demo_town.xodr"));
    // The map version was read from the sidecar's own ASAM header, not guessed from the file name.
    assert_eq!(element(&d, "map_version"), Some("1.7"));
    assert_eq!(element(&d, "osi_version"), Some("3.5.0"));
    assert_eq!(element(&d, "simulator"), Some("carla-0.9.15"));

    let refs = simref::references(&d);
    assert_eq!(refs.len(), 4);
    assert_eq!(refs[0].kind, SimRefKind::Scenario);
    assert_eq!(refs[1].version.as_deref(), Some("1.7"));

    let rendered = simref::render_references(&d);
    assert!(
        rendered.contains("map: maps/demo_town.xodr (version 1.7)"),
        "{rendered}"
    );
    assert!(rendered.contains("simulator: carla-0.9.15"), "{rendered}");

    // The references travel with the PROV emit, so a consumer sees them without re-running Veridex.
    let prov = serde_json::to_string(&veridex_core::to_prov(&d)).expect("prov");
    assert!(prov.contains("veridex:scenario_ref"), "{prov}");
    assert!(prov.contains("carla-0.9.15"), "{prov}");
}

#[test]
fn an_explicitly_recorded_map_version_wins_over_the_sidecar_header() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("town.xodr"),
        "<OpenDRIVE><header revMajor=\"1\" revMinor=\"7\"/></OpenDRIVE>",
    )
    .expect("write xodr");

    let mut meta = BTreeMap::new();
    meta.insert("opendrive".into(), "town.xodr".to_string());
    meta.insert("map_version".into(), "hdmap-2026.03".to_string());
    let mcap_path = dir.path().join("run.mcap");
    write_mcap_with_metadata(&mcap_path, meta);

    let d = ingest(&mcap_path);
    assert_eq!(element(&d, "map_ref"), Some("town.xodr"));
    assert_eq!(element(&d, "map_version"), Some("hdmap-2026.03"));
}

#[test]
fn a_dataset_without_references_reports_and_emits_nothing_extra() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meta = BTreeMap::new();
    meta.insert("license".into(), "CC-BY-4.0".to_string());
    let mcap_path = dir.path().join("run.mcap");
    write_mcap_with_metadata(&mcap_path, meta);

    let d = ingest(&mcap_path);
    assert!(simref::references(&d).is_empty());
    assert!(simref::render_references(&d).is_empty());
    assert_eq!(element(&d, "scenario_ref"), None);
}

#[test]
fn a_reference_pointing_outside_the_dataset_is_not_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let outside = dir.path().join("outside.xodr");
    std::fs::write(
        &outside,
        "<OpenDRIVE><header revMajor=\"1\" revMinor=\"7\"/></OpenDRIVE>",
    )
    .expect("write xodr");
    let inner = dir.path().join("logs");
    std::fs::create_dir_all(&inner).expect("mkdir");

    let mut meta = BTreeMap::new();
    meta.insert("opendrive".into(), "../outside.xodr".to_string());
    let mcap_path = inner.join("run.mcap");
    write_mcap_with_metadata(&mcap_path, meta);

    let d = ingest(&mcap_path);
    // The reference is still recorded honestly; only the escaping file read is refused.
    assert_eq!(element(&d, "map_ref"), Some("../outside.xodr"));
    assert_eq!(element(&d, "map_version"), None);
}

#[test]
fn the_same_bytes_ingest_to_the_same_references_regardless_of_metadata_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut meta = BTreeMap::new();
    meta.insert("simulator".into(), "carla-0.9.15".to_string());
    meta.insert("scenario_file".into(), "cut_in.xosc".to_string());
    let a = dir.path().join("a.mcap");
    write_mcap_with_metadata(&a, meta.clone());
    let b = dir.path().join("b.mcap");
    write_mcap_with_metadata(&b, meta);

    let da = ingest(&a);
    let db = ingest(&b);
    assert_eq!(simref::references(&da), simref::references(&db));
    // A bare file name carries no dotted version, so none is invented.
    assert_eq!(element(&da, "scenario_ref"), Some("cut_in.xosc"));
    assert_eq!(element(&da, "scenario_version"), None);
}

#[test]
fn a_symlink_out_of_the_dataset_is_not_followed() {
    // The path components are clean (`link/secret.xosc` has no `..`), but the link leads outside the
    // dataset — following it would read a file the caller never pointed Veridex at.
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            outside.path().join("secret.xosc"),
            "<OpenSCENARIO><FileHeader revMajor=\"9\" revMinor=\"9\"/></OpenSCENARIO>",
        )
        .expect("write");

        let dir = tempfile::tempdir().expect("tempdir");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");

        assert_eq!(
            simref::sidecar_version(dir.path(), SimRefKind::Scenario, "link/secret.xosc"),
            None,
            "a symlink out of the dataset must not be read"
        );
    }
}
