//! The demo ASAM MDF 4.x (MF4) measurement — written the
//! way a real logger writes one, with the records deflated into `##DZ` chunks chained by an `##HL`
//! header list and a `##DL` data list. [`VARIANTS`]:
//!
//! - `saturated` (the default) — a ~4 s, 100 Hz vehicle raster whose `steering_angle` is pinned at
//!   its positive end-stop for most of the drive → `STATISTICAL.SATURATED`. The controller cannot tell
//!   "at the limit" from "wants to go further", so a policy trained on it imitates an observation
//!   that stopped tracking intent.
//! - `clean` — the same raster with the wheel actually turning, no findings.
//! - `gap` — the same raster with ~0.5 s of records missing from the middle, the shape a logger
//!   writes when a buffer overruns → `TEMPORAL.GAP`.
//! - `uncompressed` — `clean`, written into a bare `##DT` instead. The two files decode to the same
//!   measurement — same streams, same frames, same content hashes — so `veridex diff` between them
//!   reports no change in the data, which is the point: how the bytes were stored is not what they
//!   mean.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_mf4 -- <output.mf4> [saturated|clean|gap|uncompressed]`

use std::io::Write;
use std::path::Path;

use crate::DemoError;

/// Every variant `write` accepts. `saturated` is the default and the one the docs show.
pub const VARIANTS: &[&str] = &["saturated", "clean", "gap", "uncompressed"];

/// `cn_data_type`: little-endian unsigned / signed integer, little-endian IEEE float.
const UINT_LE: u8 = 0;
const INT_LE: u8 = 2;
const FLOAT_LE: u8 = 4;

/// One record: `[0..8) t f64 | [8..10) speed u16 | [10..12) steering i16 | [12..14) rpm u16 | pad`.
const RECORD_LEN: usize = 16;

/// The raster: 100 Hz for four seconds, chunked the way a logger flushes.
const HZ: usize = 100;
const SECONDS: usize = 4;
const RECORDS_PER_CHUNK: usize = 150;

pub fn write(path: &Path, variant: &str) -> Result<(), DemoError> {
    crate::check_variant(variant, VARIANTS)?;
    let uncompressed = variant == "uncompressed";
    let clean = variant == "clean" || uncompressed;
    let gap = variant == "gap";

    let records = records(clean || gap, gap);
    let count = records.len() / RECORD_LEN;

    let mut b = Mf4Builder::new();
    let data_at = if uncompressed {
        b.block(b"##DT", &[], &records)
    } else {
        // What a logger actually produces: the drive is flushed in chunks, each deflated, and the
        // chunks are chained by a data list behind a header list.
        let chunks: Vec<u64> = records
            .chunks(RECORDS_PER_CHUNK * RECORD_LEN)
            .map(|chunk| b.zipped_data(chunk))
            .collect();
        let dl = b.data_list(&chunks, (RECORDS_PER_CHUNK * RECORD_LEN) as u64);
        b.header_list(dl)
    };

    // What the file says about where its samples came from: an ECU on the powertrain CAN bus. This
    // is the one provenance element an MF4 carries natively, so the demo carries it.
    let si = b.source("Powertrain ECU", "chassis-can", 1, 2);

    // `phys = 0 + 0.1 x raw` on the speed channel: a raw count of tenths of a km/h.
    let speed_cc = b.linear_conversion(0.0, 0.1);
    let rpm = b.channel("engine_rpm", false, UINT_LE, 12, 16, None);
    let steering = b.channel("steering_angle", false, INT_LE, 10, 16, None);
    let speed = b.channel("vehicle_speed", false, UINT_LE, 8, 16, Some(speed_cc));
    let time = b.channel("t", true, FLOAT_LE, 0, 64, None);
    b.patch_link(time, 0, speed);
    b.patch_link(speed, 0, steering);
    b.patch_link(steering, 0, rpm);

    let cg = b.channel_group(count as u64, RECORD_LEN as u32);
    b.patch_link(cg, 1, time);
    b.patch_link(cg, 3, si); // cg_si_acq_source
    let dg = b.data_group();
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, data_at);

    // The header comment a real logger writes: an XML `<common_properties>` list beside a free-text
    // description. Without it the demo modelled a file no recorder produces — the one place MF4
    // states its time source, its operator and its licence, left empty — and the fixture that stands
    // in for a real measurement has to carry what a real measurement carries.
    let md = b.header_comment();

    // 2024-03-01T12:00:00Z, in nanoseconds since the epoch.
    let bytes = b.finish(dg, 1_709_294_400_000_000_000, md);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(path, &bytes)?;
    Ok(())
}

/// How the records of the measurement `write` just produced are stored, for a caller to report.
pub fn storage(variant: &str) -> &'static str {
    if variant == "uncompressed" {
        "uncompressed ##DT"
    } else {
        "##HL/##DL of deflated ##DZ chunks"
    }
}

