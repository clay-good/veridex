//! Zarr adapter: the format Diffusion Policy, UMI, and the replay-buffer tooling around them ship
//! in, read into the CDM.
//!
//! A Zarr store is a directory tree of plain files, and the layout this corpus shares is a **replay
//! buffer**: every step of every episode concatenated into one flat array per key, with the episode
//! boundaries kept separately.
//!
//! ```text
//! store.zarr/
//!   data/
//!     action        [N, 2]        float32   -> one stream per key, sliced per episode
//!     state         [N, 5]        float64
//!     img           [N, H, W, 3]  uint8
//!   meta/
//!     episode_ends  [E]           int64     -> cumulative end index of each episode
//! ```
//!
//! So `episode_ends` *is* the episode structure: `[4, 10]` means episode 0 is rows 0..4 and episode 1
//! is rows 4..10. Veridex slices every `data/` array at those boundaries. Nothing else is inferred —
//! rows past the last boundary belong to no episode, and the report says so rather than quietly
//! attaching them to the last one, because an off-by-one in a replay buffer is exactly the kind of
//! boundary corruption this tool exists to catch.
//!
//! A store that is not a replay buffer still reads: a group of arrays is one episode, and a group of
//! groups-of-arrays is one episode each — the same rules the HDF5 adapter applies to a tree of the
//! same shape.
//!
//! **The timeline is the step index, unless the store says otherwise.** Zarr has no notion of time,
//! and a `timestamp` array becomes a clock only when its `.zattrs` declares its `units`, for the same
//! reason as in HDF5: whether a bare `time` column is seconds or nanoseconds is not stated, and
//! guessing it fabricates every rate, duration, and skew verdict derived from it.

mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, Episode, Frame, Label, Modality, Provenance, ProvenanceClass,
    ProvenanceElement, ProvenanceScope, Stream, ValueRef, META_DECLARED_EPISODES,
};

use self::store::{read_attrs, RowReader, ZarrArray, ARRAY_FILE, GROUP_FILE, V3_FILE};

use super::stats::FeatureAccum;
use super::{
    Adapter, Coverage, DecompressionBudget, Detection, FrameBudget, IngestError, IngestOptions,
    IngestReport, Ingested, Source, UnmappedField,
};

pub(crate) const FORMAT_ID: &str = "zarr";

/// The Zarr format versions this adapter reads. Zarr v3 renames every metadata file and changes the
/// codec model, so a v3 store is refused by version rather than mis-read.
pub(crate) const SUPPORTED_VERSIONS: &[&str] = &["2"];

/// The clock frames are stamped on when the store records no time of its own.
const CLOCK_STEP_INDEX: &str = "zarr-step-index";

/// The clock frames are stamped on when the store records timestamps *and* declares their units.
const CLOCK_TIME: &str = "zarr-time";

/// The group a replay buffer keeps its per-step arrays under.
const DATA_GROUP: &str = "data";

/// The group a replay buffer keeps its episode boundaries under, and the array that holds them.
const META_GROUP: &str = "meta";
const EPISODE_ENDS: &str = "episode_ends";

/// Leaf names that hold a timeline, when one is recorded.
const TIME_LEAVES: &[&str] = &["timestamp", "timestamps", "time", "times"];

/// The attribute a timeline array must carry for its numbers to be read as time.
const UNITS_ATTR: &str = "units";

/// Attributes carrying an episode's or a store's natural-language task.
const TASK_ATTRS: &[&str] = &["language_instruction", "task"];

/// This reader's own store attributes, checked before the shared well-known table.
///
/// Two of them disagree with it deliberately. `author` here is who wrote the *file*, which is not
/// the same question `provenance.annotator` asks (who labelled the data), so it stays its own
/// element rather than inheriting the recording-metadata reading; and `date` is kept as `created`,
/// which the shared table has no row for. Everything else — `sensor`, `device`, `derived_from`,
/// `calibration`, the rig-lineage keys — now resolves through [`super::provenance_key_for`], so a key
/// called `sensor` means the same thing in a store attribute as in an MCAP `Metadata` record.
const PROVENANCE_ATTRS: &[(&str, &str)] = &[
    ("license", "license"),
    ("author", "author"),
    ("date", "created"),
];

/// How deep the tree is walked below an episode group.
const MAX_GROUP_DEPTH: usize = 16;

/// The ceiling on how much of one attribute's rendered value is carried into metadata.
const MAX_ATTR_TEXT: usize = 4096;

/// The ceiling on values per frame for which per-dimension statistics are recomputed. See the same
/// constant in the HDF5 adapter: per-position statistics of an image answer nothing.
const MAX_STATS_DIMS: u64 = 256;

/// The ceiling on arrays one store may hold.
///
/// Every array becomes a stream in every episode that owns it, and both counts come from the store's
/// own directory tree. A frame budget bounds frames, not the `Stream` structs around them, so a store
/// of thousands of tiny arrays across thousands of groups is bounded here instead.
const MAX_ARRAYS: usize = 100_000;

/// The ceiling on episodes a store's `episode_ends` may declare.
///
/// The boundaries are read before any data is, and each one becomes an episode with a stream per
/// key — a few hundred bytes of metadata that would otherwise decide how much this process
/// allocates. Chosen far above any real replay buffer, which holds thousands of episodes at most.
const MAX_EPISODES: u64 = 1_000_000;

/// Adapter for a Zarr v2 store.
pub struct ZarrAdapter;

fn parse_error(message: impl Into<String>) -> IngestError {
    store::parse_error(message)
}

