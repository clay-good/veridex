//! Behavior tests for the ASAM MDF 4.x (MF4) adapter, driven by real MF4 files built byte by byte.
//!
//! There is no MF4 writer crate in the dependency set, so the fixtures here assemble the block graph
//! directly — which is also the point: the adapter is tested against the on-disk layout, not against
//! a writer that shares its assumptions.

use veridex_core::adapter::mdf4::Mdf4Adapter;
use veridex_core::adapter::{Adapter, Detection, IngestError, IngestOptions, Source};
use veridex_core::cdm::Modality;

/// An MF4 file under construction. Blocks are appended in order and their offsets recorded, so links
/// can be patched in afterwards.
struct Mf4Builder {
    bytes: Vec<u8>,
}

/// Data types used by the fixtures (MDF `cn_data_type`).
const UINT_LE: u8 = 0;
const INT_LE: u8 = 2;
const FLOAT_LE: u8 = 4;

/// The exact size of the `##HD` block this builder writes: header + 6 links + 32 bytes of data. The
/// header always sits at offset 64, so the builder reserves this hole up front and fills it last.
const HD_LEN: usize = 24 + 6 * 8 + 32;

impl Mf4Builder {
    /// A new file with a `4.10` identification block and the header hole reserved after it.
    fn new(program: &[u8; 8]) -> Self {
        let mut bytes = vec![0u8; 64 + HD_LEN];
        bytes[0..8].copy_from_slice(b"MDF     ");
        bytes[8..16].copy_from_slice(b"4.10    ");
        bytes[16..24].copy_from_slice(program);
        // Version number 410.
        bytes[28..30].copy_from_slice(&410u16.to_le_bytes());
        Self { bytes }
    }

