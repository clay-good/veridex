//! HDF5 adapter: the format `robomimic`, MimicGen, RoboTurk, and most hand-rolled lab collectors
//! ship in, read into the CDM.
//!
//! An HDF5 robot dataset is a tree of groups and datasets. The layout these collectors share is:
//!
//! ```text
//! /data                      (attrs: total, env_args, …)
//!   /demo_0                  (attrs: num_samples, …)  -> one episode
//!     actions                [T, 7]  float32          -> one stream, T frames
//!     rewards                [T]     float64          -> one stream
//!     /obs
//!       robot0_eef_pos       [T, 3]  float32          -> one stream
//!       agentview_image      [T, H, W, 3] uint8       -> one stream
//!   /demo_1
//!     …
//! ```
//!
//! So a **group of arrays becomes an episode**, every array under it becomes a **stream**, and an
//! array's first dimension is that stream's frame count. Nothing else in the tree is invented: a
//! group that holds no arrays is not an episode, and an array whose first dimension is zero is a
//! stream with no frames rather than a stream Veridex made up.
//!
//! **The timeline is the step index, unless the file says otherwise.** HDF5 has no notion of time,
//! and the collectors above record none: there is no per-step timestamp in a `robomimic` file. So
//! frames are stamped with their step index on a clock named `hdf5-step-index`, and the ingest
//! report states the omission — exactly as the RLDS adapter does, and for the same reason: an index
//! passes every temporal check trivially, and that pass would read as "these sensors are
//! synchronized" in a signed certificate. A file that *does* record time gets measured time, but
//! only when it also declares its units in a `units` attribute. Veridex will not guess whether a
//! `time` column is seconds or nanoseconds; guessing wrong is a fabricated rate, a fabricated
//! duration, and a clock-skew verdict about a clock nobody recorded.
//!
//! Row bytes are fingerprinted into `frame.value_ref.content_hash` (never stored), so the CDM
//! content hash is sensitive to actual recorded content, exactly as in the other adapters.

mod format;

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, Episode, Frame, Label, Modality, Provenance, ProvenanceClass,
    ProvenanceElement, ProvenanceScope, Stream, ValueRef, META_DECLARED_FRAMES,
};

use self::format::{
    AttrValue, DatasetInfo, Datatype, DatatypeClass, H5File, ObjectHeader, SkippedLink,
};
use super::stats::FeatureAccum;

use super::{
    Adapter, Coverage, DecompressionBudget, Detection, FrameBudget, IngestError, IngestOptions,
    IngestReport, Ingested, Source, UnmappedField,
};

pub(crate) const FORMAT_ID: &str = "hdf5";

/// The clock HDF5 frames are stamped on when the file records no time of its own. Named for what it
/// is — a step index, not a wall clock — so no reader of the CDM, a report, or a certificate can
/// mistake it for measured time.
const CLOCK_STEP_INDEX: &str = "hdf5-step-index";

/// The clock frames are stamped on when the file records timestamps *and* declares their units.
const CLOCK_TIME: &str = "hdf5-time";

/// The group robot collectors put their episodes under.
const EPISODE_ROOT: &str = "data";

/// Leaf names that hold an episode's timeline, when one is recorded.
const TIME_LEAVES: &[&str] = &["timestamp", "timestamps", "time", "times"];

/// The attribute a timestamp dataset must carry for its numbers to be read as time.
const UNITS_ATTR: &str = "units";

/// The attribute a collector records an episode's own step count in (`robomimic`'s).
const DECLARED_LENGTH_ATTR: &str = "num_samples";

/// The attribute holding a dataset-wide frame total (`robomimic` puts it on `/data`).
const DECLARED_TOTAL_ATTR: &str = "total";

/// Attributes carrying an episode's natural-language task.
const TASK_ATTRS: &[&str] = &["language_instruction", "task"];

/// Root attributes that are lineage facts rather than free-form metadata.
const PROVENANCE_ATTRS: &[(&str, &str)] = &[
    ("license", "license"),
    ("author", "author"),
    ("date", "created"),
];

/// How deep the object tree is walked below one episode group.
///
/// Real files nest one or two levels (`demo_0/obs/…`). The tree is an address graph that can be made
/// cyclic, and visited-address tracking already breaks an exact cycle; this bounds a merely absurd
/// one.
const MAX_GROUP_DEPTH: usize = 16;

/// The ceiling on how much of one attribute's text is carried into metadata.
///
/// An attribute value is arbitrary attacker-controlled bytes, and `env_args`-style JSON blobs are
/// genuinely a few kilobytes; the whole string would otherwise land in the CDM, the content hash,
/// and every report.
const MAX_ATTR_TEXT: usize = 4096;

/// The ceiling on values per frame for which per-dimension statistics are recomputed.
///
/// Per-dimension statistics exist so a saturating gripper or a NaN in joint 6 is caught rather than
/// hidden behind element 0. That reasoning applies to a robot signal, not to pixels: a 480x640x3
/// camera frame has 921,600 positions, each of which would get its own accumulator, and "the
/// statistics of pixel (211, 47, 2)" answers nothing anyone asks. Wide arrays are still scanned for
/// non-finite values, and the ingest report names them.
const MAX_STATS_DIMS: u64 = 256;

/// Adapter for an HDF5 robot dataset.
pub struct Hdf5Adapter;

fn parse_error(message: impl Into<String>) -> IngestError {
    IngestError::Parse {
        format_id: FORMAT_ID,
        message: message.into(),
    }
}

/// What an ingest could not map, gathered as it goes so the report can disclose all of it.
#[derive(Default)]
struct Notes {
    /// Source objects that exist but the CDM cannot represent.
    unmapped: Vec<UnmappedField>,
    /// Links this reader did not follow.
    skipped_links: Vec<SkippedLink>,
    /// Attributes that could not be parsed, and why.
    skipped_attrs: Vec<String>,
    /// Numeric arrays too wide for per-dimension statistics (an image, not a robot signal).
    wide_arrays: BTreeSet<String>,
}