/// What an ingest could not map, so the report can disclose all of it.
#[derive(Default)]
struct Notes {
    unmapped: Vec<UnmappedField>,
    /// Arrays that hold data this reader could not read.
    ///
    /// Separate from `unmapped` because they mean different things to a reader of the verdict:
    /// "unmapped" is a field the CDM has no shape for, which costs nothing; this is data that is
    /// *there* and went unread, so every result is over less of the store than it appears to be.
    /// Only this one raises `COVERAGE.SOURCE_UNREAD`.
    unread: Vec<UnmappedField>,
    /// Numeric arrays too wide for per-dimension statistics.
    wide_arrays: BTreeSet<String>,
}

/// One array found in the store, with the path it is named by.
struct ArrayEntry {
    /// The path below the group it was found in, `/`-joined.
    path: String,
    array: ZarrArray,
}

impl ArrayEntry {
    fn leaf(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// One episode: which arrays it owns, and which of their rows it covers.
struct EpisodeSlice {
    index: u64,
    /// The store path this episode came from, for provenance.
    source: String,
    /// The half-open row range within each array (the whole array for a per-episode-group layout).
    start: u64,
    end: u64,
    /// For a group-per-episode layout, the group whose arrays belong to this episode. Its own arrays
    /// and *only* its own: without this, every episode would carry every group's data, so each
    /// episode would hold its neighbours' frames and no per-episode verdict would mean anything.
    /// `None` for a replay buffer, where every array spans every episode.
    prefix: Option<String>,
}

impl EpisodeSlice {
    /// Whether `path` names an array this episode owns, and what the stream is called within it.
    ///
    /// The name is the path *below* the episode's group, so the same sensor is the same stream name in
    /// every episode — which is what makes the cross-episode dtype, shape, and stream-presence checks
    /// comparable at all.
    fn stream_name<'a>(&self, path: &'a str) -> Option<&'a str> {
        match &self.prefix {
            None => Some(path),
            Some(prefix) => path.strip_prefix(prefix)?.strip_prefix('/'),
        }
    }
}

