//! Integration tests for the CAN+DBC adapter, driven by real `.dbc` + candump `.log` files.

use std::fs;

use veridex_core::adapter::candbc::CanDbcAdapter;
use veridex_core::adapter::{Adapter, Detection, IngestOptions, Source};
use veridex_core::cdm::Modality;
use veridex_core::check::Check;

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

/// A CAN signal is the one payload in this crate that is *decoded* rather than fingerprinted — a
/// wheel speed is a number, not an opaque blob — so the statistical family can grade it. Until it
/// did, the example the abstention finding was written around was a real gap: a log with a wheel
/// speed pinned at its rail for most of the recording scored `data 100` with no statistical
/// findings, over a certificate listing all five statistical checks as run.
#[test]
fn a_signal_pinned_at_its_rail_is_flagged_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    // `EngineSpeed` sits exactly at its 16-bit maximum for 7 of every 10 frames.
    let mut log = String::new();
    for i in 0..100u32 {
        let raw: u32 = if i % 10 < 7 { 0xFFFF } else { i * 100 };
        log.push_str(&format!(
            "({:.6}) can0 100#{:02X}{:02X}000000000000\n",
            1000.0 + f64::from(i) * 0.01,
            raw & 0xFF,
            (raw >> 8) & 0xFF,
        ));
    }
    fs::write(dir.path().join("drive.log"), log).unwrap();

    let outcome = veridex_core::pipeline::run_check(
        &veridex_core::default_registry(),
        &Source::Local(dir.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    let saturated = outcome
        .verdict
        .findings
        .iter()
        .find(|f| f.code == "STATISTICAL.SATURATED")
        .expect("the pinned signal is flagged");
    assert!(
        saturated.message.contains("EngineData.EngineSpeed") && saturated.message.contains("70%"),
        "{}",
        saturated.message
    );

    // And the abstention that stood in for this is gone: the values were measured.
    assert!(
        !outcome
            .verdict
            .findings
            .iter()
            .any(|f| f.code == "STATISTICAL.UNMEASURED_VALUES"),
        "a CAN log's values are read, so the family does not abstain on it"
    );
}

/// The statistics are recomputed from the decoded values, not from the raw frame bytes — the
/// difference is the DBC's factor and offset, which is the whole point of decoding.
#[test]
fn recomputed_statistics_are_of_the_decoded_signal_not_the_raw_bits() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vehicle.dbc"), DBC).unwrap();
    // Raw 0, 4, 8, 12 on a signal with factor 0.25 → 0.0, 1.0, 2.0, 3.0.
    let mut log = String::new();
    for (i, raw) in [0u32, 4, 8, 12].iter().enumerate() {
        log.push_str(&format!(
            "({:.6}) can0 100#{:02X}{:02X}000000000000\n",
            1000.0 + i as f64 * 0.01,
            raw & 0xFF,
            (raw >> 8) & 0xFF,
        ));
    }
    fs::write(dir.path().join("drive.log"), log).unwrap();

    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let speed = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "EngineData.EngineSpeed")
        .expect("the signal stream");
    let stats = speed
        .observed_stats
        .expect("statistics are recomputed from the values");
    assert_eq!((stats.min, stats.max, stats.mean), (0.0, 3.0, 1.5));
    assert_eq!(
        speed.observed_non_finite,
        Some(0),
        "the values were read and every one was finite — which is what tells a clean signal from \
         one nobody measured"
    );
    assert!(
        speed.stats.is_none(),
        "a DBC stores no summary statistics, so there is nothing to compare against"
    );
}