/// One array found under an episode group.
struct ArrayEntry {
    /// The path below the episode group, `/`-joined (`obs/robot0_eef_pos`).
    path: String,
    header: ObjectHeader,
    info: DatasetInfo,
}

/// One episode's group, before anything under it is read.
struct EpisodeGroup {
    /// The link name in the file (`demo_0`).
    name: String,
    /// The full path from the root (`/data/demo_0`), for provenance and value references.
    path: String,
    addr: u64,
}

impl Adapter for Hdf5Adapter {
    fn format_id(&self) -> &'static str {
        FORMAT_ID
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        // Superblock versions, which is the only version number an HDF5 file states.
        &["0", "1", "2", "3"]
    }

    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(path) if path.is_file() && H5File::is_hdf5(path) => {
                Detection::Yes { version: None }
            }
            _ => Detection::No,
        }
    }

    /// An HDF5 file keeps its structure in the container, but not in its *data*: the group tree,
    /// each array's datatype and shape, and every object attribute are headers, and reading them
    /// touches no chunk. On a robomimic-shaped file of hundreds of gigabytes, that is the whole
    /// difference between a manifest check and a full read.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        let Source::Local(path) = source else {
            return Err(parse_error("remote HDF5 ingestion is not supported"));
        };
        let file_bytes = std::fs::metadata(path)
            .map_err(|e| IngestError::Io(e.to_string()))?
            .len();
        let mut file = H5File::open(path, DecompressionBudget::new(options, file_bytes))?;

        let mut notes = Notes::default();

        let root_addr = file.root_addr();
        let root = file.object_header(root_addr)?;
        let root_links = file.group_links(&root, &mut notes.skipped_links)?;
        let root_attrs = read_attributes(&mut file, &root, &mut notes.skipped_attrs);

        // Where the episodes live: under `/data` when the file has one (what the robot collectors
        // write), otherwise directly under the root.
        let (episode_parent_path, episode_parent_addr, parent_attrs) =
            match named_link(&root_links, EPISODE_ROOT) {
                Some(addr) => {
                    let header = file.object_header(addr)?;
                    if !header.is_group() {
                        return Err(parse_error(format!(
                            "`/{EPISODE_ROOT}` is a dataset, not a group, so the file's episodes \
                             cannot be located"
                        )));
                    }
                    let attrs = read_attributes(&mut file, &header, &mut notes.skipped_attrs);
                    (format!("/{EPISODE_ROOT}"), addr, attrs)
                }
                None => (String::from(""), root_addr, Vec::new()),
            };
        let parent_header = file.object_header(episode_parent_addr)?;
        let parent_links = file.group_links(&parent_header, &mut notes.skipped_links)?;

        // What the root holds beside the episodes. Only when the episodes came from `/data`: with a
        // flat file the root *is* the episode parent, and `episode_groups` already names everything
        // sitting beside the episode groups.
        let mut unread_sources = if episode_parent_path.is_empty() {
            Vec::new()
        } else {
            root_siblings(&mut file, &root_links, &mut notes)?
        };

        let mut groups = episode_groups(
            &mut file,
            &episode_parent_path,
            &parent_links,
            &mut notes,
            &mut unread_sources,
        )?;
        // A file whose arrays sit directly under the parent — what a simple collector writes when it
        // records one trajectory per file — is one episode, not zero. The parent group *is* the
        // episode; nothing is fabricated, because the arrays are still the only source of frames.
        let flat = groups.is_empty();
        if flat {
            if !parent_links.is_empty() {
                groups.push(EpisodeGroup {
                    name: EPISODE_ROOT.into(),
                    path: if episode_parent_path.is_empty() {
                        "/".into()
                    } else {
                        episode_parent_path.clone()
                    },
                    addr: episode_parent_addr,
                });
            } else {
                return Err(parse_error(format!(
                    "nothing under `{}`: the file holds no groups and no arrays, so there is no \
                     timeline to read",
                    if episode_parent_path.is_empty() {
                        "/"
                    } else {
                        &episode_parent_path
                    }
                )));
            }
        }

        // Episode indices come from the trailing number in each group's name (`demo_12` -> 12) when
        // every name has one and they are all distinct; otherwise from the group's position in name
        // order. Either way the group's own name is preserved in its provenance, so an index this
        // adapter derived is never mistaken for one the file stated.
        let indices = episode_indices(&groups);
        let available: BTreeSet<u64> = indices.iter().copied().collect();
        let selected = if options.sample.is_partial() {
            options.sample.select(&available)
        } else {
            available.clone()
        };

        let mut budget = FrameBudget::new(options);
        let mut episodes: Vec<Episode> = Vec::new();
        let mut provenance: Vec<Provenance> = Vec::new();
        let file_uri = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(FORMAT_ID)
            .to_string();

        for (group, index) in groups.iter().zip(indices.iter().copied()) {
            if !selected.contains(&index) {
                continue;
            }
            let episode = build_episode(
                &mut file,
                group,
                index,
                &file_uri,
                options.metadata_only,
                &mut budget,
                &mut notes,
            )?;
            provenance.push(Provenance {
                scope: ProvenanceScope::Episode(index),
                elements: vec![ProvenanceElement {
                    key: "upstream".into(),
                    value: Some(format!("{file_uri}:{}", group.path)),
                    class: ProvenanceClass::Known,
                }],
            });
            episodes.push(episode);
        }
        // Emitted in episode-index order — the order the canonical form and every report use — so a
        // derived index never leaves the CDM in the order the file's link index happened to use.
        episodes.sort_by_key(|e| e.index);
        // A tree of groups that holds no arrays at all is not a dataset with empty episodes; it is a
        // file this adapter did not understand. Saying so beats emitting a verdict over nothing,
        // which would score a clean pass on a file Veridex never read.
        if episodes.iter().all(|e| e.streams.is_empty()) {
            return Err(parse_error(
                "no arrays under any episode group, so there are no frames to check — this file's \
                 layout is not one this adapter maps",
            ));
        }

        // Dataset-level facts: only what the file's own attributes record.
        let mut metadata: Vec<(String, String)> = vec![
            ("source_format".into(), FORMAT_ID.into()),
            (
                "hdf5_superblock_version".into(),
                file.superblock_version().to_string(),
            ),
        ];
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some(FORMAT_ID.into()),
            class: ProvenanceClass::Known,
        }];
        for (name, value) in root_attrs.iter().chain(parent_attrs.iter()) {
            if let Some((_, key)) = PROVENANCE_ATTRS
                .iter()
                .find(|(attr, _)| attr.eq_ignore_ascii_case(name))
            {
                if let AttrValue::Text(text) = value {
                    elements.push(ProvenanceElement {
                        key: (*key).into(),
                        value: Some(text.clone()),
                        class: ProvenanceClass::Known,
                    });
                }
            }
            match value {
                AttrValue::Text(text) => metadata.push((format!("h5:{name}"), text.clone())),
                AttrValue::Number(n) => metadata.push((format!("h5:{name}"), format_number(*n))),
                AttrValue::Opaque(reason) => notes.unmapped.push(UnmappedField {
                    source_path: format!("attribute {name}"),
                    note: (*reason).into(),
                }),
            }
        }
        // A declared frame total is an assertion *about* the content, which a structural check
        // compares against the frames actually ingested — so it is only carried when the whole
        // dataset was ingested. Under a sample it would be compared against a fraction of itself,
        // and where an episode's arrays disagree on their row count there is no single frame count
        // for it to be compared against either (the check sums each episode's longest stream, which
        // over-counts a ragged episode and would fail a sound file).
        let declared_total = root_attrs
            .iter()
            .chain(parent_attrs.iter())
            .find(|(name, _)| name == DECLARED_TOTAL_ATTR)
            .map(|(_, v)| v.clone());
        let ragged_episodes: Vec<u64> = dataset_episodes_with_ragged_rows(&episodes);
        // Said only when there is actually a declared total to withhold; otherwise the note would
        // report the absence of something the file never claimed.
        if !ragged_episodes.is_empty() && declared_total.is_some() {
            notes.unmapped.push(UnmappedField {
                source_path: format!("@{DECLARED_TOTAL_ATTR}"),
                note: format!(
                    "episode(s) {} hold arrays with different numbers of rows, so the dataset has \
                     no single frame count to compare a declared total against; the declared total \
                     is not carried",
                    ragged_episodes
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        if !options.sample.is_partial() && ragged_episodes.is_empty() {
            if let Some(AttrValue::Number(total)) = declared_total.as_ref() {
                if *total >= 0.0 && total.fract() == 0.0 && *total <= u64::MAX as f64 {
                    metadata.push((
                        META_DECLARED_FRAMES.to_string(),
                        format!("{}", *total as u64),
                    ));
                }
            }
        }

        provenance.push(Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        });

        // Named in path order, so `inspect` prints the same list whatever order the file's link
        // index happened to enumerate its objects in.
        unread_sources.sort_by(|a, b| a.source_path.cmp(&b.source_path));

        let mut unmapped = std::mem::take(&mut notes.unmapped);
        for link in &notes.skipped_links {
            unmapped.push(UnmappedField {
                source_path: link.name.clone(),
                note: link.reason.into(),
            });
        }
        for reason in &notes.skipped_attrs {
            unmapped.push(UnmappedField {
                source_path: "attribute".into(),
                note: format!("an attribute could not be read: {reason}"),
            });
        }

        let episodes_ingested = episodes.len() as u64;
        let dataset = Dataset {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(FORMAT_ID)
                .to_string(),
            metadata,
            provenance,
            episodes,
            // HDF5 robot datasets record no sensor-rig calibration.
            calibration: None,
        };

        let measured = dataset
            .episodes
            .iter()
            .flat_map(|e| e.streams.iter())
            .any(|s| s.clock_kind == ClockKind::Measured);
        let mut mapped_fields = vec![
            "group of arrays -> episode".into(),
            "array -> stream (first dimension -> frames)".into(),
            "array datatype -> stream.dtype".into(),
            "array shape after the first dimension -> stream.shape".into(),
            "object attributes -> dataset metadata".into(),
        ];
        let mut omitted_fields = vec![
            "image/point payload decoding (frames are fingerprints, not pixels)".into(),
            "declared sample rates (HDF5 states none, so the rate and gap checks have no nominal \
             period to grade against)"
                .into(),
        ];
        if options.metadata_only {
            mapped_fields.push(
                "array header rows + group length attribute -> declared episode lengths".into(),
            );
            omitted_fields.push(
                "row values, frame timestamps, and content hashes (no chunk was read; only the \
                 group tree, the array headers and the object attributes were)"
                    .into(),
            );
            omitted_fields.push(
                "recomputed statistics, saturation and non-finite counts (every one is derived \
                 from values, and no value was read)"
                    .into(),
            );
        } else {
            mapped_fields.push("row bytes -> frame.value_ref.content_hash (SHA-256)".into());
            mapped_fields.push(
                "numeric array values -> recomputed per-dimension statistics, saturation, and \
                 non-finite counts"
                    .into(),
            );
        }
        if !notes.wide_arrays.is_empty() {
            let named: Vec<String> = notes.wide_arrays.iter().take(4).cloned().collect();
            let rest = notes.wide_arrays.len().saturating_sub(named.len());
            omitted_fields.push(format!(
                "per-dimension statistics for array(s) with more than {MAX_STATS_DIMS} values per \
                 frame ({}{}) — they are scanned for non-finite values, but per-position statistics \
                 of an image answer nothing the statistical checks ask",
                named.join(", "),
                if rest > 0 {
                    format!(" and {rest} more")
                } else {
                    String::new()
                }
            ));
        }
        if measured {
            mapped_fields.push(format!(
                "timestamp array + `{UNITS_ATTR}` attribute -> frame.ts (measured)"
            ));
        } else {
            omitted_fields.push(format!(
                "per-frame timestamps (this file records none with declared units; the timeline is \
                 the step index on clock `{CLOCK_STEP_INDEX}`, so the rate, gap, jitter and skew \
                 checks have no measured time to grade)"
            ));
        }

        let coverage = if options.metadata_only {
            Coverage::MetadataOnly {
                episodes_declared: episodes_ingested,
            }
        } else if options.sample.is_partial() {
            Coverage::Sample {
                sample: options.sample.clone(),
                episodes_ingested,
            }
        } else {
            Coverage::Full
        };

        Ok(Ingested {
            dataset,
            report: IngestReport {
                unread_sources,
                format_id: FORMAT_ID,
                source_version: Some(format!("superblock v{}", file.superblock_version())),
                coverage,
                mapped_fields,
                unmapped_fields: unmapped,
                omitted_fields,
            },
        })
    }
}

/// The indices of episodes whose arrays disagree on how many rows they hold.
fn dataset_episodes_with_ragged_rows(episodes: &[Episode]) -> Vec<u64> {
    episodes
        .iter()
        .filter(|e| {
            e.streams
                .iter()
                .map(|s| s.frames.len())
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        })
        .map(|e| e.index)
        .collect()
}

/// The address a link with this name points at.
fn named_link(links: &[(String, u64)], name: &str) -> Option<u64> {
    links
        .iter()
        .find(|(link, _)| link == name)
        .map(|(_, addr)| *addr)
}

/// The groups under `parent` that hold arrays — the file's episodes — in name order.
/// Objects at the root that sit beside `/data`, the group the episodes were read from.
///
/// A `robomimic` file is not only `/data`: it commonly carries a `/mask` group of filter keys (which
/// demonstrations are in the train split, which in the validation split), and hand-rolled collectors
/// park calibration tables, reward models and raw logs at the root the same way. None of it is under
/// an episode group, so none of it is read — and until this walked the root, none of it was said
/// either: the file's whole second half could be absent from the report while coverage read `Full`.
///
/// The split follows the line the rest of the adapter draws. An object that **holds array rows** is
/// data that is *there* and went unread, so it becomes an [`IngestReport::unread_sources`] entry and
/// travels into the verdict as `COVERAGE.SOURCE_UNREAD`. An object that holds none — an empty group,
/// a scalar array, a zero-row array, a committed datatype — is a note about shape, not a hole in
/// coverage, so it lands in `unmapped_fields`.
///
/// Headers only: no chunk is read to answer this, so a metadata-only run discloses exactly what a
/// full read discloses.
fn root_siblings(
    file: &mut H5File,
    root_links: &[(String, u64)],
    notes: &mut Notes,
) -> Result<Vec<UnmappedField>, IngestError> {
    let mut unread: Vec<UnmappedField> = Vec::new();
    for (name, addr) in root_links {
        if name == EPISODE_ROOT {
            continue;
        }
        let path = format!("/{name}");
        // A sibling whose header will not read is not a reason to fail a file whose episodes are
        // perfectly sound — the same judgement this adapter makes about an unreadable attribute.
        // It is still something the file holds and this run could not look at, so it is disclosed
        // as unread rather than passed over.
        let header = match file.object_header(*addr) {
            Ok(header) => header,
            Err(e) => {
                unread.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this object sits beside `/{EPISODE_ROOT}` and its header could not be \
                         read, so whatever it holds went unread: {e}"
                    ),
                });
                continue;
            }
        };
        if header.is_group() {
            let mut visited = BTreeSet::new();
            if holds_array_rows(file, *addr, 0, &mut visited, notes)? {
                unread.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this group sits beside `/{EPISODE_ROOT}`, where the episodes are, so no \
                         episode is built from it and the arrays under it were not read"
                    ),
                });
            } else {
                notes.unmapped.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this group sits beside `/{EPISODE_ROOT}` and holds no arrays, so there is \
                         nothing under it to read"
                    ),
                });
            }
            continue;
        }
        if !header.is_dataset() {
            notes.unmapped.push(UnmappedField {
                source_path: path,
                note: "this object is neither a group nor a dataset (a committed datatype, say); \
                       no stream is built from it"
                    .into(),
            });
            continue;
        }
        let info = match file.dataset_info(&header) {
            Ok(info) => info,
            Err(e) => {
                unread.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this array sits beside `/{EPISODE_ROOT}` and its header could not be \
                         read, so its rows went unread: {e}"
                    ),
                });
                continue;
            }
        };
        match info.dims.first() {
            Some(rows) if *rows > 0 => unread.push(UnmappedField {
                source_path: path,
                note: format!(
                    "this array sits beside `/{EPISODE_ROOT}`, where the episodes are, so it \
                     belongs to no episode and its {rows} row(s) were not read"
                ),
            }),
            Some(_) => notes.unmapped.push(UnmappedField {
                source_path: path,
                note: format!(
                    "this array sits beside `/{EPISODE_ROOT}` and has no rows, so there is nothing \
                     under it to read"
                ),
            }),
            None => notes.unmapped.push(UnmappedField {
                source_path: path,
                note: "a scalar array has no first dimension, so it carries no timeline; it is \
                       metadata, not a stream"
                    .into(),
            }),
        }
    }
    Ok(unread)
}

