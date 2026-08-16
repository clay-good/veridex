//! Scenario, map, and simulation **references** — which OpenSCENARIO scenario, which OpenDRIVE road
//! network, which OSI / simulator version a log was recorded or replayed against (design A3, format
//! priority 4).
//!
//! Veridex reads the *reference and its version*, never the scenario semantics: it does not
//! interpret manoeuvres, road geometry, or ground truth. A reference names a sidecar file
//! (`scenarios/cut_in.xosc`) or a version string (`carla 0.9.15`); when the sidecar actually exists
//! next to the dataset, the ASAM revision declared in its header is read from the file's own bytes so
//! the recorded version is extracted, not inferred.
//!
//! Everything here lands in provenance as ordinary elements. It never changes the coverage
//! denominator and produces no findings — a dataset without simulation references is not worse, just
//! not simulated.

use std::path::Path;

use crate::cdm::{Dataset, ProvenanceClass};

/// The kind of simulation reference a source key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SimRefKind {
    /// An ASAM OpenSCENARIO scenario definition (`.xosc`).
    Scenario,
    /// An ASAM OpenDRIVE road network / HD map (`.xodr`).
    Map,
    /// An ASAM OSI (Open Simulation Interface) version.
    Osi,
    /// The simulator or replay tool the data came from.
    Simulator,
}

/// The kinds in a stable display order.
pub const SIM_REF_KINDS: &[SimRefKind] = &[
    SimRefKind::Scenario,
    SimRefKind::Map,
    SimRefKind::Osi,
    SimRefKind::Simulator,
];

impl SimRefKind {
    /// The provenance key this kind's reference is recorded under.
    pub fn provenance_key(self) -> &'static str {
        match self {
            SimRefKind::Scenario => "scenario_ref",
            SimRefKind::Map => "map_ref",
            SimRefKind::Osi => "osi_version",
            SimRefKind::Simulator => "simulator",
        }
    }

    /// The provenance key the reference's *version* is recorded under, for the kinds whose reference
    /// and version are separate facts. `Osi`/`Simulator` references are themselves versions.
    ///
    /// Note `Map` shares the existing `map_version` rig-lineage element (design A3): an explicitly
    /// recorded map version always wins, and an OpenDRIVE header revision only fills it when absent.
    pub fn version_key(self) -> Option<&'static str> {
        match self {
            SimRefKind::Scenario => Some("scenario_version"),
            SimRefKind::Map => Some("map_version"),
            SimRefKind::Osi | SimRefKind::Simulator => None,
        }
    }

    /// Short human label for reports.
    pub fn label(self) -> &'static str {
        match self {
            SimRefKind::Scenario => "scenario",
            SimRefKind::Map => "map",
            SimRefKind::Osi => "osi",
            SimRefKind::Simulator => "simulator",
        }
    }

    /// The sidecar file extension whose header declares this kind's version, if any.
    fn sidecar_ext(self) -> Option<&'static str> {
        match self {
            SimRefKind::Scenario => Some("xosc"),
            SimRefKind::Map => Some("xodr"),
            SimRefKind::Osi | SimRefKind::Simulator => None,
        }
    }
}

/// Map a source metadata key (case-insensitive) to the simulation reference it names, or `None`.
/// Conservative: only well-known spellings map, and none of them collide with the rig-lineage keys
/// the adapters already recognize.
pub fn simref_key_for(key: &str) -> Option<SimRefKind> {
    match key.trim().to_ascii_lowercase().as_str() {
        "scenario" | "scenario_file" | "scenario_ref" | "openscenario" | "openscenario_file"
        | "xosc" | "xosc_file" => Some(SimRefKind::Scenario),
        "map_file" | "map_ref" | "opendrive" | "opendrive_file" | "xodr" | "xodr_file"
        | "road_network" | "hd_map" => Some(SimRefKind::Map),
        "osi" | "osi_version" | "open_simulation_interface" => Some(SimRefKind::Osi),
        "simulator" | "sim" | "sim_version" | "simulation_tool" | "sim_tool" => {
            Some(SimRefKind::Simulator)
        }
        _ => None,
    }
}