impl Adapter for ZarrAdapter {
    fn format_id(&self) -> &'static str {
        FORMAT_ID
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        SUPPORTED_VERSIONS
    }

    fn detect(&self, source: &Source) -> Detection {
        let Source::Local(dir) = source else {
            return Detection::No;
        };
        if !dir.is_dir() {
            return Detection::No;
        }
        if dir.join(GROUP_FILE).is_file() || dir.join(ARRAY_FILE).is_file() {
            return Detection::Yes {
                version: Some("2".into()),
            };
        }
        // A v3 store is still recognized as Zarr, so it is refused *by version* rather than reported
        // as a format nothing understands — the difference between "re-save this as v2" and "we have
        // no idea what this is".
        if dir.join(V3_FILE).is_file() {
            return Detection::Yes {
                version: Some("3".into()),
            };
        }
        Detection::No
    }

    /// A Zarr store keeps its structure *outside* its data: `.zarray` states every array's dtype,
    /// per-row shape and row count, `.zattrs` carries the store's own metadata, and `meta/` holds
    /// the episode boundaries — a few kilobytes in front of a replay buffer that may be hundreds of
    /// gigabytes of chunks, none of which this mode opens.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        let Source::Local(root) = source else {
            return Err(parse_error("remote Zarr ingestion is not supported"));
        };
        // Zarr v3 renames every metadata file and changes the codec model, so nothing below would
        // read it correctly. Refused by version, carrying what the store itself declares.
        if !root.join(GROUP_FILE).is_file() && !root.join(ARRAY_FILE).is_file() {
            let declared = std::fs::read(root.join(V3_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v.get("zarr_format").and_then(|f| f.as_u64()))
                .map(|f| f.to_string());
            return Err(IngestError::UnsupportedVersion {
                format_id: FORMAT_ID,
                version: declared.or_else(|| Some("3".into())),
                supported: SUPPORTED_VERSIONS,
            });
        }

        let mut notes = Notes::default();
        let mut budget = FrameBudget::new(options);
        // A Zarr store is a tree of files rather than one file, so the expansion budget is scaled by
        // the total size of the chunks on disk.
        let mut decompression = DecompressionBudget::new(options, store_bytes(root));

        let root_attrs = read_attrs(root)?;

        // Which group holds the per-step arrays, and how the episodes are delimited.
        let data_dir = if root.join(DATA_GROUP).join(GROUP_FILE).is_file() {
            root.join(DATA_GROUP)
        } else {
            root.to_path_buf()
        };
        let mut arrays = Vec::new();
        if root.join(ARRAY_FILE).is_file() {
            // The store *is* an array: one stream, and one episode covering its rows. `zarr.open()`
            // reads such a store as an array, so refusing it would be this reader's limitation
            // dressed up as the store's.
            arrays.push(ArrayEntry {
                path: root
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("values")
                    .to_string(),
                array: ZarrArray::open(root)?,
            });
        } else {
            collect_arrays(&data_dir, "", 0, &mut arrays, &mut notes)?;
        }
        arrays.sort_by(|a, b| a.path.cmp(&b.path));

        let ends = read_episode_ends(root, &mut decompression)?;
        let slices = match &ends {
            Some(ends) => replay_buffer_slices(ends, &arrays, &mut notes)?,
            None => group_slices(&data_dir, &mut arrays, &mut notes)?,
        };
        if slices.is_empty() {
            return Err(parse_error(format!(
                "no episodes: the store holds no `meta/episode_ends` and no group of arrays, so \
                 there is no timeline to read{}",
                why_nothing_was_read(&notes)
            )));
        }

        let available: BTreeSet<u64> = slices.iter().map(|s| s.index).collect();
        let selected = if options.sample.is_partial() {
            options.sample.select(&available)
        } else {
            available.clone()
        };

        // Read within each episode's own scope. A replay buffer's timeline spans the whole store, so
        // one read serves every episode; a group-per-episode layout has one timeline per group, and
        // taking the first one found would stamp every episode with another episode's recorded times.
        let mut timelines: BTreeMap<u64, Option<Vec<i64>>> = BTreeMap::new();
        // Whether each episode's own arrays declare a timeline with usable units. Only consulted
        // under `--metadata-only`, where no stamps are read and the clock has to come from the
        // metadata — and per episode, because a group-per-episode layout has one timeline per
        // group, so a store-wide answer would put a neighbour's clock on an untimed episode.
        let mut declares_clock: BTreeMap<u64, bool> = BTreeMap::new();
        for slice in &slices {
            if !selected.contains(&slice.index) {
                continue;
            }
            let owned: Vec<&ArrayEntry> = arrays
                .iter()
                .filter(|a| slice.stream_name(&a.path).is_some())
                .collect();
            // A metadata-only run opens no chunk, so it reads no stamps — but *whether* the store
            // records measured time is declared in `.zarray`/`.zattrs`, and that is a fact about
            // the source rather than about the ingest. Resolved either way, so the clock the CDM
            // reports is the store's own.
            let timeline = if options.metadata_only {
                let declared = resolve_timeline(&owned, &mut notes)?.is_some();
                declares_clock.insert(slice.index, declared);
                None
            } else {
                read_timeline(&owned, &mut decompression, &mut notes)?
            };
            timelines.insert(slice.index, timeline);
        }
        let store_name = crate::adapter::dataset_id_from_path(root, FORMAT_ID);

        let mut episodes: Vec<Episode> = Vec::new();
        let mut provenance: Vec<Provenance> = Vec::new();
        for slice in &slices {
            if !selected.contains(&slice.index) {
                continue;
            }
            let timeline = timelines.get(&slice.index).and_then(|t| t.as_ref());
            let mut streams = Vec::new();
            for entry in &arrays {
                let Some(name) = slice.stream_name(&entry.path) else {
                    continue;
                };
                let built = if options.metadata_only {
                    declared_stream(
                        entry,
                        name,
                        slice,
                        declares_clock.get(&slice.index).copied().unwrap_or(false),
                        &mut budget,
                        &mut notes,
                    )?
                } else {
                    build_stream(
                        entry,
                        name,
                        slice,
                        &store_name,
                        timeline,
                        &mut budget,
                        &mut decompression,
                        &mut notes,
                    )?
                };
                if let Some(stream) = built {
                    streams.push(stream);
                }
            }
            // `all()` on an empty collection is *true*, so an episode that produced no streams at
            // all — every array in the store is shorter than the range `episode_ends` declares for
            // it — read as "fully timed" and went on to index the timeline with that range. A
            // ~700-byte store (`episode_ends = [100, 100]` over a 10-element timeline) aborted the
            // process: "range start index 100 out of range for slice of length 10". Veridex is
            // pointed at datasets from strangers, so a hostile file must not be able to panic it.
            //
            // The length check is the same reasoning one step further: a timeline is evidence about
            // the rows it actually covers, and an episode reaching past its end is not timed by it.
            let fully_timed = timeline.is_some()
                && !streams.is_empty()
                && streams.iter().all(|s| s.clock_kind == ClockKind::Measured)
                && timeline.is_some_and(|t| (slice.end as usize) <= t.len());
            let (start_ts, end_ts) = match timeline {
                // Measured bounds only when the recorded timeline covers the whole episode; a mixed
                // episode would otherwise state nanosecond bounds for streams on a step index.
                Some(stamps) if fully_timed => {
                    let window = &stamps[slice.start as usize..slice.end as usize];
                    (window.iter().copied().min(), window.iter().copied().max())
                }
                Some(_) => (None, None),
                None => {
                    let steps = streams.iter().map(|s| s.frames.len()).max().unwrap_or(0);
                    if steps > 0 {
                        (Some(0), Some(steps as i64 - 1))
                    } else {
                        (None, None)
                    }
                }
            };
            provenance.push(Provenance {
                scope: ProvenanceScope::Episode(slice.index),
                elements: vec![ProvenanceElement {
                    key: "upstream".into(),
                    value: Some(slice.source.clone()),
                    class: ProvenanceClass::Known,
                }],
            });
            episodes.push(Episode {
                index: slice.index,
                start_ts,
                end_ts,
                streams,
                task: task_of(&root_attrs),
                labels: task_of(&root_attrs)
                    .map(|t| {
                        vec![Label {
                            key: "language".into(),
                            value: t,
                            ts: None,
                        }]
                    })
                    .unwrap_or_default(),
                // A Zarr replay buffer records no ego trajectory.
                ego_poses: None,
                ego_frame: None,
                // On a full run the boundaries *are* the length, so recording it as a separate
                // claim would give the length check `n` to compare against `n`. A metadata-only run
                // reads no rows, so the boundaries are the only length there is — recorded, so the
                // report can say how long the store declares each episode to be.
                declared_frame_count: options
                    .metadata_only
                    .then(|| slice.end.saturating_sub(slice.start)),
            });
        }
        episodes.sort_by_key(|e| e.index);
        if episodes.iter().all(|e| e.streams.is_empty()) {
            return Err(parse_error(format!(
                "no arrays under any episode, so there are no frames to check{}",
                why_nothing_was_read(&notes)
            )));
        }

        let mut metadata: Vec<(String, String)> = vec![
            ("source_format".into(), FORMAT_ID.into()),
            ("zarr_format".into(), "2".into()),
        ];
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some(FORMAT_ID.into()),
            class: ProvenanceClass::Known,
        }];
        for (name, value) in &root_attrs {
            let rendered = render_attr(value);
            if let Some(key) = PROVENANCE_ATTRS
                .iter()
                .find(|(attr, _)| attr.eq_ignore_ascii_case(name))
                .map(|(_, key)| *key)
                .or_else(|| crate::adapter::provenance_key_for(name))
            {
                elements.push(ProvenanceElement {
                    key: key.into(),
                    value: Some(rendered.clone()),
                    class: ProvenanceClass::Known,
                });
            }
            metadata.push((format!("zarr:{name}"), rendered));
        }
        // The episode count the store itself declares, comparable only against a full ingest.
        if !options.sample.is_partial() {
            if let Some(ends) = &ends {
                metadata.push((META_DECLARED_EPISODES.to_string(), ends.len().to_string()));
            }
        } else {
            metadata.push((
                META_DECLARED_EPISODES.to_string(),
                selected.len().to_string(),
            ));
        }
        provenance.push(Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        });

        let measured = episodes
            .iter()
            .flat_map(|e| e.streams.iter())
            .any(|s| s.clock_kind == ClockKind::Measured);
        let mut mapped_fields = vec![
            "array -> stream (first dimension -> frames)".into(),
            ".zarray dtype -> stream.dtype".into(),
            ".zarray shape after the first dimension -> stream.shape".into(),
            ".zattrs -> dataset metadata".into(),
        ];
        if options.metadata_only {
            mapped_fields
                .push(".zarray shape[0] + meta/episode_ends -> declared episode lengths".into());
        } else {
            mapped_fields.push("row bytes -> frame.value_ref.content_hash (SHA-256)".into());
        }
        if episodes
            .iter()
            .flat_map(|e| e.streams.iter())
            .any(|s| s.observed_stats.is_some() || s.observed_non_finite.is_some())
        {
            mapped_fields.push(
                "numeric array values -> recomputed per-dimension statistics, saturation, and \
                 non-finite counts"
                    .into(),
            );
        }
        if ends.is_some() {
            mapped_fields.push("meta/episode_ends -> episode boundaries".into());
        }
        let mut omitted_fields = vec![
            "image/point payload decoding (frames are fingerprints, not pixels)".into(),
            "declared sample rates (Zarr states none, so the rate and gap checks have no nominal \
             period to grade against)"
                .into(),
        ];
        if options.metadata_only {
            omitted_fields.push(
                "row values, frame timestamps, and content hashes (no data chunk was opened; only \
                 the `.zarray`/`.zattrs` metadata and the `meta/` group that delimits the episodes \
                 were read)"
                    .into(),
            );
            omitted_fields.push(
                "recomputed statistics, saturation and non-finite counts (every one is derived \
                 from values, and no value was read)"
                    .into(),
            );
        }
        if measured {
            mapped_fields.push(format!(
                "timeline array + `{UNITS_ATTR}` attribute -> frame.ts (measured)"
            ));
        } else {
            omitted_fields.push(format!(
                "per-frame timestamps (this store records none with declared units; the timeline is \
                 the step index on clock `{CLOCK_STEP_INDEX}`, so the rate, gap, jitter and skew \
                 checks have no measured time to grade)"
            ));
        }
        if !notes.wide_arrays.is_empty() {
            let named: Vec<String> = notes.wide_arrays.iter().take(4).cloned().collect();
            let rest = notes.wide_arrays.len().saturating_sub(named.len());
            omitted_fields.push(format!(
                "per-dimension statistics for array(s) with more than {MAX_STATS_DIMS} values per \
                 frame ({}{}) — they are scanned for non-finite values",
                named.join(", "),
                if rest > 0 {
                    format!(" and {rest} more")
                } else {
                    String::new()
                }
            ));
        }

        let episodes_ingested = episodes.len() as u64;
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
            dataset: Dataset {
                id: store_name
                    .strip_suffix(".zarr")
                    .unwrap_or(&store_name)
                    .to_string(),
                metadata,
                provenance,
                episodes,
                // A Zarr replay buffer records no sensor-rig calibration.
                calibration: None,
            },
            report: IngestReport {
                unread_sources: notes.unread,
                format_id: FORMAT_ID,
                source_version: Some("2".into()),
                coverage,
                mapped_fields,
                unmapped_fields: notes.unmapped,
                omitted_fields,
            },
        })
    }
}

