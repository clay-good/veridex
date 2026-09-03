//! ASAM MDF 4.x (MF4) adapter: the dominant automotive measurement format, read into the CDM.
//!
//! An MF4 file is a linked graph of typed blocks: the identification block, then a header (`##HD`)
//! that chains data groups (`##DG`), each holding channel groups (`##CG`) whose channels (`##CN`)
//! describe fixed-offset fields inside every record of the group's data block. This adapter
//! walks that graph, takes each group's **time master** channel as the timeline, and emits one CDM
//! [`Stream`] per measured channel with a frame per record.
//!
//! A group's data reaches the file in one of four shapes, and all four are read: an uncompressed
//! `##DT`, a deflated `##DZ` (plain or byte-column transposed), a `##DL` data list splitting the
//! records across several of those, and an `##HL` header list wrapping such a list. Real loggers
//! write the compressed and listed shapes, not the bare `##DT`, so reading only `##DT` meant reading
//! nothing off the files the format is actually used for.
//!
//! A data group is read whether it is **sorted** — one channel group whose records fill the block —
//! or **unsorted**, where several rasters' records are interleaved behind their `cg_record_id`s, the
//! way a bus logger writes them as the samples arrive. An unsorted group is demultiplexed into one
//! contiguous stream per channel group first, each at that group's own record length.
//!
//! Scope, stated honestly rather than guessed at (design D2 — never silently drop what could affect a
//! verdict). Decoded: little-endian integer channels at **any bit offset and any width up to 64
//! bits** — which is how an automotive measurement stores bus signals — plus big-endian integers and
//! IEEE floats in whole bytes on a byte boundary, with every numeric `##CC` conversion applied:
//! identity, linear, rational, the two value-to-value look-up tables, and value-range-to-value. A
//! bit-packed big-endian field is declined rather than guessed at, because MDF's bit numbering for a
//! straddling Motorola field is not the DBC sawtooth and a wrong reading there is a plausible number,
//! not a failure.
//!
//! Everything else contributes no frames — or no *physical* value — and is reported, so a reader
//! always knows what the verdict did and did not cover, split two ways because the two mean different
//! things. **Unread** (a `COVERAGE.SOURCE_UNREAD` warning in the verdict): a `##DZ` holding something
//! other than a `DT` record stream or using an undefined zip type, a data list whose elements do not
//! all resolve, an unsorted record tagged with an id no channel group claims, a variable-length
//! signal-data group, a group with no usable time master, a channel declaring per-sample
//! invalidation, a group declaring more cycles than its block holds, a bit-packed big-endian field, a
//! channel that runs past the end of its record, and a numeric `##CC` conversion left unevaluated —
//! the physical value is defined in the file as a rule, and the raw count stood in for it. All of it
//! is in the file and nobody read it, so every result is over less of the measurement than it appears
//! to be. **Unmapped** (a note about shape, costing the reader nothing): non-numeric channels, and
//! the four text-valued conversions, whose physical value is a string a numeric stream cannot hold
//! and whose raw code is the honest thing to record.
//!
//! Decompression is charged to the shared [`DecompressionBudget`] before a decompressor is pointed
//! at a stream, and each block's read is capped at the length it declares, so a forged expansion is
//! refused rather than allocated.
//!
//! A `--metadata-only` run describes a measurement from its `##HD`/`##DG`/`##CG`/`##CN` header tree
//! without opening a data block at all — the fastest way to inventory a large measurement, and the
//! only way to describe one whose data block this reader declines.
//!
//! Provenance comes from what the file states about itself: the identification block's writing
//! program becomes `recorder`, and the `##SI` acquisition sources a channel group or channel points
//! at — the ECU, bus, I/O device or tool the samples came from — become `sensor`, each qualified by
//! its bus or path. Both are extracted from the source bytes, so both are `known`, never asserted.
//!
//! Values are fingerprinted into `frame.value_ref.content_hash` (never stored), so the CDM content
//! hash is sensitive to actual measured content, exactly as in the other adapters.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::io::Read;

use sha2::{Digest, Sha256};

use crate::cdm::{
    ClockKind, Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};

