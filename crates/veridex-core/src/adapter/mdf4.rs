//! ASAM MDF 4.x (MF4) adapter: the dominant automotive measurement format, read into the CDM.
//!
//! An MF4 file is a linked graph of typed blocks: the identification block, then a header (`##HD`)
//! that chains data groups (`##DG`), each holding channel groups (`##CG`) whose channels (`##CN`)
//! describe fixed-offset fields inside every record of the group's data block (`##DT`). This adapter
//! walks that graph, takes each group's **time master** channel as the timeline, and emits one CDM
//! [`Stream`] per measured channel with a frame per record.
//!
//! Scope, stated honestly rather than guessed at (design D2 — never silently drop what could affect a
//! verdict). Decoded: byte-aligned integer and float channels (little- and big-endian) in an
//! uncompressed `##DT` block of a sorted data group, with identity or linear (`##CC` type 1)
//! conversion applied. Everything else — compressed (`##DZ`) or listed (`##DL`) data, unsorted groups
//! carrying record ids, non-byte-aligned or non-numeric channels, other conversion types — is
//! reported as an `unmapped` field and contributes no frames, so a reader always knows what the
//! verdict did and did not cover.
//!
//! Values are fingerprinted into `frame.value_ref.content_hash` (never stored), so the CDM content
//! hash is sensitive to actual measured content, exactly as in the other adapters.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};

use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};

const FORMAT_ID: &str = "mf4";
const CLOCK_ID: &str = "mf4-master";

/// MDF 4.x versions this adapter reads. The block layout is stable across the 4.x line; a 3.x file
/// (a different, non-block format) is rejected rather than mis-parsed.
const SUPPORTED_VERSIONS: &[&str] = &["4.00", "4.10", "4.11", "4.20"];

/// Adapter for an ASAM MDF 4.x (`.mf4`) measurement file.
pub struct Mdf4Adapter;

/// A block header: the 4-byte id (`##DG`), the total block length, and the number of links that
/// follow the header.
#[derive(Debug, Clone, Copy)]
struct BlockHeader {
    id: [u8; 4],
    length: u64,
    link_count: u64,
}

/// Bytes of a block's header at `at`, or `None` if the file is too short or the length is absurd.
fn block_header(bytes: &[u8], at: u64) -> Option<BlockHeader> {
    let at = usize::try_from(at).ok()?;
    let raw = bytes.get(at..at.checked_add(24)?)?;
    let mut id = [0u8; 4];
    id.copy_from_slice(&raw[0..4]);
    let length = u64::from_le_bytes(raw[8..16].try_into().ok()?);
    let link_count = u64::from_le_bytes(raw[16..24].try_into().ok()?);
    // A block must at least contain its own header and links, and must fit in the file.
    let links_len = link_count.checked_mul(8)?;
    if length < 24u64.checked_add(links_len)? {
        return None;
    }
    // Checked: `length` comes straight from the file, so a value near u64::MAX would wrap this
    // addition — a panic in debug, and in release a bogus header that passes validation and yields an
    // empty "valid" dataset instead of a parse error.
    match (at as u64).checked_add(length) {
        Some(end) if end <= bytes.len() as u64 => {}
        _ => return None,
    }
    Some(BlockHeader {
        id,
        length,
        link_count,
    })
}

