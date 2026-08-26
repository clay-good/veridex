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

    assert_eq!(doc["@type"], "sc:Dataset");
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

#[test]
fn autonomy_lineage_appears_in_both_emits() {
    let d = dataset_with(vec![
        el("platform", Some("av-07"), ProvenanceClass::Known),
        el("region", Some("us-ca-sf"), ProvenanceClass::Known),
        el("consent", Some("obtained"), ProvenanceClass::Known),
    ]);
    // PROV: descriptive veridex: properties on the entity.
    let prov = to_prov(&d);
    let entity = &prov["@graph"][0];
    assert_eq!(entity["veridex:platform"], "av-07");
    assert_eq!(entity["veridex:region"], "us-ca-sf");
    assert_eq!(entity["veridex:consent"], "obtained");
    // Croissant: every element in the classified veridex:provenance list.
    let cr = to_croissant(&d, &content_hash(&d).to_hex());
    let keys: Vec<&str> = cr["veridex:provenance"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"platform") && keys.contains(&"region") && keys.contains(&"consent"));
}

/// Every `@id` here is built by interpolating free text — a dataset id derived from a directory
/// name, an annotator lifted from source metadata. A space is enough to make the IRI ill-formed, and
/// a JSON-LD processor drops an ill-formed node together with every triple about it. The document
/// still parses as JSON and the CLI still reports success, so the failure is silent: an
/// unattributed, or entirely empty, provenance graph for a folder named `my robot data`.
#[test]
fn a_space_in_a_name_cannot_dissolve_the_prov_graph() {
    let mut d = dataset_with(vec![
        el("annotator", Some("Jane Doe & Co"), ProvenanceClass::Known),
        el("upstream", Some("open x/v2 (2026)"), ProvenanceClass::Known),
    ]);
    d.id = "my robot data <2026>".into();

    let doc = to_prov(&d);
    let graph = doc["@graph"].as_array().unwrap();

    // No @id anywhere may contain a character that is not legal in an IRI.
    for node in graph {
        let id = node["@id"].as_str().expect("every node is identified");
        assert!(
            !id.contains(|c: char| c.is_whitespace()),
            "ill-formed IRI, node will be dropped by a JSON-LD processor: {id:?}"
        );
        for bad in ['<', '>', '"', '{', '}', '|', '\\', '^', '`'] {
            assert!(!id.contains(bad), "illegal IRI character {bad:?} in {id:?}");
        }
    }

    // The attribution and derivation edges must still resolve to nodes that are present, which is
    // the property that was actually being lost.
    let entity = &graph[0];
    let attributed = entity["prov:wasAttributedTo"].as_array().unwrap();
    let agent_id = attributed[0]["@id"].as_str().unwrap();
    assert!(
        graph.iter().any(|n| n["@id"] == *agent_id),
        "attribution points at a node that is not in the graph"
    );
    let upstream_id = entity["prov:wasDerivedFrom"]["@id"].as_str().unwrap();
    assert!(
        graph.iter().any(|n| n["@id"] == *upstream_id),
        "derivation points at a node that is not in the graph"
    );

    // Encoding the identifier must not cost the reader the human-readable name.
    let agent = graph.iter().find(|n| n["@id"] == *agent_id).unwrap();
    assert_eq!(agent["veridex:label"], "Jane Doe & Co");
}

/// A JSON-LD document means whatever its `@context` says it means, and the previous version of this
/// file asserted the *spelling* of every term while asserting nothing about what those terms
/// expanded to. Two of them expanded to the wrong IRI, so a Croissant reader saw neither the
/// conformance declaration nor the hash that pins which data the document describes — while every
/// string-equality assertion here passed.
#[test]
fn croissant_terms_expand_to_the_iris_croissant_readers_look_for() {
    let d = dataset_with(vec![el(
        "license",
        Some("apache-2.0"),
        ProvenanceClass::Known,
    )]);
    let doc = to_croissant(&d, "deadbeef");
    let context = &doc["@context"];

    /// Expand a term through a JSON-LD context: an explicit mapping (resolving a `prefix:suffix`
    /// value through the context's own prefixes), else `@vocab` + the term.
    fn expand(context: &serde_json::Value, term: &str) -> String {
        let mapped = context
            .get(term)
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let value = match mapped {
            Some(v) => v,
            None => {
                return format!(
                    "{}{term}",
                    context["@vocab"].as_str().expect("a context has a @vocab")
                )
            }
        };
        match value.split_once(':') {
            Some((prefix, suffix)) if context.get(prefix).is_some() => format!(
                "{}{suffix}",
                context[prefix].as_str().expect("prefix maps to an IRI")
            ),
            _ => value,
        }
    }

    // What Croissant's reference implementation reads: DCTERMS for conformsTo, schema.org for the
    // file hash. Getting either wrong is silent — the document parses and says nothing.
    assert_eq!(
        expand(context, "conformsTo"),
        "http://purl.org/dc/terms/conformsTo"
    );
    assert_eq!(expand(context, "sha256"), "https://schema.org/sha256");
    assert_eq!(
        expand(context, "encodingFormat"),
        "https://schema.org/encodingFormat"
    );
    // And the type resolves to schema.org's Dataset through the `sc` prefix the canonical context
    // defines.
    assert_eq!(context["sc"], "https://schema.org/");
    assert_eq!(doc["@type"], "sc:Dataset");
    assert_eq!(doc["conformsTo"], "http://mlcommons.org/croissant/1.0");
}
