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

    /// A `##CC` of any type, with `params` as its `cc_val` array.
    fn conversion(&mut self, cc_type: u8, params: &[f64]) -> u64 {
        let mut data = Vec::new();
        data.push(cc_type);
        data.push(0u8); // precision
        data.extend_from_slice(&0u16.to_le_bytes()); // flags
        data.extend_from_slice(&0u16.to_le_bytes()); // ref_count
        data.extend_from_slice(&(params.len() as u16).to_le_bytes()); // val_count
        data.extend_from_slice(&0f64.to_le_bytes()); // phy_range_min
        data.extend_from_slice(&0f64.to_le_bytes()); // phy_range_max
        for p in params {
            data.extend_from_slice(&p.to_le_bytes());
        }
        self.block(b"##CC", &[0, 0, 0, 0], &data)
    }

    /// A conversion type the adapter does not apply: an algebraic formula (type 3), whose physical
    /// value is a number this reader has no expression evaluator for.
    fn algebraic_conversion(&mut self) -> u64 {
        self.conversion(3, &[])
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

    /// A channel with explicit `cn_flags` (e.g. an invalidation-bit declaration).
    #[allow(clippy::too_many_arguments)]
    fn channel_with_flags(
        &mut self,
        name: &str,
        channel_type: u8,
        sync_type: u8,
        data_type: u8,
        bit_offset: u8,
        byte_offset: u32,
        bit_count: u32,
        conversion: Option<u64>,
        flags: u32,
    ) -> u64 {
        let at = self.channel(
            name,
            channel_type,
            sync_type,
            data_type,
            bit_offset,
            byte_offset,
            bit_count,
            conversion,
        );
        // cn_flags sits at data offset 12, i.e. after the header, 8 links, and 12 data bytes.
        let off = at as usize + 24 + 8 * 8 + 12;
        self.bytes[off..off + 4].copy_from_slice(&flags.to_le_bytes());
        at
    }

    fn channel_group(&mut self, cycle_count: u64, data_bytes: u32) -> u64 {
        self.channel_group_with_inval(cycle_count, data_bytes, 0)
    }

    /// A `##SI` source-information block: `si_tx_name`, `si_tx_path`, then `si_type` / `si_bus_type`.
    fn source(&mut self, name: &str, path: &str, si_type: u8, bus_type: u8) -> u64 {
        let name_at = self.text(name);
        let path_at = if path.is_empty() { 0 } else { self.text(path) };
        let mut data = vec![si_type, bus_type, 0];
        data.extend_from_slice(&[0u8; 5]);
        self.block(b"##SI", &[name_at, path_at, 0], &data)
    }

    /// A channel group tagged with the `cg_record_id` its records carry in an unsorted data group.
    fn channel_group_with_id(&mut self, record_id: u64, cycle_count: u64, data_bytes: u32) -> u64 {
        let at = self.channel_group_with_inval(cycle_count, data_bytes, 0);
        let off = at as usize + 24 + 6 * 8;
        self.bytes[off..off + 8].copy_from_slice(&record_id.to_le_bytes());
        at
    }

    /// A channel group with explicit `cg_flags` (e.g. the variable-length signal-data bit).
    fn channel_group_with_flags(&mut self, cycle_count: u64, data_bytes: u32, flags: u16) -> u64 {
        let at = self.channel_group_with_inval(cycle_count, data_bytes, 0);
        // cg_flags sits at data offset 16: after the header, 6 links, record_id and cycle_count.
        let off = at as usize + 24 + 6 * 8 + 16;
        self.bytes[off..off + 2].copy_from_slice(&flags.to_le_bytes());
        at
    }

    fn channel_group_with_inval(
        &mut self,
        cycle_count: u64,
        data_bytes: u32,
        inval_bytes: u32,
    ) -> u64 {
        let mut data = Vec::new();
        data.extend_from_slice(&0u64.to_le_bytes()); // record_id
        data.extend_from_slice(&cycle_count.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes()); // flags
        data.extend_from_slice(&0u16.to_le_bytes()); // path separator
        data.extend_from_slice(&0u32.to_le_bytes()); // reserved
        data.extend_from_slice(&data_bytes.to_le_bytes());
        data.extend_from_slice(&inval_bytes.to_le_bytes());
        self.block(b"##CG", &[0, 0, 0, 0, 0, 0], &data)
    }

    /// A `##DZ` block holding `records` deflated. `zip_type` 0 is plain deflate; 1 transposes the
    /// bytes into columns of `record_len` first, which is what a logger writes.
    fn zipped_data(&mut self, records: &[u8], zip_type: u8, record_len: u32) -> u64 {
        use std::io::Write;
        let staged: Vec<u8> = if zip_type == 1 {
            transpose(records, record_len as usize)
        } else {
            records.to_vec()
        };
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&staged).expect("deflate");
        let payload = enc.finish().expect("deflate");

        let mut data = Vec::new();
        data.extend_from_slice(b"DT");
        data.push(zip_type);
        data.push(0);
        data.extend_from_slice(&record_len.to_le_bytes()); // dz_zip_parameter
        data.extend_from_slice(&(records.len() as u64).to_le_bytes());
        data.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        data.extend_from_slice(&payload);
        self.block(b"##DZ", &[], &data)
    }

    /// A `##DL` chaining `parts` (each a `##DT` or `##DZ`) as one record stream.
    fn data_list(&mut self, parts: &[u64], equal_length: u64) -> u64 {
        let mut links = vec![0u64]; // dl_dl_next
        links.extend_from_slice(parts);
        let mut data = vec![1u8, 0, 0, 0]; // dl_flags: equal length
        data.extend_from_slice(&(parts.len() as u32).to_le_bytes());
        data.extend_from_slice(&equal_length.to_le_bytes());
        self.block(b"##DL", &links, &data)
    }

    /// An `##HL` header list wrapping a `##DL`.
    fn header_list(&mut self, dl: u64) -> u64 {
        let mut data = Vec::new();
        data.extend_from_slice(&0u16.to_le_bytes()); // hl_flags
        data.push(0); // hl_zip_type
        data.extend_from_slice(&[0u8; 5]);
        self.block(b"##HL", &[dl], &data)
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
    let dt = b.block(b"##DT", &[], &well_formed_records(0..records));
    finish_canonical_graph(b, dt, records)
}

