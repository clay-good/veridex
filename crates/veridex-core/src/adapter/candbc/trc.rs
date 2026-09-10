//! Reading a PEAK **PCAN-Trace** (`.trc`) log — what PCAN-View and the PEAK driver stack write.
//!
//! A `.trc` is a text log with a `;`-prefixed header block and one line per frame. Five layouts are
//! in the wild and this reader handles all of them: versions 1.0, 1.1 and 1.3 put their fields in
//! fixed columns, while 2.0 and 2.1 declare their own column order in a `;$COLUMNS=` line and are
//! read from that rather than from an assumed position.
//!
//! Timestamps are milliseconds from the measurement's start, and the start itself is a
//! `;$STARTTIME=` OLE automation date — days since 1899-12-30. Both are read, so a `.trc` lands on
//! the same wall clock a candump log does and the two can be read as one recording. A file that
//! states no start time keeps the measurement-relative clock its lines carry, which is internally
//! consistent — what every temporal check measures — rather than a fabricated epoch.
//!
//! Field layouts follow the format as implemented by the reference reader (`python-can`'s
//! `can/io/trc.py`), not this module's own conventions: a fixture written to a reader's assumptions
//! agrees with itself whatever they are.

use std::collections::BTreeMap;
use std::path::Path;

use super::{parse_hex_bytes, CanFrame};

/// The largest payload a CAN-FD frame carries; a line declaring more is not a frame this reader can
/// represent.
const MAX_FD_BYTES: usize = 64;

/// Days from the OLE automation epoch (1899-12-30) to the Unix epoch (1970-01-01).
const OLE_EPOCH_OFFSET_DAYS: f64 = 25_569.0;

/// What one `.trc` yielded.
pub(super) struct TrcLog {
    /// The bus frames, in file order.
    pub frames: Vec<CanFrame>,
    /// Lines that carried traffic this reader did not turn into a frame, by reason.
    pub skipped: BTreeMap<&'static str, u64>,
    /// Lines that were neither a header, a comment, nor a frame.
    pub unparsed: u64,
    /// Content lines seen at all, so a caller can tell "none of it parsed" from "it was empty".
    pub content_lines: u64,
}

/// Whether `text` looks like a PCAN-Trace log rather than a candump one.
///
/// Decided on the content: a `.trc` opens with a `;` header block, and a candump line never starts
/// with one. The extension is not consulted, for the same reason the BLF dispatch ignores it.
pub(super) fn looks_like(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|first| first.starts_with(';'))
}

/// Read every CAN frame `text` holds.
pub(super) fn read(text: &str, path: &Path) -> TrcLog {
    let _ = path;
    let mut log = TrcLog {
        frames: Vec::new(),
        skipped: BTreeMap::new(),
        unparsed: 0,
        content_lines: 0,
    };
    // `None` until the header declares one; the lines are then read relative to zero.
    let mut start_ns: i64 = 0;
    let mut version = (1u32, 0u32);
    let mut columns: Option<Vec<String>> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(directive) = line.strip_prefix(";$") {
            let (key, value) = directive.split_once('=').unwrap_or((directive, ""));
            match key.trim().to_ascii_uppercase().as_str() {
                "FILEVERSION" => version = parse_version(value.trim()),
                "STARTTIME" => start_ns = ole_date_ns(value.trim()).unwrap_or(0),
                "COLUMNS" => {
                    columns = Some(
                        value
                            .split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect(),
                    )
                }
                _ => {}
            }
            continue;
        }
        if line.starts_with(';') {
            continue; // an ordinary comment line, and the header block is all comments
        }
        log.content_lines += 1;
        match parse_line(line, version, columns.as_deref(), start_ns) {
            Ok(frame) => log.frames.push(frame),
            Err(reason) => {
                if reason == UNPARSED {
                    log.unparsed += 1;
                } else {
                    *log.skipped.entry(reason).or_insert(0) += 1;
                }
            }
        }
    }
    log
}

/// A line that is not a frame at all, as opposed to a frame this reader declines to decode.
const UNPARSED: &str = "unparsed";
/// A remote-transmission frame: it requests data and carries none.
const REMOTE: &str = "remote-transmission frame(s), which request data and carry none";
/// A frame whose declared length no CAN frame can hold.
const OVERLONG: &str = "frame(s) declaring more payload than a CAN frame can hold";
/// A status or error record rather than bus traffic.
const NOT_TRAFFIC: &str = "record(s) that report the adapter's status rather than bus traffic";

/// `;$FILEVERSION=2.1` → `(2, 1)`.
fn parse_version(text: &str) -> (u32, u32) {
    let (major, minor) = text.split_once('.').unwrap_or((text, "0"));
    (
        major.trim().parse().unwrap_or(1),
        minor.trim().parse().unwrap_or(0),
    )
}

/// An OLE automation date — days since 1899-12-30 — as nanoseconds from the Unix epoch.
///
/// Returns `None` for anything that is not a positive date, so a header a writer left blank leaves
/// the recording on its own relative clock instead of placing it in 1899.
fn ole_date_ns(text: &str) -> Option<i64> {
    let days: f64 = text.parse().ok()?;
    if !days.is_finite() || days <= OLE_EPOCH_OFFSET_DAYS {
        return None;
    }
    let seconds = (days - OLE_EPOCH_OFFSET_DAYS) * 86_400.0;
    (seconds.is_finite() && seconds < 1e12).then_some((seconds * 1e9) as i64)
}