/// The measurement's records. `turning` swings the wheel instead of pinning it at its end-stop;
/// `gap` drops half a second out of the middle, the way a logger does when a buffer overruns.
fn records(turning: bool, gap: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..HZ * SECONDS {
        // The dropped window: 1.50 s to 2.00 s never reaches the file.
        if gap && (150..200).contains(&i) {
            continue;
        }
        let t = i as f64 / HZ as f64;
        // A gentle acceleration, in tenths of a km/h so the linear conversion has work to do.
        let speed_raw = (300.0 + 40.0 * t) as u16;
        // 2048 is the sensor's positive end-stop. Pinned there for most of the drive unless the
        // wheel is actually turning.
        let steering: i16 = if turning {
            (1800.0 * crate::mcap::wave(t * 1.1)) as i16
        } else if i < 300 {
            2048
        } else {
            2048 - (i as i16 - 300) * 5
        };
        let rpm = (1500.0 + 300.0 * crate::mcap::wave(t * 2.3)) as u16;

        out.extend_from_slice(&t.to_le_bytes());
        out.extend_from_slice(&speed_raw.to_le_bytes());
        out.extend_from_slice(&steering.to_le_bytes());
        out.extend_from_slice(&rpm.to_le_bytes());
        out.extend_from_slice(&[0u8; 2]); // pad to the 16-byte record
    }
    out
}

/// An MF4 file under construction. Blocks are appended in order and their offsets recorded, so links
/// can be patched in once their targets exist.
struct Mf4Builder {
    bytes: Vec<u8>,
}

/// Header + 6 links + 32 bytes of data. The `##HD` always sits at offset 64, so the builder reserves
/// this hole up front and fills it last.
const HD_LEN: usize = 24 + 6 * 8 + 32;

impl Mf4Builder {
    fn new() -> Self {
        let mut bytes = vec![0u8; 64 + HD_LEN];
        bytes[0..8].copy_from_slice(b"MDF     ");
        bytes[8..16].copy_from_slice(b"4.10    ");
        bytes[16..24].copy_from_slice(b"veridex ");
        bytes[28..30].copy_from_slice(&410u16.to_le_bytes());
        Self { bytes }
    }

    /// Append a block: 4-byte id, reserved, total length, link count, `links`, then `data`.
    fn block(&mut self, id: &[u8; 4], links: &[u64], data: &[u8]) -> u64 {
        let at = self.bytes.len() as u64;
        let length = 24 + (links.len() * 8) + data.len();
        self.bytes.extend_from_slice(id);
        self.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.bytes.extend_from_slice(&(length as u64).to_le_bytes());
        self.bytes
            .extend_from_slice(&(links.len() as u64).to_le_bytes());
        for l in links {
            self.bytes.extend_from_slice(&l.to_le_bytes());
        }
        self.bytes.extend_from_slice(data);
        at
    }