/// The canonical fixture's record bytes for record indices `range`.
fn well_formed_records(range: std::ops::Range<usize>) -> Vec<u8> {
    let mut data = Vec::new();
    for i in range {
        let t = i as f64 * 0.1;
        data.extend_from_slice(&t.to_le_bytes());
        data.extend_from_slice(&((i as u16) * 2).to_le_bytes());
        data.extend_from_slice(&[0u8; 2]); // padding to a 4-byte boundary
        data.extend_from_slice(&(20i32 - i as i32).to_le_bytes());
    }
    data
}

/// Transpose `records` into byte columns, the way an MDF writer stages data for `dz_zip_type` 1.
fn transpose(records: &[u8], columns: usize) -> Vec<u8> {
    let lines = records.len() / columns;
    let mut out = Vec::with_capacity(records.len());
    for column in 0..columns {
        for line in 0..lines {
            out.push(records[line * columns + column]);
        }
    }
    out.extend_from_slice(&records[lines * columns..]);
    out
}

/// The canonical fixture's channel graph around an already-written data block at `data_at`.
fn finish_canonical_graph(mut b: Mf4Builder, data_at: u64, records: usize) -> Vec<u8> {
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
    b.patch_link(dg, 2, data_at);

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
fn a_metadata_only_run_says_how_much_it_declined_to_read() {
    // "No sample values were read" and "none of the 400 records this file declares were read" are
    // the same fact and very different statements. A metadata-only inspect shows three streams and
    // zero frames, and without the count nothing says whether the measurement behind them is four
    // samples or four million. The `##CG` headers state it and reading them is the whole of what a
    // metadata-only run does — it was parsed and discarded, under a doc comment claiming the run
    // reports "how many samples each declares".
    //
    // Pinned against a full read of the same file, because the number is only worth printing if it
    // is the number: one record is one sample of every channel in the group, so the declared count
    // is the frames a full read yields *per stream*.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo.mf4");
    veridex_demo::mf4::write(&path, "clean").expect("write the demo measurement");

    let ingest_at = |opts: &IngestOptions| {
        Mdf4Adapter
            .ingest(&Source::Local(path.to_path_buf()), opts)
            .expect("ingest")
    };
    let full = ingest_at(&IngestOptions::default());
    let per_stream = full.dataset.episodes[0].streams[0].frames.len();
    assert!(per_stream > 0, "the full read decodes frames");

    let meta = ingest_at(&IngestOptions {
        metadata_only: true,
        ..IngestOptions::default()
    });
    let note = meta
        .report
        .omitted_fields
        .iter()
        .find(|o| o.contains("record(s) the ##CG headers declare"))
        .expect("the metadata-only omission note names the record count");
    assert!(
        note.contains(&format!("{per_stream} record(s)")),
        "the declared count must be the frames a full read yields per stream ({per_stream}): {note}"
    );
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
        assert_eq!(
            s.clock_id, "mf4-master#0.0",
            "each raster is its own timeline"
        );
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
fn an_unapplied_numeric_conversion_reaches_the_verdict_as_data_that_went_unread() {
    // An algebraic formula produces a *number*, and it is in the file as a rule. Not evaluating it
    // leaves every value of the stream a raw count summarized as though it were the physical
    // quantity — so the run has to say so in the verdict, not in a note only `inspect` prints.
    let ingested = ingest(&converted_file(|b| b.algebraic_conversion()));
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 1);
    assert!(
        ingested.report.unread_sources.iter().any(|u| u
            .note
            .contains("conversion type 3 (algebraic formula) is not applied")),
        "{:?}",
        ingested.report.unread_sources
    );
}

/// The canonical conversion fixture: a `value` channel carrying the raw counts 0, 1, 2, 3, 4 under
/// whatever `##CC` the caller builds.
fn converted_file(make_cc: impl FnOnce(&mut Mf4Builder) -> u64) -> Vec<u8> {
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..5u32 {
        data.extend_from_slice(&(f64::from(i) * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);
    let cc = make_cc(&mut b);
    let value = b.channel("value", 0, 0, UINT_LE, 0, 8, 32, Some(cc));
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, value);
    let cg = b.channel_group(5, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    b.finish(dg, 0)
}

/// The converted values of the fixture's single stream, for raw counts 0..5.
fn converted_values(bytes: &[u8]) -> Vec<f64> {
    let ingested = ingest(bytes);
    assert!(
        ingested.report.unread_sources.is_empty(),
        "the conversion must be applied, not disclosed: {:?}",
        ingested.report.unread_sources
    );
    let stream = &ingested.dataset.episodes[0].streams[0];
    let stats = stream.observed_stats.expect("statistics");
    // The recomputed statistics are over the converted values; min/max/mean pin the whole curve
    // well enough for these tables, and the per-sample assertions below use them.
    vec![stats.min, stats.max, stats.mean]
}

#[test]
fn a_text_valued_conversion_records_the_raw_code_and_costs_the_reader_nothing() {
    // A value-to-text conversion turns a code into a string, which a numeric CDM stream has no
    // shape for. Recording the raw code is the honest answer, so it is `unmapped`, not `unread`.
    let ingested = ingest(&converted_file(|b| b.conversion(7, &[0.0, 1.0])));
    assert!(
        ingested.report.unmapped_fields.iter().any(|u| u
            .note
            .contains("conversion type 7 (value-to-text) is not applied")),
        "{:?}",
        ingested.report.unmapped_fields
    );
    assert!(
        !ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("conversion type 7")),
        "a code the CDM cannot hold as text is not unread data: {:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_rational_conversion_is_applied_rather_than_leaving_raw_counts_in_the_verdict() {
    // A sensor's calibration curve is not always a straight line. `(2x + 1) / 1` over raw 0..4.
    let bytes = converted_file(|b| b.conversion(2, &[0.0, 2.0, 1.0, 0.0, 0.0, 1.0]));
    assert_eq!(converted_values(&bytes), vec![1.0, 9.0, 5.0]);
}

#[test]
fn a_value_to_value_table_is_interpolated_between_its_keys() {
    // Keys 0 and 4 map to 100 and 500; raw 1, 2, 3 land a quarter, half and three quarters along.
    let bytes = converted_file(|b| b.conversion(4, &[0.0, 100.0, 4.0, 500.0]));
    assert_eq!(converted_values(&bytes), vec![100.0, 500.0, 300.0]);
}

#[test]
fn a_value_to_value_table_without_interpolation_takes_the_nearest_key() {
    // The same table read as type 5: every raw count snaps to whichever key it is closest to, so
    // nothing between 100 and 500 is ever invented.
    let bytes = converted_file(|b| b.conversion(5, &[0.0, 100.0, 4.0, 500.0]));
    // raw 0,1,2 -> 100 (2 ties to the lower key); raw 3,4 -> 500. Mean = (100*3 + 500*2) / 5.
    assert_eq!(converted_values(&bytes), vec![100.0, 500.0, 260.0]);
}

#[test]
fn a_value_range_table_maps_each_range_and_falls_back_to_its_default() {
    // 0..2 -> 10, 2..4 -> 20, anything else -> -1. Raw 4 is in no range (the upper bound is open).
    let bytes = converted_file(|b| b.conversion(6, &[0.0, 2.0, 10.0, 2.0, 4.0, 20.0, -1.0]));
    // raw 0,1 -> 10; raw 2,3 -> 20; raw 4 -> -1. Mean = (10+10+20+20-1)/5.
    assert_eq!(converted_values(&bytes), vec![-1.0, 20.0, 11.8]);
}

#[test]
fn a_table_whose_keys_are_out_of_order_is_not_applied() {
    // The lookup walks the table assuming ascending keys, which MDF requires. A file that breaks
    // that would be read at the wrong entry — a plausible wrong number for every sample — so the
    // conversion is declined and disclosed instead.
    let ingested = ingest(&converted_file(|b| {
        b.conversion(4, &[4.0, 500.0, 0.0, 100.0])
    }));
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("conversion type 4")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_data_block_that_cannot_be_read_reaches_the_verdict_as_data_that_went_unread() {
    // The distinction this fixes: a data block this reader declines is not a field the CDM cannot
    // hold — it is the measurement, sitting in the file, that nobody decoded. Filed as `unmapped` it
    // cost the reader nothing and raised no finding, so a fleet log came back with no frames,
    // `Coverage::Full`, and a verdict that said nothing about it. It has to reach the verdict, which
    // only `unread_sources` does. (`##SD` holds signal data, never a record stream.)
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.block(b"##SD", &[], &[0u8; 32]);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    let cg = b.channel_group(3, 8);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dz);
    let bytes = b.finish(dg, 0);
    let path = write_temp(&bytes, ".mf4");

    let out = veridex_core::run_check(
        &veridex_core::default_registry(),
        &Source::Local(path.to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the file still ingests");
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "COVERAGE.SOURCE_UNREAD"),
        "the verdict says nothing about the data it did not read: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| f.code.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_compressed_block_of_something_other_than_records_is_reported_not_mis_decoded() {
    let mut b = Mf4Builder::new(b"veridex ");
    // A `##DZ` states which block it was compressed from. `SD` is signal data, not the record stream
    // a channel group is decoded against, so decoding it would produce plausible-but-wrong values.
    let mut data = Vec::new();
    data.extend_from_slice(b"SD");
    data.extend_from_slice(&[0u8; 30]);
    let dz = b.block(b"##DZ", &[], &data);
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
            .unread_sources
            .iter()
            .any(|u| u.note.contains("##DZ")
                && u.note.contains("not decoded")
                && u.note.contains("`SD`")),
        "{:?}",
        ingested.report.unread_sources
    );
}

/// An unsorted data group: two rasters interleaved in one record stream behind their `cg_record_id`s.
/// Group 1 is a 16-byte record (`t` f64, `speed` u16); group 2 is a 12-byte one (`t` f64, `temp`
/// i32). The differing strides are the point — a demultiplexer that assumed one length would
/// misalign everything after the first record of the other group.
///
/// `bad_id` interleaves one record tagged with an id no channel group claims.
fn unsorted_file(records: usize, bad_id: bool) -> Vec<u8> {
    let mut b = Mf4Builder::new(b"veridex ");

    let mut data = Vec::new();
    for i in 0..records {
        let t = i as f64 * 0.1;
        data.push(1); // record id: the 16-byte raster
        data.extend_from_slice(&t.to_le_bytes());
        data.extend_from_slice(&((i as u16) * 2).to_le_bytes());
        data.extend_from_slice(&[0u8; 6]);

        data.push(2); // record id: the 12-byte raster
        data.extend_from_slice(&t.to_le_bytes());
        data.extend_from_slice(&(20i32 - i as i32).to_le_bytes());

        if bad_id && i == 1 {
            data.push(9);
            data.extend_from_slice(&[0u8; 12]);
        }
    }
    let dt = b.block(b"##DT", &[], &data);

    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 8, 16, None);
    let time_a = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time_a, 0, speed);
    let cg_a = b.channel_group_with_id(1, records as u64, 16);
    b.patch_link(cg_a, 1, time_a);

    let temp = b.channel("temperature", 0, 0, INT_LE, 0, 8, 32, None);
    let time_b = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time_b, 0, temp);
    let cg_b = b.channel_group_with_id(2, records as u64, 12);
    b.patch_link(cg_b, 1, time_b);
    b.patch_link(cg_a, 0, cg_b);

    // Record id size 1 → records from several groups are interleaved behind ids.
    let dg = b.data_group(1);
    b.patch_link(dg, 1, cg_a);
    b.patch_link(dg, 2, dt);
    b.finish(dg, 0)
}