/// Whether anything under an object holds array rows, answered from headers alone.
///
/// Stops at the first array with rows: the question is whether there is unread data here, not how
/// much. A tree deeper than [`MAX_GROUP_DEPTH`], or one whose links this reader could not follow,
/// answers `true` — "I stopped walking" is not "there is nothing here", and the honest direction for
/// an unanswered question about coverage is to disclose.
fn holds_array_rows(
    file: &mut H5File,
    addr: u64,
    depth: usize,
    visited: &mut BTreeSet<u64>,
    notes: &mut Notes,
) -> Result<bool, IngestError> {
    if depth > MAX_GROUP_DEPTH {
        return Ok(true);
    }
    if !visited.insert(addr) {
        return Ok(false);
    }
    // Every failure below answers `true` for the same reason the depth bound does: "I could not
    // look" is not "there is nothing here", and a corrupt sibling must not fail a file whose
    // episodes are sound.
    let Ok(header) = file.object_header(addr) else {
        return Ok(true);
    };
    let before = notes.skipped_links.len();
    let Ok(links) = file.group_links(&header, &mut notes.skipped_links) else {
        return Ok(true);
    };
    if notes.skipped_links.len() != before {
        return Ok(true);
    }
    for (_, child_addr) in links {
        let Ok(child) = file.object_header(child_addr) else {
            return Ok(true);
        };
        if child.is_group() {
            if holds_array_rows(file, child_addr, depth + 1, visited, notes)? {
                return Ok(true);
            }
            continue;
        }
        if !child.is_dataset() {
            continue;
        }
        match file.dataset_info(&child) {
            Ok(info) => {
                if info.dims.first().is_some_and(|rows| *rows > 0) {
                    return Ok(true);
                }
            }
            Err(_) => return Ok(true),
        }
    }
    Ok(false)
}