    fn offset(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Append a block: 4-byte id, reserved, length, link count, `links`, then `data`.
    fn block(&mut self, id: &[u8; 4], links: &[u64], data: &[u8]) -> u64 {
        let at = self.offset();
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

    /// Overwrite link `n` of the block at `at` (blocks are written before their targets exist).
    fn patch_link(&mut self, at: u64, n: usize, value: u64) {
        let off = at as usize + 24 + n * 8;
        self.bytes[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn text(&mut self, s: &str) -> u64 {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.block(b"##TX", &[], &data)
    }

    /// A linear conversion block: `phys = p1 + p2 * raw`.
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

    /// A conversion type the adapter does not apply (rational, type 2).
    fn rational_conversion(&mut self) -> u64 {
        let mut data = Vec::new();
        data.push(2u8);
        data.extend_from_slice(&[0u8; 7]);
        data.extend_from_slice(&[0u8; 16]);
        self.block(b"##CC", &[0, 0, 0, 0], &data)
    }

    /// A channel block. Returns its offset; `cn_next` is patched by the caller.
    #[allow(clippy::too_many_arguments)]
    fn channel(
        &mut self,
        name: &str,
        channel_type: u8,
        sync_type: u8,
        data_type: u8,
        bit_offset: u8,
        byte_offset: u32,
        bit_count: u32,
        conversion: Option<u64>,
    ) -> u64 {
        let name_at = self.text(name);
        let mut data = vec![channel_type, sync_type, data_type, bit_offset];
        data.extend_from_slice(&byte_offset.to_le_bytes());
        data.extend_from_slice(&bit_count.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // flags
        data.extend_from_slice(&0u32.to_le_bytes()); // inval_bit_pos
        data.extend_from_slice(&[0u8; 4]); // precision, reserved, attachment_count
        data.extend_from_slice(&[0u8; 48]); // value/limit ranges
                                            // Links: cn_next, composition, tx_name, si_source, cc_conversion, data, md_unit, md_comment.
        self.block(
            b"##CN",
            &[0, 0, name_at, 0, conversion.unwrap_or(0), 0, 0, 0],
            &data,
        )
    }

    fn channel_group(&mut self, cycle_count: u64, data_bytes: u32) -> u64 {
        let mut data = Vec::new();
        data.extend_from_slice(&0u64.to_le_bytes()); // record_id
        data.extend_from_slice(&cycle_count.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // flags
        data.extend_from_slice(&0u16.to_le_bytes()); // path separator
        data.extend_from_slice(&0u32.to_le_bytes()); // reserved
        data.extend_from_slice(&data_bytes.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // inval_bytes
        self.block(b"##CG", &[0, 0, 0, 0, 0, 0], &data)
    }

    fn data_group(&mut self, rec_id_size: u8) -> u64 {
        let mut data = vec![rec_id_size];
        data.extend_from_slice(&[0u8; 7]);
        self.block(b"##DG", &[0, 0, 0, 0], &data)
    }

    /// Fill the reserved header hole at offset 64, chaining `dg_first`, and return the file.
    fn finish(mut self, dg_first: u64, start_time_ns: u64) -> Vec<u8> {
        let mut hd = Vec::with_capacity(HD_LEN);
        hd.extend_from_slice(b"##HD");
        hd.extend_from_slice(&0u32.to_le_bytes());
        hd.extend_from_slice(&(HD_LEN as u64).to_le_bytes());
        hd.extend_from_slice(&6u64.to_le_bytes());
        for l in [dg_first, 0, 0, 0, 0, 0] {
            hd.extend_from_slice(&l.to_le_bytes());
        }
        hd.extend_from_slice(&start_time_ns.to_le_bytes());
        hd.extend_from_slice(&[0u8; 24]);
        assert_eq!(hd.len(), HD_LEN);
        self.bytes[64..64 + HD_LEN].copy_from_slice(&hd);
        self.bytes
    }
}

/// The canonical fixture: one sorted data group whose records hold a float64 time master (seconds),
/// a uint16 speed with a linear conversion (`10 + 0.5 × raw`), and an int32 temperature.
///
/// Record layout: `[0..8) time f64 | [8..10) speed u16 | [12..16) temp i32` — 16 bytes.
fn well_formed_file(records: usize) -> Vec<u8> {
    let mut b = Mf4Builder::new(b"veridex ");

    let mut data = Vec::new();
    for i in 0..records {
        let t = i as f64 * 0.1;
        data.extend_from_slice(&t.to_le_bytes());
        data.extend_from_slice(&((i as u16) * 2).to_le_bytes());
        data.extend_from_slice(&[0u8; 2]); // padding to a 4-byte boundary
        data.extend_from_slice(&(20i32 - i as i32).to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);

    let cc = b.linear_conversion(10.0, 0.5);
    let temp = b.channel("temperature", 0, 0, INT_LE, 0, 12, 32, None);
    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 8, 16, Some(cc));
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, speed);
    b.patch_link(speed, 0, temp);

    let cg = b.channel_group(records as u64, 16);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    b.finish(dg, 1_700_000_000_000_000_000)
}

fn write_temp(bytes: &[u8], suffix: &str) -> tempfile::TempPath {
    use std::io::Write;
    let mut f = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("temp file");
    f.write_all(bytes).expect("write");
    f.flush().expect("flush");
    f.into_temp_path()
}

fn ingest(bytes: &[u8]) -> veridex_core::adapter::Ingested {
    let path = write_temp(bytes, ".mf4");
    Mdf4Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
}

#[test]
fn detects_an_mf4_file_by_its_identification_block() {
    let path = write_temp(&well_formed_file(4), ".mf4");
    assert_eq!(
        Mdf4Adapter.detect(&Source::Local(path.to_path_buf())),
        Detection::Yes {
            version: Some("4.10".into())
        }
    );
    // The extension alone is not enough: a file that isn't MDF is declined, not mis-parsed.
    let bogus = write_temp(b"not an mdf file at all", ".mf4");
    assert_eq!(
        Mdf4Adapter.detect(&Source::Local(bogus.to_path_buf())),
        Detection::No
    );
    // And an MF4 by content but not by name is left to whichever adapter claims it.
    let other = write_temp(&well_formed_file(2), ".bin");
    assert_eq!(
        Mdf4Adapter.detect(&Source::Local(other.to_path_buf())),
        Detection::No
    );
}

#[test]
fn channels_become_streams_with_the_time_master_as_the_timeline() {
    let ingested = ingest(&well_formed_file(5));
    let ep = &ingested.dataset.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    // The master channel is the timeline, not a stream of its own.
    assert_eq!(names, vec!["speed", "temperature"]);
    for s in &ep.streams {
        assert_eq!(s.modality, Modality::CanSignal);
        assert_eq!(s.clock_id, "mf4-master");
        assert_eq!(s.frames.len(), 5, "one frame per record");
    }
    // 0.1 s spacing, in nanoseconds.
    let ts: Vec<i64> = ep.streams[0].frames.iter().map(|f| f.ts).collect();
    assert_eq!(ts[0], 0);
    assert_eq!(ts[1], 100_000_000);
    assert_eq!(ts[4], 400_000_000);
    assert_eq!(ep.start_ts, Some(0));
    assert_eq!(ep.end_ts, Some(400_000_000));

    // The writing program is provenance read from the file's own bytes.
    let recorder = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == "recorder")
        .and_then(|e| e.value.clone());
    assert_eq!(recorder.as_deref(), Some("veridex"));
    assert_eq!(ingested.report.source_version.as_deref(), Some("4.10"));
}

#[test]
fn decoded_values_are_fingerprinted_so_the_content_hash_tracks_them() {
    // Two files identical in structure and timing, differing only in one measured value, must not
    // hash the same — otherwise a tampered measurement would verify against the original.
    let a = ingest(&well_formed_file(5)).dataset;
    let mut altered = well_formed_file(5);
    // Flip the last record's temperature field (record 4, bytes 12..16 of a 16-byte record).
    let dt_data_start = altered
        .windows(4)
        .position(|w| w == b"##DT")
        .expect("##DT present")
        + 24;
    let at = dt_data_start + 4 * 16 + 12;
    altered[at] ^= 0xFF;
    let b = ingest(&altered).dataset;

    assert_eq!(
        a.episodes[0].streams[0].frames.len(),
        b.episodes[0].streams[0].frames.len()
    );
    assert_ne!(
        veridex_core::content_hash(&a),
        veridex_core::content_hash(&b),
        "a changed measurement must change the CDM content hash"
    );
}

#[test]
fn a_linear_conversion_is_applied_to_the_physical_value() {
    // `speed` carries phys = 10.0 + 0.5 * raw; `temperature` carries none. Their fingerprints differ
    // from what the raw values would produce, which is what proves the conversion ran.
    let with_cc = ingest(&well_formed_file(3)).dataset;
    let mut no_cc_bytes = well_formed_file(3);
    // Turn the linear conversion block into a 1:1 conversion by rewriting its type byte.
    let cc_at = no_cc_bytes
        .windows(4)
        .position(|w| w == b"##CC")
        .expect("##CC present");
    no_cc_bytes[cc_at + 24 + 4 * 8] = 0; // cc_type = 1:1
    let without_cc = ingest(&no_cc_bytes).dataset;

    let speed_a = &with_cc.episodes[0].streams[0];
    let speed_b = &without_cc.episodes[0].streams[0];
    assert_eq!(speed_a.name, "speed");
    assert_ne!(
        speed_a.frames[1].value_ref.content_hash, speed_b.frames[1].value_ref.content_hash,
        "the converted physical value must differ from the raw value"
    );
    // The unconverted channel is untouched either way.
    let temp_a = &with_cc.episodes[0].streams[1];
    let temp_b = &without_cc.episodes[0].streams[1];
    assert_eq!(
        temp_a.frames[1].value_ref.content_hash,
        temp_b.frames[1].value_ref.content_hash
    );
}

#[test]
fn an_unapplied_conversion_is_reported_rather_than_silently_ignored() {
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..3u32 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);
    let cc = b.rational_conversion();
    let value = b.channel("rational_value", 0, 0, UINT_LE, 0, 8, 32, Some(cc));
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, value);
    let cg = b.channel_group(3, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    // The channel is still ingested — with its raw value — and the unapplied conversion is disclosed.
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 1);
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("conversion type 2 is not applied")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_compressed_data_block_is_reported_and_contributes_no_frames() {
    let mut b = Mf4Builder::new(b"veridex ");
    // A ##DZ block stands in for compressed data; the adapter must decline rather than mis-read it.
    let dz = b.block(b"##DZ", &[], &[0u8; 32]);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    let cg = b.channel_group(3, 8);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dz);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("##DZ") && u.note.contains("not decoded")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn an_unsorted_data_group_is_reported_rather_than_mis_decoded() {
    let mut b = Mf4Builder::new(b"veridex ");
    let dt = b.block(b"##DT", &[], &[0u8; 32]);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    let cg = b.channel_group(2, 8);
    b.patch_link(cg, 1, time);
    // Record id size 1 → records from several groups are interleaved behind ids.
    let dg = b.data_group(1);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("unsorted data group")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_group_without_a_time_master_contributes_nothing_and_says_why() {
    let mut b = Mf4Builder::new(b"veridex ");
    let dt = b.block(b"##DT", &[], &[0u8; 16]);
    // A measured channel, but no master: its samples have no honest timestamps.
    let value = b.channel("value", 0, 0, UINT_LE, 0, 0, 32, None);
    let cg = b.channel_group(4, 4);
    b.patch_link(cg, 1, value);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("no time master channel")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn an_over_declared_cycle_count_reads_what_is_there_and_discloses_the_shortfall() {
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..2u32 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);
    let value = b.channel("value", 0, 0, UINT_LE, 0, 8, 32, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, value);
    // Claim 100 cycles when the data block holds 2.
    let cg = b.channel_group(100, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert_eq!(ingested.dataset.episodes[0].streams[0].frames.len(), 2);
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.contains("declares 100 cycles")),
        "{:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_truncated_or_hostile_file_errors_or_yields_nothing_but_never_panics() {
    let good = well_formed_file(5);
    // Every prefix of a valid file: some parse to nothing, none may panic.
    for cut in [0, 8, 16, 40, 64, 100, 150, 200, good.len() / 2] {
        if cut > good.len() {
            continue;
        }
        let path = write_temp(&good[..cut], ".mf4");
        let _ = Mdf4Adapter.ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        );
    }
    // A byte-corrupted file must also survive.
    for at in [0usize, 64, 96, 128, 160, 200] {
        if at >= good.len() {
            continue;
        }
        let mut corrupt = good.clone();
        corrupt[at] ^= 0xFF;
        let path = write_temp(&corrupt, ".mf4");
        let _ = Mdf4Adapter.ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        );
    }
}

#[test]
fn a_non_4_x_file_is_rejected_as_an_unsupported_version() {
    let mut bytes = well_formed_file(2);
    bytes[8..16].copy_from_slice(b"3.30    ");
    let path = write_temp(&bytes, ".mf4");
    let err = Mdf4Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a 3.x file must be rejected");
    assert!(
        matches!(err, IngestError::UnsupportedVersion { .. }),
        "{err:?}"
    );
}

#[test]
fn the_registry_autodetects_an_mf4_file() {
    let path = write_temp(&well_formed_file(3), ".mf4");
    let ingested = veridex_core::default_registry()
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("registry ingests MF4");
    assert_eq!(ingested.report.format_id, "mf4");
}