#[test]
fn an_unsorted_data_group_is_demultiplexed_into_its_channel_groups() {
    // How a bus logger writes several rasters into one data block as the samples arrive. The whole
    // group used to be declined, so a file written this way ingested to no frames at all — every
    // check ran on nothing and passed, and only the coverage warning said otherwise.
    let ingested = ingest(&unsorted_file(5, false));
    let ep = &ingested.dataset.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["speed", "temperature"]);
    for s in &ep.streams {
        assert_eq!(s.frames.len(), 5, "every record of each raster is decoded");
        let ts: Vec<i64> = s.frames.iter().map(|f| f.ts).collect();
        assert_eq!(
            ts,
            vec![0, 100_000_000, 200_000_000, 300_000_000, 400_000_000]
        );
    }
    // Each raster carries its own time master, so they are two clocks, not one — otherwise the
    // cross-stream temporal checks would compare one raster's span against the other's.
    assert_eq!(ep.streams[0].clock_id, "mf4-master#0.0");
    assert_eq!(ep.streams[1].clock_id, "mf4-master#0.1");
    // The values really were sliced at each group's own stride.
    let speed = ep.streams[0].observed_stats.expect("speed statistics");
    assert_eq!((speed.min, speed.max), (0.0, 8.0));
    let temp = ep.streams[1]
        .observed_stats
        .expect("temperature statistics");
    assert_eq!((temp.min, temp.max), (16.0, 20.0));
    assert!(
        ingested.report.unread_sources.is_empty(),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn an_unsorted_record_with_an_unknown_id_refuses_the_group() {
    // A record's length is known only from its id, so an id no channel group claims leaves every
    // later record at an unknown offset. There is no partial answer: decoding what came before it
    // would silently truncate the measurement while the run still read as complete.
    let ingested = ingest(&unsorted_file(5, true));
    assert!(
        ingested.dataset.episodes[0].streams.is_empty(),
        "the stream cannot be resynchronized, so nothing may be decoded from it"
    );
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("record id 9") && u.note.contains("unknown offset")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_variable_length_signal_data_group_is_reported_not_sliced_at_a_fixed_stride() {
    // A VLSD group's records are length-prefixed rather than fixed-stride, so reading them at
    // `cg_data_bytes` would read every one of them at the wrong offset — a full set of confidently
    // wrong values, which is worse than none.
    let mut b = Mf4Builder::new(b"veridex ");
    let dt = b.block(b"##DT", &[], &[0u8; 64]);
    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 8, 16, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, speed);
    let cg = b.channel_group_with_flags(4, 16, 0x01);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("variable-length signal-data")),
        "{:?}",
        ingested.report.unread_sources
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
            .unread_sources
            .iter()
            .any(|u| u.note.contains("no time master channel")),
        "{:?}",
        ingested.report.unread_sources
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
            .unread_sources
            .iter()
            .any(|u| u.note.contains("declares 100 cycles")),
        "{:?}",
        ingested.report.unread_sources
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

#[test]
fn shared_blocks_are_visited_once_rather_than_re_walked_per_parent() {
    // Links may legally point at shared blocks. With a visited set per chain, n data groups each
    // re-walking the same n channel groups each re-walking the same n channels is O(n³) streams —
    // measured at 1.35 GB from a 33 KB file before this was fixed.
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..4u32 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);

    // One channel chain, one channel group, then N data groups all pointing at that same group.
    let value = b.channel("value", 0, 0, UINT_LE, 0, 8, 32, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, value);
    let cg = b.channel_group(4, 12);
    b.patch_link(cg, 1, time);

    let n = 30;
    let mut groups = Vec::new();
    for _ in 0..n {
        let dg = b.data_group(0);
        b.patch_link(dg, 1, cg);
        b.patch_link(dg, 2, dt);
        groups.push(dg);
    }
    for w in groups.windows(2) {
        b.patch_link(w[0], 0, w[1]);
    }
    let ingested = ingest(&b.finish(groups[0], 0));

    // The shared channel group is decoded once, not once per data group.
    assert_eq!(
        ingested.dataset.episodes[0].streams.len(),
        1,
        "a shared block must not be re-walked per parent"
    );
}