/// The `n`-th link of the block at `at`, or `None` when the block has fewer links.
fn link(bytes: &[u8], at: u64, header: &BlockHeader, n: u64) -> Option<u64> {
    if n >= header.link_count {
        return None;
    }
    let off = usize::try_from(at).ok()? + 24 + usize::try_from(n).ok()? * 8;
    let raw = bytes.get(off..off + 8)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

/// A link that is set (MDF encodes "absent" as 0).
fn opt_link(bytes: &[u8], at: u64, header: &BlockHeader, n: u64) -> Option<u64> {
    link(bytes, at, header, n).filter(|&l| l != 0)
}

/// The block's data section — everything after the header and links.
fn data_section<'a>(bytes: &'a [u8], at: u64, header: &BlockHeader) -> Option<&'a [u8]> {
    let start = usize::try_from(at).ok()? + 24 + usize::try_from(header.link_count).ok()? * 8;
    let end = usize::try_from(at).ok()? + usize::try_from(header.length).ok()?;
    bytes.get(start..end)
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn le_f64(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The text of a `##TX` block, trimmed at its terminating NUL.
fn text_block(bytes: &[u8], at: u64) -> Option<String> {
    let header = block_header(bytes, at)?;
    if &header.id != b"##TX" {
        return None;
    }
    let data = data_section(bytes, at, &header)?;
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    Some(String::from_utf8_lossy(&data[..end]).trim().to_string())
}

/// A channel's value conversion. Only the two conversions that need no lookup table are applied; any
/// other type leaves the raw value untouched and is reported as unmapped.
#[derive(Debug, Clone, Copy)]
enum Conversion {
    /// Physical value is the raw value.
    Identity,
    /// `phys = p1 + p2 * raw` (`##CC` type 1).
    Linear { p1: f64, p2: f64 },
}

impl Conversion {
    fn apply(self, raw: f64) -> f64 {
        match self {
            Conversion::Identity => raw,
            Conversion::Linear { p1, p2 } => p1 + p2 * raw,
        }
    }
}

/// Read a `##CC` conversion block. Returns the conversion and, when the type is one this adapter
/// does not apply, that type number so the caller can report it.
fn conversion_at(bytes: &[u8], at: u64) -> (Conversion, Option<u8>) {
    let Some(header) = block_header(bytes, at) else {
        return (Conversion::Identity, None);
    };
    if &header.id != b"##CC" {
        return (Conversion::Identity, None);
    }
    let Some(data) = data_section(bytes, at, &header) else {
        return (Conversion::Identity, None);
    };
    let cc_type = data.first().copied().unwrap_or(0);
    match cc_type {
        // 0 = 1:1 (no conversion).
        0 => (Conversion::Identity, None),
        // 1 = linear: the first two conversion parameters are the offset and factor.
        1 => {
            // Data layout: type u8, precision u8, flags u16, ref_count u16, val_count u16,
            // phy_range_min f64, phy_range_max f64, then val_count f64 parameters.
            let params_at = 1 + 1 + 2 + 2 + 2 + 8 + 8;
            match (le_f64(data, params_at), le_f64(data, params_at + 8)) {
                (Some(p1), Some(p2)) => (Conversion::Linear { p1, p2 }, None),
                _ => (Conversion::Identity, Some(1)),
            }
        }
        other => (Conversion::Identity, Some(other)),
    }
}

/// One channel, resolved from its `##CN` block.
#[derive(Debug, Clone)]
struct Channel {
    name: String,
    /// `2` = time/other master, `3` = virtual master, `0` = a measured value.
    channel_type: u8,
    sync_type: u8,
    data_type: u8,
    bit_offset: u8,
    byte_offset: u32,
    bit_count: u32,
    /// `cn_flags`, read so invalidation-bit declarations can be honored rather than ignored.
    flags: u32,
    conversion: Conversion,
    /// A conversion type present in the file but not applied.
    unapplied_conversion: Option<u8>,
}

impl Channel {
    /// Whether this channel is the group's **time** master — the timeline every other channel in the
    /// group is sampled against.
    fn is_time_master(&self) -> bool {
        (self.channel_type == 2 || self.channel_type == 3) && self.sync_type == 1
    }

    /// Whether the channel declares that some or all of its samples may be invalid — `cn_flags` bit 0
    /// ("all values invalid") or bit 1 ("invalidation bit valid").
    fn declares_invalidation(&self) -> bool {
        self.flags & 0b11 != 0
    }

    /// Whether this adapter can decode the channel's raw value: a byte-aligned, whole-byte integer or
    /// float. Anything else is reported rather than guessed at.
    fn is_decodable(&self) -> bool {
        if self.bit_offset != 0 {
            return false;
        }
        match self.data_type {
            // Integers, either byte order, in whole bytes.
            0..=3 => matches!(self.bit_count, 8 | 16 | 32 | 64),
            // IEEE 754 floats are only 32- or 64-bit.
            4 | 5 => matches!(self.bit_count, 32 | 64),
            // Strings, byte arrays, MIME and CANopen types are not measurements.
            _ => false,
        }
    }

    /// Decode this channel's physical value out of one record.
    fn value(&self, record: &[u8]) -> Option<f64> {
        let start = usize::try_from(self.byte_offset).ok()?;
        let len = usize::try_from(self.bit_count).ok()? / 8;
        let raw = record.get(start..start.checked_add(len)?)?;
        let value = match (self.data_type, len) {
            // Unsigned, little- then big-endian.
            (0, _) => uint_le(raw) as f64,
            (1, _) => uint_be(raw) as f64,
            // Signed, sign-extended from the field width.
            (2, _) => sign_extend(uint_le(raw), len * 8) as f64,
            (3, _) => sign_extend(uint_be(raw), len * 8) as f64,
            // IEEE 754 floats.
            (4, 4) => f32::from_le_bytes(raw.try_into().ok()?) as f64,
            (4, 8) => f64::from_le_bytes(raw.try_into().ok()?),
            (5, 4) => f32::from_be_bytes(raw.try_into().ok()?) as f64,
            (5, 8) => f64::from_be_bytes(raw.try_into().ok()?),
            _ => return None,
        };
        Some(self.conversion.apply(value))
    }
}

fn uint_le(raw: &[u8]) -> u64 {
    raw.iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

fn uint_be(raw: &[u8]) -> u64 {
    raw.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b))
}

/// Sign-extend a `bits`-wide two's-complement value held in a `u64`.
fn sign_extend(value: u64, bits: usize) -> i64 {
    if bits == 0 || bits >= 64 {
        return value as i64;
    }
    let shift = 64 - bits;
    ((value << shift) as i64) >> shift
}

