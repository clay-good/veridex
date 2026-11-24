//! Progress and cancellation: a long run that can be watched, and interrupted without the result
//! reading as a verdict.
//!
//! The property that matters is the second one. A run that stops partway has asked some of its
//! questions and not others, and in a report there is nothing to tell a question never asked from
//! one asked and answered cleanly — which is the defect every abstention in this catalog exists to
//! prevent, here at the level of the run rather than the check.

use std::cell::RefCell;
use std::path::PathBuf;

use veridex_core::adapter::{default_registry, Cancel, IngestOptions, Source};
use veridex_core::engine::{CoverageNote, RunControl, RunInputs};
use veridex_core::{content_hash, RunConfig};

fn demo_dataset(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("demo.mcap");
    veridex_demo::mcap::write(&path, "av").expect("the demo rig");
    path
}

fn ingest(path: &std::path::Path, options: IngestOptions) -> veridex_core::Ingested {
    default_registry()
        .ingest(&Source::Local(path.to_path_buf()), &options)
        .expect("the demo rig ingests")
}

#[test]
fn progress_reports_every_check_once_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo_dataset(dir.path()), IngestOptions::default());
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");

    let seen = RefCell::new(Vec::new());
    let note = |p: veridex_core::RunProgress| {
        seen.borrow_mut().push((p.completed, p.total, p.check_id));
    };
    let verdict = engine.run_with_inputs(
        &ingested.dataset,
        content_hash(&ingested.dataset),
        &RunConfig::default(),
        RunInputs::default(),
        RunControl {
            progress: Some(&note),
            cancelled: None,
        },
    );
    let seen = seen.into_inner();
    assert_eq!(
        seen.len(),
        verdict.executed_checks.len(),
        "one report per check that ran"
    );
    // The count climbs by one each time and the denominator never moves, so a caller can render a
    // fraction without keeping its own tally.
    for (i, (completed, total, _)) in seen.iter().enumerate() {
        assert_eq!(*completed, i + 1);
        assert_eq!(*total, seen.len());
    }
    assert!(verdict.cancelled.is_none(), "nothing was cancelled");
}

#[test]
fn a_cancelled_run_keeps_what_it_found_and_refuses_to_look_complete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo_dataset(dir.path()), IngestOptions::default());
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");

    // Stop after five checks, deterministically — a wall-clock deadline would make this test a
    // race, and what is under test is the shape of the result, not the clock.
    let ran = std::cell::Cell::new(0usize);
    let stop = || {
        let n = ran.get();
        ran.set(n + 1);
        n >= 5
    };
    let verdict = engine.run_with_inputs(
        &ingested.dataset,
        content_hash(&ingested.dataset),
        &RunConfig::default(),
        RunInputs::default(),
        RunControl {
            progress: None,
            cancelled: Some(&stop),
        },
    );

    let stopped = verdict.cancelled.expect("the run says it was cancelled");
    assert_eq!(stopped.checks_completed, 5);
    assert!(stopped.checks_total > 5, "{stopped:?}");
    assert_eq!(
        verdict.executed_checks.len(),
        5,
        "only the checks that ran are recorded as executed"
    );
    // The disclosure reaches the findings, which is the one channel every renderer and the
    // certificate's own summary read.
    let disclosure = verdict
        .findings
        .iter()
        .find(|f| f.code == "COVERAGE.CANCELLED")
        .expect("a cancelled run discloses itself");
    assert_eq!(
        disclosure.severity,
        veridex_core::check::Severity::Warning,
        "a warning, so a cancelled run can never present as a clean pass"
    );
    assert_ne!(
        verdict.status,
        veridex_core::Status::Pass,
        "and the status follows from it: {:?}",
        verdict.status
    );

    // Certification is refused, for the reason a sampled run is.
    let err = veridex_core::certificate::Certificate::certifiable(&verdict)
        .expect_err("a cancelled run cannot be certified");
    assert!(err.contains("cancelled"), "{err}");
}

#[test]
fn a_cancelled_run_hashes_differently_from_the_complete_one() {
    // The two must never be interchangeable under a signature: a reader holding only the result
    // hash has to be able to tell that this one stopped early.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo_dataset(dir.path()), IngestOptions::default());
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = content_hash(&ingested.dataset);

    let whole = engine.run(&ingested.dataset, hash, &RunConfig::default());
    let never = || false;
    let also_whole = engine.run_with_inputs(
        &ingested.dataset,
        hash,
        &RunConfig::default(),
        RunInputs::default(),
        RunControl {
            progress: None,
            cancelled: Some(&never),
        },
    );
    assert_eq!(
        whole.result_content_hash, also_whole.result_content_hash,
        "a run nobody cancelled hashes exactly as it did before cancellation existed"
    );

    let stop = || true;
    let cut = engine.run_with_inputs(
        &ingested.dataset,
        hash,
        &RunConfig::default(),
        RunInputs::default(),
        RunControl {
            progress: None,
            cancelled: Some(&stop),
        },
    );
    assert_ne!(whole.result_content_hash, cut.result_content_hash);
    assert_eq!(
        cut.executed_checks.len(),
        0,
        "stopped before the first check"
    );
}

#[test]
fn a_cancelled_ingest_fails_rather_than_returning_half_a_dataset() {
    // Unlike the check loop there is no honest partial answer: the episodes a stopped read never
    // reached are not a sample, they are wherever it stopped — so the read fails by name.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = demo_dataset(dir.path());
    let err = default_registry()
        .ingest(
            &Source::Local(path),
            &IngestOptions {
                cancelled: Some(Cancel::new(|| true)),
                ..IngestOptions::default()
            },
        )
        .expect_err("a read told to stop before it began");
    assert!(
        matches!(err, veridex_core::adapter::IngestError::Cancelled { .. }),
        "{err}"
    );
    assert!(format!("{err}").contains("cancelled"), "{err}");
}

#[test]
fn a_run_nobody_interrupted_carries_no_cancellation_at_all() {
    // The field and its finding are absent, not present-and-empty: an existing verdict's JSON and
    // its hash are what they were.
    let dir = tempfile::tempdir().expect("tempdir");
    let ingested = ingest(&demo_dataset(dir.path()), IngestOptions::default());
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let verdict = engine.run_over(
        &ingested.dataset,
        content_hash(&ingested.dataset),
        &RunConfig::default(),
        CoverageNote::Full,
    );
    assert!(verdict.cancelled.is_none());
    assert!(!verdict
        .findings
        .iter()
        .any(|f| f.code == "COVERAGE.CANCELLED"));
    let json = veridex_core::render_json(&verdict, None);
    assert!(
        !json.contains("cancelled"),
        "the field is omitted from the JSON of a finished run"
    );
}