fn episode_groups(
    file: &mut H5File,
    parent_path: &str,
    links: &[(String, u64)],
    notes: &mut Notes,
    unread: &mut Vec<UnmappedField>,
) -> Result<Vec<EpisodeGroup>, IngestError> {
    let mut groups: Vec<EpisodeGroup> = Vec::new();
    for (name, addr) in links {
        let header = file.object_header(*addr)?;
        if !header.is_group() {
            // An array (or a committed datatype) sitting beside the episode groups is not itself an
            // episode. It is still something the file holds and this ingest did not read, so it is
            // named rather than passed over — and an array with rows is named as a *coverage hole*,
            // because the rows are there and nothing read them.
            let path = format!("{parent_path}/{name}");
            let rows = header
                .is_dataset()
                .then(|| file.dataset_info(&header))
                .transpose()?
                .and_then(|info| info.dims.first().copied())
                .filter(|rows| *rows > 0);
            match rows {
                Some(rows) => unread.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this array sits beside the episode groups, so it belongs to no episode \
                         and its {rows} row(s) were not read"
                    ),
                }),
                None => notes.unmapped.push(UnmappedField {
                    source_path: path,
                    note: "this object sits beside the episode groups but is not a group, so no \
                           episode is built from it"
                        .into(),
                }),
            }
            continue;
        }
        groups.push(EpisodeGroup {
            name: name.clone(),
            path: format!("{parent_path}/{name}"),
            addr: *addr,
        });
    }
    // Sorted by name so ingestion is deterministic regardless of the order the file's index happens
    // to enumerate its links in.
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(groups)
}