/// Read a `##CN` block into a [`Channel`].
fn channel_at(bytes: &[u8], at: u64) -> Option<Channel> {
    let header = block_header(bytes, at)?;
    if &header.id != b"##CN" {
        return None;
    }
    let data = data_section(bytes, at, &header)?;
    // Link 2 is the channel's name (a ##TX block); link 4 is its conversion (##CC).
    let name = opt_link(bytes, at, &header, 2)
        .and_then(|l| text_block(bytes, l))
        .filter(|n| !n.is_empty())?;
    let (conversion, unapplied_conversion) = match opt_link(bytes, at, &header, 4) {
        Some(l) => conversion_at(bytes, l),
        None => (Conversion::Identity, None),
    };
    Some(Channel {
        name,
        channel_type: *data.first()?,
        sync_type: *data.get(1)?,
        data_type: *data.get(2)?,
        bit_offset: *data.get(3)?,
        byte_offset: le_u32(data, 4)?,
        bit_count: le_u32(data, 8)?,
        flags: le_u32(data, 12).unwrap_or(0),
        conversion,
        unapplied_conversion,
    })
}

/// The seconds-to-nanoseconds conversion for a master channel, saturating rather than wrapping on an
/// absurd value so a corrupt file cannot produce a nonsense timeline.
fn seconds_to_ns(seconds: f64) -> Option<i64> {
    if !seconds.is_finite() {
        return None;
    }
    let ns = seconds * 1e9;
    if ns > i64::MAX as f64 {
        return Some(i64::MAX);
    }
    if ns < i64::MIN as f64 {
        return Some(i64::MIN);
    }
    Some(ns as i64)
}

/// What a single channel group contributed.
struct GroupResult {
    streams: Vec<Stream>,
    unmapped: Vec<UnmappedField>,
    /// Records this reader did not read — a compressed data block, an unsorted group, a channel
    /// group behind a malformed link.
    ///
    /// Separate from `unmapped` because they mean different things to a reader of the verdict:
    /// "unmapped" is a field the CDM has no shape for, which costs nothing; this is measurement
    /// data that is *there* and went unread, so every result is over less of the file than it
    /// appears to be. Only this one raises `COVERAGE.SOURCE_UNREAD`.
    unread: Vec<UnmappedField>,
}

