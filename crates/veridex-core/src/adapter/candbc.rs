//! CAN + DBC adapter: decode raw CAN-bus frames into named signal streams using a DBC database.
//!
//! A CAN log on its own is opaque bytes; the DBC is the signal database that gives those bytes
//! meaning. This adapter ingests a **directory** holding exactly one `.dbc` and one or more candump
//! ASCII logs (`.log` / `.asc`), decodes each frame's signals per the DBC, and emits one CDM
//! [`Stream`] per signal (`Modality::CanSignal`). The fidelity signal the ingestion spec asks for is
//! surfaced: **DBC-coverage gaps** — CAN ids seen in the log with no DBC definition — reported as
//! `unmapped` fields.
//!
//! Scope: both DBC byte orders are decoded — little-endian (Intel, `@1`) and big-endian (Motorola,
//! `@0`) — with factor/offset applied and signed signals sign-extended. Decoded values are
//! fingerprinted into `frame.value_ref.content_hash`, so the CDM hash is sensitive to actual signal
//! content.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::cdm::{ClockKind, Dataset, Episode, Frame, Modality, Stream, ValueRef};

use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};

const CLOCK_ID: &str = "can";

/// Adapter for a CAN+DBC dataset directory.
pub struct CanDbcAdapter;

/// One decoded signal definition from the DBC.
#[derive(Debug, Clone)]
struct DbcSignal {
    name: String,
    start_bit: u32,
    length: u32,
    little_endian: bool,
    signed: bool,
    factor: f64,
    offset: f64,
}

/// One CAN message definition (id → its signals).
#[derive(Debug, Clone, Default)]
struct DbcMessage {
    name: String,
    signals: Vec<DbcSignal>,
}

/// Parse the subset of DBC we need: `BO_` message headers and their `SG_` signal lines.
fn parse_dbc(text: &str) -> BTreeMap<u32, DbcMessage> {
    let mut messages: BTreeMap<u32, DbcMessage> = BTreeMap::new();
    let mut current: Option<u32> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("BO_ ") {
            // BO_ <id> <Name>: <dlc> <transmitter>
            let mut it = rest.split_whitespace();
            let id = it.next().and_then(|s| s.parse::<u32>().ok());
            let name = it.next().unwrap_or("").trim_end_matches(':').to_string();
            if let Some(id) = id {
                // Mask off the extended-frame flag bit if present (bit 31).
                let id = id & 0x1FFF_FFFF;
                messages.entry(id).or_insert_with(|| DbcMessage {
                    name,
                    signals: Vec::new(),
                });
                current = Some(id);
            } else {
                current = None;
            }
        } else if let Some(rest) = line.strip_prefix("SG_ ") {
            if let (Some(id), Some(sig)) = (current, parse_sg(rest)) {
                if let Some(m) = messages.get_mut(&id) {
                    m.signals.push(sig);
                }
            }
        } else if line.is_empty() {
            // A blank line ends a message block in some DBCs; keep `current` for tolerant parsing.
        }
    }
    messages
}

/// Parse a `SG_` body: `<Name> : <start>|<len>@<order><sign> (<factor>,<offset>) [min|max] "unit" recv`.
fn parse_sg(rest: &str) -> Option<DbcSignal> {
    let (name, after) = rest.split_once(':')?;
    let name = name.trim().to_string();
    let after = after.trim();
    // `<start>|<len>@<order><sign>` then a space then `(factor,offset)`.
    let mut parts = after.split_whitespace();
    let layout = parts.next()?; // start|len@order sign
    let scaling = parts.next()?; // (factor,offset)

    let (start, rest2) = layout.split_once('|')?;
    let (len, order_sign) = rest2.split_once('@')?;
    let start_bit: u32 = start.trim().parse().ok()?;
    let length: u32 = len.trim().parse().ok()?;
    let mut oc = order_sign.chars();
    let order = oc.next()?; // '1' little-endian (Intel), '0' big-endian (Motorola)
    let sign = oc.next()?; // '+' unsigned, '-' signed
    let little_endian = order == '1';
    let signed = sign == '-';

    let scaling = scaling.trim_start_matches('(').trim_end_matches(')');
    let (factor, offset) = scaling.split_once(',')?;
    let factor: f64 = factor.trim().parse().ok()?;
    let offset: f64 = offset.trim().parse().ok()?;

    Some(DbcSignal {
        name,
        start_bit,
        length,
        little_endian,
        signed,
        factor,
        offset,
    })
}