/// The episode index for each group: the trailing number in its name when every name has a distinct
/// one, otherwise its position in name order.
fn episode_indices(groups: &[EpisodeGroup]) -> Vec<u64> {
    let parsed: Option<Vec<u64>> = groups.iter().map(|g| trailing_number(&g.name)).collect();
    if let Some(numbers) = parsed {
        let distinct: BTreeSet<u64> = numbers.iter().copied().collect();
        if distinct.len() == numbers.len() {
            return numbers;
        }
    }
    (0..groups.len() as u64).collect()
}

/// The trailing run of digits in a name (`demo_12` -> 12), or `None` when it has none or the number
/// does not fit a `u64`.
fn trailing_number(name: &str) -> Option<u64> {
    let digits: String = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Every attribute of an object, rendered into what metadata can carry.
fn read_attributes(
    file: &mut H5File,
    header: &ObjectHeader,
    skipped: &mut Vec<String>,
) -> Vec<(String, AttrValue)> {
    file.attributes(header, skipped)
        .iter()
        .map(|attr| {
            let value = match file.attribute_value(attr) {
                AttrValue::Text(text) if text.len() > MAX_ATTR_TEXT => {
                    let mut cut = MAX_ATTR_TEXT;
                    while cut > 0 && !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    AttrValue::Text(format!("{}… (truncated)", &text[..cut]))
                }
                other => other,
            };
            (attr.name.clone(), value)
        })
        .collect()
}

/// Walk one episode group, collecting every array below it.
fn collect_arrays(
    file: &mut H5File,
    addr: u64,
    prefix: &str,
    depth: usize,
    visited: &mut BTreeSet<u64>,
    out: &mut Vec<ArrayEntry>,
    notes: &mut Notes,
) -> Result<(), IngestError> {
    if depth > MAX_GROUP_DEPTH {
        return Err(parse_error(format!(
            "the object tree nests deeper than {MAX_GROUP_DEPTH} levels below an episode group"
        )));
    }
    if !visited.insert(addr) {
        // A hard link may point back at an ancestor. Following it would repeat every stream under
        // it, so the loop is cut and said out loud rather than silently duplicating a timeline.
        notes.unmapped.push(UnmappedField {
            source_path: prefix.to_string(),
            note: "this group is linked more than once in the tree; the repeat is not walked again"
                .into(),
        });
        return Ok(());
    }
    let header = file.object_header(addr)?;
    let links = file.group_links(&header, &mut notes.skipped_links)?;
    for (name, child_addr) in links {
        let child = file.object_header(child_addr)?;
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if child.is_group() {
            collect_arrays(file, child_addr, &path, depth + 1, visited, out, notes)?;
            continue;
        }
        if !child.is_dataset() {
            notes.unmapped.push(UnmappedField {
                source_path: path,
                note: "this object is neither a group nor a dataset (a committed datatype, say); \
                       no stream is built from it"
                    .into(),
            });
            continue;
        }
        let info = file.dataset_info(&child)?;
        out.push(ArrayEntry {
            path,
            header: child,
            info,
        });
    }
    Ok(())
}

/// The timeline an episode records, when it records one with declared units: `(frame count, ns
/// timestamps)`.
/// Read the episode's timestamp array.
///
/// Takes the frame budget for the reason `build_stream` does, and charges it *before* allocating:
/// this path used to be the one hole in that contract. A 20M-row float64 `time` array was read in
/// full and only then compared against `max_frames: 10` — the correct refusal arrived after 59
/// seconds and 160 MB. Worse on a hostile file: a datatype patched to zero width makes the row
/// readers non-terminating (a zero-length read succeeds, a zero-width fixed-point decodes to 0.0,
/// and the offset never advances), so a 24 MB input grew past 2.6 GB with no ceiling and no error a
/// CI gate could report. The identical construction on a non-timeline array was refused instantly,
/// because `build_stream` charges first.
/// The array carrying this episode's clock, the nanoseconds one of its units is worth, and how many
/// rows it holds.
///
/// Everything here comes from the array's *header* and its attributes — which array looks like a
/// timeline, and whether it states its units — so a run that reads no chunk can still answer
/// *whether this episode records measured time*, which is a fact about the file rather than about
/// the ingest.
fn resolve_timeline<'a>(
    file: &mut H5File,
    arrays: &'a [ArrayEntry],
    notes: &mut Notes,
) -> Result<Option<(&'a ArrayEntry, f64, u64)>, IngestError> {
    let Some(entry) = arrays.iter().find(|a| {
        TIME_LEAVES.contains(&leaf_name(&a.path))
            && a.info.datatype.is_numeric()
            && matches!(a.info.dims.len(), 1 | 2)
            && a.info.dims.get(1).map_or(true, |d| *d == 1)
    }) else {
        return Ok(None);
    };
    let attrs = read_attributes(file, &entry.header, &mut notes.skipped_attrs);
    let units = attrs
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(UNITS_ATTR))
        .and_then(|(_, value)| match value {
            AttrValue::Text(text) => Some(text.trim().to_ascii_lowercase()),
            _ => None,
        });
    let Some(units) = units else {
        notes.unmapped.push(UnmappedField {
            source_path: entry.path.clone(),
            note: format!(
                "a timeline array with no `{UNITS_ATTR}` attribute cannot be placed on a clock — \
                 whether its numbers are seconds or nanoseconds is not stated, and guessing would \
                 fabricate every rate and duration derived from it"
            ),
        });
        return Ok(None);
    };
    let Some(scale) = ns_per_unit(&units) else {
        notes.unmapped.push(UnmappedField {
            source_path: entry.path.clone(),
            note: format!(
                "the timeline array declares units `{units}`, which is not one this adapter knows \
                 (s, ms, us, ns)"
            ),
        });
        return Ok(None);
    };
    let rows = entry.info.dims.first().copied().unwrap_or(0);
    Ok(Some((entry, scale, rows)))
}