#[test]
fn a_conversion_on_the_time_master_that_cannot_be_applied_stops_the_group() {
    // An unapplied conversion on a signal costs that signal; on the master it would silently shift
    // every timestamp in the group, so nothing may be decoded from it.
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..3u32 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);
    let cc = b.algebraic_conversion();
    let value = b.channel("value", 0, 0, UINT_LE, 0, 8, 32, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, Some(cc));
    b.patch_link(time, 0, value);
    let cg = b.channel_group(3, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested.report.unread_sources.iter().any(|u| u
            .note
            .contains("time master carries ##CC conversion type 3")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_channel_declaring_invalidation_bits_is_not_decoded() {
    // Veridex does not evaluate per-sample invalidation bits, so decoding such a channel would
    // present samples the file marked invalid as real measurements.
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..3u32 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&i.to_le_bytes());
        data.extend_from_slice(&[0xFF, 0xFF]); // invalidation bytes: every sample invalid
    }
    let dt = b.block(b"##DT", &[], &data);
    let value = b.channel_with_flags("value", 0, 0, UINT_LE, 0, 8, 32, None, 0x02);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, value);
    let cg = b.channel_group_with_inval(3, 12, 2);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let ingested = ingest(&b.finish(dg, 0));

    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("per-sample invalidation")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn an_absurd_block_length_is_refused_rather_than_overflowing() {
    // The `##HD` header's declared length comes straight from the file. A value near u64::MAX used to
    // wrap the `at + length` containment check: a panic in debug (the mode the suite runs in), and in
    // release a bogus header that passed validation, so a corrupt file was accepted as a clean,
    // signable, zero-episode dataset.
    let mut bytes = well_formed_file(2);
    // The header block sits at offset 64; its length field is bytes 8..16 of the block.
    bytes[64 + 8..64 + 16].copy_from_slice(&(u64::MAX - 8).to_le_bytes());
    let path = write_temp(&bytes, ".mf4");
    let result = Mdf4Adapter.ingest(
        &Source::Local(path.to_path_buf()),
        &IngestOptions::default(),
    );
    match result {
        Err(_) => {}
        Ok(ingested) => panic!(
            "a header claiming {} bytes must be refused, not accepted as a {}-episode dataset",
            u64::MAX - 8,
            ingested.dataset.episodes.len()
        ),
    }
}

// ---- Reading the header tree without opening a data block ----

fn metadata_only(bytes: &[u8]) -> veridex_core::adapter::Ingested {
    let path = write_temp(bytes, ".mf4");
    Mdf4Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions {
                metadata_only: true,
                ..IngestOptions::default()
            },
        )
        .expect("ingest")
}