/// One raw CAN frame from the log.
struct CanFrame {
    ts_ns: i64,
    id: u32,
    data: Vec<u8>,
}

/// Parse a candump ASCII line: `(<seconds>) <iface> <hexid>#<hexdata>`. Returns `None` for a line
/// that isn't a frame (blank, comment) so a malformed log is skipped, never a panic.
fn parse_candump_line(line: &str) -> Option<CanFrame> {
    let line = line.trim();
    let (ts_part, rest) = line.strip_prefix('(')?.split_once(')')?;
    let seconds: f64 = ts_part.trim().parse().ok()?;
    // saturating conversion to ns; real timestamps are far below i64::MAX.
    let ts_ns = (seconds * 1e9).clamp(i64::MIN as f64, i64::MAX as f64) as i64;
    // `<iface> <hexid>#<hexdata>`
    let mut it = rest.split_whitespace();
    let _iface = it.next()?;
    let frame = it.next()?;
    let (id_hex, data_hex) = frame.split_once('#')?;
    let id = u32::from_str_radix(id_hex.trim(), 16).ok()? & 0x1FFF_FFFF;
    let data = parse_hex_bytes(data_hex.trim())?;
    Some(CanFrame { ts_ns, id, data })
}

/// Parse an even-length hex string into bytes (up to 8 for a classic CAN frame).
fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.len() > 16 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

/// Decode a signal's physical value from a frame's data bytes, or `None` when the signal's shape is
/// unusable (zero-length, wider than 64 bits) or the frame is too short to hold its bits.
fn decode_signal(sig: &DbcSignal, data: &[u8]) -> Option<f64> {
    // Reject a zero-length or over-wide signal up front. This guards every shift below: a shift by
    // >= 64 panics in debug and silently mis-computes in release.
    if sig.length == 0 || sig.length > 64 {
        return None;
    }
    let bits = if sig.little_endian {
        raw_bits_le(sig.start_bit, sig.length, data)?
    } else {
        raw_bits_be(sig.start_bit, sig.length, data)?
    };
    let value: f64 = if !sig.signed {
        // Unsigned: direct u64 -> f64 (correct across the full 64-bit range).
        bits as f64
    } else if sig.length < 64 && (bits >> (sig.length - 1)) & 1 == 1 {
        // Signed with the top bit set: sign-extend into a negative value.
        ((bits as i64) - (1i64 << sig.length)) as f64
    } else {
        // Non-negative, or a full 64-bit signal already in two's-complement form.
        bits as i64 as f64
    };
    Some(value * sig.factor + sig.offset)
}

/// Raw bits of a little-endian (Intel) signal: `start_bit` is the signal's least-significant bit in
/// the sawtooth numbering (bit `n` = byte `n / 8`, bit `n % 8` counted from that byte's LSB), and the
/// signal runs upward from there.
fn raw_bits_le(start_bit: u32, length: u32, data: &[u8]) -> Option<u64> {
    let end_bit = start_bit.checked_add(length)?;
    if start_bit >= 64 || end_bit as usize > data.len() * 8 {
        return None;
    }
    // Assemble up to 8 data bytes as a little-endian u64, then shift/mask.
    let mut raw: u64 = 0;
    for (i, &b) in data.iter().take(8).enumerate() {
        raw |= (b as u64) << (i * 8);
    }
    let mask = if length == 64 {
        u64::MAX
    } else {
        (1u64 << length) - 1
    };
    Some((raw >> start_bit) & mask)
}