fn read_timeline(
    file: &mut H5File,
    arrays: &[ArrayEntry],
    notes: &mut Notes,
    budget: &mut FrameBudget,
) -> Result<Option<(u64, Vec<i64>)>, IngestError> {
    let Some((entry, scale, _)) = resolve_timeline(file, arrays, notes)? else {
        return Ok(None);
    };
    let count = entry.info.dims.first().copied().unwrap_or(0);
    // Charged on what the file *declares*, before a single row is read or a byte reserved.
    budget.take(FORMAT_ID, count)?;
    let datatype = entry.info.datatype.clone();
    let info = entry.info.clone();
    let mut stamps = Vec::with_capacity(count.min(1 << 20) as usize);
    let mut reader = file.rows(&info)?;
    for i in 0..count {
        let bytes = reader.row(i)?;
        let Some(value) = datatype.decode(&bytes) else {
            return Err(parse_error(format!(
                "the timeline array holds a {} value this adapter cannot decode",
                datatype.name()
            )));
        };
        if !value.is_finite() {
            return Err(parse_error(
                "the timeline array holds a non-finite timestamp, so the episode has no usable \
                 clock",
            ));
        }
        // Saturating rather than wrapping: a corrupt timestamp must not wrap into a plausible one.
        let ns = (value * scale).clamp(i64::MIN as f64, i64::MAX as f64);
        stamps.push(ns as i64);
    }
    Ok(Some((count, stamps)))
}

/// Nanoseconds per unit of a declared timestamp unit.
fn ns_per_unit(units: &str) -> Option<f64> {
    match units {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1e9),
        "ms" | "millisecond" | "milliseconds" => Some(1e6),
        "us" | "µs" | "microsecond" | "microseconds" => Some(1e3),
        "ns" | "nanosecond" | "nanoseconds" => Some(1.0),
        _ => None,
    }
}