use super::{
    Adapter, Coverage, DecompressionBudget, Detection, IngestError, IngestOptions, IngestReport,
    Ingested, Source, UnmappedField,
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

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
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

/// A channel's value conversion: the rule that turns the raw bits in a record into the physical
/// quantity they stand for.
///
/// Every numeric conversion MDF defines is applied. What is left — the algebraic-formula type, which
/// needs an expression evaluator, and the four text-valued types, whose physical value is a string
/// the CDM's numeric stream has no shape for — leaves the raw value untouched and is reported.
#[derive(Debug, Clone)]
enum Conversion {
    /// Physical value is the raw value.
    Identity,
    /// `phys = p1 + p2 * raw` (`##CC` type 1).
    Linear { p1: f64, p2: f64 },
    /// `phys = (p1·raw² + p2·raw + p3) / (p4·raw² + p5·raw + p6)` (`##CC` type 2). How a sensor's
    /// calibration curve is stored when it is not a straight line.
    Rational { p: [f64; 6] },
    /// A value-to-value look-up table (`##CC` types 4 and 5): ascending `(key, value)` pairs, read
    /// with linear interpolation between them (type 4) or by nearest key (type 5).
    Table {
        pairs: Vec<(f64, f64)>,
        interpolate: bool,
    },
    /// A value-range-to-value look-up table (`##CC` type 6): `(min, max, value)` triples matched on
    /// `min <= raw < max`, with a default for a raw value in no range.
    RangeTable {
        ranges: Vec<(f64, f64, f64)>,
        default: f64,
    },
}

impl Conversion {
    fn apply(&self, raw: f64) -> f64 {
        match self {
            Conversion::Identity => raw,
            Conversion::Linear { p1, p2 } => p1 + p2 * raw,
            // A zero denominator yields a non-finite value rather than a silently substituted one.
            // That is the file's own conversion saying the sample has no physical value there, and
            // `STATISTICAL.NON_FINITE_OBSERVED` is the finding that says so.
            Conversion::Rational { p } => {
                (p[0] * raw * raw + p[1] * raw + p[2]) / (p[3] * raw * raw + p[4] * raw + p[5])
            }
            Conversion::Table { pairs, interpolate } => table_lookup(pairs, *interpolate, raw),
            Conversion::RangeTable { ranges, default } => ranges
                .iter()
                .find(|(min, max, _)| raw >= *min && raw < *max)
                .map_or(*default, |(_, _, value)| *value),
        }
    }
}

/// Read `raw` off an ascending `(key, value)` table.
///
/// Type 4 interpolates linearly between the two keys that bracket the value and clamps outside the
/// table; type 5 takes the value of the nearest key. Both are what a sensor linearization curve is
/// stored as, and reading the raw count instead reports a detector count as though it were a
/// temperature.
fn table_lookup(pairs: &[(f64, f64)], interpolate: bool, raw: f64) -> f64 {
    let Some(&(first_key, first_value)) = pairs.first() else {
        return raw;
    };
    let &(last_key, last_value) = pairs.last().expect("non-empty, checked above");
    if raw <= first_key {
        return first_value;
    }
    if raw >= last_key {
        return last_value;
    }
    // The keys ascend (verified when the block was read), so the first key above `raw` and the one
    // before it bracket it.
    let upper = pairs
        .iter()
        .position(|(key, _)| *key >= raw)
        .expect("`raw` is below the last key");
    let (hi_key, hi_value) = pairs[upper];
    let (lo_key, lo_value) = pairs[upper - 1];
    if interpolate {
        let span = hi_key - lo_key;
        if span == 0.0 {
            return lo_value;
        }
        lo_value + (raw - lo_key) * (hi_value - lo_value) / span
    } else if raw - lo_key <= hi_key - raw {
        lo_value
    } else {
        hi_value
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
    // Data layout: type u8, precision u8, flags u16, ref_count u16, val_count u16,
    // phy_range_min f64, phy_range_max f64, then val_count f64 parameters.
    const PARAMS_AT: usize = 1 + 1 + 2 + 2 + 2 + 8 + 8;
    // `cc_val_count` is the file's claim about how many parameters follow; the block's own length
    // is the bound. A forged count cannot make this read past the block or allocate beyond it.
    let declared = le_u16(data, 6).unwrap_or(0) as usize;
    let available = data.len().saturating_sub(PARAMS_AT) / 8;
    let count = declared.min(available);
    let param = |i: usize| le_f64(data, PARAMS_AT + i * 8);
    match cc_type {
        // 0 = 1:1 (no conversion).
        0 => (Conversion::Identity, None),
        // 1 = linear: the first two conversion parameters are the offset and factor.
        1 => match (param(0), param(1)) {
            (Some(p1), Some(p2)) if count >= 2 => (Conversion::Linear { p1, p2 }, None),
            _ => (Conversion::Identity, Some(1)),
        },
        // 2 = rational: a quadratic over a quadratic. How a sensor's calibration curve is stored
        // when it is not a straight line.
        2 if count >= 6 => {
            let mut p = [0f64; 6];
            for (i, slot) in p.iter_mut().enumerate() {
                match param(i) {
                    Some(v) => *slot = v,
                    None => return (Conversion::Identity, Some(2)),
                }
            }
            (Conversion::Rational { p }, None)
        }
        // 4 = value-to-value with interpolation, 5 = without: ascending `(key, value)` pairs.
        4 | 5 if count >= 2 => {
            let mut pairs: Vec<(f64, f64)> = Vec::with_capacity(count / 2);
            for i in 0..count / 2 {
                match (param(i * 2), param(i * 2 + 1)) {
                    (Some(key), Some(value)) => pairs.push((key, value)),
                    _ => return (Conversion::Identity, Some(cc_type)),
                }
            }
            // The lookup walks the table assuming ascending keys, which MDF requires. A file whose
            // table is out of order would be read at the wrong entry, so it is not applied at all.
            // A NaN key is "not ascending" too: it compares to nothing, so no walk of the table
            // can be relied on.
            if pairs.windows(2).any(|w| {
                !matches!(
                    w[0].0.partial_cmp(&w[1].0),
                    Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                )
            }) {
                return (Conversion::Identity, Some(cc_type));
            }
            (
                Conversion::Table {
                    pairs,
                    interpolate: cc_type == 4,
                },
                None,
            )
        }
        // 6 = value-range-to-value: `(min, max, value)` triples then a default.
        6 if count >= 4 => {
            let triples = (count - 1) / 3;
            let mut ranges: Vec<(f64, f64, f64)> = Vec::with_capacity(triples);
            for i in 0..triples {
                match (param(i * 3), param(i * 3 + 1), param(i * 3 + 2)) {
                    (Some(min), Some(max), Some(value)) => ranges.push((min, max, value)),
                    _ => return (Conversion::Identity, Some(6)),
                }
            }
            let Some(default) = param(triples * 3) else {
                return (Conversion::Identity, Some(6));
            };
            (Conversion::RangeTable { ranges, default }, None)
        }
        other => (Conversion::Identity, Some(other)),
    }
}

/// `##CC` types whose physical value is *text*. A numeric CDM stream has no shape for a string, so
/// recording the raw code is the honest answer and costs the reader nothing — unlike a numeric
/// conversion that went unevaluated, which leaves raw counts summarized as physical quantities.
const TEXT_VALUED_CONVERSIONS: &[u8] = &[7, 8, 9, 10];

/// What `##CC` type `cc` is called, so a report names the rule rather than a number.
fn cc_type_name(cc: u8) -> &'static str {
    match cc {
        0 => "1:1",
        1 => "linear",
        2 => "rational",
        3 => "algebraic formula",
        4 => "value-to-value, interpolated",
        5 => "value-to-value",
        6 => "value-range-to-value",
        7 => "value-to-text",
        8 => "value-range-to-text",
        9 => "text-to-value",
        10 => "text-to-text",
        _ => "unknown",
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

    /// Whether this adapter can decode the channel's raw value. Anything else is reported rather
    /// than guessed at.
    ///
    /// A **little-endian (Intel) integer** is decoded at any bit offset and any width up to 64 bits.
    /// That is how an automotive measurement stores bus signals — a 12-bit pedal position starting
    /// three bits into a byte is ordinary, not exotic — and refusing them meant an MF4 full of CAN
    /// traffic produced almost no streams. A **big-endian (Motorola) integer** is decoded only in
    /// whole bytes on a byte boundary: MDF's bit numbering for a straddling Motorola field is not
    /// the DBC sawtooth, and a wrong reading here is a plausible number, not a failure.
    fn is_decodable(&self) -> bool {
        match self.data_type {
            // Little-endian integers, any width, any bit offset within the record.
            0 | 2 => (1..=64).contains(&self.bit_count) && self.bit_offset < 8,
            // Big-endian integers, whole bytes only.
            1 | 3 => self.bit_offset == 0 && matches!(self.bit_count, 8 | 16 | 32 | 64),
            // IEEE 754 floats are only 32- or 64-bit, and are never bit-packed.
            4 | 5 => self.bit_offset == 0 && matches!(self.bit_count, 32 | 64),
            // Strings, byte arrays, MIME and CANopen types are not measurements.
            _ => false,
        }
    }

    /// Decode this channel's physical value out of one record.
    fn value(&self, record: &[u8]) -> Option<f64> {
        let start = usize::try_from(self.byte_offset).ok()?;
        let bits = usize::try_from(self.bit_count).ok()?;
        let value = match self.data_type {
            // Little-endian: the field's least significant bit sits `bit_offset` bits into the byte
            // at `byte_offset` and runs upward, so the bytes it touches are read little-endian, the
            // offset shifted away, and the width masked off. It spans at most nine bytes (seven bits
            // of offset plus sixty-four of value), which is why the accumulator is a `u128`.
            0 | 2 => {
                let offset = usize::from(self.bit_offset);
                let span = offset.checked_add(bits)?.div_ceil(8);
                let raw = record.get(start..start.checked_add(span)?)?;
                let mut acc: u128 = 0;
                for (i, &byte) in raw.iter().enumerate() {
                    acc |= u128::from(byte) << (8 * i);
                }
                let field = ((acc >> offset) & mask(bits)) as u64;
                if self.data_type == 0 {
                    field as f64
                } else {
                    sign_extend(field, bits) as f64
                }
            }
            // Big-endian and floats: whole bytes on a byte boundary, guarded by `is_decodable`.
            _ => {
                let len = bits / 8;
                let raw = record.get(start..start.checked_add(len)?)?;
                match (self.data_type, len) {
                    (1, _) => uint_be(raw) as f64,
                    (3, _) => sign_extend(uint_be(raw), len * 8) as f64,
                    (4, 4) => f32::from_le_bytes(raw.try_into().ok()?) as f64,
                    (4, 8) => f64::from_le_bytes(raw.try_into().ok()?),
                    (5, 4) => f32::from_be_bytes(raw.try_into().ok()?) as f64,
                    (5, 8) => f64::from_be_bytes(raw.try_into().ok()?),
                    _ => return None,
                }
            }
        };
        Some(self.conversion.apply(value))
    }
}

/// A mask of the low `bits` bits. `bits` is never above 64 here, so the shift cannot overflow.
fn mask(bits: usize) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
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

/// One acquisition source named by an `##SI` block: the device, ECU, bus or tool that produced the
/// samples of a channel group or a channel.
///
/// This is the one thing an MF4 says about *where its data came from*. Until it was read, an MF4
/// scored 0/6 on provenance coverage while the file itself named the ECU and the bus it sat on.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AcquisitionSource {
    /// `si_tx_name` — what the source calls itself.
    name: String,
    /// `si_tx_path` — where it sits (a bus name, a device path). Empty when the block omits it.
    path: String,
    /// `si_type`: 0 other, 1 ECU, 2 bus, 3 I/O, 4 tool, 5 user.
    kind: u8,
    /// `si_bus_type`: 0 none, 1 other, 2 CAN, 3 LIN, 4 MOST, 5 FlexRay, 6 K-line, 7 Ethernet, 8 USB.
    bus: u8,
}

impl AcquisitionSource {
    /// How the source is written into provenance: its name, qualified by its bus or path when the
    /// file gives one, because two ECUs called `Gateway` on different buses are two sources.
    fn label(&self) -> String {
        let qualifier = match (SI_BUS_TYPES.get(self.bus as usize), self.path.as_str()) {
            (Some(&bus), _) if self.bus != 0 => bus.to_string(),
            (_, path) if !path.is_empty() => path.to_string(),
            _ => return self.name.clone(),
        };
        format!("{} ({qualifier})", self.name)
    }
}

/// `si_type` names, indexed by the code. Anything past the end is a value MDF does not define.
const SI_TYPES: &[&str] = &["other", "ECU", "bus", "I/O", "tool", "user"];
/// How many acquisition sources are named individually in `provenance.sensor`.
///
/// A gateway log can name hundreds, and every one would become a provenance element built from a
/// count in an untrusted file. Past this the remainder is counted rather than recorded — and the
/// trim is disclosed, because a cap that fires in silence is a report that looks complete over less
/// than it covers.
const MAX_NAMED_SOURCES: usize = 8;
/// `si_bus_type` names, indexed by the code.
const SI_BUS_TYPES: &[&str] = &[
    "none", "other", "CAN", "LIN", "MOST", "FlexRay", "K-line", "Ethernet", "USB",
];

/// Read the `##SI` block at `at`, or `None` when it is absent, malformed, or names nothing.
///
/// A source with no name is not a source: it would put an empty string into `provenance.sensor`,
/// which reads as extracted knowledge and is not any.
fn source_at(bytes: &[u8], at: u64) -> Option<AcquisitionSource> {
    let header = block_header(bytes, at)?;
    if &header.id != b"##SI" {
        return None;
    }
    let name = opt_link(bytes, at, &header, 0)
        .and_then(|tx| text_block(bytes, tx))
        .filter(|n| !n.is_empty())?;
    let path = opt_link(bytes, at, &header, 1)
        .and_then(|tx| text_block(bytes, tx))
        .unwrap_or_default();
    let data = data_section(bytes, at, &header).unwrap_or(&[]);
    Some(AcquisitionSource {
        name,
        path,
        kind: data.first().copied().unwrap_or(0),
        bus: data.get(1).copied().unwrap_or(0),
    })
}

/// Record `source` against this group if it is not already known. Sources repeat — every channel of
/// an ECU's raster points at the same `##SI` — and provenance should name each one once.
fn note_source(out: &mut GroupResult, source: Option<AcquisitionSource>) {
    if let Some(source) = source {
        if !out.sources.contains(&source) {
            out.sources.push(source);
        }
    }
}

/// What a single channel group contributed.
struct GroupResult {
    streams: Vec<Stream>,
    /// Records (cycles) the `##CG` block headers declare across this group's channel groups.
    ///
    /// One record holds one sample of every channel in its group, so this is the frame count a full
    /// read produces *per stream*, not the total across them. Read under `--metadata-only`, where no
    /// data block is opened: it is what turns "no sample values were read" into "none of the 400
    /// records this file declares were read". A reader otherwise sees three streams and zero frames
    /// with nothing saying how big the measurement they declined to read actually is.
    declared_cycles: u64,
    unmapped: Vec<UnmappedField>,
    /// Acquisition sources named by the `##SI` blocks this group's channel groups and channels
    /// point at, in the order first seen.
    sources: Vec<AcquisitionSource>,
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
        // An MF4 file is a graph of links between blocks anywhere in the file, so it is read whole;
        // one larger than this ingest will hold is refused by name rather than by the allocator, and
        // `--metadata-only` describes it from its block headers at any size.
        let bytes = super::read_source_whole(
            path,
            FORMAT_ID,
            options,
            "raise the ceiling with --max-source-bytes (0 removes it); an MF4 is held whole even for \
             a header-only run, because its block graph is a web of offsets into the file",
        )?;

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
        // hold: only this list raises `COVERAGE.SOURCE_UNREAD`, so a group whose records could not
        // be resolved never comes back as no frames and a clean verdict.
        let mut unread: Vec<UnmappedField> = Vec::new();
        // Every distinct `##SI` acquisition source named anywhere in the file, in the order first
        // seen — the file's own account of which device produced its samples.
        let mut sources: Vec<AcquisitionSource> = Vec::new();
        // Samples the `##CG` headers declare across every group, read under `--metadata-only` so the
        // omission note can say how large the measurement this run declined to read actually is.
        let mut declared_cycles: u64 = 0;
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
        let mut frames = super::FrameBudget::new(options);
        // Decompressed bytes are charged against the source's own size, exactly as on the MCAP and
        // Parquet paths: a `##DZ` may declare any expansion it likes, and the claim is refused
        // before a decompressor is pointed at it.
        let mut budget = DecompressionBudget::new(options, bytes.len() as u64);
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
                &mut frames,
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
            declared_cycles = declared_cycles.saturating_add(result.declared_cycles);
            unmapped.extend(result.unmapped);
            unread.extend(result.unread);
            for source in result.sources {
                if !sources.contains(&source) {
                    sources.push(source);
                }
            }
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
        // The `##SI` acquisition sources: the file's own statement of which ECU, bus, tool or I/O
        // device produced its samples — extracted from the source bytes, so `known`. Without it an
        // MF4 scored 0/6 on provenance while naming its hardware in every channel group.
        if !sources.is_empty() {
            let named: Vec<String> = sources
                .iter()
                .take(MAX_NAMED_SOURCES)
                .map(|s| s.label())
                .collect();
            let mut value = named.join(", ");
            if sources.len() > MAX_NAMED_SOURCES {
                // A gateway log can name hundreds of ECUs. The count still reaches the reader; the
                // list stays a sentence a person can read.
                value.push_str(&format!(" (+{} more)", sources.len() - MAX_NAMED_SOURCES));
            }
            metadata.push(("mdf_acquisition_sources".into(), value.clone()));
            metadata.push((
                "mdf_acquisition_source_kinds".into(),
                sources
                    .iter()
                    .map(|s| SI_TYPES.get(s.kind as usize).copied().unwrap_or("unknown"))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
            // One element per source, not one joined string. Provenance is a list precisely so a
            // measurement acquired from several devices records several of them — and a lineage
            // document that turns three ECUs into a single agent called "A, B, C" names an agent
            // nobody has. The metadata entry above keeps the readable one-line form.
            for source in sources.iter().take(MAX_NAMED_SOURCES) {
                elements.push(ProvenanceElement {
                    key: "sensor".into(),
                    value: Some(source.label()),
                    class: ProvenanceClass::Known,
                });
            }
            // A cap that trims in silence is a report that looks complete over less than it covers.
            // The metadata line above already counts the remainder; this puts it where `inspect`
            // and the report's unmapped list will show it too.
            if sources.len() > MAX_NAMED_SOURCES {
                unmapped.push(UnmappedField {
                    source_path: "##SI acquisition sources".into(),
                    note: format!(
                        "{} of {} acquisition sources are named in provenance; the rest are counted \
                         in `mdf_acquisition_sources` rather than recorded as elements",
                        MAX_NAMED_SOURCES,
                        sources.len()
                    ),
                });
            }
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
                ego_frame: None,
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
                mapped_fields: {
                    let mut m = if options.metadata_only {
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
                    };
                    // Claimed only when the file actually named one. A mapped field is a statement
                    // that this run read something, and a measurement with no `##SI` block anywhere
                    // would otherwise have the report say it read a source it never saw.
                    if !sources.is_empty() {
                        m.push("##SI acquisition source -> provenance.sensor".into());
                    }
                    m
                },
                unmapped_fields: unmapped,
                omitted_fields: {
                    let mut o = vec![
                        "episode-segmentation (MF4 records one continuous measurement)".into(),
                        "declared-rate (MF4 channels declare no nominal sample rate)".into(),
                    ];
                    if options.metadata_only {
                        // Quantified, not merely named. "No sample values were read" and "none of
                        // the 6,000 samples this file declares were read" are the same fact and
                        // very different statements: a reader sees three streams and zero frames,
                        // and without the count nothing says how big the thing they declined to
                        // read is. The `##CG` headers state it, and reading them is the whole of
                        // what a metadata-only run does.
                        o.push(format!(
                            "sample values, timestamps and content hashes for the \
                             {declared_cycles} record(s) the ##CG headers declare — one sample of \
                             every channel in the group each, so a full read yields that many \
                             frames per stream (no ##DT or ##DZ data block was opened; only the \
                             ##HD/##DG/##CG/##CN header tree was read)"
                        ));
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

/// One `##CG` resolved from the header tree: which records belong to it, how long each one is, and
/// the channels that slice it.
struct ChannelGroupInfo {
    /// Position in the data group's `##CG` chain, for locators.
    index: u64,
    /// `cg_record_id` — the tag an unsorted group's records carry to say they are this group's.
    record_id: u64,
    cycle_count: u64,
    /// Data bytes plus invalidation bytes: the stride from one record to the next.
    record_len: usize,
    inval_bytes: usize,
    /// A variable-length signal-data group, whose records are length-prefixed rather than fixed.
    vlsd: bool,
    channels: Vec<Channel>,
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
    frames: &mut super::FrameBudget,
    budget: &mut DecompressionBudget,
) -> Result<GroupResult, IngestError> {
    let mut out = GroupResult {
        streams: Vec::new(),
        declared_cycles: 0,
        unmapped: Vec::new(),
        unread: Vec::new(),
        sources: Vec::new(),
    };
    let locator = |what: &str| format!("##DG[{group_index}].{what}");

    let rec_id_size = data_section(bytes, dg_at, dg)
        .and_then(|d| d.first().copied())
        .unwrap_or(0);
    // ASAM defines exactly these widths. A record id of any other size cannot be read, and reading
    // records at a guessed stride would yield a full set of confidently wrong values.
    if !matches!(rec_id_size, 0 | 1 | 2 | 4 | 8) && !metadata_only {
        out.unread.push(UnmappedField {
            source_path: locator("records"),
            note: format!(
                "data group declares a {rec_id_size}-byte record id, which is not a size MDF \
                 defines; its channels contribute no frames"
            ),
        });
        return Ok(out);
    }

    // The group's data block: `##DT` verbatim, `##DZ` decompressed, `##DL`/`##HL` stitched back into
    // one record stream. A metadata-only run reads no record at all, so it never resolves the block.
    let data_link = opt_link(bytes, dg_at, dg, 2);
    let resolved: Cow<'_, [u8]> = if metadata_only {
        Cow::Borrowed(&[][..])
    } else {
        match data_link {
            Some(at) => match resolve_records(bytes, at, &locator, seen, budget, &mut out)? {
                Some(records) => records,
                // The reason is already filed in `out.unread`; a group whose records could not be
                // resolved contributes no frames rather than a partial, misaligned decode.
                None => return Ok(out),
            },
            None => {
                out.unmapped.push(UnmappedField {
                    source_path: locator("data"),
                    note: "data group carries no readable data block".into(),
                });
                return Ok(out);
            }
        }
    };
    let records: &[u8] = &resolved;

    // Every channel group in the chain is resolved before any is decoded: an unsorted group's
    // records are tagged with the id of whichever group they belong to, so none of them can be
    // located until all the groups are known.
    let mut groups: Vec<ChannelGroupInfo> = Vec::new();
    let mut cg_at = opt_link(bytes, dg_at, dg, 1);
    let mut cg_index = 0u64;
    while let Some(at) = cg_at {
        if !seen.insert(at) {
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
        // `cg_si_acq_source`: the device or bus this raster was acquired from.
        let acq = opt_link(bytes, at, &cg, 3).and_then(|si| source_at(bytes, si));
        note_source(&mut out, acq);
        let cg_data = data_section(bytes, at, &cg);
        let record_id = cg_data.and_then(|d| le_u64(d, 0)).unwrap_or(0);
        let cycle_count = cg_data.and_then(|d| le_u64(d, 8)).unwrap_or(0);
        let cg_flags = cg_data.and_then(|d| le_u16(d, 16)).unwrap_or(0);
        let data_bytes = cg_data.and_then(|d| le_u32(d, 24)).unwrap_or(0) as usize;
        let inval_bytes = cg_data.and_then(|d| le_u32(d, 28)).unwrap_or(0) as usize;

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
            // `cn_si_source`: a channel may name a source of its own, finer than its group's.
            let cn_source = opt_link(bytes, cn, &header, 3).and_then(|si| source_at(bytes, si));
            note_source(&mut out, cn_source);
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

        groups.push(ChannelGroupInfo {
            index: cg_index,
            record_id,
            cycle_count,
            record_len: data_bytes + inval_bytes,
            inval_bytes,
            vlsd: cg_flags & CG_FLAG_VLSD != 0,
            channels,
        });
        cg_index += 1;
        cg_at = opt_link(bytes, at, &cg, 0);
    }

    if metadata_only {
        for group in &groups {
            declare_channel_group(
                group.cycle_count,
                &group.channels,
                &clock_id_for(group_index, group.index),
                frames,
                &mut out,
            )?;
        }
        return Ok(out);
    }

    // A variable-length signal-data group's records are length-prefixed, not fixed-stride, so
    // slicing them at `cg_data_bytes` would read every one of them at the wrong offset.
    for group in &groups {
        if group.vlsd {
            out.unread.push(UnmappedField {
                source_path: format!("##DG[{group_index}].##CG[{}]", group.index),
                note: "variable-length signal-data channel group is not decoded; its channels \
                       contribute no frames"
                    .into(),
            });
        }
    }
    groups.retain(|g| !g.vlsd);

    // Which records belong to which channel group. A sorted group is the whole stream; an unsorted
    // one interleaves several groups' records behind their ids and has to be demultiplexed first.
    let per_group: Vec<Cow<'_, [u8]>> = if rec_id_size == 0 {
        // A sorted data group holds exactly one channel group. Decoding a second against the same
        // records from offset 0 would produce plausible-but-wrong values, so report and drop it.
        if groups.len() > 1 {
            out.unread.push(UnmappedField {
                source_path: locator("channel-groups"),
                note:
                    "sorted data group holds more than one channel group; only the first is decoded"
                        .into(),
            });
            groups.truncate(1);
        }
        vec![Cow::Borrowed(records)]
    } else {
        match demultiplex(records, rec_id_size as usize, &groups, &locator, &mut out) {
            Some(split) => split.into_iter().map(Cow::Owned).collect(),
            None => return Ok(out),
        }
    };

    for (group, group_records) in groups.iter().zip(per_group) {
        let cg_index = group.index;
        let locate = |name: &str| format!("##DG[{group_index}].##CG[{cg_index}].{name}");
        // A group materializes channels × records frames; charge that before decoding.
        let decodable = group.channels.iter().filter(|c| c.is_decodable()).count() as u64;
        // A zero-length record divides into no frames at all (and must not divide by zero).
        let available = group_records
            .len()
            .checked_div(group.record_len)
            .unwrap_or(0) as u64;
        let planned = if group.cycle_count == 0 {
            available
        } else {
            available.min(group.cycle_count)
        };
        frames.take(FORMAT_ID, decodable.saturating_mul(planned))?;
        // Whatever this group could not contribute is recorded as an unmapped field inside.
        decode_channel_group(
            &group_records,
            group.record_len,
            group.cycle_count,
            group.inval_bytes,
            &group.channels,
            &clock_id_for(group_index, cg_index),
            &locate,
            &mut out,
        );
    }
    Ok(out)
}

/// The clock id of one channel group's raster.
///
/// Every `##CG` carries its own time master, so two channel groups are two independent timelines
/// even inside one `##DG` — which an unsorted data group routinely holds. Naming the clock after the
/// data group alone made them share one id, and the cross-stream temporal checks would then compare
/// one raster's span and rate against another's and report the difference as a defect.
fn clock_id_for(group_index: u64, cg_index: u64) -> String {
    format!("{CLOCK_ID}#{group_index}.{cg_index}")
}

/// `cg_flags` bit 0: a variable-length signal-data channel group.
const CG_FLAG_VLSD: u16 = 0x01;

/// Split an unsorted data group's interleaved record stream into one contiguous stream per channel
/// group, in the same order as `groups`.
///
/// An unsorted group is how a bus logger writes several rasters into one data block as they arrive:
/// each record is prefixed with the `cg_record_id` of the group it belongs to, and the groups'
/// records are interleaved in time order. Until this existed the whole group was declined, so a file
/// written that way ingested to no frames at all.
///
/// `None` means the stream could not be split and the reason is filed in `out.unread`. There is no
/// partial answer to give: a record's length is known only from its id, so the first id that matches
/// no channel group leaves every later record at an unknown offset — the stream cannot be
/// resynchronized, and decoding what came before it would silently truncate the measurement.
fn demultiplex(
    records: &[u8],
    rec_id_size: usize,
    groups: &[ChannelGroupInfo],
    locator: &dyn Fn(&str) -> String,
    out: &mut GroupResult,
) -> Option<Vec<Vec<u8>>> {
    let mut split: Vec<Vec<u8>> = vec![Vec::new(); groups.len()];
    let mut at = 0usize;
    while at < records.len() {
        let Some(id) = record_id(records, at, rec_id_size) else {
            // A tail too short to hold even an id. Disclosed, because those bytes are records that
            // did not reach a stream.
            out.unread.push(UnmappedField {
                source_path: locator("records"),
                note: format!(
                    "record stream ends with {} byte(s) too few to hold a record id; they \
                     contribute no frames",
                    records.len() - at
                ),
            });
            break;
        };
        at += rec_id_size;
        let Some(index) = groups.iter().position(|g| g.record_id == id) else {
            out.unread.push(UnmappedField {
                source_path: locator("records"),
                note: format!(
                    "record id {id} matches no channel group in this data group, so every later \
                     record is at an unknown offset; its channels contribute no frames"
                ),
            });
            return None;
        };
        let len = groups[index].record_len;
        if len == 0 {
            // A zero-stride record never advances, so the walk would not terminate.
            out.unread.push(UnmappedField {
                source_path: locator("records"),
                note: format!(
                    "channel group with record id {id} declares a zero-length record, so the \
                     stream cannot be walked; this data group's channels contribute no frames"
                ),
            });
            return None;
        }
        let Some(record) = records.get(at..at.checked_add(len)?) else {
            out.unread.push(UnmappedField {
                source_path: locator("records"),
                note: format!(
                    "record stream ends mid-record ({} byte(s) of a {len}-byte record); they \
                     contribute no frames",
                    records.len() - at
                ),
            });
            break;
        };
        split[index].extend_from_slice(record);
        at += len;
    }
    Some(split)
}

/// Read a record id of `size` bytes (1, 2, 4 or 8) at `at`.
fn record_id(records: &[u8], at: usize, size: usize) -> Option<u64> {
    let raw = records.get(at..at.checked_add(size)?)?;
    let mut buf = [0u8; 8];
    buf[..size].copy_from_slice(raw);
    Some(u64::from_le_bytes(buf))
}

/// The only original block type this reader reconstructs from a `##DZ`. A `##DZ` records which
/// block it was compressed from; `SD`/`RD` hold signal or reduction data and `DV`/`DI`/`RV`/`RI` are
/// column-oriented (MDF 4.2), none of which is the record stream a channel group is decoded against.
/// Decoding one as if it were would produce plausible-but-wrong values, so it is reported instead.
const DZ_RECORD_ORIGIN: &[u8] = b"DT";

/// Deflate, optionally preceded by a byte-column transposition — the two `dz_zip_type` values ASAM
/// defines. Anything else is a future encoding this reader must not guess at.
const ZIP_DEFLATE: u8 = 0;
const ZIP_TRANSPOSED_DEFLATE: u8 = 1;

/// Resolve a data group's record stream from whatever block its `dg_data` link points at.
///
/// Real loggers do not write a bare `##DT`. They compress it (`##DZ`), split it across a data list
/// (`##DL`), or both behind a header list (`##HL`) — so a reader that handles only `##DT` reads
/// nothing off the files the format is actually used for. This resolves all four into one contiguous
/// record stream, borrowed when the data is already uncompressed and contiguous.
///
/// `None` means the records could not be resolved and the reason has been filed in
/// `out.unread`: whatever the caller does next, it must not be to decode part of a stream, because a
/// missing list element shifts every record after it.
fn resolve_records<'a>(
    bytes: &'a [u8],
    at: u64,
    locator: &dyn Fn(&str) -> String,
    seen: &mut BTreeSet<u64>,
    budget: &mut DecompressionBudget,
    out: &mut GroupResult,
) -> Result<Option<Cow<'a, [u8]>>, IngestError> {
    let Some(header) = block_header(bytes, at) else {
        out.unread.push(UnmappedField {
            source_path: locator("data"),
            note: format!(
                "truncated or malformed data block at offset {at}; its channels contribute no frames"
            ),
        });
        return Ok(None);
    };
    match &header.id {
        b"##DT" | b"##DZ" => resolve_leaf(bytes, at, &header, locator, budget, out),
        b"##HL" | b"##DL" => {
            // A header list is a one-block wrapper naming the list's zip type; the list itself is
            // what holds the data links.
            let first = if &header.id == b"##HL" {
                match opt_link(bytes, at, &header, 0) {
                    Some(dl) => Some(dl),
                    // An empty list resolves to an empty record stream, which reads as a group that
                    // simply held no samples. Naming the cause keeps it from looking that way.
                    None => {
                        out.unread.push(UnmappedField {
                            source_path: locator("data"),
                            note: "##HL header list links to no data list; this data group's \
                                   channels contribute no frames"
                                .into(),
                        });
                        return Ok(None);
                    }
                }
            } else {
                Some(at)
            };
            let mut joined: Vec<u8> = Vec::new();
            let mut list_at = first;
            while let Some(dl_at) = list_at {
                // The shared visited set: a list that links back to itself must not spin.
                if !seen.insert(dl_at) {
                    out.unread.push(UnmappedField {
                        source_path: locator("data"),
                        note: "data list loops back on itself; this data group's channels \
                               contribute no frames"
                            .into(),
                    });
                    return Ok(None);
                }
                let Some(dl) = block_header(bytes, dl_at) else {
                    out.unread.push(UnmappedField {
                        source_path: locator("data"),
                        note: format!(
                            "truncated or malformed ##DL block at offset {dl_at}; this data \
                             group's channels contribute no frames"
                        ),
                    });
                    return Ok(None);
                };
                if &dl.id != b"##DL" {
                    out.unread.push(UnmappedField {
                        source_path: locator("data"),
                        note: format!(
                            "expected a ##DL block at offset {dl_at}, found `{}`; this data \
                             group's channels contribute no frames",
                            String::from_utf8_lossy(&dl.id)
                        ),
                    });
                    return Ok(None);
                }
                let dl_data = data_section(bytes, dl_at, &dl).unwrap_or(&[]);
                // `dl_count` is bounded by the links the block actually carries, so a forged count
                // cannot make this loop longer than the file.
                let declared = le_u32(dl_data, 4).unwrap_or(0) as u64;
                let count = declared.min(dl.link_count.saturating_sub(1));
                for i in 0..count {
                    let Some(leaf_at) = opt_link(bytes, dl_at, &dl, 1 + i) else {
                        continue;
                    };
                    let Some(leaf_header) = block_header(bytes, leaf_at) else {
                        out.unread.push(UnmappedField {
                            source_path: locator("data"),
                            note: format!(
                                "data-list element {i} is truncated or malformed; this data \
                                 group's channels contribute no frames"
                            ),
                        });
                        return Ok(None);
                    };
                    match resolve_leaf(bytes, leaf_at, &leaf_header, locator, budget, out)? {
                        Some(part) => joined.extend_from_slice(&part),
                        None => return Ok(None),
                    }
                }
                list_at = opt_link(bytes, dl_at, &dl, 0);
            }
            Ok(Some(Cow::Owned(joined)))
        }
        other => {
            out.unread.push(UnmappedField {
                source_path: locator("data"),
                note: format!(
                    "`{}` data block is not decoded (##DT, ##DZ, ##DL and ##HL are); its channels \
                     contribute no frames",
                    String::from_utf8_lossy(other)
                ),
            });
            Ok(None)
        }
    }
}

/// One data-list element or one whole data block: `##DT` verbatim, `##DZ` decompressed.
fn resolve_leaf<'a>(
    bytes: &'a [u8],
    at: u64,
    header: &BlockHeader,
    locator: &dyn Fn(&str) -> String,
    budget: &mut DecompressionBudget,
    out: &mut GroupResult,
) -> Result<Option<Cow<'a, [u8]>>, IngestError> {
    match &header.id {
        b"##DT" => Ok(Some(Cow::Borrowed(
            data_section(bytes, at, header).unwrap_or(&[]),
        ))),
        b"##DZ" => Ok(inflate_dz(bytes, at, header, locator, budget, out)?.map(Cow::Owned)),
        other => {
            out.unread.push(UnmappedField {
                source_path: locator("data"),
                note: format!(
                    "`{}` data block is not decoded (##DT and ##DZ are); its channels contribute \
                     no frames",
                    String::from_utf8_lossy(other)
                ),
            });
            Ok(None)
        }
    }
}

/// Decompress one `##DZ` block into the record bytes it was made from.
///
/// The block states what it was compressed from, how, and how long it was both ways. Every one of
/// those is a claim by the file, so each is checked rather than trusted: the expansion is charged to
/// the decompression budget *before* a decompressor sees the stream, the read is hard-capped at the
/// declared original length so a corrupt stream cannot expand past it, and a result that does not
/// match the declared length is reported rather than decoded — a short buffer would silently drop
/// the tail of the measurement.
fn inflate_dz(
    bytes: &[u8],
    at: u64,
    header: &BlockHeader,
    locator: &dyn Fn(&str) -> String,
    budget: &mut DecompressionBudget,
    out: &mut GroupResult,
) -> Result<Option<Vec<u8>>, IngestError> {
    let decline = |out: &mut GroupResult, note: String| {
        out.unread.push(UnmappedField {
            source_path: locator("data"),
            note,
        });
    };
    let data = data_section(bytes, at, header).unwrap_or(&[]);
    let origin = data.get(0..2).unwrap_or(&[]);
    if origin != DZ_RECORD_ORIGIN {
        decline(
            out,
            format!(
                "##DZ data block holds `{}` data, not the `DT` record stream, and is not decoded; \
                 its channels contribute no frames",
                String::from_utf8_lossy(origin).trim_end_matches('\0')
            ),
        );
        return Ok(None);
    }
    let zip_type = data.get(2).copied().unwrap_or(u8::MAX);
    if zip_type != ZIP_DEFLATE && zip_type != ZIP_TRANSPOSED_DEFLATE {
        decline(
            out,
            format!(
                "##DZ data block uses zip type {zip_type}, which is not decoded; its channels \
                 contribute no frames"
            ),
        );
        return Ok(None);
    }
    let zip_parameter = le_u32(data, 4).unwrap_or(0) as usize;
    let org_len = le_u64(data, 8).unwrap_or(0);
    let comp_len = le_u64(data, 16).unwrap_or(0);
    let Some(payload) = usize::try_from(comp_len)
        .ok()
        .and_then(|n| data.get(24..24usize.checked_add(n)?))
    else {
        decline(
            out,
            format!(
                "##DZ data block declares {comp_len} compressed bytes it does not hold; its \
                 channels contribute no frames"
            ),
        );
        return Ok(None);
    };
    // Charged first: the file's own claim about how far it expands is refused before the time and
    // memory of decompressing it is spent.
    budget.take(FORMAT_ID, org_len)?;
    let Ok(cap) = usize::try_from(org_len) else {
        decline(
            out,
            format!("##DZ data block declares {org_len} bytes, more than this machine can hold"),
        );
        return Ok(None);
    };
    let mut inflated = Vec::new();
    if let Err(e) = flate2::read::ZlibDecoder::new(payload)
        .take(org_len)
        .read_to_end(&mut inflated)
    {
        decline(
            out,
            format!("##DZ data block did not decompress ({e}); its channels contribute no frames"),
        );
        return Ok(None);
    }
    if inflated.len() != cap {
        decline(
            out,
            format!(
                "##DZ data block declares {org_len} decompressed bytes but produced {}; its \
                 channels contribute no frames",
                inflated.len()
            ),
        );
        return Ok(None);
    }
    if zip_type == ZIP_TRANSPOSED_DEFLATE {
        match untranspose(&inflated, zip_parameter) {
            Some(v) => inflated = v,
            None => {
                decline(
                    out,
                    "##DZ data block is transposed by a zero-width column, which cannot be \
                     reversed; its channels contribute no frames"
                        .into(),
                );
                return Ok(None);
            }
        }
    }
    Ok(Some(inflated))
}

/// Reverse the byte-column transposition a `dz_zip_type` of 1 applies before deflating.
///
/// The writer lays the records out column-major — every record's first byte, then every record's
/// second — because like-typed bytes compress far better adjacently. `columns` is the record length,
/// so the buffer is `columns` rows of `lines` bytes and this reads it back row-major. A tail shorter
/// than one full row is left where it is, exactly as the writer left it.
fn untranspose(data: &[u8], columns: usize) -> Option<Vec<u8>> {
    if columns == 0 {
        return None;
    }
    let lines = data.len() / columns;
    let mut out = Vec::with_capacity(data.len());
    for line in 0..lines {
        for column in 0..columns {
            out.push(data[column * lines + line]);
        }
    }
    out.extend_from_slice(&data[lines * columns..]);
    Some(out)
}

/// Describe one channel group's channels as streams with no frames, from its block headers alone.
///
/// The `--metadata-only` counterpart to [`decode_channel_group`]: a `##CG` states how many cycles it
/// holds and each `##CN` states its name, so the recording's shape — which signals, on which raster,
/// and how many records each group declares — is readable without touching a data block at all. The
/// cycle count is summed into [`GroupResult::declared_cycles`] and reaches the reader through the
/// omission note, which is what turns "no sample values were read" into "none of the 400 records
/// this file declares were read". That is what
/// inventories a large measurement without decompressing it, and what still describes one whose data
/// block a full read declines.
fn declare_channel_group(
    cycle_count: u64,
    channels: &[Channel],
    clock_id: &str,
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
            clock_id: clock_id.to_string(),
            // The clock describes the measurement — every MF4 raster has a recorded time master —
            // not this ingest. The temporal checks abstain here for want of frames, which the
            // coverage note states, rather than for want of a clock.
            clock_kind: ClockKind::Measured,
            dtype: Some("float64".into()),
            shape: None,
            dim_names: None,
            frames: Vec::new(),
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            latched: None,
            declared_range: None,
            point_fields: None,
            observed_point_counts: None,
            observed_header_stamps: None,
            observed_sequence: None,
            observed_fix_availability: None,
            media: None,
            frame_id: None,
        });
    }
    out.declared_cycles = out.declared_cycles.saturating_add(cycle_count);
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
    clock_id: &str,
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
                    "channel is data type {} at bit offset {} × {} bits, which is not decoded \
                     (little-endian integers are decoded at any bit offset and width; big-endian \
                     ones and floats only in whole bytes on a byte boundary)",
                    channel.data_type, channel.bit_offset, channel.bit_count
                ),
            });
            continue;
        }
        if let Some(cc) = channel.unapplied_conversion {
            if reported_conversions.insert(cc) {
                // Where this lands is the difference between "the CDM has no shape for it" and
                // "the physical value is defined in this file and nobody computed it". A text-valued
                // conversion turns a code into a string, which a numeric stream cannot hold, and the
                // raw code is the honest thing to record — that costs the reader nothing. An
                // algebraic formula produces a *number*, which is in the file as a rule and was not
                // evaluated, so every value this stream carries is a raw count summarized as though
                // it were the physical quantity. That has to reach the verdict.
                let field = UnmappedField {
                    source_path: locate(&channel.name),
                    note: format!(
                        "##CC conversion type {cc} ({}) is not applied; the raw value is recorded \
                         instead",
                        cc_type_name(cc)
                    ),
                };
                if TEXT_VALUED_CONVERSIONS.contains(&cc) {
                    out.unmapped.push(field);
                } else {
                    out.unread.push(field);
                }
            }
        }
        let mut frames: Vec<Frame> = Vec::with_capacity(count);
        // The same physical values, accumulated as they are read. An MF4 channel is decoded — the
        // `##CC` conversion is applied and the result is a number — so the statistical family can
        // grade it, exactly as it grades a CAN signal off a DBC. Without this a fleet measurement
        // whose steering angle sits at its end-stop for the whole drive scored `data 100` with no
        // statistical findings. Single-pass and holding no values: an MF4 is far larger than memory.
        let mut accum = super::stats::FeatureAccum::default();
        for (i, ts) in timestamps.iter().enumerate() {
            let (Some(ts), Some(value)) = (
                *ts,
                channel.value(&records[i * record_len..(i + 1) * record_len]),
            ) else {
                continue;
            };
            accum.push_cell(&[Some(value)]);
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
            clock_id: clock_id.to_string(),
            // Real recorded timestamps: every temporal check applies.
            clock_kind: ClockKind::Measured,
            dtype: Some("float64".into()),
            shape: None,
            dim_names: None,
            frames,
            // MF4 stores no summary statistics of its own — a `##CC` conversion is a rule for
            // turning raw bits into physical values, not a summary of them — so there is nothing to
            // compare against, only what was recomputed from the values read.
            stats: None,
            dim_stats: None,
            observed_stats: accum.stats(),
            observed_saturation: accum.saturation(),
            // The values were read, so `Some(0)` says every one was finite. Not vacuous: a linear
            // conversion with a large factor can overflow a raw extreme to infinity.
            observed_non_finite: Some(accum.non_finite()),
            // One channel is one scalar; there are no dimensions to break out.
            observed_dim_stats: None,
            latched: None,
            declared_range: None,
            point_fields: None,
            observed_point_counts: None,
            observed_header_stamps: None,
            observed_sequence: None,
            observed_fix_availability: None,
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
        assert_eq!(uint_be(&[0x01, 0x02]), 0x0102);
        assert_eq!(uint_be(&[]), 0);
        // The little-endian path is the bit-field one: a whole-byte channel is the case where the
        // offset is zero and the width is a multiple of eight.
        let le = |data_type: u8, bit_offset: u8, bit_count: u32, record: &[u8]| {
            Channel {
                name: "x".into(),
                channel_type: 0,
                sync_type: 0,
                data_type,
                bit_offset,
                byte_offset: 0,
                bit_count,
                flags: 0,
                conversion: Conversion::Identity,
                unapplied_conversion: None,
            }
            .value(record)
        };
        assert_eq!(le(0, 0, 16, &[0x01, 0x02]), Some(0x0201 as f64));
        // A 12-bit field three bits into the word.
        assert_eq!(le(0, 3, 12, &[0xF8, 0xFF, 0x00]), Some(4095.0));
        // The same field read as signed is -1, not 4095.
        assert_eq!(le(2, 3, 12, &[0xF8, 0xFF, 0x00]), Some(-1.0));
        // A field the record is too short to hold yields nothing rather than a partial read.
        assert_eq!(le(0, 0, 32, &[0x01, 0x02]), None);
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
    fn which_channels_this_reader_can_slice() {
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
        // Little-endian integers at any offset and any width up to 64 bits: how bus signals are
        // stored, and the whole reason this reader sees an automotive measurement at all.
        assert!(channel(0, 3, 16).is_decodable());
        assert!(channel(0, 0, 12).is_decodable());
        assert!(channel(2, 7, 1).is_decodable());
        assert!(channel(0, 0, 64).is_decodable());
        assert!(!channel(0, 0, 65).is_decodable());
        assert!(!channel(0, 0, 0).is_decodable());
        // A bit-packed big-endian field is declined: MDF's numbering for a straddling Motorola
        // field is not the DBC sawtooth, and guessing yields a plausible wrong number.
        assert!(!channel(1, 3, 12).is_decodable());
        assert!(channel(1, 0, 16).is_decodable());
        // Non-numeric, and a float that isn't 32/64 bits or isn't byte-aligned.
        assert!(!channel(6, 0, 32).is_decodable());
        assert!(!channel(4, 0, 16).is_decodable());
        assert!(!channel(4, 1, 32).is_decodable());
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
