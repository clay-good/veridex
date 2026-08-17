//! Multiplexed CAN messages, and two shapes of untrusted DBC that used to abort the process.
//!
//! A multiplexed message reuses the same payload bytes for different signals from frame to frame,
//! and one signal — the multiplexor, marked `M` — says which set the current frame carries. The
//! indicator was never parsed, so every multiplexed signal was decoded from every frame of its id:
//! a frame whose selector said `m0` still produced an `m1` sample, reading `m0`'s bytes through
//! `m1`'s layout. That is a plausible number that was never on the bus, given a CDM stream of its
//! own and graded by every check.

use std::collections::BTreeMap;
use std::fs;

use veridex_core::adapter::candbc::CanDbcAdapter;
use veridex_core::adapter::{Adapter, IngestOptions, Source};
use veridex_core::cdm::Dataset;

/// Message 512: an 8-bit selector in byte 0, then two 16-bit signals sharing bytes 1–2.
const MUX_DBC: &str = "\
BO_ 512 Muxed: 8 ECU
 SG_ Selector M : 0|8@1+ (1,0) [0|255] \"\" Vector__XXX
 SG_ ValueA m0 : 8|16@1+ (1,0) [0|65535] \"\" Vector__XXX
 SG_ ValueB m1 : 8|16@1+ (0.1,0) [0|6553.5] \"\" Vector__XXX
";

fn ingest(dbc: &str, log: &str) -> Dataset {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("bus.dbc"), dbc).unwrap();
    fs::write(dir.path().join("drive.log"), log).unwrap();
    CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("candbc ingest")
        .dataset
}

/// Every stream and how many samples it carries.
fn sample_counts(ds: &Dataset) -> BTreeMap<String, usize> {
    ds.episodes
        .iter()
        .flat_map(|e| e.streams.iter())
        .map(|s| (s.name.clone(), s.frames.len()))
        .collect()
}

/// Two frames, both selecting `m0`. `ValueB` was never transmitted, so it must not exist as data.
#[test]
fn a_multiplexed_signal_absent_from_every_frame_produces_no_samples() {
    // Selector = 0, payload 0x03E8 = 1000 little-endian → ValueA = 1000.
    let log = "\
(1000.000000) can0 200#00E80300000000
(1000.100000) can0 200#00E80300000000
";
    let ds = ingest(MUX_DBC, log);
    let counts = sample_counts(&ds);

    assert_eq!(counts.get("Muxed.Selector"), Some(&2));
    assert_eq!(counts.get("Muxed.ValueA"), Some(&2));
    assert!(
        counts.get("Muxed.ValueB").is_none_or(|n| *n == 0),
        "the selector said m0 in every frame; ValueB was never on the bus: {counts:?}"
    );
}

/// The indicator must be stripped from the name, not carried into the CDM as part of it.
#[test]
fn the_multiplexer_indicator_is_not_part_of_the_signal_name() {
    let log = "(1000.000000) can0 200#00E80300000000\n";
    let ds = ingest(MUX_DBC, log);
    for name in sample_counts(&ds).keys() {
        assert!(
            !name.ends_with(" M") && !name.contains(" m"),
            "the mux indicator is metadata, not part of the signal's name: {name}"
        );
    }
}

/// Both sets do appear when the bus really carries both, and each decodes through its own scaling.
#[test]
fn each_multiplexed_set_decodes_on_the_frames_that_carry_it() {
    let log = "\
(1000.000000) can0 200#00E80300000000
(1000.100000) can0 200#01E80300000000
";
    let ds = ingest(MUX_DBC, log);
    let counts = sample_counts(&ds);
    assert_eq!(counts.get("Muxed.ValueA"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("Muxed.ValueB"), Some(&1), "{counts:?}");

    // ValueA has factor 1 and ValueB factor 0.1 over the same 0x03E8 bytes, so a set decoded
    // through the wrong layout would be visible as the wrong number.
    let ts_of = |name: &str| {
        ds.episodes[0]
            .streams
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.frames.iter().map(|f| f.ts).collect::<Vec<_>>())
    };
    assert_eq!(ts_of("Muxed.ValueA"), Some(vec![1_000_000_000_000]));
    assert_eq!(ts_of("Muxed.ValueB"), Some(vec![1_000_100_000_000]));
}

/// An ordinary, non-multiplexed DBC is unaffected.
#[test]
fn a_message_with_no_multiplexor_still_decodes_every_signal_on_every_frame() {
    let dbc = "\
BO_ 256 Plain: 8 ECU
 SG_ Speed : 0|16@1+ (0.25,0) [0|16383] \"kph\" Vector__XXX
 SG_ Temp : 16|8@1+ (1,-40) [-40|215] \"degC\" Vector__XXX
";
    let log = "\
(1000.000000) can0 100#4001000000000000
(1000.100000) can0 100#8002000000000000
";
    let counts = sample_counts(&ingest(dbc, log));
    assert_eq!(counts.get("Plain.Speed"), Some(&2));
    assert_eq!(counts.get("Plain.Temp"), Some(&2));
}

// ---- untrusted-input shapes that used to abort the process ---------------------------------------

/// `1i64 << 63` is `i64::MIN`, so sign-extending a 63-bit signed signal overflowed and panicked in
/// any debug or CI build. A DBC is untrusted input and may declare any width; the existing coverage
/// tested 64 and 65 and stepped over 63.
#[test]
fn a_sixty_three_bit_signed_signal_does_not_abort_the_run() {
    let dbc = "\
BO_ 256 Wide: 8 ECU
 SG_ S : 0|63@1- (1,0) [0|0] \"\" Vector__XXX
";
    let log = "(1000.000000) can0 100#FFFFFFFFFFFFFFFF\n";
    let ds = ingest(dbc, log);
    let stream = ds.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "Wide.S")
        .expect("the signal decodes rather than aborting");
    assert_eq!(stream.frames.len(), 1);
}

/// Every declared width, including the ones a real DBC never uses.
#[test]
fn no_declared_signal_width_aborts_the_run() {
    for length in 1..=65u32 {
        let dbc =
            format!("BO_ 256 W: 8 ECU\n SG_ S : 0|{length}@1- (1,0) [0|0] \"\" Vector__XXX\n");
        let log = "(1000.000000) can0 100#FFFFFFFFFFFFFFFF\n";
        let _ = ingest(&dbc, log);
    }
}

// ---- timestamps are composed, not multiplied through an f64 --------------------------------------

/// Epoch-scale seconds times 1e9 needs 61 bits of mantissa and `f64` has 53, so two lines exactly
/// 1 µs apart came out 1024 ns apart — synthetic jitter handed to the temporal checks as though the
/// bus had produced it. Immaterial at a 10 ms raster, material at 1 kHz.
#[test]
fn adjacent_candump_timestamps_keep_their_microsecond_spacing() {
    let dbc = "\
BO_ 256 Plain: 8 ECU
 SG_ Speed : 0|16@1+ (1,0) [0|65535] \"kph\" Vector__XXX
";
    let log = "\
(1755000000.123456) can0 100#0100000000000000
(1755000000.123457) can0 100#0200000000000000
";
    let ds = ingest(dbc, log);
    let ts: Vec<i64> = ds.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "Plain.Speed")
        .expect("stream")
        .frames
        .iter()
        .map(|f| f.ts)
        .collect();

    assert_eq!(ts.len(), 2);
    assert_eq!(ts[0], 1_755_000_000_123_456_000);
    assert_eq!(
        ts[1] - ts[0],
        1_000,
        "two lines 1 µs apart are 1000 ns apart, not 1024"
    );
}