/// Build one episode from its group.
fn build_episode(
    file: &mut H5File,
    group: &EpisodeGroup,
    index: u64,
    file_uri: &str,
    metadata_only: bool,
    budget: &mut FrameBudget,
    notes: &mut Notes,
) -> Result<Episode, IngestError> {
    let mut arrays = Vec::new();
    let mut visited = BTreeSet::new();
    collect_arrays(file, group.addr, "", 0, &mut visited, &mut arrays, notes)?;
    // Sorted by path so the streams of an episode are built in a file-independent order.
    arrays.sort_by(|a, b| a.path.cmp(&b.path));

    // A metadata-only run resolves *whether* this episode is on a measured clock, and over how many
    // rows, without reading a stamp: both are in the timeline array's header and attributes.
    let (timeline, declared_clock_rows) = if metadata_only {
        let rows = resolve_timeline(file, &arrays, notes)?.map(|(_, _, rows)| rows);
        // `Some(0)` when the file records no timeline at all, so every stream compares unequal and
        // lands on the step index — the same answer a full read gives.
        (None, Some(rows.unwrap_or(0)))
    } else {
        (read_timeline(file, &arrays, notes, budget)?, None)
    };

    let header = file.object_header(group.addr)?;
    let attrs = read_attributes(file, &header, &mut notes.skipped_attrs);
    let declared_frame_count = attrs
        .iter()
        .find(|(name, _)| name == DECLARED_LENGTH_ATTR)
        .and_then(|(_, value)| match value {
            AttrValue::Number(n) if *n >= 0.0 && n.fract() == 0.0 && *n <= u64::MAX as f64 => {
                Some(*n as u64)
            }
            _ => None,
        });
    let task = attrs
        .iter()
        .find(|(name, _)| TASK_ATTRS.iter().any(|t| t.eq_ignore_ascii_case(name)))
        .and_then(|(_, value)| match value {
            AttrValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        });

    let mut streams: Vec<Stream> = Vec::new();
    for entry in &arrays {
        match build_stream(
            file,
            entry,
            group,
            file_uri,
            timeline.as_ref(),
            declared_clock_rows,
            budget,
            notes,
        )? {
            Some(stream) => streams.push(stream),
            None => notes.unmapped.push(UnmappedField {
                source_path: format!("{}/{}", group.path, entry.path),
                note: unmappable_reason(&entry.info),
            }),
        }
    }

    let labels = task
        .as_ref()
        .map(|t| {
            vec![Label {
                key: "language".into(),
                value: t.clone(),
                ts: None,
            }]
        })
        .unwrap_or_default();

    // Whether the timeline describes *every* stream in the episode. It only does when they all hold
    // the same number of rows; a shorter or longer array is not on that clock.
    // `!streams.is_empty()` because `all()` is *true* over nothing: an episode that produced no
    // streams read as fully timed and took measured nanosecond bounds off a timeline that describes
    // none of its content. (The Zarr adapter had the same vacuous `all()` and could reach a slice
    // with it, which aborted the process on a ~700-byte store.)
    let fully_timed = match &timeline {
        Some((count, _)) => {
            !streams.is_empty()
                && streams
                    .iter()
                    .all(|s| s.frames.len() as u64 == *count && s.clock_kind == ClockKind::Measured)
        }
        None => false,
    };
    if timeline.is_some() && !fully_timed {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{}/[timeline]", group.path),
            note: "the timeline array does not cover every array in this episode (they hold \
                   different numbers of rows), so the episode's own start and end are left unset \
                   rather than stated in a unit only some of its streams share"
                .into(),
        });
    }

    // The episode's span. Measured time only when the recorded timeline covers the whole episode —
    // otherwise its bounds would be nanoseconds describing streams that are on a step index, which
    // `Episode::duration_ns` would subtract into a duration and the outlier check would compare
    // against real ones. Without a timeline the span is the step index: an episode of n steps runs
    // 0..n-1, and an empty one spans nothing.
    let (start_ts, end_ts) = match &timeline {
        Some((_, stamps)) if fully_timed && !stamps.is_empty() => {
            (stamps.iter().copied().min(), stamps.iter().copied().max())
        }
        Some(_) => (None, None),
        None => {
            let steps = streams
                .iter()
                .map(|s| s.frames.len() as u64)
                .max()
                .unwrap_or(0);
            if steps > 0 {
                (Some(0), Some(steps as i64 - 1))
            } else {
                (None, None)
            }
        }
    };

    // A per-episode declared count is comparable against the episode's frames only when its arrays
    // agree on how many rows they hold. Where they disagree the episode has no single length, and the
    // boundary check would charge the source for the longest array it happens to hold.
    let row_counts: BTreeSet<u64> = streams.iter().map(|s| s.frames.len() as u64).collect();
    let ragged = row_counts.len() > 1;
    if ragged && declared_frame_count.is_some() {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{}@{DECLARED_LENGTH_ATTR}", group.path),
            note:
                "this episode's arrays hold different numbers of rows, so it has no single frame \
                   count to compare the declared one against; the declared count is not carried"
                    .into(),
        });
    }

    Ok(Episode {
        index,
        start_ts,
        end_ts,
        streams,
        task,
        labels,
        // An HDF5 manipulation dataset records no ego trajectory.
        ego_poses: None,
        declared_frame_count: declared_frame_count.filter(|_| !ragged),
    })
}

/// Why an array could not become a stream.
fn unmappable_reason(info: &DatasetInfo) -> String {
    if info.dims.is_empty() {
        return "a scalar array has no first dimension, so it carries no timeline; it is metadata, \
                not a stream"
            .into();
    }
    match info.datatype.class {
        DatatypeClass::VariableLength => {
            "a variable-length array stores heap references rather than values, so its rows cannot \
             be fingerprinted reproducibly; no stream is built from it"
                .into()
        }
        _ => format!(
            "an array of type {} is not one this adapter maps",
            info.datatype.name()
        ),
    }
}

