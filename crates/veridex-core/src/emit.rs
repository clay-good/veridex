//! Provenance emit: render the CDM's extracted provenance as portable metadata.
//!
//! Veridex rides existing standards rather than inventing a rival format (design D9): it emits
//! **MLCommons Croissant** (JSON-LD) for distribution, plus a **minimal W3C PROV** lineage. Both
//! carry only what the CDM actually knows — each provenance element keeps its known / asserted /
//! unknown class, and nothing is fabricated.

use serde_json::{json, Map, Value};

use crate::cdm::{Dataset, ProvenanceClass, ProvenanceElement};

/// All provenance elements across every record, sorted by key for deterministic output.
fn collect_elements(dataset: &Dataset) -> Vec<&ProvenanceElement> {
    let mut out: Vec<&ProvenanceElement> = Vec::new();
    for record in &dataset.provenance {
        for el in &record.elements {
            out.push(el);
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}

/// Find the value of a known-or-asserted provenance element by key.
fn known_value<'a>(dataset: &'a Dataset, key: &str) -> Option<&'a str> {
    dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == key && e.class != ProvenanceClass::Unknown && e.value.is_some())
        .and_then(|e| e.value.as_deref())
}

/// Emit an MLCommons Croissant (JSON-LD) document carrying the dataset identity, its CDM content
/// hash, and its extracted provenance.
pub fn to_croissant(dataset: &Dataset, cdm_content_hash: &str) -> Value {
    // Each provenance element, preserving its class so consumers see what is known vs. unknown.
    let provenance: Vec<Value> = collect_elements(dataset)
        .iter()
        .map(|e| {
            json!({
                "key": e.key,
                "value": e.value,
                "class": e.class.tag(),
            })
        })
        .collect();

    let mut doc = Map::new();
    doc.insert(
        "@context".into(),
        json!({
            "@vocab": "https://schema.org/",
            "cr": "http://mlcommons.org/croissant/",
            "sha256": "cr:sha256",
            "veridex": "https://veridex.dev/ns#"
        }),
    );
    doc.insert("@type".into(), json!("Dataset"));
    doc.insert(
        "conformsTo".into(),
        json!("http://mlcommons.org/croissant/1.0"),
    );
    doc.insert("name".into(), json!(dataset.id));
    doc.insert(
        "description".into(),
        json!(format!(
            "Veridex-extracted provenance for `{}`.",
            dataset.id
        )),
    );

    // Map recognized provenance onto standard Croissant/schema.org fields when known.
    if let Some(license) = known_value(dataset, "license") {
        doc.insert("license".into(), json!(license));
    }
    if let Some(annotator) = known_value(dataset, "annotator") {
        doc.insert(
            "creator".into(),
            json!({ "@type": "Person", "name": annotator }),
        );
    }

    // The CDM content hash as a Croissant FileObject the certificate also binds to.
    doc.insert(
        "distribution".into(),
        json!([{
            "@type": "cr:FileObject",
            "@id": "cdm",
            "name": "canonical-dataset-model",
            "description": "The canonicalized Veridex CDM this metadata describes.",
            "sha256": cdm_content_hash
        }]),
    );

    // The honest, classified provenance list.
    doc.insert("veridex:provenance".into(), json!(provenance));

    Value::Object(doc)
}

/// Known provenance elements that name an agent the dataset can be honestly attributed to, each
/// paired with the PROV agent subtype that fits it. Iterated in this order for deterministic output.
const PROV_AGENTS: &[(&str, &str)] = &[
    ("recorder", "prov:SoftwareAgent"),
    ("annotator", "prov:Person"),
    ("sensor", "prov:Agent"),
];

/// Emit a minimal W3C PROV document: the dataset as a `prov:Entity`, attributed to each known agent
/// (recorder / annotator / sensor) and derived from a known upstream dataset. Agents and the
/// upstream appear as nodes in the graph so the attributions resolve; nothing is fabricated.
pub fn to_prov(dataset: &Dataset) -> Value {
    let entity_id = format!("veridex:dataset/{}", dataset.id);

    // Build agent nodes and the entity's attribution references from known provenance.
    let mut agent_nodes: Vec<Value> = Vec::new();
    let mut attributed: Vec<Value> = Vec::new();
    for (key, prov_type) in PROV_AGENTS {
        if let Some(value) = known_value(dataset, key) {
            let id = format!("veridex:agent/{key}/{value}");
            agent_nodes.push(json!({
                "@id": id,
                "@type": prov_type,
                "veridex:role": key,
                "veridex:label": value,
            }));
            attributed.push(json!({ "@id": id }));
        }
    }

    let mut entity = Map::new();
    entity.insert("@id".into(), json!(entity_id));
    entity.insert("@type".into(), json!("prov:Entity"));
    if !attributed.is_empty() {
        entity.insert("prov:wasAttributedTo".into(), json!(attributed));
    }
    if let Some(upstream) = known_value(dataset, "upstream") {
        let upstream_id = format!("veridex:dataset/{upstream}");
        entity.insert("prov:wasDerivedFrom".into(), json!({ "@id": upstream_id }));
        agent_nodes.push(json!({ "@id": upstream_id, "@type": "prov:Entity" }));
    }

    // Entity first, then the agents/upstream it references.
    let mut graph = vec![Value::Object(entity)];
    graph.extend(agent_nodes);

    json!({
        "@context": { "prov": "http://www.w3.org/ns/prov#", "veridex": "https://veridex.dev/ns#" },
        "@graph": graph
    })
}