/// Pull a dotted version number (`1.2`, `0.9.15`) out of a reference value such as
/// `OpenSCENARIO 1.2` or `carla-0.9.15`. Requires at least one dot between digit runs, so a file name
/// like `town10.xodr` or `cut_in.xosc` yields nothing rather than a bogus version.
pub fn version_from_value(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Walk a `digits ( '.' digits )+` run.
        let start = i;
        let mut end = i;
        let mut dots = 0usize;
        while end < bytes.len() {
            if bytes[end].is_ascii_digit() {
                end += 1;
            } else if bytes[end] == b'.' && end + 1 < bytes.len() && bytes[end + 1].is_ascii_digit()
            {
                dots += 1;
                end += 1;
            } else {
                break;
            }
        }
        if dots > 0 {
            return Some(value[start..end].to_string());
        }
        i = end.max(start + 1);
    }
    None
}

/// The largest sidecar prefix Veridex reads to find an ASAM header. Headers are the first element of
/// the document; this bounds the read so a multi-gigabyte road network costs nothing.
const SIDECAR_HEADER_BYTES: usize = 64 * 1024;

/// Read the ASAM revision declared in an XML header — the `revMajor`/`revMinor` attributes that both
/// OpenSCENARIO's `<FileHeader>` and OpenDRIVE's `<header>` carry — as `"major.minor"`.
///
/// Deliberately a targeted attribute scan, not an XML parse: Veridex reads the declared version and
/// nothing else, and must survive a malformed or truncated document without erroring.
pub fn asam_header_version(text: &str) -> Option<String> {
    let major = xml_attr(text, "revMajor")?;
    let minor = xml_attr(text, "revMinor")?;
    if !major.chars().all(|c| c.is_ascii_digit()) || !minor.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{major}.{minor}"))
}

/// The first quoted value of `name="..."` (or `name='...'`) in `text`.
fn xml_attr<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = text;
    while let Some(at) = rest.find(name) {
        let after = rest[at + name.len()..].trim_start();
        if let Some(after) = after.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = &after[1..];
                if let Some(close) = body.find(quote) {
                    return Some(&body[..close]);
                }
            }
        }
        rest = &rest[at + name.len()..];
    }
    None
}

/// Read the ASAM version a referenced sidecar declares in its own header, when the reference names a
/// file of the expected kind that really exists under `root`.
///
/// Refuses anything that isn't a plain relative path inside the dataset: an absolute path, a Windows
/// prefix, or any `..` component is ignored rather than followed, so a hostile metadata value can
/// never make Veridex read outside the dataset directory.
pub fn sidecar_version(root: &Path, kind: SimRefKind, reference: &str) -> Option<String> {
    let ext = kind.sidecar_ext()?;
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let rel = Path::new(reference);
    if rel.is_absolute() {
        return None;
    }
    if !rel
        .extension()
        .is_some_and(|e| e.to_str().is_some_and(|e| e.eq_ignore_ascii_case(ext)))
    {
        return None;
    }
    use std::path::Component;
    if !rel
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let path = root.join(rel);
    if !path.is_file() {
        return None;
    }
    let head = read_prefix(&path, SIDECAR_HEADER_BYTES)?;
    asam_header_version(&String::from_utf8_lossy(&head))
}

/// Read at most `limit` bytes from the start of a file, or `None` if it can't be read.
fn read_prefix(path: &Path, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(limit as u64).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// One resolved simulation reference as it will be reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimReference {
    /// What kind of reference this is.
    pub kind: SimRefKind,
    /// The reference itself (a sidecar path, a tool name, or a bare version).
    pub value: String,
    /// The version that goes with it, when it is a separate recorded fact.
    pub version: Option<String>,
}

/// Collect the simulation references a dataset's provenance carries, in [`SIM_REF_KINDS`] order.
/// Only real (non-placeholder), known-or-asserted elements count, matching how provenance is scored
/// and emitted everywhere else.
pub fn references(dataset: &Dataset) -> Vec<SimReference> {
    let value_of = |key: &str| -> Option<String> {
        dataset
            .provenance
            .iter()
            .flat_map(|r| &r.elements)
            .find(|e| e.key == key && e.class != ProvenanceClass::Unknown && e.has_real_value())
            .and_then(|e| e.value.clone())
    };
    SIM_REF_KINDS
        .iter()
        .filter_map(|&kind| {
            let value = value_of(kind.provenance_key())?;
            let version = kind.version_key().and_then(value_of);
            Some(SimReference {
                kind,
                value,
                version,
            })
        })
        .collect()
}