#[test]
fn the_header_tree_alone_names_the_channels_the_measurement_declares() {
    // `##DG` → `##CG` → `##CN` states every channel's name and raster, separately from the data
    // block that holds the samples. Reading only that describes the measurement.
    let bytes = well_formed_file(5);
    let summary = metadata_only(&bytes);
    let full = ingest(&bytes);

    assert_eq!(
        summary.report.coverage,
        veridex_core::adapter::Coverage::MetadataOnly {
            episodes_declared: 1
        }
    );
    let names = |i: &veridex_core::adapter::Ingested| -> Vec<String> {
        let mut n: Vec<String> = i.dataset.episodes[0]
            .streams
            .iter()
            .map(|s| s.name.clone())
            .collect();
        n.sort();
        n
    };
    assert_eq!(names(&summary), names(&full));
    assert!(summary.dataset.episodes[0]
        .streams
        .iter()
        .all(|s| s.frames.is_empty()));
    assert!(
        !summary
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("content_hash")),
        "a run that read no sample must not claim to have hashed one: {:?}",
        summary.report.mapped_fields
    );
}

#[test]
fn a_measurement_whose_data_block_is_declined_is_describable_only_this_way() {
    // The reason the mode earns its place here. A full read declines a data block it cannot decode
    // into a record stream and the file yields nothing but a coverage warning. The header tree is
    // never in that block, so it still says which signals the measurement holds.
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.block(b"##SD", &[], &[0u8; 32]);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 64, 32, None);
    b.patch_link(time, 0, speed);
    let cg = b.channel_group(3, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dz);
    let bytes = b.finish(dg, 0);

    assert!(
        ingest(&bytes).dataset.episodes[0].streams.is_empty(),
        "a full read declines a data block that is not a record stream, which is the point"
    );
    let summary = metadata_only(&bytes);
    assert_eq!(
        summary.dataset.episodes[0]
            .streams
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["speed"],
        "the header tree names the signal even though its samples are compressed"
    );
    assert!(summary
        .report
        .omitted_fields
        .iter()
        .any(|o| o.contains("##DT")));
}

/// An MF4 channel is decoded — the `##CC` conversion is applied and the result is a number — so the
/// statistical family can grade it, exactly as it grades a CAN signal off a DBC. Until it did, a
/// fleet measurement whose steering angle sits at its end-stop for the whole drive scored `data 100`
/// with no statistical findings.
#[test]
fn channel_values_are_measured_not_only_fingerprinted() {
    let d = ingest(&well_formed_file(5)).dataset;
    let speed = d.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "speed")
        .expect("the speed channel");
    // raw 0, 2, 4, 6, 8 under `10 + 0.5 x raw` → 10, 11, 12, 13, 14.
    let stats = speed
        .observed_stats
        .expect("statistics are recomputed from the values");
    assert_eq!((stats.min, stats.max, stats.mean), (10.0, 14.0, 12.0));
    assert_eq!(
        speed.observed_non_finite,
        Some(0),
        "the values were read and every one was finite"
    );
    assert!(
        speed.stats.is_none(),
        "MF4 stores no summary statistics, so there is nothing to compare against"
    );
}

