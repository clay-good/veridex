//! Integration tests for the CAN+DBC adapter, driven by real `.dbc` + candump `.log` files.

use std::fs;

use veridex_core::adapter::candbc::CanDbcAdapter;
use veridex_core::adapter::{Adapter, Detection, IngestOptions, Source};
use veridex_core::cdm::Modality;

/// `WheelSpeedBE` (Motorola, bytes 4–5 MSB-first) and `WheelSpeedLE` (Intel, bytes 6–7) are laid out
/// over byte-swapped copies of the same value, so a correct decoder gives them identical samples.
const DBC: &str = "\
BO_ 256 EngineData: 8 ECU
 SG_ EngineSpeed : 0|16@1+ (0.25,0) [0|16383.75] \"rpm\" Vector__XXX
 SG_ CoolantTemp : 16|8@1+ (1,-40) [-40|215] \"degC\" Vector__XXX
 SG_ WheelSpeedBE : 39|16@0+ (1,0) [0|65535] \"kph\" Vector__XXX
 SG_ WheelSpeedLE : 48|16@1+ (1,0) [0|65535] \"kph\" Vector__XXX
";

/// Two frames of the defined message plus one frame of an undefined id (a DBC-coverage gap).
const LOG: &str = "\
(1000.000000) can0 100#4001000012343412
(1000.100000) can0 100#80020000ABCDCDAB
(1000.200000) can0 200#DEADBEEF
";

fn write_dataset() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    fs::write(dir.path().join("drive.log"), LOG).unwrap();
    dir
}

#[test]
fn detects_a_directory_with_a_dbc() {
    let dir = write_dataset();
    assert_eq!(
        CanDbcAdapter.detect(&Source::Local(dir.path().to_path_buf())),
        Detection::Yes {
            version: Some("dbc".into())
        }
    );
    // A directory with no .dbc is not recognized.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        CanDbcAdapter.detect(&Source::Local(empty.path().to_path_buf())),
        Detection::No
    );
}

#[test]
fn decodes_signals_into_named_streams() {
    let dir = write_dataset();
    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let ep = &ingested.dataset.episodes[0];
    // One stream per defined signal, both byte orders included.
    assert_eq!(ep.streams.len(), 4);
    let speed = ep
        .streams
        .iter()
        .find(|s| s.name == "EngineData.EngineSpeed")
        .expect("EngineSpeed stream");
    assert_eq!(speed.modality, Modality::CanSignal);
    assert_eq!(speed.clock_id, "can");
    // Two frames of message 256 → two samples per signal; each carries a decoded-value fingerprint.
    assert_eq!(speed.frames.len(), 2);
    assert!(speed
        .frames
        .iter()
        .all(|f| f.value_ref.content_hash.is_some()));
    // The two different raw values fingerprint differently.
    assert_ne!(
        speed.frames[0].value_ref.content_hash,
        speed.frames[1].value_ref.content_hash
    );
    // Episode spans the two decoded frames (the undefined-id frame carries no signal).
    assert_eq!(ep.start_ts, Some(1_000_000_000_000));
}

#[test]
fn a_motorola_signal_decodes_to_the_same_samples_as_its_intel_twin() {
    let dir = write_dataset();
    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let ep = &ingested.dataset.episodes[0];
    let fingerprints = |name: &str| {
        ep.streams
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} stream"))
            .frames
            .iter()
            .map(|f| f.value_ref.content_hash)
            .collect::<Vec<_>>()
    };
    let big_endian = fingerprints("EngineData.WheelSpeedBE");
    // The Motorola signal is decoded, not skipped: two samples, and they differ from each other.
    assert_eq!(big_endian.len(), 2);
    assert_ne!(big_endian[0], big_endian[1]);
    // Byte-swapped copies of the same value must decode identically under the two byte orders.
    assert_eq!(big_endian, fingerprints("EngineData.WheelSpeedLE"));
    // Nothing is reported as an undecodable byte order any more.
    assert!(
        !ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.note.to_lowercase().contains("byte order")),
        "byte order is no longer an ingestion gap: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn an_undefined_can_id_is_reported_as_a_coverage_gap() {
    let dir = write_dataset();
    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|u| u.source_path.contains("0x200") && u.note.contains("coverage gap")),
        "the undefined CAN id 0x200 must be reported as a DBC-coverage gap"
    );
}

#[test]
fn the_registry_autodetects_a_candbc_directory() {
    let dir = write_dataset();
    let registry = veridex_core::default_registry();
    let ingested = registry
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("registry ingest");
    assert_eq!(ingested.report.format_id, "candbc");
}