/// The failure this check exists for. A CAN log decoded against the wrong DBC does not error: the
/// bytes are the right length, every signal produces a number, and the timeline is intact. What
/// gives it away is that the numbers stop fitting the ranges the database itself declares.
#[test]
fn a_log_decoded_against_the_wrong_ranges_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    // The same signal layout as `DBC`, but the database claims a span a tenth as wide — what a DBC
    // from a different vehicle variant looks like against this log.
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 ECU\n \
         SG_ EngineSpeed : 0|16@1+ (0.25,0) [0|1000] \"rpm\" Vector__XXX\n",
    )
    .unwrap();
    let mut log = String::new();
    for i in 0..20u32 {
        // Raw 0xF000 → 15,360 rpm, far past the declared 1,000.
        let raw = if i == 5 { 0xF000u32 } else { 100 };
        log.push_str(&format!(
            "({:.6}) can0 100#{:02X}{:02X}000000000000\n",
            1000.0 + f64::from(i) * 0.01,
            raw & 0xFF,
            (raw >> 8) & 0xFF,
        ));
    }
    fs::write(dir.path().join("drive.log"), log).unwrap();

    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    let speed = &ingested.dataset.episodes[0].streams[0];
    let declared = speed
        .declared_range
        .expect("the DBC's [min|max] is carried into the CDM");
    assert_eq!((declared.min, declared.max), (0.0, 1000.0));

    let f = veridex_core::checks::statistical::DeclaredRangeConformance.run(&ingested.dataset);
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].code, "STATISTICAL.OUT_OF_DECLARED_RANGE");
    assert_eq!(f[0].severity, veridex_core::check::Severity::Warning);
    assert!(
        f[0].message.contains("[0, 1000]") && f[0].message.contains("15360"),
        "{}",
        f[0].message
    );
}

/// A DBC that states no real range — `[0|0]`, which is what a writer emits for "unspecified" —
/// declares nothing, and a check that read it as a bound would report every non-zero sample as out
/// of range. The absence has to stay an absence.
#[test]
fn a_dbc_with_no_stated_range_declares_nothing() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 ECU\n \
         SG_ EngineSpeed : 0|16@1+ (0.25,0) [0|0] \"rpm\" Vector__XXX\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("drive.log"),
        "(1000.000000) can0 100#40010000AABBCCDD\n(1000.100000) can0 100#80020000AABBCCDD\n",
    )
    .unwrap();

    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    assert!(ingested.dataset.episodes[0].streams[0]
        .declared_range
        .is_none());
    assert!(
        veridex_core::checks::statistical::DeclaredRangeConformance
            .run(&ingested.dataset)
            .is_empty(),
        "nothing declared, nothing to conform to"
    );
}