impl Adapter for Mdf4Adapter {
    fn format_id(&self) -> &'static str {
        FORMAT_ID
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        SUPPORTED_VERSIONS
    }

    fn detect(&self, source: &Source) -> Detection {
        let Source::Local(path) = source else {
            return Detection::No;
        };
        let is_mf4_name = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mf4") || e.eq_ignore_ascii_case("mdf"));
        if !is_mf4_name || !path.is_file() {
            return Detection::No;
        }
        // Confirm with the file's own identification block rather than trusting the extension.
        match read_id_block(path) {
            Some(version) => Detection::Yes {
                version: Some(version),
            },
            None => Detection::No,
        }
    }

    /// An MF4 file is one recording, ingested as one episode, so there is no episode axis to sample.
    fn supports_sampling(&self) -> bool {
        false
    }

    /// An MF4 file states its structure in block headers — `##DG` → `##CG` → `##CN` give every
    /// channel's name and how many cycles its raster declares — separately from the `##DT`/`##DZ`
    /// blocks that hold the samples. Reading only the headers describes the measurement, and it is
    /// the one way to describe a *compressed* measurement at all: a full read declines `##DZ`.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        // An MF4 measurement becomes one episode, so there is nothing to sample along.
        let Source::Local(path) = source else {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: "remote MF4 ingestion is not supported".into(),
            });
        };
        let bytes = std::fs::read(path).map_err(|e| IngestError::Io(e.to_string()))?;

        let version = id_version(&bytes).ok_or_else(|| IngestError::Parse {
            format_id: FORMAT_ID,
            message: "not an MDF file (missing `MDF` identification block)".into(),
        })?;
        if !version.starts_with('4') {
            return Err(IngestError::UnsupportedVersion {
                format_id: FORMAT_ID,
                version: Some(version),
                supported: SUPPORTED_VERSIONS,
            });
        }

        // The header block always sits immediately after the 64-byte identification block.
        let hd_at = 64u64;
        let hd = block_header(&bytes, hd_at).ok_or_else(|| IngestError::Parse {
            format_id: FORMAT_ID,
            message: "truncated or malformed header (##HD) block".into(),
        })?;
        if &hd.id != b"##HD" {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: "expected a ##HD block at offset 64".into(),
            });
        }
        let hd_data = data_section(&bytes, hd_at, &hd);
        let start_time_ns = hd_data.and_then(|d| le_u64(d, 0));

        // Walk the data groups, collecting streams and everything we could not decode.
        let mut streams: Vec<Stream> = Vec::new();
        let mut unmapped: Vec<UnmappedField> = Vec::new();
        // Measurement data this reader did not read, kept apart from the fields the CDM cannot
        // hold: only this list raises `COVERAGE.SOURCE_UNREAD`, and a file whose every data block
        // is compressed used to come back with no frames and nothing in the verdict saying why.
        let mut unread: Vec<UnmappedField> = Vec::new();
        let mut names_used: BTreeSet<String> = BTreeSet::new();
        // Next disambiguation suffix to try per colliding base name, so each collision is one probe.
        let mut next_suffix: std::collections::BTreeMap<String, u64> =
            std::collections::BTreeMap::new();
        let mut group_index = 0u64;
        let mut dg_at = opt_link(&bytes, hd_at, &hd, 0);
        // A malformed file must not spin *or* explode. One visited set spans every chain — data
        // groups, channel groups, and channels alike — because links may legally point at shared
        // blocks: with a set per parent chain, n data groups each re-walking the same n channel
        // groups each re-walking the same n channels is O(n³) streams, and a 33 KB file measured
        // 1.35 GB of allocation. Visiting each block once makes the whole walk linear in file size.
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        let mut budget = super::FrameBudget::new(options);
        while let Some(at) = dg_at {
            if !seen.insert(at) {
                unmapped.push(UnmappedField {
                    source_path: format!("##DG @{at}"),
                    note: "data-group chain loops back on itself; stopped walking".into(),
                });
                break;
            }
            let Some(dg) = block_header(&bytes, at) else {
                unmapped.push(UnmappedField {
                    source_path: format!("##DG @{at}"),
                    note: "truncated or malformed data-group block; skipped".into(),
                });
                break;
            };
            if &dg.id != b"##DG" {
                unmapped.push(UnmappedField {
                    source_path: format!("##DG @{at}"),
                    note: format!(
                        "expected a ##DG block, found `{}`; stopped walking",
                        String::from_utf8_lossy(&dg.id)
                    ),
                });
                break;
            }
            let result = ingest_data_group(
                &bytes,
                at,
                &dg,
                group_index,
                options.metadata_only,
                &mut seen,
                &mut budget,
            )?;
            for mut stream in result.streams {
                // Channel names are only unique within a group, and not always even there;
                // disambiguate until the name is genuinely free so no stream is dropped — or worse,
                // silently duplicated — by a collision.
                if !names_used.insert(stream.name.clone()) {
                    // Resume from the last suffix tried for this base rather than restarting at 0.
                    // Restarting made the k-th collision do k probes, so a file whose channels all
                    // share one name cost O(n²): 16,000 such channels in 1.3 MB measured 18 seconds,
                    // and 100 MB extrapolated to hours of CPU inside a CI gate.
                    let base = stream.name.clone();
                    let n = next_suffix.entry(base.clone()).or_insert(0u64);
                    loop {
                        let candidate = format!("{base}#{group_index}.{n}");
                        *n += 1;
                        if names_used.insert(candidate.clone()) {
                            stream.name = candidate;
                            break;
                        }
                    }
                }
                streams.push(stream);
            }
            unmapped.extend(result.unmapped);
            unread.extend(result.unread);
            group_index += 1;
            dg_at = opt_link(&bytes, at, &dg, 0);
        }

        let (min_ts, max_ts) = streams.iter().flat_map(|s| &s.frames).map(|f| f.ts).fold(
            (None, None),
            |(lo, hi): (Option<i64>, Option<i64>), ts| {
                (
                    Some(lo.map_or(ts, |l: i64| l.min(ts))),
                    Some(hi.map_or(ts, |h: i64| h.max(ts))),
                )
            },
        );

        // Provenance: the writing program, recorded in the identification block, and the measurement
        // start time from the header. Both are read from the source bytes, so both are `known`.
        let mut metadata = vec![("source_format".into(), FORMAT_ID.into())];
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some(FORMAT_ID.into()),
            class: ProvenanceClass::Known,
        }];
        if let Some(program) = id_program(&bytes) {
            metadata.push(("mdf_program".into(), program.clone()));
            elements.push(ProvenanceElement {
                key: "recorder".into(),
                value: Some(program),
                class: ProvenanceClass::Known,
            });
        }
        metadata.push(("mdf_version".into(), version.clone()));
        if let Some(start) = start_time_ns {
            metadata.push(("mdf_start_time_ns".into(), start.to_string()));
        }

        let dataset = Dataset {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(FORMAT_ID)
                .to_string(),
            metadata,
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements,
            }],
            episodes: vec![Episode {
                index: 0,
                start_ts: min_ts,
                end_ts: max_ts,
                streams,
                task: None,
                labels: vec![],
                ego_poses: None,
                declared_frame_count: None,
            }],
            calibration: None,
        };

        Ok(Ingested {
            dataset,
            report: IngestReport {
                unread_sources: unread,
                format_id: FORMAT_ID,
                source_version: Some(version),
                coverage: if options.metadata_only {
                    Coverage::MetadataOnly {
                        episodes_declared: 1,
                    }
                } else {
                    Coverage::Full
                },
                mapped_fields: if options.metadata_only {
                    vec![
                        "##CN channel -> stream (name from its ##TX)".into(),
                        "identification block program -> provenance.recorder".into(),
                    ]
                } else {
                    vec![
                        "##CN channel -> stream (name from its ##TX)".into(),
                        "time master channel -> frame.ts".into(),
                        "##CC linear/identity conversion -> physical value".into(),
                        "physical value -> frame.value_ref.content_hash (SHA-256)".into(),
                        "identification block program -> provenance.recorder".into(),
                    ]
                },
                unmapped_fields: unmapped,
                omitted_fields: {
                    let mut o = vec![
                        "episode-segmentation (MF4 records one continuous measurement)".into(),
                        "declared-rate (MF4 channels declare no nominal sample rate)".into(),
                    ];
                    if options.metadata_only {
                        o.push(
                            "sample values, timestamps and content hashes (no ##DT or ##DZ data \
                             block was opened; only the ##HD/##DG/##CG/##CN header tree was read)"
                                .into(),
                        );
                        o.push(
                            "the physical/raw distinction (a ##CC conversion is applied to values, \
                             and no value was read)"
                                .into(),
                        );
                    }
                    o
                },
            },
        })
    }
}