/// The measurement that motivates it: a channel pinned at one value for most of the recording is a
/// saturated signal, and it is now reported rather than passing as clean data.
#[test]
fn a_channel_pinned_at_its_rail_is_flagged() {
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..40u16 {
        let t = f64::from(i) * 0.1;
        data.extend_from_slice(&t.to_le_bytes());
        // 30 of 40 samples at the same maximum, the rest walking below it.
        let raw: u16 = if i < 30 { 4095 } else { i * 10 };
        data.extend_from_slice(&raw.to_le_bytes());
        data.extend_from_slice(&[0u8; 6]);
    }
    let dt = b.block(b"##DT", &[], &data);
    let angle = b.channel("steering_angle", 0, 0, UINT_LE, 0, 8, 16, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, angle);
    let cg = b.channel_group(40, 16);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    let bytes = b.finish(dg, 1_700_000_000_000_000_000);

    let path = write_temp(&bytes, ".mf4");
    let outcome = veridex_core::pipeline::run_check(
        &veridex_core::default_registry(),
        &Source::Local(path.to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    let saturated = outcome
        .verdict
        .findings
        .iter()
        .find(|f| f.code == "STATISTICAL.SATURATED")
        .expect("the pinned channel is flagged");
    assert!(
        saturated.message.contains("steering_angle") && saturated.message.contains("75%"),
        "{}",
        saturated.message
    );
    assert!(
        !outcome
            .verdict
            .findings
            .iter()
            .any(|f| f.code == "STATISTICAL.UNMEASURED_VALUES"),
        "an MF4's channel values are read, so the family does not abstain on it"
    );
}

// --- Compressed and listed data blocks: the shapes a real logger actually writes -----------------

/// Everything the canonical fixture decodes: each stream's name, its frame timestamps, the value
/// fingerprints those frames carry, and the statistics recomputed from the values. Two files that
/// agree on all four decoded to the same measurement.
type DecodedShape = Vec<(
    String,
    Vec<i64>,
    Vec<Option<[u8; 32]>>,
    Option<veridex_core::cdm::StreamStats>,
)>;

fn decoded_shape(bytes: &[u8]) -> DecodedShape {
    ingest(bytes).dataset.episodes[0]
        .streams
        .iter()
        .map(|s| {
            (
                s.name.clone(),
                s.frames.iter().map(|f| f.ts).collect(),
                s.frames.iter().map(|f| f.value_ref.content_hash).collect(),
                s.observed_stats,
            )
        })
        .collect()
}

#[test]
fn a_deflated_data_block_decodes_to_the_same_measurement_as_an_uncompressed_one() {
    // The shape a real logger writes. Until `##DZ` was decompressed, a fleet measurement ingested to
    // zero frames: every temporal, statistical and structural check ran on nothing and passed.
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.zipped_data(&well_formed_records(0..5), 0, 16);
    let compressed = finish_canonical_graph(b, dz, 5);

    assert_eq!(
        decoded_shape(&compressed),
        decoded_shape(&well_formed_file(5)),
        "a compressed measurement must decode to exactly the uncompressed one"
    );
    let ingested = ingest(&compressed);
    assert!(
        ingested.report.unread_sources.is_empty(),
        "nothing went unread: {:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_transposed_deflated_block_is_untransposed_before_it_is_decoded() {
    // `dz_zip_type` 1 lays the records out byte-column-major before deflating, because like-typed
    // bytes compress far better adjacently. Reading it without reversing that does not fail — it
    // yields a full set of confidently wrong values, which is worse.
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.zipped_data(&well_formed_records(0..5), 1, 16);
    let transposed = finish_canonical_graph(b, dz, 5);

    assert_eq!(
        decoded_shape(&transposed),
        decoded_shape(&well_formed_file(5)),
        "a transposed block must decode to the same measurement as a plain one"
    );
}

#[test]
fn a_data_list_is_stitched_back_into_one_record_stream() {
    // A logger writes its data out in chunks as the drive runs, and a `##DL` chains them. Read one
    // chunk and the measurement silently ends early; read them out of order and every record after
    // the first chunk is misaligned.
    let mut b = Mf4Builder::new(b"veridex ");
    let first = b.block(b"##DT", &[], &well_formed_records(0..3));
    let second = b.block(b"##DT", &[], &well_formed_records(3..5));
    let dl = b.data_list(&[first, second], 3 * 16);
    let listed = finish_canonical_graph(b, dl, 5);

    assert_eq!(
        decoded_shape(&listed),
        decoded_shape(&well_formed_file(5)),
        "the list's chunks must rejoin into the single record stream they were split from"
    );
}

#[test]
fn a_header_list_of_compressed_chunks_is_read_through_to_its_records() {
    // The combination a real MF4 arrives in: `##HL` → `##DL` → several `##DZ`.
    let mut b = Mf4Builder::new(b"veridex ");
    let first = b.zipped_data(&well_formed_records(0..2), 1, 16);
    let second = b.zipped_data(&well_formed_records(2..5), 0, 16);
    let dl = b.data_list(&[first, second], 2 * 16);
    let hl = b.header_list(dl);
    let file = finish_canonical_graph(b, hl, 5);

    assert_eq!(
        decoded_shape(&file),
        decoded_shape(&well_formed_file(5)),
        "a header list must resolve through its data list to the records"
    );
}

#[test]
fn a_data_list_with_an_unreadable_element_refuses_the_whole_group() {
    // Half a list is not a shorter measurement, it is a misaligned one: every record after the
    // missing chunk would be read at the wrong offset. So the group contributes nothing and says so.
    let mut b = Mf4Builder::new(b"veridex ");
    let good = b.block(b"##DT", &[], &well_formed_records(0..3));
    let bad = b.block(b"##SD", &[], &[0u8; 32]);
    let dl = b.data_list(&[good, bad], 3 * 16);
    let file = finish_canonical_graph(b, dl, 5);

    let ingested = ingest(&file);
    assert!(
        ingested.dataset.episodes[0].streams.is_empty(),
        "a partially readable list must not be decoded as if it were whole"
    );
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("not decoded")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_compressed_block_that_lies_about_its_expansion_is_refused_not_allocated() {
    // `dz_org_data_length` is a claim by the file. Believed, a 60-byte block asks for 8 GiB.
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.zipped_data(&well_formed_records(0..5), 0, 16);
    // Overwrite the declared original length in place: header (24) + 0 links + 8 bytes of prologue.
    let at = dz as usize + 24 + 8;
    b.bytes[at..at + 8].copy_from_slice(&(8u64 << 30).to_le_bytes());
    let file = finish_canonical_graph(b, dz, 5);
    let path = write_temp(&file, ".mf4");

    let err = Mdf4Adapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a forged expansion must be refused");
    assert!(
        matches!(err, IngestError::DecompressionBudgetExceeded { .. }),
        "{err:?}"
    );
}

#[test]
fn a_compressed_block_whose_stream_is_corrupt_is_reported_rather_than_half_read() {
    // A truncated deflate stream decompresses to fewer bytes than declared. Taking that as the whole
    // measurement drops its tail in silence, and the shortened run still reads as complete.
    let mut b = Mf4Builder::new(b"veridex ");
    let dz = b.zipped_data(&well_formed_records(0..5), 0, 16);
    // Corrupt the last byte of the deflate payload.
    let end = b.bytes.len();
    b.bytes[end - 1] ^= 0xff;
    let file = finish_canonical_graph(b, dz, 5);

    let ingested = ingest(&file);
    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("##DZ")),
        "{:?}",
        ingested.report.unread_sources
    );
}

// --- `##SI`: what the measurement says about where its samples came from ------------------------

/// The canonical fixture with a `##SI` acquisition source on its channel group, and optionally a
/// finer one on the `speed` channel.
fn file_with_sources(per_channel: bool) -> Vec<u8> {
    let mut b = Mf4Builder::new(b"veridex ");
    let dt = b.block(b"##DT", &[], &well_formed_records(0..5));

    let cg_si = b.source("Powertrain ECU", "chassis", 1, 2); // ECU on CAN
    let cn_si = b.source("Wheel speed sensor", "", 3, 0); // I/O, no bus

    let cc = b.linear_conversion(10.0, 0.5);
    let temp = b.channel("temperature", 0, 0, INT_LE, 0, 12, 32, None);
    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 8, 16, Some(cc));
    if per_channel {
        b.patch_link(speed, 3, cn_si);
    }
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, speed);
    b.patch_link(speed, 0, temp);

    let cg = b.channel_group(5, 16);
    b.patch_link(cg, 1, time);
    b.patch_link(cg, 3, cg_si);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);
    b.finish(dg, 1_700_000_000_000_000_000)
}

fn sensor_provenance(ingested: &veridex_core::adapter::Ingested) -> Option<String> {
    ingested.dataset.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "sensor")
        .and_then(|e| e.value.clone())
}