/// And the ordinary case: values inside the range their database declares produce no finding.
#[test]
fn values_within_the_declared_range_are_clean() {
    let dir = write_dataset();
    let ingested = CanDbcAdapter
        .ingest(
            &Source::Local(dir.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest");
    assert!(veridex_core::checks::statistical::DeclaredRangeConformance
        .run(&ingested.dataset)
        .is_empty());
}

// --- What the database says about which ECU produced the traffic --------------------------------

fn ingest_dir(dir: &std::path::Path) -> veridex_core::adapter::Ingested {
    CanDbcAdapter
        .ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
        .expect("ingest")
}

fn provenance_value(ingested: &veridex_core::adapter::Ingested, key: &str) -> Option<String> {
    ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .find(|e| e.key == key)
        .and_then(|e| e.value.clone())
}

#[test]
fn the_transmitting_ecu_becomes_sensor_provenance() {
    // A `BO_` line names the ECU that puts that message on the bus. That is what produced the data,
    // and it is exactly what `provenance.sensor` asks — yet a CAN+DBC dataset used to carry no
    // provenance record at all, not even the `source_format` element every other adapter emits, so
    // it scored 0/6 while its own database named the node.
    let dir = write_dataset();
    let ingested = ingest_dir(dir.path());
    assert_eq!(
        provenance_value(&ingested, "sensor").as_deref(),
        Some("ECU")
    );
    assert_eq!(
        provenance_value(&ingested, "source_format").as_deref(),
        Some("candbc")
    );
    assert!(
        ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("BO_ transmitter") && f.contains("provenance.sensor")),
        "{:?}",
        ingested.report.mapped_fields
    );

    let engine = veridex_core::checks::default_engine().unwrap();
    let hash = veridex_core::content_hash(&ingested.dataset);
    let verdict = engine.run(&ingested.dataset, hash, &veridex_core::RunConfig::default());
    assert!(
        verdict
            .findings
            .iter()
            .all(|f| f.code != "PROVENANCE.MISSING_SENSOR"),
        "an extracted transmitter clears the MISSING_SENSOR finding"
    );
}

#[test]
fn every_transmitting_node_is_named_once() {
    // A bus carries several ECUs, and each is named once however many of its messages the log holds.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 ECM\n SG_ Rpm : 0|16@1+ (1,0) [0|65535] \"rpm\" Vector__XXX\n\
         BO_ 257 BrakeData: 8 ABS\n SG_ Pressure : 0|16@1+ (1,0) [0|65535] \"bar\" Vector__XXX\n\
         BO_ 258 MoreEngine: 8 ECM\n SG_ Torque : 0|16@1+ (1,0) [0|65535] \"Nm\" Vector__XXX\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("drive.log"),
        "(1000.000000) can0 100#0100000000000000\n\
         (1000.100000) can0 101#0200000000000000\n\
         (1000.200000) can0 102#0300000000000000\n\
         (1000.300000) can0 100#0400000000000000\n",
    )
    .unwrap();
    // One element per node, not one joined string: a bus with several ECUs on it has several
    // sensors, and a lineage document naming a single agent called "A, B" names an agent nobody has.
    let ingested = ingest_dir(dir.path());
    let sensors: Vec<String> = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|r| &r.elements)
        .filter(|e| e.key == "sensor")
        .filter_map(|e| e.value.clone())
        .collect();
    assert_eq!(
        sensors,
        vec!["ABS".to_string(), "ECM".to_string()],
        "each node once, in a stable order"
    );
}

#[test]
fn the_dbc_placeholder_node_is_not_a_sensor() {
    // `Vector__XXX` is the DBC's way of saying "no node specified". Recording it would put a value
    // that is present in form and empty in substance into the coverage score — the exact thing
    // `has_real_value` exists to keep out.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 Vector__XXX\n SG_ Rpm : 0|16@1+ (1,0) [0|65535] \"rpm\" Vector__XXX\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("drive.log"),
        "(1000.000000) can0 100#0100000000000000\n",
    )
    .unwrap();
    let ingested = ingest_dir(dir.path());
    assert_eq!(provenance_value(&ingested, "sensor"), None);
    assert!(
        !ingested
            .report
            .mapped_fields
            .iter()
            .any(|f| f.contains("BO_ transmitter")),
        "{:?}",
        ingested.report.mapped_fields
    );
    // The dataset still ingests and still carries the format element.
    assert_eq!(
        provenance_value(&ingested, "source_format").as_deref(),
        Some("candbc")
    );
}

#[test]
fn a_node_that_never_transmitted_is_not_claimed() {
    // The database declares what the *network* holds; the log holds what was actually on the wire.
    // Naming an ECU whose messages never appeared would attribute this data to a node that produced
    // none of it.
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("vehicle.dbc"),
        "BO_ 256 EngineData: 8 ECM\n SG_ Rpm : 0|16@1+ (1,0) [0|65535] \"rpm\" Vector__XXX\n\
         BO_ 999 NeverSeen: 8 GATEWAY\n SG_ X : 0|8@1+ (1,0) [0|255] \"\" Vector__XXX\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("drive.log"),
        "(1000.000000) can0 100#0100000000000000\n",
    )
    .unwrap();
    assert_eq!(
        provenance_value(&ingest_dir(dir.path()), "sensor").as_deref(),
        Some("ECM"),
        "only the node whose traffic is actually in the log"
    );
}