/// Ingest one `##DG`: resolve its channel groups, decode records, and emit a stream per channel.
#[allow(clippy::too_many_arguments)]
fn ingest_data_group(
    bytes: &[u8],
    dg_at: u64,
    dg: &BlockHeader,
    group_index: u64,
    metadata_only: bool,
    seen: &mut BTreeSet<u64>,
    budget: &mut super::FrameBudget,
) -> Result<GroupResult, IngestError> {
    let mut out = GroupResult {
        streams: Vec::new(),
        unmapped: Vec::new(),
        unread: Vec::new(),
    };
    let locator = |what: &str| format!("##DG[{group_index}].{what}");

    let rec_id_size = data_section(bytes, dg_at, dg)
        .and_then(|d| d.first().copied())
        .unwrap_or(0);
    if rec_id_size != 0 && !metadata_only {
        // An unsorted group interleaves records from several channel groups behind record ids.
        out.unread.push(UnmappedField {
            source_path: locator("records"),
            note: format!(
                "unsorted data group (record id size {rec_id_size}) is not decoded; its channels contribute no frames"
            ),
        });
        return Ok(out);
    }

    // The group's data block. Only an uncompressed ##DT is read; compressed/listed data is reported.
    // A metadata-only run reads no record at all, so it never resolves the block — which is what
    // lets it describe a measurement whose data is compressed, the shape a real logger writes.
    let data_link = opt_link(bytes, dg_at, dg, 2);
    let records: &[u8] = if metadata_only {
        &[]
    } else {
        match data_link.and_then(|at| block_header(bytes, at).map(|h| (at, h))) {
            Some((at, h)) if &h.id == b"##DT" => data_section(bytes, at, &h).unwrap_or(&[]),
            Some((_, h)) => {
                out.unread.push(UnmappedField {
                source_path: locator("data"),
                note: format!(
                    "`{}` data block is not decoded (only uncompressed ##DT is); its channels contribute no frames",
                    String::from_utf8_lossy(&h.id)
                ),
            });
                return Ok(out);
            }
            None => {
                out.unmapped.push(UnmappedField {
                    source_path: locator("data"),
                    note: "data group carries no readable data block".into(),
                });
                return Ok(out);
            }
        }
    };

    let mut cg_at = opt_link(bytes, dg_at, dg, 1);
    let mut cg_index = 0u64;
    while let Some(at) = cg_at {
        if !seen.insert(at) {
            break;
        }
        // A sorted data group holds exactly one channel group. Decoding a second against the same
        // records from offset 0 would produce plausible-but-wrong values, so report and stop.
        if cg_index > 0 {
            out.unread.push(UnmappedField {
                source_path: locator("channel-groups"),
                note:
                    "sorted data group holds more than one channel group; only the first is decoded"
                        .into(),
            });
            break;
        }
        // This module's own doc promises that everything not decoded "is reported as an `unmapped`
        // field ... so a reader always knows what the verdict did and did not cover". These two
        // arms broke that promise the loudest way possible: a `##DG` whose `cg_first` link points
        // at a malformed or non-`##CG` block lost its *entire* data group, and the run came back
        // with no streams, an empty `unmapped`, and `Coverage::Full`.
        let Some(cg) = block_header(bytes, at) else {
            out.unread.push(UnmappedField {
                source_path: format!("##DG[{group_index}].##CG @{at}"),
                note: "truncated or malformed channel-group block; this data group's channels \
                       contribute no frames"
                    .into(),
            });
            break;
        };
        if &cg.id != b"##CG" {
            out.unread.push(UnmappedField {
                source_path: format!("##DG[{group_index}].##CG @{at}"),
                note: format!(
                    "expected a ##CG block, found `{}`; this data group's channels contribute no \
                     frames",
                    String::from_utf8_lossy(&cg.id)
                ),
            });
            break;
        }
        let cg_data = data_section(bytes, at, &cg);
        let cycle_count = cg_data.and_then(|d| le_u64(d, 8)).unwrap_or(0);
        let data_bytes = cg_data.and_then(|d| le_u32(d, 24)).unwrap_or(0) as usize;
        let inval_bytes = cg_data.and_then(|d| le_u32(d, 28)).unwrap_or(0) as usize;
        let record_len = data_bytes + inval_bytes;

        // Collect the group's channels.
        let mut channels: Vec<Channel> = Vec::new();
        let mut cn_at = opt_link(bytes, at, &cg, 1);
        while let Some(cn) = cn_at {
            if !seen.insert(cn) {
                break;
            }
            let Some(header) = block_header(bytes, cn) else {
                break;
            };
            match channel_at(bytes, cn) {
                Some(channel) => channels.push(channel),
                // A channel whose `##TX` name block is missing, empty, or unreadable, or whose
                // header is short, was dropped in silence -- so a real, decodable measurement
                // vanished from a run that still reported `Coverage::Full`.
                None => out.unmapped.push(UnmappedField {
                    source_path: format!("##DG[{group_index}].##CG[{cg_index}].##CN @{cn}"),
                    note: "channel has no readable name or a malformed header; it contributes no \
                           stream"
                        .into(),
                }),
            }
            cn_at = opt_link(bytes, cn, &header, 0);
        }

        let locate = |name: &str| format!("##DG[{group_index}].##CG[{cg_index}].{name}");
        if metadata_only {
            declare_channel_group(cycle_count, &channels, group_index, budget, &mut out)?;
            cg_index += 1;
            cg_at = opt_link(bytes, at, &cg, 0);
            continue;
        }
        // A group materializes channels × records frames; charge that before decoding.
        let decodable = channels.iter().filter(|c| c.is_decodable()).count() as u64;
        // A zero-length record divides into no frames at all (and must not divide by zero).
        let available = records.len().checked_div(record_len).unwrap_or(0) as u64;
        let planned = if cycle_count == 0 {
            available
        } else {
            available.min(cycle_count)
        };
        budget.take(FORMAT_ID, decodable.saturating_mul(planned))?;
        // Whatever this group could not contribute is recorded as an unmapped field inside.
        decode_channel_group(
            records,
            record_len,
            cycle_count,
            inval_bytes,
            &channels,
            group_index,
            &locate,
            &mut out,
        );

        cg_index += 1;
        cg_at = opt_link(bytes, at, &cg, 0);
    }
    Ok(out)
}