/// Render the simulation references as a human-readable block, empty when the dataset carries none.
/// Descriptive only — this is what the data says it was recorded against, never a pass or fail.
pub fn render_references(dataset: &Dataset) -> String {
    use std::fmt::Write;
    let refs = references(dataset);
    if refs.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let _ = writeln!(out, "  scenario & map references:");
    for r in &refs {
        match &r.version {
            Some(v) => {
                let _ = writeln!(out, "      {}: {} (version {v})", r.kind.label(), r.value);
            }
            None => {
                let _ = writeln!(out, "      {}: {}", r.kind.label(), r.value);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_keys_map_to_their_kind_and_unknown_keys_do_not() {
        assert_eq!(simref_key_for("OpenSCENARIO"), Some(SimRefKind::Scenario));
        assert_eq!(simref_key_for(" xosc_file "), Some(SimRefKind::Scenario));
        assert_eq!(simref_key_for("opendrive"), Some(SimRefKind::Map));
        assert_eq!(simref_key_for("osi_version"), Some(SimRefKind::Osi));
        assert_eq!(simref_key_for("sim_version"), Some(SimRefKind::Simulator));
        assert_eq!(simref_key_for("weather"), None);
        // Rig-lineage keys stay with rig lineage — no key means two different things.
        assert_eq!(simref_key_for("map_version"), None);
        assert_eq!(simref_key_for("map"), None);
    }

    #[test]
    fn a_dotted_version_is_pulled_from_a_value_but_a_file_name_is_not() {
        assert_eq!(
            version_from_value("OpenSCENARIO 1.2").as_deref(),
            Some("1.2")
        );
        assert_eq!(
            version_from_value("carla-0.9.15").as_deref(),
            Some("0.9.15")
        );
        assert_eq!(version_from_value("3.5.0").as_deref(), Some("3.5.0"));
        // A bare file name has digits but no dotted version: better nothing than a wrong version.
        assert_eq!(version_from_value("town10.xodr"), None);
        assert_eq!(version_from_value("cut_in.xosc"), None);
        assert_eq!(version_from_value("no digits here"), None);
    }

    #[test]
    fn an_asam_header_revision_is_read_from_either_standards_header() {
        let xosc = r#"<?xml version="1.0"?><OpenSCENARIO><FileHeader revMajor="1" revMinor="2" date="2026-01-01"/></OpenSCENARIO>"#;
        assert_eq!(asam_header_version(xosc).as_deref(), Some("1.2"));
        let xodr = "<OpenDRIVE>\n  <header revMajor='1' revMinor='7' name='town'>\n";
        assert_eq!(asam_header_version(xodr).as_deref(), Some("1.7"));
    }

    #[test]
    fn a_malformed_or_partial_header_yields_no_version_rather_than_a_guess() {
        assert_eq!(asam_header_version("<FileHeader revMajor=\"1\"/>"), None);
        assert_eq!(
            asam_header_version("<FileHeader revMajor= revMinor=/>"),
            None
        );
        assert_eq!(
            asam_header_version("<FileHeader revMajor=\"x\" revMinor=\"y\"/>"),
            None
        );
        assert_eq!(asam_header_version(""), None);
        // Truncated mid-attribute: no closing quote, so no value.
        assert_eq!(
            asam_header_version("<header revMajor=\"1\" revMinor=\"7"),
            None
        );
    }

    #[test]
    fn a_sidecar_version_is_read_only_for_a_real_relative_file_of_the_right_kind() {
        let dir = std::env::temp_dir().join(format!(
            "veridex-simref-{}-{}",
            std::process::id(),
            "sidecar"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scenarios")).expect("mkdir");
        std::fs::write(
            dir.join("scenarios/cut_in.xosc"),
            r#"<OpenSCENARIO><FileHeader revMajor="1" revMinor="2"/></OpenSCENARIO>"#,
        )
        .expect("write");

        assert_eq!(
            sidecar_version(&dir, SimRefKind::Scenario, "scenarios/cut_in.xosc").as_deref(),
            Some("1.2")
        );
        // Wrong kind for the extension, a missing file, and an escaping or absolute path all decline.
        assert_eq!(
            sidecar_version(&dir, SimRefKind::Map, "scenarios/cut_in.xosc"),
            None
        );
        assert_eq!(
            sidecar_version(&dir, SimRefKind::Scenario, "scenarios/absent.xosc"),
            None
        );
        assert_eq!(
            sidecar_version(&dir, SimRefKind::Scenario, "../escape.xosc"),
            None
        );
        assert_eq!(
            sidecar_version(&dir, SimRefKind::Scenario, "/etc/passwd.xosc"),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
