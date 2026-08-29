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

/// Frames on an id the DBC does not define carried payload down the bus and went into no stream.
/// That is unread data, not a field the CDM has no shape for — so it has to reach the *verdict*,
/// where a reader of the score can see it, and not only `inspect`.
#[test]
fn an_undefined_can_id_is_reported_as_an_unread_source() {
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
            .unread_sources
            .iter()
            .any(|u| u.source_path.contains("0x200") && u.note.contains("coverage gap")),
        "the undefined CAN id 0x200 must be disclosed as unread: {:?}",
        ingested.report.unread_sources
    );

    let outcome = veridex_core::pipeline::run_check(
        &veridex_core::default_registry(),
        &Source::Local(dir.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    let finding = outcome
        .verdict
        .findings
        .iter()
        .find(|f| f.code == "COVERAGE.SOURCE_UNREAD")
        .expect("undecoded bus traffic surfaces as a coverage finding");
    assert_eq!(finding.severity, veridex_core::check::Severity::Warning);
    assert!(finding.message.contains("0x200"), "{}", finding.message);
}

/// A partial DBC over a busy bus can leave hundreds of ids undefined. Every unread source is named
/// in the finding, so the list is bounded — and the frames behind the unnamed ids are still counted,
/// because a bounded disclosure must not become a shortened one.
#[test]
fn many_undefined_ids_are_counted_rather_than_all_listed() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    let mut log = String::from(
        "(1000.000000) can0 100#4001000012343412
",
    );
    // 20 undefined ids; the busiest carries the most frames, so it must be among those named.
    for i in 0..20u32 {
        for n in 0..=i {
            log.push_str(&format!(
                "(1000.{:06}) can0 {:03X}#DEADBEEF
",
                i * 100 + n,
                0x200 + i
            ));
        }
    }
    fs::write(dir.path().join("drive.log"), log).unwrap();

    let unread = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .report
        .unread_sources;
    let named: Vec<&str> = unread.iter().map(|u| u.source_path.as_str()).collect();
    assert_eq!(named.len(), 9, "eight ids plus one remainder: {named:?}");
    assert!(
        named.contains(&"can id 0x213"),
        "the busiest id is named: {named:?}"
    );
    let rest = unread
        .iter()
        .find(|u| u.source_path == "12 further can id(s)")
        .expect("the ids past the cap are counted");
    // The eight busiest ids (0x20c..0x213) carry 13..20 frames; the twelve named only in the
    // remainder carry 1..12, which is 78 frames that no stream holds.
    assert!(
        rest.note.starts_with("78 more frame(s)"),
        "the frames behind the unnamed ids are still counted: {}",
        rest.note
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

/// A log every line of which fails to parse used to ingest as a successful, zero-finding,
/// `Coverage::Full` dataset with an empty `unmapped` — a signable clean bill of health over a file
/// that produced nothing. Reading silence as a pass, at the adapter layer, where no check can
/// recover from it.
#[test]
fn a_log_that_parsed_to_nothing_is_refused_rather_than_ingested_clean() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    fs::write(
        dir.path().join("drive.log"),
        // CAN-FD (`##`), an RTR frame, and binary garbage — none of them candump frames.
        "(1000.000000) can0 100##140010000\n\
         (1000.100000) can0 100#R\n\
         \x01\x02 not a log line at all\n",
    )
    .unwrap();

    let err = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect_err("a log that parsed to nothing must not ingest as a clean dataset");
    let text = err.to_string();
    assert!(
        text.contains("none of the 3 content line(s)"),
        "the refusal must say how much did not parse: {text}"
    );
}

/// A log where *some* lines fail is not refused — but those frames were on the bus and are not in
/// the verdict, which is exactly what the fidelity report exists to say.
#[test]
fn partially_unreadable_log_lines_are_reported_as_a_coverage_gap() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    fs::write(
        dir.path().join("drive.log"),
        "# a comment, which is not content\n\
         \n\
         (1000.000000) can0 100#4001000012343412\n\
         (1000.100000) can0 100##140010000\n",
    )
    .unwrap();

    let report = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("one good line is still a dataset")
        .report;

    let note = report
        .unread_sources
        .iter()
        .find(|u| u.source_path == "candump log lines")
        .expect("the skipped line must be disclosed as unread, which is what reaches the verdict");
    assert!(
        note.note.contains("1 of 2 content line(s)"),
        "blank lines and comments are not content: {}",
        note.note
    );
}
