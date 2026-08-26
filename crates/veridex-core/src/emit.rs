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
    // Sorted by full content (key, value, class) — the encoder's key. Sorting by `key` alone leaves
    // ties resolved by Vec order, so two datasets with an identical content hash could emit
    // contradictory attribution.
    out.sort_by(|a, b| {
        crate::canonical::element_sort_key(a).cmp(&crate::canonical::element_sort_key(b))
    });
    out
}

/// Find the value of a known-or-asserted provenance element by key. Placeholder values (`unknown`,
/// `n/a`, …) are skipped: emitting them into a mapped schema.org field like `license` would present
/// fake provenance as real. The classified `veridex:provenance` list still carries every element.
fn known_value<'a>(dataset: &'a Dataset, key: &str) -> Option<&'a str> {
    // Deterministic across permutations: pick the content-smallest matching element rather than
    // whichever happens to come first in Vec order.
    dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .filter(|e| e.key == key && e.class != ProvenanceClass::Unknown && e.has_real_value())
        .min_by(|a, b| {
            crate::canonical::element_sort_key(a).cmp(&crate::canonical::element_sort_key(b))
        })
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
        // Modeled on the canonical Croissant 1.0 context, because a JSON-LD document means whatever
        // its context says it means and nothing else. Two terms here were wrong in a way no reader
        // would report: `conformsTo` under `@vocab` expands to `https://schema.org/conformsTo`,
        // while Croissant tooling looks for `http://purl.org/dc/terms/conformsTo` — so the document
        // declared conformance that no Croissant reader could see. `sha256` was mapped to
        // `cr:sha256`, while the reference implementation reads `https://schema.org/sha256`, so the
        // one field that pins *which* data this describes was invisible too. Both were syntactically
        // valid JSON-LD and semantically silent.
        json!({
            "@vocab": "https://schema.org/",
            "sc": "https://schema.org/",
            "cr": "http://mlcommons.org/croissant/",
            "dct": "http://purl.org/dc/terms/",
            "conformsTo": "dct:conformsTo",
            "veridex": "https://veridex.dev/ns#"
        }),
    );
    doc.insert("@type".into(), json!("sc:Dataset"));
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

/// Autonomy rig-lineage elements (design A3) surfaced as descriptive `veridex:` properties on the
/// entity — they attribute the recording (firmware, platform, drive, region, map, consent/redaction),
/// not an agent. Iterated in this order for deterministic output.
const PROV_ENTITY_PROPERTIES: &[&str] = &[
    "firmware",
    "calibration_session",
    "platform",
    "drive",
    "region",
    "map_version",
    "redaction",
    "consent",
    // Scenario/map/simulation references the log was recorded or replayed against.
    "scenario_ref",
    "scenario_version",
    "map_ref",
    "osi_version",
    "simulator",
];

/// Percent-encode one segment of a compact IRI.
///
/// Every value interpolated into an `@id` here is free text: `dataset.id` is derived from a
/// directory name, and the agent labels are lifted verbatim from source metadata. A space is all it
/// takes — `veridex:dataset/my robot data` is not a well-formed IRI, and a JSON-LD processor drops
/// the node *and every triple about it* rather than erroring. The document still looks like valid
/// JSON and the CLI still reports success, so `veridex provenance --emit prov` silently produced an
/// unattributed or entirely empty graph for the ordinary case of a dataset folder with a space in
/// its name, or an annotator recorded as "Jane Doe".
///
/// Conservative allow-list: unreserved characters per RFC 3986 pass through, everything else is
/// percent-encoded. The human-readable form is preserved separately in `veridex:label`, so nothing
/// is lost to the reader by encoding the identifier.
fn iri_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Emit a minimal W3C PROV document: the dataset as a `prov:Entity`, attributed to each known agent
/// (recorder / annotator / sensor) and derived from a known upstream dataset. Agents and the
/// upstream appear as nodes in the graph so the attributions resolve; nothing is fabricated.
pub fn to_prov(dataset: &Dataset) -> Value {
    let entity_id = format!("veridex:dataset/{}", iri_segment(&dataset.id));

    // Build agent nodes and the entity's attribution references from known provenance.
    let mut agent_nodes: Vec<Value> = Vec::new();
    let mut attributed: Vec<Value> = Vec::new();
    for (key, prov_type) in PROV_AGENTS {
        if let Some(value) = known_value(dataset, key) {
            let id = format!("veridex:agent/{key}/{}", iri_segment(value));
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
    // Autonomy rig lineage as descriptive properties on the entity (known values only).
    for key in PROV_ENTITY_PROPERTIES {
        if let Some(value) = known_value(dataset, key) {
            entity.insert(format!("veridex:{key}"), json!(value));
        }
    }
    if let Some(upstream) = known_value(dataset, "upstream") {
        let upstream_id = format!("veridex:dataset/{}", iri_segment(upstream));
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

/// Render provenance as a pretty JSON string in the requested format — `croissant` (default) or
/// `prov`. Shared by the CLI's `veridex provenance` and the Python `veridex.provenance` binding, so
/// both emit byte-identical documents. Returns `Err` with a message for an unknown format.
pub fn render_provenance(dataset: &Dataset, emit: &str) -> Result<String, String> {
    let doc = match emit {
        "croissant" => to_croissant(dataset, &crate::content_hash(dataset).to_hex()),
        "prov" => to_prov(dataset),
        other => {
            return Err(format!(
                "unknown emit `{other}` (expected `croissant` or `prov`)"
            ))
        }
    };
    Ok(serde_json::to_string_pretty(&doc).expect("provenance serializes"))
}