    fn patch_link(&mut self, at: u64, n: usize, value: u64) {
        let off = at as usize + 24 + n * 8;
        self.bytes[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn text(&mut self, s: &str) -> u64 {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.block(b"##TX", &[], &data)
    }

    /// A `##DZ` holding `records` deflated, staged column-major first (`dz_zip_type` 1) because
    /// like-typed bytes compress far better adjacently — which is what a logger writes.
    fn zipped_data(&mut self, records: &[u8]) -> u64 {
        let lines = records.len() / RECORD_LEN;
        let mut staged = Vec::with_capacity(records.len());
        for column in 0..RECORD_LEN {
            for line in 0..lines {
                staged.push(records[line * RECORD_LEN + column]);
            }
        }
        staged.extend_from_slice(&records[lines * RECORD_LEN..]);

        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&staged).expect("deflate");
        let payload = enc.finish().expect("deflate");

        let mut data = Vec::new();
        data.extend_from_slice(b"DT"); // dz_org_block_type
        data.push(1); // dz_zip_type: transposition + deflate
        data.push(0);
        data.extend_from_slice(&(RECORD_LEN as u32).to_le_bytes()); // dz_zip_parameter
        data.extend_from_slice(&(records.len() as u64).to_le_bytes());
        data.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        data.extend_from_slice(&payload);
        self.block(b"##DZ", &[], &data)
    }

    /// A `##DL` chaining `parts` as one record stream.
    fn data_list(&mut self, parts: &[u64], equal_length: u64) -> u64 {
        let mut links = vec![0u64]; // dl_dl_next
        links.extend_from_slice(parts);
        let mut data = vec![1u8, 0, 0, 0]; // dl_flags: equal length
        data.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        data.extend_from_slice(&equal_length.to_le_bytes());
        self.block(b"##DL", &links, &data)
    }

    fn header_list(&mut self, dl: u64) -> u64 {
        let mut data = Vec::new();
        data.extend_from_slice(&0u16.to_le_bytes()); // hl_flags
        data.push(1); // hl_zip_type, matching the blocks it lists
        data.extend_from_slice(&[0u8; 5]);
        self.block(b"##HL", &[dl], &data)
    }

    /// A `##SI` source-information block: which device acquired these samples, and on which bus.
    /// `si_type` 1 is an ECU; `si_bus_type` 2 is CAN.
    fn source(&mut self, name: &str, path: &str, si_type: u8, bus_type: u8) -> u64 {
        let name_at = self.text(name);
        let path_at = self.text(path);
        let mut data = vec![si_type, bus_type, 0];
        data.extend_from_slice(&[0u8; 5]);
        self.block(b"##SI", &[name_at, path_at, 0], &data)
    }

    /// A `##CC` linear conversion: `phys = p1 + p2 x raw`.
    fn linear_conversion(&mut self, p1: f64, p2: f64) -> u64 {
        let mut data = Vec::new();
        data.push(1u8); // cc_type = linear
        data.push(0u8); // precision
        data.extend_from_slice(&0u16.to_le_bytes()); // flags
        data.extend_from_slice(&0u16.to_le_bytes()); // ref_count
        data.extend_from_slice(&2u16.to_le_bytes()); // val_count
        data.extend_from_slice(&0f64.to_le_bytes()); // phy_range_min
        data.extend_from_slice(&0f64.to_le_bytes()); // phy_range_max
        data.extend_from_slice(&p1.to_le_bytes());
        data.extend_from_slice(&p2.to_le_bytes());
        self.block(b"##CC", &[0, 0, 0, 0], &data)
    }

    /// A `##CN`. Returns its offset; `cn_next` is patched by the caller. A `master` channel is the
    /// group's time base (`cn_type` 2, `cn_sync_type` 1 — time); every other is a measured value.
    fn channel(
        &mut self,
        name: &str,
        master: bool,
        data_type: u8,
        byte_offset: u32,
        bit_count: u32,
        conversion: Option<u64>,
    ) -> u64 {
        let name_at = self.text(name);
        let (channel_type, sync_type) = if master { (2u8, 1u8) } else { (0u8, 0u8) };
        let mut data = vec![channel_type, sync_type, data_type, 0];
        data.extend_from_slice(&byte_offset.to_le_bytes());
        data.extend_from_slice(&bit_count.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // cn_flags
        data.extend_from_slice(&0u32.to_le_bytes()); // inval_bit_pos
        data.extend_from_slice(&[0u8; 4]); // precision, reserved, attachment_count
        data.extend_from_slice(&[0u8; 48]); // value / limit ranges
                                            // cn_next, composition, tx_name, si_source, cc_conversion, data, md_unit, md_comment.
        self.block(
            b"##CN",
            &[0, 0, name_at, 0, conversion.unwrap_or(0), 0, 0, 0],
            &data,
        )
    }

    fn channel_group(&mut self, cycle_count: u64, data_bytes: u32) -> u64 {
        let mut data = Vec::new();
        data.extend_from_slice(&0u64.to_le_bytes()); // cg_record_id
        data.extend_from_slice(&cycle_count.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // cg_flags
        data.extend_from_slice(&0u16.to_le_bytes()); // path separator
        data.extend_from_slice(&0u32.to_le_bytes()); // reserved
        data.extend_from_slice(&data_bytes.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // cg_inval_bytes
        self.block(b"##CG", &[0, 0, 0, 0, 0, 0], &data)
    }

    /// A sorted `##DG` — record id size 0, so its records carry no interleaving prefix.
    fn data_group(&mut self) -> u64 {
        let mut data = vec![0u8];
        data.extend_from_slice(&[0u8; 7]);
        self.block(b"##DG", &[0, 0, 0, 0], &data)
    }

    /// The `##MD` header comment, as CANape or a fleet logger writes one.
    fn header_comment(&mut self) -> u64 {
        let xml = "<HDcomment><TX>Chassis dynamometer pull, cell 4</TX>\
                   <common_properties>\
                   <e name=\"time_source\">PTP grandmaster</e>\
                   <e name=\"operator\">A. Operator</e>\
                   <e name=\"license\">CC-BY-4.0</e>\
                   </common_properties></HDcomment>";
        let mut payload = xml.as_bytes().to_vec();
        payload.push(0);
        self.block(b"##MD", &[], &payload)
    }

    fn finish(mut self, dg_first: u64, start_time_ns: u64, md_comment: u64) -> Vec<u8> {
        let mut hd = Vec::with_capacity(HD_LEN);
        hd.extend_from_slice(b"##HD");
        hd.extend_from_slice(&0u32.to_le_bytes());
        hd.extend_from_slice(&(HD_LEN as u64).to_le_bytes());
        hd.extend_from_slice(&6u64.to_le_bytes());
        for l in [dg_first, 0, 0, 0, 0, md_comment] {
            hd.extend_from_slice(&l.to_le_bytes());
        }
        hd.extend_from_slice(&start_time_ns.to_le_bytes());
        hd.extend_from_slice(&[0u8; 24]);
        self.bytes[64..64 + HD_LEN].copy_from_slice(&hd);
        self.bytes
    }
}
