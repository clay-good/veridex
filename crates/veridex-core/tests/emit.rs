//! Behavior tests for Croissant / PROV provenance emit.

use veridex_core::cdm::{
    Dataset, Episode, Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope,
};
use veridex_core::{content_hash, render_provenance, to_croissant, to_prov};

fn dataset_with(elements: Vec<ProvenanceElement>) -> Dataset {
    Dataset {
        id: "acme/pick".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        }],
        episodes: vec![Episode {
            index: 0,
            start_ts: None,
            end_ts: None,
            streams: vec![],
            task: None,
            labels: vec![],
            ego_poses: None,
            declared_frame_count: None,
        }],
    }
}

fn el(key: &str, value: Option<&str>, class: ProvenanceClass) -> ProvenanceElement {
    ProvenanceElement {
        key: key.into(),
        value: value.map(|v| v.into()),
        class,
    }
}

#[test]
fn croissant_is_well_formed_and_binds_content_hash() {
    let d = dataset_with(vec![
        el("license", Some("apache-2.0"), ProvenanceClass::Known),
        el("annotator", Some("team-a"), ProvenanceClass::Asserted),
    ]);
    let hash = content_hash(&d).to_hex();
    let doc = to_croissant(&d, &hash);

    assert_eq!(doc["@type"], "Dataset");
    assert_eq!(doc["conformsTo"], "http://mlcommons.org/croissant/1.0");
    assert_eq!(doc["name"], "acme/pick");
    // Known license/annotator map onto standard schema.org fields.
    assert_eq!(doc["license"], "apache-2.0");
    assert_eq!(doc["creator"]["name"], "team-a");
    // The CDM content hash is carried as a distribution sha256.
    assert_eq!(doc["distribution"][0]["sha256"], hash);
    // It is valid JSON (serializes without error).
    assert!(serde_json::to_string(&doc).is_ok());
}

#[test]
fn croissant_preserves_provenance_classes_and_does_not_fabricate() {
    let d = dataset_with(vec![
        el("license", Some("mit"), ProvenanceClass::Known),
        el("sensor", None, ProvenanceClass::Unknown),
    ]);
    let doc = to_croissant(&d, "deadbeef");
    let prov = doc["veridex:provenance"].as_array().unwrap();

    let license = prov.iter().find(|e| e["key"] == "license").unwrap();
    assert_eq!(license["class"], "known");
    assert_eq!(license["value"], "mit");

    let sensor = prov.iter().find(|e| e["key"] == "sensor").unwrap();
    assert_eq!(sensor["class"], "unknown");
    assert!(
        sensor["value"].is_null(),
        "unknown sensor must not get a fabricated value"
    );

    // An unknown license must not appear as a schema.org license.
    let d2 = dataset_with(vec![el("license", None, ProvenanceClass::Unknown)]);
    let doc2 = to_croissant(&d2, "x");
    assert!(doc2.get("license").is_none());
}

#[test]
fn placeholder_license_is_not_emitted_as_a_schema_org_field() {
    // A license "known" as the literal "unknown" is fake provenance: it must not populate the mapped
    // schema.org `license` field, though it still appears (classified) in the honest list.
    let d = dataset_with(vec![el("license", Some("unknown"), ProvenanceClass::Known)]);
    let doc = to_croissant(&d, "x");
    assert!(
        doc.get("license").is_none(),
        "a placeholder license must not be emitted as a real schema.org license"
    );
    let prov = doc["veridex:provenance"].as_array().unwrap();
    let license = prov.iter().find(|e| e["key"] == "license").unwrap();
    assert_eq!(
        license["value"], "unknown",
        "the honest list still records it"
    );
}

#[test]
fn prov_attributes_and_derives_from_known_elements() {
    let d = dataset_with(vec![
        el("annotator", Some("alice"), ProvenanceClass::Known),
        el("upstream", Some("open-x"), ProvenanceClass::Known),
    ]);
    let doc = to_prov(&d);
    let entity = &doc["@graph"][0];
    assert_eq!(entity["@type"], "prov:Entity");
    // Attribution is a list of agent references (a dataset can have several agents).
    let attributed = entity["prov:wasAttributedTo"].as_array().unwrap();
    assert!(attributed
        .iter()
        .any(|a| a["@id"].as_str().unwrap().contains("alice")));
    assert!(entity["prov:wasDerivedFrom"]["@id"]
        .as_str()
        .unwrap()
        .contains("open-x"));
}

#[test]
fn prov_attributes_the_recorder_as_a_software_agent() {
    // The MCAP header's writing library surfaces as a `recorder` element; PROV must attribute the
    // dataset to it as a prov:SoftwareAgent, and the agent node must appear in the graph.
    let d = dataset_with(vec![el(
        "recorder",
        Some("mcap-rust/0.25.0"),
        ProvenanceClass::Known,
    )]);
    let doc = to_prov(&d);
    let graph = doc["@graph"].as_array().unwrap();

    let attributed = doc["@graph"][0]["prov:wasAttributedTo"].as_array().unwrap();
    let recorder_id = attributed[0]["@id"].as_str().unwrap();
    assert!(recorder_id.contains("recorder"));

    let agent = graph
        .iter()
        .find(|n| n["@id"] == recorder_id)
        .expect("recorder agent node present in graph");
    assert_eq!(agent["@type"], "prov:SoftwareAgent");
    assert_eq!(agent["veridex:label"], "mcap-rust/0.25.0");
}

#[test]
fn prov_omits_attribution_when_no_agents_are_known() {
    // A dataset with only source_format (no agent elements) yields a bare entity — no fabricated
    // attribution.
    let d = dataset_with(vec![el(
        "source_format",
        Some("mcap"),
        ProvenanceClass::Known,
    )]);
    let doc = to_prov(&d);
    assert!(doc["@graph"][0].get("prov:wasAttributedTo").is_none());
    assert_eq!(doc["@graph"].as_array().unwrap().len(), 1);
}

#[test]
fn render_provenance_matches_the_underlying_emitters_and_rejects_unknown_formats() {
    let d = dataset_with(vec![el("license", Some("MIT"), ProvenanceClass::Known)]);

    // `croissant` (default) and `prov` render exactly what the direct emitters produce, pretty-printed.
    let croissant = render_provenance(&d, "croissant").expect("croissant renders");
    let expected_croissant =
        serde_json::to_string_pretty(&to_croissant(&d, &content_hash(&d).to_hex())).unwrap();
    assert_eq!(croissant, expected_croissant);

    let prov = render_provenance(&d, "prov").expect("prov renders");
    assert_eq!(prov, serde_json::to_string_pretty(&to_prov(&d)).unwrap());

    // An unknown format is a clear error, not a silent default.
    let err = render_provenance(&d, "yaml").unwrap_err();
    assert!(err.contains("unknown emit"));
}