/// Parse one data line into a frame, or name why it is not one.
fn parse_line(
    line: &str,
    version: (u32, u32),
    columns: Option<&[String]>,
    start_ns: i64,
) -> Result<CanFrame, &'static str> {
    let cols: Vec<&str> = line.split_whitespace().collect();
    if version.0 >= 2 {
        return parse_v2(&cols, columns.ok_or(UNPARSED)?, start_ns);
    }
    parse_v1(&cols, version, start_ns)
}

/// Versions 1.0, 1.1 and 1.3, whose fields sit in fixed positions.
///
/// The layouts differ by what is inserted between the time and the id: 1.1 adds a direction, and 1.3
/// adds a channel *and* a reserved field. The message number ends with `)` in every one of them,
/// which is what tells a data line from a stray line of prose.
fn parse_v1(cols: &[&str], version: (u32, u32), start_ns: i64) -> Result<CanFrame, &'static str> {
    if cols.len() < 4 || !cols[0].ends_with(')') {
        return Err(UNPARSED);
    }
    let ts_ns = millis_ns(cols[1], start_ns).ok_or(UNPARSED)?;
    // (index of the id, the bus this line names)
    let (id_at, bus) = match version {
        (1, 0) => (2usize, 1u32),
        (1, 1) => (3, 1),
        _ => (4, cols.get(2).and_then(|c| c.parse().ok()).unwrap_or(1)),
    };
    let id_text = cols.get(id_at).ok_or(UNPARSED)?;
    let id = u32::from_str_radix(id_text.trim(), 16).map_err(|_| UNPARSED)?;
    // 1.3 keeps a reserved field between the id and the length.
    let dlc_at = if version == (1, 3) {
        id_at + 2
    } else {
        id_at + 1
    };
    let dlc: usize = cols
        .get(dlc_at)
        .and_then(|c| c.parse().ok())
        .ok_or(UNPARSED)?;
    let rest = &cols[(dlc_at + 1).min(cols.len())..];
    frame_from(rest, id, dlc, bus, ts_ns)
}

/// Versions 2.0 and 2.1, whose `;$COLUMNS=` line declares the order of what follows.
fn parse_v2(cols: &[&str], names: &[String], start_ns: i64) -> Result<CanFrame, &'static str> {
    let at = |key: &str| {
        names
            .iter()
            .position(|n| n == key)
            .and_then(|i| cols.get(i))
    };
    let ts_ns = millis_ns(at("O").ok_or(UNPARSED)?, start_ns).ok_or(UNPARSED)?;
    let id = u32::from_str_radix(at("I").ok_or(UNPARSED)?.trim(), 16).map_err(|_| UNPARSED)?;
    // A `.trc` records more than bus traffic: bus load, error frames and adapter status all appear
    // as lines with their own type. Decoding one as a message would put its bytes through the DBC.
    if let Some(kind) = at("T").map(|t| t.trim().to_ascii_uppercase()) {
        match kind.as_str() {
            "DT" | "FD" | "FB" | "FE" => {}
            "RR" => return Err(REMOTE),
            _ => return Err(NOT_TRAFFIC),
        }
    }
    let dlc: usize = at("L")
        .or_else(|| at("l"))
        .and_then(|c| c.trim().parse().ok())
        .ok_or(UNPARSED)?;
    let bus: u32 = at("B").and_then(|c| c.trim().parse().ok()).unwrap_or(1);
    // The data column is the last declared one, and a payload is several whitespace-separated bytes
    // — so everything from where it starts is the payload.
    let data_at = names.iter().position(|n| n == "D").ok_or(UNPARSED)?;
    let rest = cols.get(data_at..).unwrap_or(&[]);
    frame_from(rest, id, dlc, bus, ts_ns)
}

/// The payload bytes and the frame they belong to, given the columns that follow the length.
///
/// A `RTR` marker in place of the payload is a remote frame: it *requests* data and carries none, so
/// decoding signals out of the bytes that are not there would put fabricated samples into the
/// streams the checks grade.
fn frame_from(
    rest: &[&str],
    id: u32,
    dlc: usize,
    bus: u32,
    ts_ns: i64,
) -> Result<CanFrame, &'static str> {
    if rest
        .first()
        .is_some_and(|c| c.eq_ignore_ascii_case("rtr") || c.eq_ignore_ascii_case("r"))
    {
        return Err(REMOTE);
    }
    if dlc > MAX_FD_BYTES {
        return Err(OVERLONG);
    }
    let joined: String = rest.concat();
    let data = parse_hex_bytes(&joined).ok_or(UNPARSED)?;
    // The declared length is what says how much of the line is payload; a writer may pad the column.
    let data = data.get(..dlc.min(data.len())).unwrap_or(&[]).to_vec();
    Ok(CanFrame {
        ts_ns,
        // The bus, numbered as PEAK numbers it. Named verbatim rather than translated to a `canN`
        // interface, for the reason the BLF reader names its channels: which Linux interface a PEAK
        // bus corresponds to is not something the file says.
        iface: format!("bus{bus}"),
        id: id & 0x1FFF_FFFF,
        data,
    })
}

/// A millisecond offset, as nanoseconds on the recording's clock.
fn millis_ns(text: &str, start_ns: i64) -> Option<i64> {
    let ms: f64 = text.trim().parse().ok()?;
    if !ms.is_finite() {
        return None;
    }
    Some(start_ns.saturating_add((ms * 1e6) as i64))
}