/// Raw bits of a big-endian (Motorola) signal. `start_bit` names the signal's *most*-significant bit
/// in the same sawtooth numbering, and the signal walks toward less-significant bits: down within the
/// byte, then continuing at bit 7 of the *next* byte. Returns `None` if any bit falls outside `data`.
fn raw_bits_be(start_bit: u32, length: u32, data: &[u8]) -> Option<u64> {
    let mut bits: u64 = 0;
    let mut bit = start_bit;
    for _ in 0..length {
        let byte = (bit / 8) as usize;
        let bit_in_byte = bit % 8;
        let b = *data.get(byte)?;
        bits = (bits << 1) | ((b >> bit_in_byte) as u64 & 1);
        // Step to the next-less-significant bit. At a byte's LSB, wrap to the MSB of the next byte:
        // that is +15 in sawtooth numbering (byte*8 + 0 -> (byte + 1) * 8 + 7).
        bit = if bit_in_byte == 0 {
            bit.checked_add(15)?
        } else {
            bit - 1
        };
    }
    Some(bits)
}

impl Adapter for CanDbcAdapter {
    fn format_id(&self) -> &'static str {
        "candbc"
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["dbc"]
    }

    fn detect(&self, source: &Source) -> Detection {
        let Source::Local(path) = source else {
            return Detection::No;
        };
        // A directory that holds at least one `.dbc` file.
        if path.is_dir() && dir_has_extension(path, "dbc") {
            Detection::Yes {
                version: Some("dbc".into()),
            }
        } else {
            Detection::No
        }
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        // A CAN log becomes one episode, so there is nothing to sample along.
        super::reject_sampling("candbc", options)?;
        let Source::Local(dir) = source else {
            return Err(IngestError::Parse {
                format_id: "candbc",
                message: "remote CAN+DBC ingestion is not supported".into(),
            });
        };

        let (dbc_path, log_paths) = find_inputs(dir)?;
        let dbc_text =
            std::fs::read_to_string(&dbc_path).map_err(|e| IngestError::Io(e.to_string()))?;
        let messages = parse_dbc(&dbc_text);

        // Read and merge all CAN frames, sorted by timestamp.
        let mut frames: Vec<CanFrame> = Vec::new();
        for log in &log_paths {
            let text = std::fs::read_to_string(log).map_err(|e| IngestError::Io(e.to_string()))?;
            frames.extend(text.lines().filter_map(parse_candump_line));
        }
        frames.sort_by_key(|f| f.ts_ns);

        // One CAN frame becomes one CDM frame *per signal its message defines*, so the total is a
        // product of two file-controlled numbers. Charge the budget before decoding anything.
        let mut budget = super::FrameBudget::new(options);
        let declared: u64 = frames
            .iter()
            .map(|f| {
                messages
                    .get(&f.id)
                    .map_or(0, |m| m.signals.len().min(u32::MAX as usize) as u64)
            })
            .sum();
        budget.take("candbc", declared)?;

        // Per signal stream (keyed by "<Message>.<Signal>"), the decoded frames.
        let mut signal_frames: BTreeMap<String, Vec<Frame>> = BTreeMap::new();
        let mut unknown_ids: BTreeMap<u32, u64> = BTreeMap::new();
        let mut min_ts: Option<i64> = None;
        let mut max_ts: Option<i64> = None;

        for frame in &frames {
            min_ts = Some(min_ts.map_or(frame.ts_ns, |m| m.min(frame.ts_ns)));
            max_ts = Some(max_ts.map_or(frame.ts_ns, |m| m.max(frame.ts_ns)));
            let Some(message) = messages.get(&frame.id) else {
                *unknown_ids.entry(frame.id).or_insert(0) += 1;
                continue;
            };
            for sig in &message.signals {
                let stream_name = format!("{}.{}", message.name, sig.name);
                let Some(value) = decode_signal(sig, &frame.data) else {
                    continue; // frame too short for this signal — skip this sample
                };
                // Fingerprint the decoded value so the CDM hash reflects signal content.
                let content_hash =
                    Sha256::digest(crate::canonical::canon_f64_bits(value).to_le_bytes()).into();
                signal_frames.entry(stream_name).or_default().push(Frame {
                    ts: frame.ts_ns,
                    value_ref: ValueRef {
                        uri: format!("can:0x{:x}", frame.id),
                        byte_offset: None,
                        byte_len: None,
                        content_hash: Some(content_hash),
                    },
                });
            }
        }

        let streams: Vec<Stream> = signal_frames
            .into_iter()
            .map(|(name, frames)| Stream {
                name,
                modality: Modality::CanSignal,
                declared_rate_hz: None,
                clock_id: CLOCK_ID.to_string(),
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
                point_fields: None,
                // A CAN log has no coordinate frame; signals are scalars, not spatial observations.
                media: None,
                frame_id: None,
            })
            .collect();

        let dataset = Dataset {
            id: dir
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("candbc")
                .to_string(),
            metadata: vec![("source_format".into(), "candbc".into())],
            provenance: vec![],
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

        // Fidelity: DBC-coverage gaps (CAN ids in the log with no DBC message definition).
        let unmapped_fields: Vec<UnmappedField> = unknown_ids
            .iter()
            .map(|(id, count)| UnmappedField {
                source_path: format!("can id 0x{id:x}"),
                note: format!("{count} frame(s) with no DBC message definition (coverage gap)"),
            })
            .collect();

        Ok(Ingested {
            dataset,
            report: IngestReport {
                format_id: "candbc",
                source_version: Some("dbc".into()),
                coverage: Coverage::Full,
                mapped_fields: vec![
                    "DBC SG_ signal -> stream (Message.Signal), Intel @1 and Motorola @0 byte order"
                        .into(),
                    "candump frame timestamp -> frame.ts".into(),
                    "decoded signal value -> frame.value_ref.content_hash (SHA-256)".into(),
                ],
                unmapped_fields,
                omitted_fields: vec![
                    "CAN frames are one continuous timeline; no episode segmentation".into(),
                ],
            },
        })
    }
}

