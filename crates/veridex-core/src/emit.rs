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

/// Every distinct known-or-asserted value recorded for a key, sorted.
///
/// The plural of [`known_value`], for the keys where more than one value is a fact about the dataset
/// rather than a contradiction: a merge really does have several upstreams. Placeholder values are
/// skipped for the same reason they are there.
fn known_values<'a>(dataset: &'a Dataset, key: &str) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for record in &dataset.provenance {
        for el in &record.elements {
            if el.key == key && el.class != ProvenanceClass::Unknown && el.has_real_value() {
                if let Some(value) = el.value.as_deref() {
                    out.push(value);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
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
    // `creator` is a schema.org `Person`, so only a value that names one goes there. A category —
    // `crowdsourced`, `machine-generated` — is still the honest answer to who annotated the data,
    // and it is carried in `veridex:provenance` below with its class, where it says what it is
    // instead of asserting a person by that name.
    // Every annotator the source named, not the first: schema.org's `creator` takes a list, and a
    // dataset annotated by two teams that credits one credits the wrong number of people. A
    // *category* (`crowdsourced`, `machine-generated`) is not a person and is carried separately —
    // see `annotator_names_an_agent`.
    let (people, categories): (Vec<&str>, Vec<&str>) = known_values(dataset, "annotator")
        .into_iter()
        .partition(|v| annotator_names_an_agent(v));
    match people.as_slice() {
        [] => {}
        [one] => {
            doc.insert("creator".into(), json!({ "@type": "Person", "name": one }));
        }
        many => {
            doc.insert(
                "creator".into(),
                json!(many
                    .iter()
                    .map(|name| json!({ "@type": "Person", "name": name }))
                    .collect::<Vec<_>>()),
            );
        }
    }
    if !categories.is_empty() {
        doc.insert(
            "veridex:annotationCreators".into(),
            json!(categories.join(", ")),
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

/// The Hugging Face `annotations_creators` vocabulary: how a dataset's annotations were *produced*,
/// not who produced them. A LeRobot card writes `crowdsourced` or `machine-generated` here, and
/// those are the honest answer to the question `provenance.annotator` asks — but they are categories,
/// not agents.
///
/// The distinction matters exactly where this file does: schema.org's `creator` is a `Person` or an
/// `Organization`, and PROV's `prov:Person` is a person. Emitting `{"@type": "Person", "name":
/// "crowdsourced"}` asserts a person by that name, which no source said and no validator would
/// catch — a fabrication of precisely the kind the Croissant output promises not to make.
const ANNOTATION_CREATOR_CATEGORIES: &[&str] = &[
    "crowdsourced",
    "expert-generated",
    "machine-generated",
    "found",
    "no-annotation",
    "other",
];

/// Whether an `annotator` value names an agent that can be attributed to, rather than a category of
/// how the annotations were made.
///
/// A value is a category only if *every* part of it is one, so a card that lists
/// `crowdsourced, Acme Labs` still attributes to the named organization.
fn annotator_names_an_agent(value: &str) -> bool {
    value.split(',').map(str::trim).any(|part| {
        !part.is_empty()
            && !ANNOTATION_CREATOR_CATEGORIES
                .iter()
                .any(|c| c.eq_ignore_ascii_case(part))
    })
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
    // Set when the `annotator` element holds a category rather than an agent, so the entity can
    // carry it as a description instead of the graph gaining a person nobody named.
    let mut annotation_categories: Option<String> = None;
    // Every value, not the first one — the same reasoning `prov:wasDerivedFrom` already applies to
    // upstreams. A rig acquired from three devices records three `sensor` elements, and a bus log
    // one per transmitting ECU; naming one of three is worse than naming none, because it looks
    // complete.
    let mut categories: Vec<&str> = Vec::new();
    for (key, prov_type) in PROV_AGENTS {
        for value in known_values(dataset, key) {
            if *key == "annotator" && !annotator_names_an_agent(value) {
                categories.push(value);
                continue;
            }
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
    if !categories.is_empty() {
        annotation_categories = Some(categories.join(", "));
    }

    let mut entity = Map::new();
    entity.insert("@id".into(), json!(entity_id));
    entity.insert("@type".into(), json!("prov:Entity"));
    if !attributed.is_empty() {
        entity.insert("prov:wasAttributedTo".into(), json!(attributed));
    }
    if let Some(categories) = annotation_categories {
        entity.insert("veridex:annotationCreators".into(), json!(categories));
    }
    // Autonomy rig lineage as descriptive properties on the entity (known values only).
    for key in PROV_ENTITY_PROPERTIES {
        if let Some(value) = known_value(dataset, key) {
            entity.insert(format!("veridex:{key}"), json!(value));
        }
    }
    // Every upstream, not the first one. A dataset merged from several parents records several
    // `upstream` elements — the CDM has always been able to hold them, since provenance is a list —
    // and a lineage document naming one of three is worse than one naming none, because it looks
    // complete. Deduplicated and sorted so the graph is deterministic; the singular form is kept for
    // a single upstream, so every existing consumer reads what it always read.
    let upstreams = known_values(dataset, "upstream");
    if !upstreams.is_empty() {
        let ids: Vec<String> = upstreams
            .iter()
            .map(|u| format!("veridex:dataset/{}", iri_segment(u)))
            .collect();
        let reference = match ids.as_slice() {
            [single] => json!({ "@id": single }),
            many => json!(many
                .iter()
                .map(|id| json!({ "@id": id }))
                .collect::<Vec<_>>()),
        };
        entity.insert("prov:wasDerivedFrom".into(), reference);
        for id in ids {
            agent_nodes.push(json!({ "@id": id, "@type": "prov:Entity" }));
        }
    }

    // Entity first, then the agents/upstream it references.
    let mut graph = vec![Value::Object(entity)];
    graph.extend(agent_nodes);

    json!({
        "@context": { "prov": "http://www.w3.org/ns/prov#", "veridex": "https://veridex.dev/ns#" },
        "@graph": graph
    })
}

/// A copy of `dataset` whose provenance also carries the attested elements, for rendering only.
///
/// Attested provenance is a run input, not part of the CDM — the content hash describes the data,
/// and a claim about the data must not change it. An *emit*, though, is exactly where a producer
/// wants their attested facts to appear: a Croissant document that omitted them would describe less
/// than the run did. So the merge happens here, on a local copy that never leaves this module, and
/// the caller still passes the dataset's own content hash.
///
/// Attested elements enter as [`ProvenanceClass::Asserted`] — the class that means "someone says
/// so" — and never overwrite an element the dataset records: a conflict is reported by the check
/// catalog, not silently resolved here.
fn with_attested(dataset: &Dataset, attested: &[crate::certificate::AttestedElement]) -> Dataset {
    let mut copy = dataset.clone();
    if attested.is_empty() {
        return copy;
    }
    copy.provenance.push(crate::cdm::Provenance {
        scope: crate::cdm::ProvenanceScope::Dataset,
        elements: attested
            .iter()
            .map(|e| ProvenanceElement {
                key: e.key.clone(),
                value: Some(e.value.clone()),
                class: ProvenanceClass::Asserted,
            })
            .collect(),
    });
    copy
}

/// [`to_croissant`], including provenance a producer attested and naming the key that signed it.
pub fn to_croissant_attested(
    dataset: &Dataset,
    cdm_content_hash: &str,
    attested: &[crate::certificate::AttestedElement],
    producer_key: &str,
) -> Value {
    let merged = with_attested(dataset, attested);
    let mut doc = to_croissant(&merged, cdm_content_hash);
    if !attested.is_empty() {
        if let Some(object) = doc.as_object_mut() {
            // Who asserted the asserted elements. A consumer that trusts only its own producers can
            // subtract exactly these; one that cannot see the key cannot.
            object.insert(
                "veridex:attestedBy".into(),
                json!({
                    "producer_key": producer_key,
                    "keys": attested.iter().map(|e| e.key.clone()).collect::<Vec<_>>(),
                }),
            );
        }
    }
    doc
}

/// [`to_prov`], including provenance a producer attested and the agent that signed for it.
pub fn to_prov_attested(
    dataset: &Dataset,
    attested: &[crate::certificate::AttestedElement],
    producer_key: &str,
) -> Value {
    let merged = with_attested(dataset, attested);
    let mut doc = to_prov(&merged);
    if attested.is_empty() {
        return doc;
    }
    // PROV has a word for this: the attested facts were attributed to the producer, who is an agent
    // identified by their signing key.
    let agent_id = format!("veridex:producer/{}", iri_segment(producer_key));
    if let Some(graph) = doc.get_mut("@graph").and_then(Value::as_array_mut) {
        if let Some(entity) = graph.first_mut().and_then(Value::as_object_mut) {
            let mut attributions = match entity.remove("prov:wasAttributedTo") {
                Some(Value::Array(existing)) => existing,
                Some(other) => vec![other],
                None => Vec::new(),
            };
            attributions.push(json!({ "@id": agent_id }));
            entity.insert("prov:wasAttributedTo".into(), Value::Array(attributions));
        }
        graph.push(json!({
            "@id": agent_id,
            "@type": "prov:Agent",
            "veridex:role": "producer-attestation",
            "veridex:label": producer_key,
            "veridex:attests": attested.iter().map(|e| e.key.clone()).collect::<Vec<_>>(),
        }));
    }
    doc
}

/// Render provenance as a pretty JSON string in the requested format — `croissant` (default) or
/// `prov`. Shared by the CLI's `veridex provenance` and the Python `veridex.provenance` binding, so
/// both emit byte-identical documents. Returns `Err` with a message for an unknown format.
pub fn render_provenance(dataset: &Dataset, emit: &str) -> Result<String, String> {
    render_provenance_attested(dataset, emit, &[], "")
}

/// [`render_provenance`], including a verified producer attestation's elements.
pub fn render_provenance_attested(
    dataset: &Dataset,
    emit: &str,
    attested: &[crate::certificate::AttestedElement],
    producer_key: &str,
) -> Result<String, String> {
    let doc = match emit {
        // The hash is the *dataset's*, computed before the merge: an attestation adds to what the
        // document says, never to what the data is.
        "croissant" => to_croissant_attested(
            dataset,
            &crate::content_hash(dataset).to_hex(),
            attested,
            producer_key,
        ),
        "prov" => to_prov_attested(dataset, attested, producer_key),
        other => {
            return Err(format!(
                "unknown emit `{other}` (expected `croissant` or `prov`)"
            ))
        }
    };
    Ok(serde_json::to_string_pretty(&doc).expect("provenance serializes"))
}