/// Describe one channel group's channels as streams with no frames, from its block headers alone.
///
/// The `--metadata-only` counterpart to [`decode_channel_group`]: a `##CG` states how many cycles it
/// holds and each `##CN` states its name, so the recording's shape — which signals, on which raster,
/// how many samples each declares — is readable without touching a data block. That is what lets
/// this mode describe a measurement whose data is compressed, which is how loggers write them and
/// which a full read declines to decode.
fn declare_channel_group(
    cycle_count: u64,
    channels: &[Channel],
    group_index: u64,
    budget: &mut super::FrameBudget,
    out: &mut GroupResult,
) -> Result<(), IngestError> {
    // No frame is read, so the frame budget never fires on its own, and one `Stream` per channel is
    // built from a count in a block header. Charge one unit each, before any is built.
    budget.take(FORMAT_ID, channels.len() as u64)?;
    for channel in channels {
        if channel.is_time_master() {
            continue;
        }
        out.streams.push(Stream {
            name: channel.name.clone(),
            modality: Modality::CanSignal,
            declared_rate_hz: None,
            clock_id: format!("{CLOCK_ID}#{group_index}"),
            // The clock describes the measurement — every MF4 raster has a recorded time master —
            // not this ingest. The temporal checks abstain here for want of frames, which the
            // coverage note states, rather than for want of a clock.
            clock_kind: ClockKind::Measured,
            dtype: Some("float64".into()),
            shape: None,
            frames: Vec::new(),
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            latched: None,
            point_fields: None,
            media: None,
            frame_id: None,
        });
    }
    let _ = cycle_count;
    Ok(())
}