/// Why a store yielded nothing, when the reason is an array this reader had to skip.
///
/// Skipping one unreadable array and disclosing it is right — a real store keeps auxiliary arrays
/// beside its data, and Python reads the rest of it. But when *nothing* survives, the refusal has to
/// carry the reason: "this store's layout is not one this adapter maps" would blame the layout for a
/// codec, and the codec is what the user has to change.
fn why_nothing_was_read(notes: &Notes) -> String {
    let reasons: Vec<&str> = notes
        .unread
        .iter()
        .chain(notes.unmapped.iter())
        .filter(|f| f.note.contains("could not be read"))
        .map(|f| f.note.as_str())
        .take(3)
        .collect();
    if reasons.is_empty() {
        return " — this store's layout is not one this adapter maps".into();
    }
    format!(" — {}", reasons.join("; "))
}

/// The total size of the store's files, which scales the expansion budget.
fn store_bytes(root: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64, depth: usize) {
        if depth > MAX_GROUP_DEPTH {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            // Symlinks are not followed here either, so a link cycle cannot make sizing the store
            // an exponential walk.
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {}
                Ok(meta) if meta.is_dir() => walk(&path, total, depth + 1),
                Ok(meta) => *total = total.saturating_add(meta.len()),
                Err(_) => {}
            }
        }
    }
    let mut total = 0;
    walk(root, &mut total, 0);
    total
}

