//! Behavior tests for the shared `run_check` pipeline — the one implementation of
//! ingest → validate → score that both the CLI and the Python bindings call, so parity is by
//! construction. Driven over a real MCAP file so the whole path (adapter → CDM → engine → score)
//! runs end-to-end.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::PathBuf;

use veridex_core::adapter::{IngestOptions, Source};
use veridex_core::{default_registry, run_check, run_check_with, RunConfig, Tolerances};

/// Write a two-stream MCAP whose clocks drift 500 ms apart (camera spans 1.0 s, robot spans 1.5 s),
/// so `TEMPORAL.CLOCK_SKEW` fires under the default 50 ms tolerance.
fn write_skewed_mcap(tag: &str) -> PathBuf {
    let mut buf = Vec::new();
    {
        let mut w = mcap::Writer::new(Cursor::new(&mut buf)).expect("writer");
        let write_stream =
            |w: &mut mcap::Writer<Cursor<&mut Vec<u8>>>, topic: &str, span_ns: u64| {
                let schema = w.add_schema(topic, "ros2msg", b"").expect("schema");
                let chan = w
                    .add_channel(schema, topic, "cdr", &BTreeMap::new())
                    .expect("channel");
                for i in 0..=10u64 {
                    let t = i * span_ns / 10;
                    w.write_to_known_channel(
                        &mcap::records::MessageHeader {
                            channel_id: chan,
                            sequence: i as u32,
                            log_time: t,
                            publish_time: t,
                        },
                        b"payload",
                    )
                    .expect("write");
                }
            };
        write_stream(&mut w, "/camera", 1_000_000_000);
        write_stream(&mut w, "/robot", 1_500_000_000);
        w.finish().expect("finish");
    }
    // The path must be unique per test: cargo runs the tests in this file concurrently in one
    // process, so keying only on the pid would make two tests share a file and race (one's cleanup
    // deletes the other's input mid-ingest). The per-test `tag` keeps them isolated.
    let mut path = std::env::temp_dir();
    path.push(format!(
        "veridex-pipeline-test-{}-{tag}.mcap",
        std::process::id()
    ));
    std::fs::write(&path, &buf).expect("write mcap");
    path
}

#[test]
fn run_check_ingests_validates_and_scores_end_to_end() {
    let path = write_skewed_mcap("end-to-end");
    let out = run_check(
        &default_registry(),
        &Source::Local(path.clone()),
        None,
        &IngestOptions::default(),
    )
    .expect("run_check succeeds on a well-formed MCAP");

    // The verdict carries the clock-skew finding, and a trust score was computed.
    assert!(out
        .verdict
        .findings
        .iter()
        .any(|f| f.code == "TEMPORAL.CLOCK_SKEW"));
    assert!(out.trust.score <= 100);
    assert!(!out.ingested.dataset.episodes.is_empty());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn run_check_with_applies_the_configured_tolerance() {
    // Guards the pipeline's own wiring: it must build the engine with the run's tolerances, not the
    // defaults. Raising the clock-skew tolerance above the 500 ms drift suppresses the finding.
    let path = write_skewed_mcap("tolerance");
    let cfg = RunConfig {
        tolerances: Tolerances {
            clock_skew_ns: 800_000_000, // 800 ms > the 500 ms drift
            ..Tolerances::default()
        },
        ..RunConfig::default()
    };
    let out = run_check_with(
        &default_registry(),
        &Source::Local(path.clone()),
        None,
        &IngestOptions::default(),
        &cfg,
    )
    .expect("run_check_with succeeds");

    assert!(
        out.verdict
            .findings
            .iter()
            .all(|f| f.code != "TEMPORAL.CLOCK_SKEW"),
        "a loose tolerance passed through run_check_with must suppress the skew finding"
    );
    let _ = std::fs::remove_file(&path);
}