/// Decode one channel group's records into streams, appending to `out`. Returns whether any stream
/// was produced.
#[allow(clippy::too_many_arguments)]
fn decode_channel_group(
    records: &[u8],
    record_len: usize,
    cycle_count: u64,
    inval_bytes: usize,
    channels: &[Channel],
    group_index: u64,
    locate: &dyn Fn(&str) -> String,
    out: &mut GroupResult,
) -> bool {
    if record_len == 0 {
        out.unmapped.push(UnmappedField {
            source_path: locate("records"),
            note: "channel group declares a zero-length record; nothing decoded".into(),
        });
        return false;
    }
    let Some(master) = channels.iter().find(|c| c.is_time_master()) else {
        out.unread.push(UnmappedField {
            source_path: locate("channels"),
            note: "channel group has no time master channel, so its samples carry no timestamps; nothing decoded".into(),
        });
        return false;
    };
    if master.channel_type == 3 {
        // A virtual master's values are implied by the record index rather than stored; without the
        // sampling interval from its conversion this adapter cannot place samples in time honestly.
        out.unread.push(UnmappedField {
            source_path: locate("master"),
            note: "virtual master channel (implied timestamps) is not decoded; nothing decoded"
                .into(),
        });
        return false;
    }
    if !master.is_decodable() {
        out.unread.push(UnmappedField {
            source_path: locate(&master.name),
            note: format!(
                "time master is data type {} at bit offset {} × {} bits, which is not decoded; nothing decoded",
                master.data_type, master.bit_offset, master.bit_count
            ),
        });
        return false;
    }
    // An unapplied conversion on a signal costs that signal's values; on the master it silently
    // corrupts *every* stream's timestamps in the group, so it must stop the group, not be noted.
    if let Some(cc) = master.unapplied_conversion {
        out.unread.push(UnmappedField {
            source_path: locate(&master.name),
            note: format!(
                "time master carries ##CC conversion type {cc}, which is not applied, so its timestamps would be wrong; nothing decoded"
            ),
        });
        return false;
    }

    // Records actually present, never more than the file holds even if the group over-declares.
    let available = records.len() / record_len;
    let count = if cycle_count == 0 {
        available
    } else {
        available.min(cycle_count as usize)
    };
    if cycle_count as usize > available {
        out.unread.push(UnmappedField {
            source_path: locate("records"),
            note: format!(
                "channel group declares {cycle_count} cycles but the data block holds {available}; read {count}"
            ),
        });
    }

    // Timestamps first: a record whose master value cannot be read contributes no sample anywhere.
    let mut timestamps: Vec<Option<i64>> = Vec::with_capacity(count);
    for i in 0..count {
        let record = &records[i * record_len..(i + 1) * record_len];
        timestamps.push(master.value(record).and_then(seconds_to_ns));
    }

    let mut produced = false;
    let mut reported_conversions: BTreeSet<u8> = BTreeSet::new();
    for channel in channels {
        if channel.is_time_master() {
            continue;
        }
        // MDF marks a sample invalid with a per-record invalidation bit. Veridex does not evaluate
        // those bits, so a channel that declares them would present invalid samples as real values.
        if inval_bytes != 0 && channel.declares_invalidation() {
            out.unread.push(UnmappedField {
                source_path: locate(&channel.name),
                note: format!(
                    "channel declares per-sample invalidation (cn_flags 0x{:x}), which is not evaluated; its samples are not decoded",
                    channel.flags
                ),
            });
            continue;
        }
        if !channel.is_decodable() {
            out.unread.push(UnmappedField {
                source_path: locate(&channel.name),
                note: format!(
                    "channel is data type {} at bit offset {} × {} bits, which is not decoded",
                    channel.data_type, channel.bit_offset, channel.bit_count
                ),
            });
            continue;
        }
        if let Some(cc) = channel.unapplied_conversion {
            if reported_conversions.insert(cc) {
                out.unmapped.push(UnmappedField {
                    source_path: locate(&channel.name),
                    note: format!(
                        "##CC conversion type {cc} is not applied; the raw value is recorded instead"
                    ),
                });
            }
        }
        let mut frames: Vec<Frame> = Vec::with_capacity(count);
        for (i, ts) in timestamps.iter().enumerate() {
            let (Some(ts), Some(value)) = (
                *ts,
                channel.value(&records[i * record_len..(i + 1) * record_len]),
            ) else {
                continue;
            };
            frames.push(Frame {
                ts,
                value_ref: ValueRef {
                    uri: format!("mf4:{}", channel.name),
                    byte_offset: None,
                    byte_len: None,
                    // Fingerprint the physical value, matching the other adapters: the CDM hash is
                    // sensitive to measured content without ever storing it.
                    content_hash: Some(
                        Sha256::digest(crate::canonical::canon_f64_bits(value).to_le_bytes())
                            .into(),
                    ),
                },
            });
        }
        if frames.is_empty() {
            continue;
        }
        produced = true;
        out.streams.push(Stream {
            name: channel.name.clone(),
            // MF4 is the automotive measurement format: its channels are bus/sensor signals, the same
            // physical thing the CAN+DBC adapter emits, so they share a modality.
            modality: Modality::CanSignal,
            declared_rate_hz: None,
            // Each channel group is its own raster with its own master; they are not a shared clock,
            // so cross-stream timing checks must not compare one raster's span against another's.
            clock_id: format!("{CLOCK_ID}#{group_index}"),
            // Real recorded timestamps: every temporal check applies.
            clock_kind: ClockKind::Measured,
            dtype: Some("float64".into()),
            shape: None,
            frames,
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            latched: None,
            point_fields: None,
            // MF4 channels declare no coordinate frame.
            media: None,
            frame_id: None,
        });
    }
    produced
}

