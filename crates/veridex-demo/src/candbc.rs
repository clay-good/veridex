//! The demo **CAN + DBC** drive, written as a Vector **BLF** — the binary container almost all
//! vehicle-bus traffic is actually recorded in.
//!
//! Why a BLF rather than a candump text log: the text form is what `can-utils` writes on Linux, and
//! the sweep already carries one. A BLF is a different reader — an object stream inside zlib
//! containers, with its own timestamp base and its own CAN-FD object types — so a drive recorded
//! this way exercises a decode path no text fixture can reach. It is written from the format's own
//! layout, never from the reader's idea of it, which is the point of every generator here.
//!
//! The database describes one message wider than a classic CAN frame, so half its signals live in
//! bytes only CAN-FD carries. A reader that truncates an FD payload to eight bytes, or that reads
//! the FD DLC *code* as a length, produces no samples for those signals at all — and a signal with
//! no samples has no stream, which is silence rather than a finding. This fixture is what makes
//! that silence visible.
//!
//! Variants:
//!
//! - `drive` (the default) — ten seconds of a healthy powertrain bus: engine speed and coolant
//!   temperature in the first eight bytes, and a four-wheel speed set in the bytes past them, at
//!   100 Hz on channel 1. Classic frames and CAN-FD frames in the same recording, in compressed
//!   containers, with one object deliberately straddling a container boundary.
//! - `railed-wheel` — the same drive with the front-left wheel-speed sensor pinned at its rail for
//!   seven frames in ten, the shape of a sensor that failed high. It is an **FD-only** signal —
//!   `WheelSpeedFL` starts at bit 96 — so the finding appears at all only if the reader read past
//!   byte eight → `STATISTICAL.SATURATED`.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_candbc -- <output-dir> [drive|railed-wheel]`

use std::io::Write;
use std::path::Path;

use crate::{check_variant, fresh_dir, DemoError};

/// Every variant [`write()`] accepts. `drive` is the default the docs show.
pub const VARIANTS: &[&str] = &["drive", "railed-wheel"];

/// The measurement start the BLF header declares: 2026-01-01T00:00:00Z.
///
/// A real recording carries a wall-clock start, and its object timestamps are relative to it. A
/// fixture that started at zero would give every frame a timestamp in 1970 — indistinguishable, to
/// every temporal check, from a recorder whose clock was never set.
const START: (u16, u16, u16, u16, u16, u16, u16, u16) = (2026, 1, 4, 1, 0, 0, 0, 0);

/// Frames per second the bus carries, and how many of them the drive holds.
const HZ: u64 = 100;
const FRAMES: u64 = 1_000;

/// The message id the drive carries. `0x123`, as ordinary as a CAN id gets.
const MSG_ID: u32 = 0x123;

/// The signal database. `PowertrainData` is 24 bytes — wider than a classic frame, which is what
/// puts the wheel speeds in territory only CAN-FD reaches.
const DBC: &str = "VERSION \"\"\n\n\
BO_ 291 PowertrainData: 24 ECU\n\
 SG_ EngineRPM : 0|16@1+ (0.25,0) [0|16383.75] \"rpm\" Vector__XXX\n\
 SG_ CoolantTemp : 16|8@1+ (1,-40) [-40|215] \"degC\" Vector__XXX\n\
 SG_ WheelSpeedFL : 96|16@1+ (0.01,0) [0|655.35] \"km/h\" Vector__XXX\n\
 SG_ WheelSpeedFR : 112|16@1+ (0.01,0) [0|655.35] \"km/h\" Vector__XXX\n\
 SG_ WheelSpeedRL : 128|16@1+ (0.01,0) [0|655.35] \"km/h\" Vector__XXX\n\
 SG_ WheelSpeedRR : 144|16@1+ (0.01,0) [0|655.35] \"km/h\" Vector__XXX\n";

/// Write the demo dataset — a `.dbc` and a `.blf` — into `dir`.
pub fn write(dir: &Path, variant: &str) -> Result<(), DemoError> {
    check_variant(variant, VARIANTS)?;
    fresh_dir(dir)?;
    std::fs::write(dir.join("vehicle.dbc"), DBC)?;
    std::fs::write(dir.join("drive.blf"), blf(variant))?;
    Ok(())
}

/// The whole BLF: its file header, then the frames in compressed containers.
fn blf(variant: &str) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = (0..FRAMES).map(|i| frame(i, variant)).collect();
    let stream: Vec<u8> = objects.concat();
    // Split mid-object on purpose: a writer's buffer fills wherever it fills, so a real recording's
    // containers cut through an object routinely. Reading each container on its own drops that
    // frame, which is a defect worth a fixture rather than a comment.
    let cut = stream.len() / 2 + 7;
    let mut out = file_header();
    out.extend(container(&stream[..cut]));
    out.extend(container(&stream[cut..]));
    out
}