#[test]
fn an_acquisition_source_becomes_the_sensor_provenance_the_file_already_named() {
    // An MF4 scored 0/6 on provenance coverage while naming, in every channel group, the ECU that
    // produced its samples and the bus it sat on. `provenance.sensor` is exactly that question, and
    // the answer was in the file the whole time.
    let ingested = ingest(&file_with_sources(false));
    assert_eq!(
        sensor_provenance(&ingested).as_deref(),
        Some("Powertrain ECU (CAN)"),
        "the source is qualified by its bus: two ECUs of the same name on different buses are two \
         sources"
    );
    // Extracted from the file's own bytes, never asserted by a producer.
    let sensor = ingested.dataset.provenance[0]
        .elements
        .iter()
        .find(|e| e.key == "sensor")
        .expect("a sensor element");
    assert_eq!(sensor.class, veridex_core::cdm::ProvenanceClass::Known);
    assert!(
        ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("##SI") && f.contains("provenance.sensor")),
        "{:?}",
        ingested.report.mapped_fields
    );
}

#[test]
fn a_channels_own_source_is_named_beside_its_groups() {
    // A channel may name a source finer than its group's — a sensor on an ECU's raster. Both are in
    // the file, so both reach provenance, each once however many channels point at them.
    let ingested = ingest(&file_with_sources(true));
    let sensors: Vec<String> = ingested.dataset.provenance[0]
        .elements
        .iter()
        .filter(|e| e.key == "sensor")
        .filter_map(|e| e.value.clone())
        .collect();
    assert_eq!(
        sensors,
        vec![
            "Powertrain ECU (CAN)".to_string(),
            "Wheel speed sensor".to_string()
        ],
        "one element per source, so a lineage document names both rather than one agent called \
         `A, B`"
    );
}

#[test]
fn a_measurement_that_names_no_source_claims_none() {
    // The mapped-field list is a statement that this run read something. A file with no `##SI`
    // anywhere must not have the report say the run read one.
    let ingested = ingest(&well_formed_file(5));
    assert_eq!(sensor_provenance(&ingested), None);
    assert!(
        !ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("##SI")),
        "{:?}",
        ingested.report.mapped_fields
    );
}

#[test]
fn a_source_block_that_names_nothing_is_not_a_source() {
    // An `##SI` whose name block is missing or empty would put an empty string into
    // `provenance.sensor` — which reads as extracted knowledge and is not any.
    let mut b = Mf4Builder::new(b"veridex ");
    let dt = b.block(b"##DT", &[], &well_formed_records(0..3));
    let empty_si = b.block(b"##SI", &[0, 0, 0], &[1, 2, 0, 0, 0, 0, 0, 0]);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    let speed = b.channel("speed", 0, 0, UINT_LE, 0, 8, 16, None);
    b.patch_link(time, 0, speed);
    let cg = b.channel_group(3, 16);
    b.patch_link(cg, 1, time);
    b.patch_link(cg, 3, empty_si);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    let ingested = ingest(&b.finish(dg, 0));
    assert_eq!(sensor_provenance(&ingested), None);
    assert!(
        !ingested.dataset.episodes[0].streams.is_empty(),
        "the measurement still reads"
    );
}

// --- Bit-packed channels: how an automotive measurement actually stores bus signals -------------

#[test]
fn a_bit_packed_little_endian_channel_is_decoded_at_its_offset_and_width() {
    // A 12-bit pedal position starting three bits into a byte is ordinary in an MF4 carrying bus
    // traffic, not exotic. Refusing every non-byte-aligned channel meant such a file produced almost
    // no streams at all: the measurement was there, and every check ran on what was left of it.
    //
    // Record layout, 8 bytes: `[0..8) t f64` is the master, then a second record byte block holding
    // `pedal` at bit 3 for 12 bits and `gear` at bit 15 for 4 bits — a straddling field and one
    // packed above it, both inside the same little-endian word.
    let mut b = Mf4Builder::new(b"veridex ");
    let pedals: [u16; 4] = [0, 1365, 2730, 4095];
    let gears: [u16; 4] = [1, 3, 5, 7];
    let mut data = Vec::new();
    for i in 0..4 {
        let t = i as f64 * 0.1;
        data.extend_from_slice(&t.to_le_bytes());
        // bits 3..15 = pedal, bits 15..19 = gear, inside a 32-bit little-endian word.
        let packed: u32 = (u32::from(pedals[i]) << 3) | (u32::from(gears[i]) << 15);
        data.extend_from_slice(&packed.to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);

    let gear = b.channel("gear", 0, 0, UINT_LE, 7, 9, 4, None);
    let pedal = b.channel("pedal", 0, 0, UINT_LE, 3, 8, 12, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, pedal);
    b.patch_link(pedal, 0, gear);

    let cg = b.channel_group(4, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    let ingested = ingest(&b.finish(dg, 0));
    let ep = &ingested.dataset.episodes[0];
    let stats = |name: &str| {
        ep.streams
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("stream `{name}`"))
            .observed_stats
            .expect("statistics")
    };
    // 12 bits three bits in: the full 0..4095 range comes back, not a byte-sliced fragment of it.
    let pedal_stats = stats("pedal");
    assert_eq!((pedal_stats.min, pedal_stats.max), (0.0, 4095.0));
    // 4 bits starting one bit into the second byte — the field is masked, not the whole byte.
    let gear_stats = stats("gear");
    assert_eq!((gear_stats.min, gear_stats.max), (1.0, 7.0));
    assert!(
        ingested.report.unread_sources.is_empty(),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_signed_bit_packed_channel_is_sign_extended_from_its_own_width() {
    // A 10-bit signed steering angle is negative above 511, not a large positive number. Masking
    // without sign-extending from the *field's* width turns every negative sample into a spike.
    let mut b = Mf4Builder::new(b"veridex ");
    let raws: [i16; 3] = [-512, -1, 511];
    let mut data = Vec::new();
    for (i, raw) in raws.iter().enumerate() {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        let field = (*raw as u16) & 0x03FF;
        data.extend_from_slice(&(u32::from(field) << 2).to_le_bytes());
    }
    let dt = b.block(b"##DT", &[], &data);

    let angle = b.channel("angle", 0, 0, INT_LE, 2, 8, 10, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, angle);
    let cg = b.channel_group(3, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    let ingested = ingest(&b.finish(dg, 0));
    let stats = ingested.dataset.episodes[0].streams[0]
        .observed_stats
        .expect("statistics");
    assert_eq!((stats.min, stats.max), (-512.0, 511.0));
}

#[test]
fn a_bit_packed_big_endian_channel_is_reported_rather_than_read_the_wrong_way_round() {
    // MDF's bit numbering for a straddling Motorola field is not the DBC sawtooth, and a wrong
    // reading here is a plausible number rather than a failure. So it is declined out loud.
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..3 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&[0u8; 4]);
    }
    let dt = b.block(b"##DT", &[], &data);

    const UINT_BE: u8 = 1;
    let packed = b.channel("packed_be", 0, 0, UINT_BE, 3, 8, 12, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, packed);
    let cg = b.channel_group(3, 12);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    let ingested = ingest(&b.finish(dg, 0));
    assert!(ingested.dataset.episodes[0].streams.is_empty());
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|u| u.note.contains("big-endian") && u.note.contains("whole bytes")),
        "{:?}",
        ingested.report.unread_sources
    );
}