/// Whether `dir` directly contains a file with the given extension.
fn dir_has_extension(dir: &Path, ext: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path()
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| x.eq_ignore_ascii_case(ext))
    })
}

/// Locate the single `.dbc` and the CAN log files (`.log` / `.asc`) in `dir`.
fn find_inputs(dir: &Path) -> Result<(std::path::PathBuf, Vec<std::path::PathBuf>), IngestError> {
    let mut dbc: Option<std::path::PathBuf> = None;
    let mut logs: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| IngestError::Io(e.to_string()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        // Never follow a symlink out of the dataset directory: the file it names is not part of the
        // data the caller pointed us at, and reading it would put its contents into the CDM.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            continue;
        }
        match path.extension().and_then(|x| x.to_str()) {
            Some(e) if e.eq_ignore_ascii_case("dbc") => {
                if dbc.is_some() {
                    return Err(IngestError::Parse {
                        format_id: "candbc",
                        message: "more than one .dbc file in the directory; expected exactly one"
                            .into(),
                    });
                }
                dbc = Some(path);
            }
            Some(e) if e.eq_ignore_ascii_case("log") || e.eq_ignore_ascii_case("asc") => {
                logs.push(path)
            }
            _ => {}
        }
    }
    let dbc = dbc.ok_or_else(|| IngestError::Parse {
        format_id: "candbc",
        message: "no .dbc file found in the directory".into(),
    })?;
    if logs.is_empty() {
        return Err(IngestError::Parse {
            format_id: "candbc",
            message: "no CAN log (.log/.asc) found alongside the .dbc".into(),
        });
    }
    logs.sort();
    Ok((dbc, logs))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBC: &str = "\
BO_ 256 EngineData: 8 ECU
 SG_ EngineSpeed : 0|16@1+ (0.25,0) [0|16383.75] \"rpm\" Vector__XXX
 SG_ CoolantTemp : 16|8@1+ (1,-40) [-40|215] \"degC\" Vector__XXX
 SG_ MotorolaSig : 24|8@0+ (1,0) [0|255] \"\" Vector__XXX