/// One frame, as a `CanFdMessage` object on channel 1.
///
/// Every frame is CAN-FD: this bus is 24 bytes wide, which a classic frame cannot carry. The DLC is
/// written as the *code* for 24 bytes (13), so a reader that mistakes the code for a length keeps
/// thirteen and loses the wheel speeds.
fn frame(i: u64, variant: &str) -> Vec<u8> {
    let t = i as f64 / HZ as f64;
    // A drive that accelerates and settles, so the values are a recording rather than a constant.
    let rpm = (900.0 + 2_400.0 * (1.0 - (-t / 3.0).exp())) / 0.25;
    let coolant = (75.0 + 15.0 * (1.0 - (-t / 6.0).exp()) + 40.0).round();
    let speed = 60.0 + 40.0 * (1.0 - (-t / 4.0).exp());
    // The front-left sensor fails high in the `railed-wheel` variant: seven frames in ten it reports
    // its 16-bit maximum, the reading a shorted signal line produces.
    let fl = if variant == "railed-wheel" && i % 10 < 7 {
        u16::MAX as f64
    } else {
        speed / 0.01
    };

    let mut data = vec![0u8; 24];
    put_u16(&mut data, 0, rpm as u16);
    data[2] = coolant as u8;
    put_u16(&mut data, 12, fl as u16);
    put_u16(&mut data, 14, (speed / 0.01) as u16);
    put_u16(&mut data, 16, (speed / 0.01) as u16);
    put_u16(&mut data, 18, (speed / 0.01) as u16);

    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_le_bytes()); // channel 1
    payload.push(0); // flags
    payload.push(13); // DLC code for 24 bytes, not the length
    payload.extend_from_slice(&MSG_ID.to_le_bytes());
    payload.extend_from_slice(&(data.len() as u32).to_le_bytes()); // frame length
    payload.push(0); // bit count
    payload.push(0); // FD flags
    payload.push(data.len() as u8); // valid data bytes — what says how much of the 64 is payload
    payload.extend_from_slice(&[0u8; 5]); // reserved
    let mut sixty_four = [0u8; 64];
    sixty_four[..data.len()].copy_from_slice(&data);
    payload.extend_from_slice(&sixty_four);

    object(100, i * 1_000_000_000 / HZ, &payload)
}

/// Little-endian `u16` at `at`.
fn put_u16(data: &mut [u8], at: usize, v: u16) {
    data[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

/// One object: the sixteen-byte base header, a version-1 object header carrying the timestamp, the
/// payload, and the padding that puts the next object on a four-byte boundary.
fn object(kind: u32, ts_ns: u64, payload: &[u8]) -> Vec<u8> {
    let header_size: u16 = 32;
    let size = header_size as u32 + payload.len() as u32;
    let mut o = Vec::with_capacity(size as usize + 3);
    o.extend_from_slice(b"LOBJ");
    o.extend_from_slice(&header_size.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes()); // header version
    o.extend_from_slice(&size.to_le_bytes());
    o.extend_from_slice(&kind.to_le_bytes());
    o.extend_from_slice(&2u32.to_le_bytes()); // flags: timestamps are nanoseconds
    o.extend_from_slice(&0u16.to_le_bytes()); // client index
    o.extend_from_slice(&0u16.to_le_bytes()); // object version
    o.extend_from_slice(&ts_ns.to_le_bytes());
    o.extend_from_slice(payload);
    while o.len() % 4 != 0 {
        o.push(0);
    }
    o
}

/// A `LogContainer` holding `objects`, zlib-compressed as a real writer leaves them.
fn container(objects: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(objects)
        .expect("writing to a Vec cannot fail");
    let compressed = encoder.finish().expect("zlib encoder finishes");

    let mut body = Vec::new();
    body.extend_from_slice(&2u16.to_le_bytes()); // compression method: zlib
    body.extend_from_slice(&[0u8; 6]); // reserved
    body.extend_from_slice(&(objects.len() as u32).to_le_bytes()); // uncompressed size
    body.extend_from_slice(&[0u8; 4]); // reserved
    body.extend_from_slice(&compressed);

    // A container carries the base object header only, with no timestamp of its own.
    let mut o = Vec::new();
    o.extend_from_slice(b"LOBJ");
    o.extend_from_slice(&16u16.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&(16 + body.len() as u32).to_le_bytes());
    o.extend_from_slice(&10u32.to_le_bytes()); // LOG_CONTAINER
    o.extend_from_slice(&body);
    while o.len() % 4 != 0 {
        o.push(0);
    }
    o
}

/// The BLF file header: the `LOGG` signature, its own size, the version fields, the counts, and the
/// measurement start and stop times as Windows `SYSTEMTIME`s.
fn file_header() -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(b"LOGG");
    h.extend_from_slice(&144u32.to_le_bytes()); // header size, as a real writer states it
    h.extend_from_slice(&[0u8; 8]); // application id and version bytes
    h.extend_from_slice(&0u64.to_le_bytes()); // file size, which a finished writer backfills
    h.extend_from_slice(&0u64.to_le_bytes()); // uncompressed size
    h.extend_from_slice(&(FRAMES as u32).to_le_bytes()); // object count
    h.extend_from_slice(&(FRAMES as u32).to_le_bytes()); // objects read
    let (y, mo, dow, d, hh, mm, ss, ms) = START;
    for part in [y, mo, dow, d, hh, mm, ss, ms] {
        h.extend_from_slice(&part.to_le_bytes());
    }
    // The stop time, ten seconds later.
    for part in [y, mo, dow, d, hh, mm, (FRAMES / HZ) as u16, ms] {
        h.extend_from_slice(&part.to_le_bytes());
    }
    h.resize(144, 0);
    h
}