/// Build one stream from one array, or `None` when the array carries no timeline.
#[allow(clippy::too_many_arguments)]
fn build_stream(
    file: &mut H5File,
    entry: &ArrayEntry,
    group: &EpisodeGroup,
    file_uri: &str,
    timeline: Option<&(u64, Vec<i64>)>,
    declared_clock_rows: Option<u64>,
    budget: &mut FrameBudget,
    notes: &mut Notes,
) -> Result<Option<Stream>, IngestError> {
    let info = entry.info.clone();
    if info.dims.is_empty() || matches!(info.datatype.class, DatatypeClass::VariableLength) {
        return Ok(None);
    }
    let rows = info.dims[0];
    // A metadata-only run reads no row, so the array's header is all there is — and it is enough to
    // describe the stream: the datatype, the per-row shape, and whether the episode's timeline
    // covers these rows. Returned here, before the frame budget is charged for rows nobody will
    // read, and before a single chunk is touched.
    if let Some(clock_rows) = declared_clock_rows {
        // One unit per stream rather than one per row: no frame is allocated, but a `Stream` is,
        // and the file's group tree is what decides how many.
        budget.take(FORMAT_ID, 1)?;
        // `clock_rows > 0` because a file with no timeline reports zero, and a zero-row array would
        // otherwise compare equal to it and be reported as measured.
        let measured = clock_rows > 0 && clock_rows == rows;
        return Ok(Some(Stream {
            name: entry.path.clone(),
            modality: modality_for(&entry.path, &info),
            declared_rate_hz: None,
            clock_id: if measured {
                CLOCK_TIME
            } else {
                CLOCK_STEP_INDEX
            }
            .to_string(),
            // The clock describes the file — it stores a timestamp array and declares its units —
            // not this ingest. The temporal checks abstain here for want of frames, which the
            // coverage note states, rather than for want of a clock.
            clock_kind: if measured {
                ClockKind::Measured
            } else {
                ClockKind::StepIndex
            },
            dtype: Some(info.datatype.name()),
            shape: (info.dims.len() > 1).then(|| info.dims[1..].to_vec()),
            frames: Vec::new(),
            // Every statistic HDF5 offers is recomputed from values, and no value was read. `None`
            // rather than `Some(0)`: the difference is what tells a clean stream from an unread one.
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            latched: None,
            declared_range: None,
            point_fields: None,
            media: None,
            frame_id: None,
        }));
    }
    budget.take(FORMAT_ID, rows)?;

    // A stream is on measured time only when the episode's timeline covers exactly its rows.
    let stamps = timeline.filter(|(count, _)| *count == rows).map(|(_, s)| s);
    let clock_kind = if stamps.is_some() {
        ClockKind::Measured
    } else {
        ClockKind::StepIndex
    };
    let clock_id = if stamps.is_some() {
        CLOCK_TIME
    } else {
        CLOCK_STEP_INDEX
    };

    // Whether the values themselves can be summarized, and at what granularity. Statistics are
    // recomputed only for a numeric array of a width this reader decodes, and per-dimension only for
    // a feature narrow enough for per-dimension statistics to mean something: a 7-DoF action has
    // seven dimensions worth tracking, a 480x640x3 camera frame has 921,600 pixel positions, which is
    // not a robot signal and would cost an accumulator each.
    let elem = info.datatype.size as u64;
    let per_row = if elem == 0 { 0 } else { row_elements(&info) };
    // Asked of the decoder rather than guessed from the width: a 2-byte float is numeric and fits in
    // eight bytes, but nothing here decodes one, and `Some(0)` non-finite over an array whose values
    // were never examined would satisfy the very check that exists to find a NaN in them.
    let decodable = info.datatype.is_numeric()
        && elem > 0
        && info.datatype.decode(&vec![0u8; elem as usize]).is_some();
    let per_dimension = decodable && per_row > 0 && per_row <= MAX_STATS_DIMS;
    if decodable && !per_dimension && per_row > MAX_STATS_DIMS {
        notes.wide_arrays.insert(entry.path.clone());
    }
    if info.datatype.is_numeric() && !decodable {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{}/{}", group.path, entry.path),
            note: format!(
                "values of type {} are not decoded by this reader, so nothing is summarized from \
                 them — the statistical checks abstain on this stream rather than reporting it clean",
                info.datatype.name()
            ),
        });
    }

    let uri = format!("{file_uri}#{}/{}", group.path, entry.path);
    let mut frames = Vec::with_capacity(rows.min(1 << 20) as usize);
    let mut accum = FeatureAccum::default();
    let mut scalars: Vec<Option<f64>> = Vec::new();
    {
        let mut reader = file.rows(&info)?;
        let row_bytes = reader.row_bytes();
        for i in 0..rows {
            let bytes = reader.row(i)?;
            let ts = match stamps {
                Some(stamps) => stamps[i as usize],
                None => i as i64,
            };
            if per_dimension {
                scalars.clear();
                for chunk in bytes.chunks_exact(elem as usize) {
                    scalars.push(info.datatype.decode(chunk));
                }
                accum.push_cell(&scalars);
            } else if decodable && matches!(info.datatype.class, DatatypeClass::Float) {
                // Too wide for per-dimension statistics, but a NaN buried in one pixel of a float
                // depth map still matters, and counting it needs no accumulator per position.
                for chunk in bytes.chunks_exact(elem as usize) {
                    if info.datatype.decode(chunk).is_some_and(|v| !v.is_finite()) {
                        accum.count_non_finite();
                    }
                }
            }
            frames.push(Frame {
                ts,
                value_ref: ValueRef {
                    uri: uri.clone(),
                    byte_offset: reader.row_offset(i),
                    byte_len: Some(row_bytes),
                    content_hash: Some(Sha256::digest(&bytes).into()),
                },
            });
        }
    }

    Ok(Some(Stream {
        name: entry.path.clone(),
        modality: modality_for(&entry.path, &info),
        // HDF5 declares no sample rate. Nothing is inferred from the timestamps: a rate Veridex
        // computed would then be graded against the timestamps it came from, which always passes.
        declared_rate_hz: None,
        clock_id: clock_id.to_string(),
        clock_kind,
        dtype: Some(info.datatype.name()),
        shape: (info.dims.len() > 1).then(|| info.dims[1..].to_vec()),
        frames,
        // HDF5 stores no summary statistics of its own, so there is nothing to compare against —
        // only what Veridex recomputed from the values it read.
        stats: None,
        dim_stats: None,
        observed_stats: accum.stats(),
        observed_saturation: accum.saturation(),
        // `Some(0)` says the values were read and every one was finite; `None` says they were never
        // read, which is what the non-finite check needs to tell a clean stream from an unexamined
        // one. An integer array cannot hold a NaN, so reading it is enough to say it is clean.
        observed_non_finite: decodable.then(|| accum.non_finite()),
        observed_dim_stats: accum.dim_stats(),
        latched: None,
        declared_range: None,
        point_fields: None,
        media: None,
        frame_id: None,
    }))
}

/// The modality an array implies.
///
/// Only two things are read as a modality, and both are facts rather than guesses: an array named
/// exactly `action`/`actions` is the commanded action (the one universal name in every robot
/// dataset), and a `uint8` array whose per-frame shape is `[H, W, C]` with 1, 3, or 4 channels is an
/// image *by its structure*. Everything else stays scalar state — inferring a modality from a
/// substring is how honest data gets mis-modelled, and a mis-modelled stream is graded by the wrong
/// checks.
fn modality_for(path: &str, info: &DatasetInfo) -> Modality {
    if matches!(leaf_name(path), "action" | "actions") {
        return Modality::Action;
    }
    let per_frame = &info.dims[1..];
    let is_image = matches!(
        info.datatype,
        Datatype {
            class: DatatypeClass::FixedPoint { signed: false },
            size: 1,
            ..
        }
    ) && per_frame.len() == 3
        && matches!(per_frame[2], 1 | 3 | 4);
    if is_image {
        Modality::Video
    } else {
        Modality::ScalarState
    }
}

/// Values in one frame of an array: the product of every dimension after the first.
fn row_elements(info: &DatasetInfo) -> u64 {
    info.dims
        .iter()
        .skip(1)
        .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
        .unwrap_or(u64::MAX)
}

/// The final segment of a `/`-joined path.
fn leaf_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// A number from an attribute, rendered without a spurious `.0` on a whole value.
fn format_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.0e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