";

    #[test]
    fn parses_dbc_messages_and_signals() {
        let m = parse_dbc(DBC);
        let msg = m.get(&256).expect("message 256");
        assert_eq!(msg.name, "EngineData");
        assert_eq!(msg.signals.len(), 3);
        let speed = &msg.signals[0];
        assert_eq!(speed.name, "EngineSpeed");
        assert_eq!(speed.start_bit, 0);
        assert_eq!(speed.length, 16);
        assert!(speed.little_endian);
        assert_eq!(speed.factor, 0.25);
    }

    #[test]
    fn decodes_little_endian_signals_with_scale_and_offset() {
        let m = parse_dbc(DBC);
        let msg = &m[&256];
        let data = [0x40, 0x01, 0x00, 0x00, 0, 0, 0, 0];
        // EngineSpeed: raw 0x0140 = 320, *0.25 = 80 rpm.
        assert_eq!(decode_signal(&msg.signals[0], &data), Some(80.0));
        // CoolantTemp: raw 0x00, +(-40) = -40 degC.
        assert_eq!(decode_signal(&msg.signals[1], &data), Some(-40.0));
    }

    #[test]
    fn decodes_signed_signals() {
        let sig = DbcSignal {
            name: "s".into(),
            start_bit: 0,
            length: 8,
            little_endian: true,
            signed: true,
            factor: 1.0,
            offset: 0.0,
        };
        // 0xFF as signed 8-bit = -1.
        assert_eq!(decode_signal(&sig, &[0xFF]), Some(-1.0));
    }

    #[test]
    fn a_too_short_frame_skips_the_signal() {
        let m = parse_dbc(DBC);
        // CoolantTemp needs bits 16..24 (byte 2); a 1-byte frame can't supply it.
        assert_eq!(decode_signal(&m[&256].signals[1], &[0x40]), None);
    }

    fn sig(start_bit: u32, length: u32, signed: bool) -> DbcSignal {
        DbcSignal {
            name: "s".into(),
            start_bit,
            length,
            little_endian: true,
            signed,
            factor: 1.0,
            offset: 0.0,
        }
    }

    fn be_sig(start_bit: u32, length: u32, signed: bool) -> DbcSignal {
        DbcSignal {
            little_endian: false,
            ..sig(start_bit, length, signed)
        }
    }

    #[test]
    fn extreme_signal_shapes_never_panic() {
        let full = [0xFFu8; 8];
        // Signed 64-bit with the top bit set: `1i64 << 64` must not be evaluated.
        assert_eq!(decode_signal(&sig(0, 64, true), &full), Some(-1.0));
        // Unsigned 64-bit of all-ones is 2^64 - 1, decoded via u64 -> f64 (not `as i64`).
        assert_eq!(
            decode_signal(&sig(0, 64, false), &full),
            Some(u64::MAX as f64)
        );
        // A zero-length signal, an over-wide one, and a start bit at/over 64 are all declined, not
        // panics (each would otherwise reach a shift by >= 64).
        assert_eq!(decode_signal(&sig(64, 0, false), &full), None);
        assert_eq!(decode_signal(&sig(0, 65, false), &full), None);
        assert_eq!(decode_signal(&sig(64, 1, false), &full), None);
        // The same shapes on the Motorola path, where the walk itself must stay in bounds.
        assert_eq!(decode_signal(&be_sig(7, 0, false), &full), None);
        assert_eq!(decode_signal(&be_sig(7, 65, false), &full), None);
        assert_eq!(decode_signal(&be_sig(u32::MAX, 1, false), &full), None);
        // A full-width Motorola signal has to start at byte 0's MSB; starting anywhere else runs off
        // the end of an 8-byte frame and is declined rather than truncated.
        assert_eq!(decode_signal(&be_sig(7, 64, true), &full), Some(-1.0));
        assert_eq!(
            decode_signal(&be_sig(7, 64, false), &full),
            Some(u64::MAX as f64)
        );
        assert_eq!(decode_signal(&be_sig(0, 64, false), &full), None);
    }

    #[test]
    fn decodes_big_endian_motorola_signals() {
        // A byte-aligned 16-bit Motorola signal starting at bit 7 spans bytes 0..2, MSB first:
        // 0x1234 = 4660.
        let data = [0x12, 0x34, 0x00, 0x00, 0, 0, 0, 0];
        assert_eq!(decode_signal(&be_sig(7, 16, false), &data), Some(4660.0));
        // The same bytes read as Intel from bit 0 give the byte-swapped 0x3412 — the two orders are
        // genuinely different, so a mislabeled signal cannot accidentally agree.
        assert_eq!(decode_signal(&sig(0, 16, false), &data), Some(13330.0)); // 0x3412
                                                                             // Signed: 0xFF80 as a 16-bit two's-complement value is -128.
        let neg = [0xFF, 0x80, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_signal(&be_sig(7, 16, true), &neg), Some(-128.0));
    }

    #[test]
    fn a_motorola_signal_crossing_a_byte_boundary_walks_the_sawtooth() {
        // 12-bit Motorola signal starting at bit 3 of byte 0: the low nibble of byte 0 (high bits)
        // followed by all of byte 1. Bytes 0xAB, 0xCD -> 0xB<<8 | 0xCD = 0xBCD = 3021.
        let data = [0xAB, 0xCD, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode_signal(&be_sig(3, 12, false), &data), Some(3021.0));
        // Two bytes are exactly enough; one byte is not, and is declined rather than truncated.
        assert_eq!(
            decode_signal(&be_sig(3, 12, false), &data[..2]),
            Some(3021.0)
        );
        assert_eq!(decode_signal(&be_sig(3, 12, false), &data[..1]), None);
    }

    #[test]
    fn scale_and_offset_apply_to_motorola_signals_too() {
        let m = parse_dbc(DBC);
        // `MotorolaSig : 24|8@0+ (1,0)` — start bit 24 is byte 3's LSB-numbered bit 0, so an
        // 8-bit big-endian signal there walks byte 3 then byte 4's high bits... but with start bit
        // 24 (bit_in_byte 0) the very next step wraps to byte 4 bit 7.
        let mut scaled = m[&256].signals[2].clone();
        scaled.factor = 0.5;
        scaled.offset = -1.0;
        // byte 3 bit 0 = 1, then byte 4 bits 7..1 = 0b1111111 -> 0b1_1111111 truncated to 8 bits:
        // bits collected = 1 then 1111111 = 0xFF -> 255 * 0.5 - 1 = 126.5.
        let data = [0, 0, 0, 0x01, 0xFE, 0, 0, 0];
        assert_eq!(decode_signal(&scaled, &data), Some(126.5));
    }

    #[test]
    fn parses_candump_lines() {
        let f = parse_candump_line("(1000.000000) can0 100#40010000").expect("frame");
        assert_eq!(f.id, 0x100);
        assert_eq!(f.data, vec![0x40, 0x01, 0x00, 0x00]);
        assert_eq!(f.ts_ns, 1_000_000_000_000);
        assert!(parse_candump_line("# a comment").is_none());
        assert!(parse_candump_line("").is_none());
    }
}