/// Every array below `dir`, in path order.
fn collect_arrays(
    dir: &Path,
    prefix: &str,
    depth: usize,
    out: &mut Vec<ArrayEntry>,
    notes: &mut Notes,
) -> Result<(), IngestError> {
    if depth > MAX_GROUP_DEPTH {
        return Err(parse_error(format!(
            "the store nests deeper than {MAX_GROUP_DEPTH} levels"
        )));
    }
    let mut children: Vec<PathBuf> = Vec::new();
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                // `symlink_metadata`, not `metadata`: a link is *not* followed. A store that links to
                // a directory outside itself would otherwise have that directory's bytes read,
                // hashed, and signed into a certificate as if they were part of the dataset — and a
                // link pointing at its own parent makes this walk exponential. The same rule the
                // LeRobot and CAN adapters already apply.
                let Ok(meta) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.file_type().is_symlink() {
                    notes.unmapped.push(UnmappedField {
                        source_path: entry.file_name().to_string_lossy().into_owned(),
                        note: "this entry is a symbolic link; it is not followed, because what it \
                               points at is not part of the store"
                            .into(),
                    });
                    continue;
                }
                if meta.is_dir() {
                    children.push(path);
                }
            }
        }
        Err(e) => return Err(IngestError::Io(e.to_string())),
    }
    // Sorted so ingestion does not depend on the order the filesystem happens to list a directory.
    children.sort();
    for child in children {
        let Some(name) = child.file_name().and_then(|n| n.to_str()) else {
            notes.unmapped.push(UnmappedField {
                source_path: child.display().to_string(),
                note: "this directory's name is not valid UTF-8, and a stream name is bound into \
                       the content hash, so it cannot be read losslessly"
                    .into(),
            });
            continue;
        };
        let path = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if child.join(ARRAY_FILE).is_file() {
            // One array this reader cannot read must not refuse the whole store: a real store keeps
            // auxiliary arrays beside its data (a structured dtype, a string index), and Python reads
            // the rest of it happily. What matters is that the omission is named, so a stream that is
            // *absent* is never mistaken for a stream that is clean.
            match ZarrArray::open(&child) {
                Ok(array) => out.push(ArrayEntry { path, array }),
                // Data that is in the store and was not read — not a field the CDM cannot hold.
                // Filed as unmapped, a store whose every array used an unsupported codec came back
                // with no streams and nothing in the verdict saying the data had gone unread.
                Err(e) => notes.unread.push(UnmappedField {
                    source_path: path,
                    note: format!(
                        "this array could not be read, so no stream is built from it: {e}"
                    ),
                }),
            }
            if out.len() > MAX_ARRAYS {
                return Err(parse_error(format!(
                    "the store holds more than {MAX_ARRAYS} arrays"
                )));
            }
        } else if child.join(GROUP_FILE).is_file() {
            collect_arrays(&child, &path, depth + 1, out, notes)?;
        } else {
            notes.unmapped.push(UnmappedField {
                source_path: path,
                note: "this directory holds neither `.zarray` nor `.zgroup`, so it is not part of \
                       the Zarr store; nothing is built from it"
                    .into(),
            });
        }
    }
    Ok(())
}

/// The replay buffer's episode boundaries, when the store keeps them.
fn read_episode_ends(
    root: &Path,
    budget: &mut DecompressionBudget,
) -> Result<Option<Vec<u64>>, IngestError> {
    let dir = root.join(META_GROUP).join(EPISODE_ENDS);
    if !dir.join(ARRAY_FILE).is_file() {
        return Ok(None);
    }
    let array = ZarrArray::open(&dir)?;
    if array.shape.len() != 1 || !array.dtype.numeric || array.dtype.float {
        return Err(parse_error(format!(
            "`{META_GROUP}/{EPISODE_ENDS}` is not a one-dimensional integer array ({} \
             dimension(s), dtype {}), so the episode boundaries cannot be read",
            array.shape.len(),
            array.dtype.name
        )));
    }
    let count = array.shape[0];
    if count > MAX_EPISODES {
        return Err(parse_error(format!(
            "`{META_GROUP}/{EPISODE_ENDS}` declares {count} episodes, over the {MAX_EPISODES} \
             ceiling"
        )));
    }
    let mut reader = RowReader::new(&array, budget)?;
    let mut ends = Vec::with_capacity(count.min(4096) as usize);
    for i in 0..count {
        let bytes = reader.row(i)?;
        let value = array.dtype.decode(&bytes).ok_or_else(|| {
            parse_error(format!(
                "an episode boundary of type {} cannot be decoded",
                array.dtype.name
            ))
        })?;
        if !(value.is_finite() && value >= 0.0) {
            return Err(parse_error(format!(
                "episode boundary {i} is {value}, which is not a row index"
            )));
        }
        ends.push(value as u64);
    }
    Ok(Some(ends))
}

