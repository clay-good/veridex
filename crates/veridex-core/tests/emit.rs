//! Behavior tests for Croissant / PROV provenance emit.

use veridex_core::cdm::{
    Dataset, Episode, Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope,
};
use veridex_core::{content_hash, to_croissant, to_prov};

fn dataset_with(elements: Vec<ProvenanceElement>) -> Dataset {
    Dataset {
        id: "acme/pick".into(),
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
fn prov_attributes_and_derives_from_known_elements() {
    let d = dataset_with(vec![
        el("annotator", Some("alice"), ProvenanceClass::Known),
        el("upstream", Some("open-x"), ProvenanceClass::Known),
    ]);
    let doc = to_prov(&d);
    let entity = &doc["@graph"][0];
    assert_eq!(entity["@type"], "prov:Entity");
    assert!(entity["prov:wasAttributedTo"]["@id"]
        .as_str()
        .unwrap()
        .contains("alice"));
    assert!(entity["prov:wasDerivedFrom"]["@id"]
        .as_str()
        .unwrap()
        .contains("open-x"));
}