#[test]
fn a_field_that_runs_past_the_record_is_declined_rather_than_read_from_the_next_one() {
    // `cn_byte_offset` and `cn_bit_count` are the file's claims. A field the record is too short to
    // hold must not be assembled from whatever bytes follow it — that reads the next record's data
    // as this one's, for every sample.
    let mut b = Mf4Builder::new(b"veridex ");
    let mut data = Vec::new();
    for i in 0..3 {
        data.extend_from_slice(&(i as f64 * 0.1).to_le_bytes());
        data.extend_from_slice(&[0xffu8; 2]);
    }
    let dt = b.block(b"##DT", &[], &data);

    // 32 bits starting at byte 8 of a 10-byte record: two bytes short.
    let over = b.channel("over_the_end", 0, 0, UINT_LE, 0, 8, 32, None);
    let time = b.channel("t", 2, 1, FLOAT_LE, 0, 0, 64, None);
    b.patch_link(time, 0, over);
    let cg = b.channel_group(3, 10);
    b.patch_link(cg, 1, time);
    let dg = b.data_group(0);
    b.patch_link(dg, 1, cg);
    b.patch_link(dg, 2, dt);

    let ingested = ingest(&b.finish(dg, 0));
    assert!(
        ingested.dataset.episodes[0].streams.is_empty(),
        "a field that does not fit its record yields no stream"
    );
}

// --- The corruption sweep, over every shape this reader now parses ------------------------------

/// One of each file shape the adapter has a distinct parsing path for.
///
/// The original sweep ran over a single uncompressed `##DT` fixture, which never reached the
/// decompressor, the data-list walker, the record demultiplexer, the bit-field slicer or the
/// conversion tables — every one of which reads lengths, counts and offsets straight out of an
/// untrusted file.
fn hostile_corpus() -> Vec<(&'static str, Vec<u8>)> {
    let compressed = {
        let mut b = Mf4Builder::new(b"veridex ");
        let dz = b.zipped_data(&well_formed_records(0..5), 0, 16);
        finish_canonical_graph(b, dz, 5)
    };
    let transposed = {
        let mut b = Mf4Builder::new(b"veridex ");
        let dz = b.zipped_data(&well_formed_records(0..5), 1, 16);
        finish_canonical_graph(b, dz, 5)
    };
    let listed = {
        let mut b = Mf4Builder::new(b"veridex ");
        let first = b.zipped_data(&well_formed_records(0..2), 1, 16);
        let second = b.block(b"##DT", &[], &well_formed_records(2..5));
        let dl = b.data_list(&[first, second], 2 * 16);
        let hl = b.header_list(dl);
        finish_canonical_graph(b, hl, 5)
    };
    vec![
        ("uncompressed", well_formed_file(5)),
        ("deflated", compressed),
        ("transposed", transposed),
        ("header-list", listed),
        ("unsorted", unsorted_file(5, false)),
        ("with-sources", file_with_sources(true)),
        (
            "rational",
            converted_file(|b| b.conversion(2, &[0.0, 2.0, 1.0, 0.0, 0.0, 1.0])),
        ),
        (
            "table",
            converted_file(|b| b.conversion(4, &[0.0, 100.0, 4.0, 500.0])),
        ),
        (
            "range-table",
            converted_file(|b| b.conversion(6, &[0.0, 2.0, 10.0, 2.0, 4.0, 20.0, -1.0])),
        ),
    ]
}

#[test]
fn every_parsing_path_survives_a_corrupted_or_truncated_file() {
    // The invariant, for every shape and every byte: ingest either errors or yields something, and
    // never panics, hangs, or allocates without bound. Each of these files states its own lengths,
    // counts, record ids and table sizes, and every one of those is a claim an attacker controls.
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("sweep.mf4");
    let mut ingests = 0usize;
    for (label, good) in hostile_corpus() {
        // Bound the sweep so the whole corpus stays well inside a test run: at most ~300 mutations
        // per shape per mode, spread evenly across the file rather than clustered at its head.
        let stride = (good.len() / 300).max(1);
        for at in (0..good.len()).step_by(stride) {
            for mutate in [0xFFu8, 0x00, 0x7F] {
                let mut corrupt = good.clone();
                corrupt[at] ^= mutate;
                std::fs::write(&path, &corrupt).expect("write");
                let _ = Mdf4Adapter.ingest(&Source::Local(path.clone()), &IngestOptions::default());
                ingests += 1;
            }
        }
        // Every prefix, at the same stride: a file that ends mid-block, mid-record, mid-deflate
        // stream or mid-table.
        for cut in (0..good.len()).step_by(stride) {
            std::fs::write(&path, &good[..cut]).expect("write");
            let _ = Mdf4Adapter.ingest(&Source::Local(path.clone()), &IngestOptions::default());
            ingests += 1;
        }
        // And the header-only read over the same corruption, which walks the block graph without
        // opening a data block — a different path with its own offsets to get wrong.
        let metadata = IngestOptions {
            metadata_only: true,
            ..IngestOptions::default()
        };
        for at in (0..good.len()).step_by(stride.max(2)) {
            let mut corrupt = good.clone();
            corrupt[at] ^= 0xFF;
            std::fs::write(&path, &corrupt).expect("write");
            let _ = Mdf4Adapter.ingest(&Source::Local(path.clone()), &metadata);
            ingests += 1;
        }
        assert!(ingests > 0, "{label} contributed no mutations");
    }
}