/// Turn `episode_ends` into row ranges, checking them against the arrays they index.
fn replay_buffer_slices(
    ends: &[u64],
    arrays: &[ArrayEntry],
    notes: &mut Notes,
) -> Result<Vec<EpisodeSlice>, IngestError> {
    let rows = arrays.iter().map(|a| a.array.shape[0]).max().unwrap_or(0);
    let mut slices = Vec::with_capacity(ends.len());
    let mut previous = 0u64;
    for (index, end) in ends.iter().copied().enumerate() {
        // A boundary that goes backwards, or past the end of the arrays it indexes, contradicts the
        // data it describes. Refused rather than clamped: clamping would invent an episode.
        if end < previous {
            return Err(parse_error(format!(
                "episode boundary {index} ({end}) is before boundary {} ({previous}); \
                 `{META_GROUP}/{EPISODE_ENDS}` must be non-decreasing",
                index.saturating_sub(1)
            )));
        }
        if end > rows {
            return Err(parse_error(format!(
                "episode boundary {index} ({end}) is past the {rows} row(s) the store's arrays \
                 hold"
            )));
        }
        slices.push(EpisodeSlice {
            index: index as u64,
            source: format!("{META_GROUP}/{EPISODE_ENDS}[{index}]"),
            start: previous,
            end,
            prefix: None,
        });
        previous = end;
    }
    // Rows past the last boundary belong to no episode. Attaching them to the last one would hide
    // exactly the off-by-one this check exists to catch.
    if previous < rows {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{DATA_GROUP}/*[{previous}..{rows}]"),
            note: format!(
                "{} row(s) sit past the last episode boundary and belong to no episode; they are \
                 not ingested",
                rows - previous
            ),
        });
    }
    Ok(slices)
}

/// Episodes for a store that is not a replay buffer: a group of arrays is one episode, and a group of
/// groups is one episode each.
fn group_slices(
    data_dir: &Path,
    arrays: &mut Vec<ArrayEntry>,
    notes: &mut Notes,
) -> Result<Vec<EpisodeSlice>, IngestError> {
    // Arrays sitting directly in the group are one episode covering all of their rows.
    let flat: Vec<&ArrayEntry> = arrays.iter().filter(|a| !a.path.contains('/')).collect();
    if !flat.is_empty() {
        let rows = flat.iter().map(|a| a.array.shape[0]).max().unwrap_or(0);
        let nested: Vec<String> = arrays
            .iter()
            .filter(|a| a.path.contains('/'))
            .map(|a| a.path.clone())
            .collect();
        for path in &nested {
            notes.unmapped.push(UnmappedField {
                source_path: path.clone(),
                note:
                    "this array is nested below a group that also holds arrays of its own, which \
                       is neither a replay buffer nor one episode per group; it is not ingested"
                        .into(),
            });
        }
        arrays.retain(|a| !a.path.contains('/'));
        return Ok(vec![EpisodeSlice {
            index: 0,
            source: data_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("/")
                .to_string(),
            start: 0,
            end: rows,
            prefix: None,
        }]);
    }
    // Otherwise each first-level group is an episode, and its arrays are its streams. Handled by
    // re-keying the array paths so each episode sees only its own.
    let mut groups: BTreeSet<String> = BTreeSet::new();
    for entry in arrays.iter() {
        if let Some((group, _)) = entry.path.split_once('/') {
            groups.insert(group.to_string());
        }
    }
    let numbers: Option<Vec<u64>> = groups.iter().map(|g| trailing_number(g)).collect();
    let distinct = numbers
        .as_ref()
        .map(|n| n.iter().copied().collect::<BTreeSet<u64>>().len() == n.len())
        .unwrap_or(false);
    Ok(groups
        .iter()
        .enumerate()
        .map(|(position, group)| {
            let rows = arrays
                .iter()
                .filter(|a| a.path.starts_with(&format!("{group}/")))
                .map(|a| a.array.shape[0])
                .max()
                .unwrap_or(0);
            EpisodeSlice {
                index: match (&numbers, distinct) {
                    (Some(numbers), true) => numbers[position],
                    _ => position as u64,
                },
                source: group.clone(),
                start: 0,
                end: rows,
                prefix: Some(group.clone()),
            }
        })
        .collect())
}

/// The final segment of a `/`-joined path.
fn leaf_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The trailing run of digits in a name (`episode_12` -> 12).
fn trailing_number(name: &str) -> Option<u64> {
    let digits: String = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    digits.parse().ok()
}

/// The store's timeline in nanoseconds, when it records one with declared units.
/// The array that carries this episode's clock, and the nanoseconds one of its units is worth.
///
/// Everything here is read from `.zarray` and `.zattrs` — which array looks like a timeline, and
/// whether it states its units — so a run that opens no chunk can still answer *whether the store
/// records measured time*, which is a fact about the source rather than about the ingest.
fn resolve_timeline<'a>(
    arrays: &[&'a ArrayEntry],
    notes: &mut Notes,
) -> Result<Option<(&'a ArrayEntry, f64)>, IngestError> {
    let Some(entry) = arrays.iter().find(|a| {
        TIME_LEAVES.contains(&a.leaf())
            && a.array.dtype.numeric
            && matches!(a.array.shape.len(), 1 | 2)
            && a.array.shape.get(1).map_or(true, |d| *d == 1)
    }) else {
        return Ok(None);
    };
    let attrs = read_attrs(&entry.array.dir)?;
    let units = attrs
        .get(UNITS_ATTR)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase());
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
    Ok(Some((entry, scale)))
}

fn read_timeline(
    arrays: &[&ArrayEntry],
    budget: &mut DecompressionBudget,
    notes: &mut Notes,
) -> Result<Option<Vec<i64>>, IngestError> {
    let Some((entry, scale)) = resolve_timeline(arrays, notes)? else {
        return Ok(None);
    };
    let mut reader = RowReader::new(&entry.array, budget)?;
    let count = entry.array.shape[0];
    let mut stamps = Vec::with_capacity(count.min(1 << 20) as usize);
    for i in 0..count {
        let bytes = reader.row(i)?;
        let value = entry.array.dtype.decode(&bytes).ok_or_else(|| {
            parse_error(format!(
                "the timeline array holds a {} value this adapter cannot decode",
                entry.array.dtype.name
            ))
        })?;
        if !value.is_finite() {
            return Err(parse_error(
                "the timeline array holds a non-finite timestamp, so the store has no usable clock",
            ));
        }
        stamps.push((value * scale).clamp(i64::MIN as f64, i64::MAX as f64) as i64);
    }
    Ok(Some(stamps))
}

fn ns_per_unit(units: &str) -> Option<f64> {
    match units {
        "s" | "sec" | "secs" | "second" | "seconds" => Some(1e9),
        "ms" | "millisecond" | "milliseconds" => Some(1e6),
        "us" | "µs" | "microsecond" | "microseconds" => Some(1e3),
        "ns" | "nanosecond" | "nanoseconds" => Some(1.0),
        _ => None,
    }
}

/// The stream an array implies over one episode's rows, with no frames in it.
///
/// The `--metadata-only` counterpart to [`build_stream`]: `.zarray` states the dtype, the per-row
/// shape and how many rows the array holds, which is everything the CDM needs to describe the
/// stream — and none of it requires opening a chunk. What is missing is every fact that lives in
/// the rows: the frames, their content hashes, and the statistics recomputed from their values.
///
/// The two disclosures [`build_stream`] makes about an array too short for its episode are made
/// here too, from the same shapes, because that defect is visible in the metadata alone.
fn declared_stream(
    entry: &ArrayEntry,
    name: &str,
    slice: &EpisodeSlice,
    declares_clock: bool,
    budget: &mut FrameBudget,
    notes: &mut Notes,
) -> Result<Option<Stream>, IngestError> {
    let array = &entry.array;
    let rows = array.shape[0];
    let (start, end) = (slice.start.min(rows), slice.end.min(rows));
    if end <= start {
        if slice.start != slice.end {
            notes.unmapped.push(UnmappedField {
                source_path: format!("{}[{}..{}]", entry.path, slice.start, slice.end),
                note: format!(
                    "this array holds {rows} row(s), so it has none in episode {}'s range; no \
                     stream is built from it for that episode",
                    slice.index
                ),
            });
        }
        return Ok(None);
    }
    if end < slice.end {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{}[{}..{}]", entry.path, slice.start, slice.end),
            note: format!(
                "this array holds only {rows} row(s), short of episode {}'s range; the stream \
                 covers what exists",
                slice.index
            ),
        });
    }
    // No frame is read, so the frame budget never fires on its own — and one `Stream` is built per
    // (episode x array), both of which come from a handful of bytes of JSON. Charge one unit per
    // stream against the same ceiling, so a store declaring a million episodes is refused here
    // rather than in the allocator.
    budget.take(FORMAT_ID, 1)?;

    let (clock_kind, clock_id) = if declares_clock {
        // The clock describes the source, not this ingest: the store records measured time and
        // says what its units are. The temporal checks abstain here for want of frames, which the
        // coverage note states, rather than for want of a clock.
        (ClockKind::Measured, CLOCK_TIME)
    } else {
        (ClockKind::StepIndex, CLOCK_STEP_INDEX)
    };

    Ok(Some(Stream {
        name: name.to_string(),
        modality: modality_for(leaf_of(name), array),
        // Zarr declares no sampling rate.
        declared_rate_hz: None,
        clock_id: clock_id.into(),
        clock_kind,
        dtype: Some(array.dtype.name.clone()),
        shape: (array.shape.len() > 1).then(|| array.shape[1..].to_vec()),
        dim_names: None,
        frames: Vec::new(),
        // Zarr stores no summary statistics, and every recomputed one comes from values this run
        // did not read.
        stats: None,
        dim_stats: None,
        observed_stats: None,
        observed_saturation: None,
        observed_non_finite: None,
        observed_dim_stats: None,
        media: None,
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
        frame_id: None,
    }))
}

/// Build one stream: one array's rows over one episode's range.
#[allow(clippy::too_many_arguments)]
fn build_stream(
    entry: &ArrayEntry,
    name: &str,
    slice: &EpisodeSlice,
    store_name: &str,
    timeline: Option<&Vec<i64>>,
    budget: &mut FrameBudget,
    decompression: &mut DecompressionBudget,
    notes: &mut Notes,
) -> Result<Option<Stream>, IngestError> {
    let array = &entry.array;
    // An array shorter than the episode's range does not describe it (a replay buffer's arrays are
    // all the same length; one that is not is disclosed rather than padded or truncated silently).
    let rows = array.shape[0];
    let (start, end) = (slice.start.min(rows), slice.end.min(rows));
    if end <= start {
        // A boundary pair that spans nothing is an *empty episode*, which the structural checks
        // report on their own; noting it once per array would bury that in noise. An array too short
        // to reach the episode at all is a different fact, and is still said.
        if slice.start != slice.end {
            notes.unmapped.push(UnmappedField {
                source_path: format!("{}[{}..{}]", entry.path, slice.start, slice.end),
                note: format!(
                    "this array holds {rows} row(s), so it has none in episode {}'s range; no \
                     stream is built from it for that episode",
                    slice.index
                ),
            });
        }
        return Ok(None);
    }
    if end < slice.end {
        notes.unmapped.push(UnmappedField {
            source_path: format!("{}[{}..{}]", entry.path, slice.start, slice.end),
            note: format!(
                "this array holds only {rows} row(s), short of episode {}'s range; the stream \
                 covers what exists",
                slice.index
            ),
        });
    }
    let count = end - start;
    budget.take(FORMAT_ID, count)?;

    // Measured time only where the timeline covers this array's rows.
    let stamps = timeline.filter(|stamps| stamps.len() as u64 >= end);
    let (clock_kind, clock_id) = match stamps {
        Some(_) => (ClockKind::Measured, CLOCK_TIME),
        None => (ClockKind::StepIndex, CLOCK_STEP_INDEX),
    };

    let elem = array.dtype.size as u64;
    let per_row = array.row_elements();
    // `float16` is numeric and fits in eight bytes, but this reader decodes no 2-byte float — so its
    // values are never examined, and reporting `Some(0)` non-finite for an array full of NaNs would
    // satisfy the check that exists to find them. Decodability is asked of the decoder, not guessed
    // from the width.
    let decodable =
        array.dtype.numeric && elem > 0 && array.dtype.decode(&vec![0u8; elem as usize]).is_some();
    let per_dimension = decodable && per_row > 0 && per_row <= MAX_STATS_DIMS;
    if decodable && !per_dimension {
        notes.wide_arrays.insert(entry.path.clone());
    }
    if array.dtype.numeric && !decodable {
        notes.unmapped.push(UnmappedField {
            source_path: entry.path.clone(),
            note: format!(
                "values of type {} are not decoded by this reader, so nothing is summarized from \
                 them — the statistical checks abstain on this stream rather than reporting it clean",
                array.dtype.name
            ),
        });
    }

    let uri = format!("{store_name}#{}", entry.path);
    let mut frames = Vec::with_capacity(count.min(1 << 20) as usize);
    let mut accum = FeatureAccum::default();
    let mut scalars: Vec<Option<f64>> = Vec::new();
    let mut reader = RowReader::new(array, decompression)?;
    let row_bytes = reader.row_bytes();
    for row in start..end {
        let bytes = reader.row(row)?;
        if per_dimension {
            scalars.clear();
            for chunk in bytes.chunks_exact(elem as usize) {
                scalars.push(array.dtype.decode(chunk));
            }
            accum.push_cell(&scalars);
        } else if decodable && array.dtype.float {
            for chunk in bytes.chunks_exact(elem as usize) {
                if array.dtype.decode(chunk).is_some_and(|v| !v.is_finite()) {
                    accum.count_non_finite();
                }
            }
        }
        frames.push(Frame {
            // Measured time as recorded; otherwise the step index *within the episode*, so every
            // episode starts at step 0 rather than at its offset into the replay buffer.
            ts: match stamps {
                Some(stamps) => stamps[row as usize],
                None => (row - start) as i64,
            },
            value_ref: ValueRef {
                uri: uri.clone(),
                // A chunked row has no single address in the store.
                byte_offset: None,
                byte_len: Some(row_bytes),
                content_hash: Some(Sha256::digest(&bytes).into()),
            },
        });
    }

    Ok(Some(Stream {
        name: name.to_string(),
        modality: modality_for(leaf_of(name), array),
        // Zarr declares no sample rate, and nothing is inferred from the timestamps.
        declared_rate_hz: None,
        clock_id: clock_id.to_string(),
        clock_kind,
        dtype: Some(array.dtype.name.clone()),
        shape: (array.shape.len() > 1).then(|| array.shape[1..].to_vec()),
        dim_names: None,
        frames,
        // Zarr stores no summary statistics of its own.
        stats: None,
        dim_stats: None,
        observed_stats: accum.stats(),
        observed_saturation: accum.saturation(),
        observed_non_finite: decodable.then(|| accum.non_finite()),
        observed_dim_stats: accum.dim_stats(),
        latched: None,
        declared_range: None,
        point_fields: None,
        observed_point_counts: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
        media: None,
        frame_id: None,
    }))
}

/// The modality an array implies — the same two facts the HDF5 adapter reads, and nothing else.
fn modality_for(leaf: &str, array: &ZarrArray) -> Modality {
    if matches!(leaf, "action" | "actions") {
        return Modality::Action;
    }
    let per_frame = &array.shape[1..];
    let is_image =
        array.dtype.name == "uint8" && per_frame.len() == 3 && matches!(per_frame[2], 1 | 3 | 4);
    if is_image {
        Modality::Video
    } else {
        Modality::ScalarState
    }
}

/// A `.zattrs` value as text: a string as itself, anything else as its compact JSON — which is what
/// the store actually said, rather than a rendering Veridex invented.
fn render_attr(value: &serde_json::Value) -> String {
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if text.len() <= MAX_ATTR_TEXT {
        return text;
    }
    let mut cut = MAX_ATTR_TEXT;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… (truncated)", &text[..cut])
}

/// The task a store's attributes record, if any.
fn task_of(attrs: &std::collections::BTreeMap<String, serde_json::Value>) -> Option<String> {
    attrs
        .iter()
        .find(|(name, _)| TASK_ATTRS.iter().any(|t| t.eq_ignore_ascii_case(name)))
        .and_then(|(_, value)| value.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}