/// The MDF version text from a file's identification block (e.g. `4.10`), or `None` if the file does
/// not start with an MDF identification block.
fn id_version(bytes: &[u8]) -> Option<String> {
    let id = bytes.get(0..8)?;
    if &id[0..3] != b"MDF" {
        return None;
    }
    let text = bytes.get(8..16)?;
    let version = String::from_utf8_lossy(text)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// The writing program recorded in the identification block (`id_prog`, 8 bytes at offset 16).
fn id_program(bytes: &[u8]) -> Option<String> {
    let raw = bytes.get(16..24)?;
    let text = String::from_utf8_lossy(raw)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Read just enough of a file to answer detection: its MDF version text.
fn read_id_block(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(64).read_to_end(&mut head).ok()?;
    id_version(&head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_extension_covers_the_field_width() {
        assert_eq!(sign_extend(0xFF, 8), -1);
        assert_eq!(sign_extend(0x7F, 8), 127);
        assert_eq!(sign_extend(0xFFFF, 16), -1);
        assert_eq!(sign_extend(1, 1), -1);
        // A full-width value is already an i64.
        assert_eq!(sign_extend(u64::MAX, 64), -1);
    }

    #[test]
    fn integers_decode_in_both_byte_orders() {
        assert_eq!(uint_le(&[0x01, 0x02]), 0x0201);
        assert_eq!(uint_be(&[0x01, 0x02]), 0x0102);
        assert_eq!(uint_le(&[]), 0);
    }

    #[test]
    fn a_linear_conversion_is_applied_and_identity_leaves_the_value_alone() {
        let linear = Conversion::Linear { p1: 10.0, p2: 0.5 };
        assert_eq!(linear.apply(4.0), 12.0);
        assert_eq!(Conversion::Identity.apply(4.0), 4.0);
    }

    #[test]
    fn a_non_finite_or_absurd_master_value_saturates_rather_than_wrapping() {
        assert_eq!(seconds_to_ns(f64::NAN), None);
        // An infinite master value is not a timestamp; the sample is dropped, not pinned to a bound.
        assert_eq!(seconds_to_ns(f64::INFINITY), None);
        assert_eq!(seconds_to_ns(f64::NEG_INFINITY), None);
        assert_eq!(seconds_to_ns(1.5), Some(1_500_000_000));
        assert_eq!(seconds_to_ns(1e30), Some(i64::MAX));
    }

    #[test]
    fn a_block_header_that_does_not_fit_the_file_is_rejected() {
        // Claims a 4 GiB block in a 24-byte file.
        let mut bytes = vec![0u8; 24];
        bytes[0..4].copy_from_slice(b"##DG");
        bytes[8..16].copy_from_slice(&(1u64 << 32).to_le_bytes());
        assert!(block_header(&bytes, 0).is_none());
        // Claims more links than the block length can hold.
        let mut bytes = vec![0u8; 24];
        bytes[0..4].copy_from_slice(b"##DG");
        bytes[8..16].copy_from_slice(&24u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&4u64.to_le_bytes());
        assert!(block_header(&bytes, 0).is_none());
    }

    #[test]
    fn only_byte_aligned_numeric_channels_are_decodable() {
        let channel = |data_type: u8, bit_offset: u8, bit_count: u32| Channel {
            name: "c".into(),
            channel_type: 0,
            sync_type: 0,
            data_type,
            bit_offset,
            byte_offset: 0,
            bit_count,
            flags: 0,
            conversion: Conversion::Identity,
            unapplied_conversion: None,
        };
        assert!(channel(0, 0, 16).is_decodable());
        assert!(channel(4, 0, 32).is_decodable());
        assert!(channel(5, 0, 64).is_decodable());
        // Bit-packed, odd-width, non-numeric, and a float that isn't 32/64 bits are all declined.
        assert!(!channel(0, 3, 16).is_decodable());
        assert!(!channel(0, 0, 12).is_decodable());
        assert!(!channel(6, 0, 32).is_decodable());
        assert!(!channel(4, 0, 16).is_decodable());
    }

    #[test]
    fn an_identification_block_yields_its_version_and_program() {
        let mut bytes = vec![0u8; 64];
        bytes[0..8].copy_from_slice(b"MDF     ");
        bytes[8..16].copy_from_slice(b"4.10    ");
        bytes[16..24].copy_from_slice(b"veridex ");
        assert_eq!(id_version(&bytes).as_deref(), Some("4.10"));
        assert_eq!(id_program(&bytes).as_deref(), Some("veridex"));
        // Anything that isn't an MDF file has no version.
        assert_eq!(id_version(b"NOTMDF__4.10____"), None);
        assert_eq!(id_version(&[]), None);
    }
}
