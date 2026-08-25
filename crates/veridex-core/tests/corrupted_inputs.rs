//! Every adapter, over deliberately damaged versions of its own valid fixtures.
//!
//! Veridex reads files it did not write, and the failure that matters here is not "the verdict is
//! wrong" — a corrupt file has no right verdict — but "the process died". A panic inside an adapter
//! is not a finding, not an exit code, and not something a CI gate can read: the prior audits found
//! four of them (an MCAP length prefix inside a chunk, two HDF5 sizes that overflowed the arithmetic
//! reading them, a vacuous `all()` over an empty collection), each a real file away from a real
//! crash.
//!
//! So this sweep truncates and flips bytes in every committed binary fixture and asserts one thing
//! per mutation: ingestion returns `Ok` or `Err`, and does not unwind. A mutation that produces a
//! readable dataset is checked as well, because a check can be handed a shape no honest adapter
//! would produce.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use veridex_core::{default_registry, IngestOptions, RunConfig, Source};

/// Deterministic byte offsets to damage, from a fixed linear congruential generator: the same
/// mutations every run, so a failure is reproducible from the seed and the index alone.
fn offsets(len: usize, count: usize, seed: u64) -> Vec<usize> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (state >> 33) as usize % len.max(1)
        })
        .collect()
}

/// Every damaged version of `bytes` this sweep tries, each with a label naming what was done.
fn mutations(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if bytes.is_empty() {
        return out;
    }
    // Truncation: the shape a half-written recording and an interrupted download both have.
    for numerator in [1usize, 2, 3, 7] {
        let cut = bytes.len() * numerator / 8;
        out.push((format!("truncated to {numerator}/8"), bytes[..cut].to_vec()));
    }
    // The header and the trailer, deliberately, rather than by chance. Format detection, the magic
    // number, the superblock, and MCAP's footer all live in the first and last few bytes, and random
    // offsets in a 100 KB file almost never land there — a sweep that only flips random bytes leaves
    // the parsing entered first, and the summary read last, untouched.
    let mut zeroed_head = bytes.to_vec();
    for byte in zeroed_head.iter_mut().take(16) {
        *byte = 0;
    }
    out.push(("first 16 bytes zeroed".to_string(), zeroed_head));
    let mut flipped_first = bytes.to_vec();
    flipped_first[0] ^= 0xFF;
    out.push(("byte 0 flipped".to_string(), flipped_first));
    let mut zeroed_tail = bytes.to_vec();
    let tail = zeroed_tail.len().saturating_sub(16);
    for byte in zeroed_tail[tail..].iter_mut() {
        *byte = 0;
    }
    out.push(("last 16 bytes zeroed".to_string(), zeroed_tail));

    // A byte flipped somewhere: a corrupt length, a broken magic number, a wrong offset.
    for (i, offset) in offsets(bytes.len(), 6, 0x5EED).into_iter().enumerate() {
        let mut damaged = bytes.to_vec();
        damaged[offset] ^= 0xFF;
        out.push((format!("byte {offset} flipped (#{i})"), damaged));
    }
    // A size field turned enormous is the shape behind every allocation abort this repo has fixed:
    // write `u64::MAX` over a few aligned words and see whether anything still tries to allocate it.
    for (i, offset) in offsets(bytes.len(), 4, 0xB16_5EED).into_iter().enumerate() {
        let mut damaged = bytes.to_vec();
        let end = (offset + 8).min(damaged.len());
        for byte in &mut damaged[offset..end] {
            *byte = 0xFF;
        }
        out.push((format!("size field at {offset} maxed (#{i})"), damaged));
    }
    out
}

/// Ingest and, when that succeeds, validate — catching an unwind rather than letting it end the run.
///
/// `format` forces an adapter rather than letting detection choose. Both paths matter and they are
/// not the same test: with a destroyed header, detection declines the file and nothing parses it,
/// which is correct behavior and also means the parsing code is never reached. Forcing the format
/// is what a user does when detection is ambiguous, and it is what makes an adapter read a file it
/// would otherwise refuse — which is exactly where this repo's four historical crashes lived.
fn survives_as(path: &Path, format: Option<&str>) -> Result<(), String> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let registry = default_registry();
        let source = Source::Local(path.to_path_buf());
        // A tighter frame budget than the default. What this sweep hunts is an unwind in the
        // parsing — a length read from the file, an offset that lands outside it — and that happens
        // long before a large dataset finishes materializing. The budgets themselves have their own
        // tests; here they would only make the sweep pay full price for the two fixtures that exist
        // precisely to be expensive.
        let options = IngestOptions {
            max_frames: Some(20_000),
            ..IngestOptions::default()
        };
        let ingested = match format {
            Some(f) => registry.ingest_as(f, &source, &options),
            None => registry.ingest(&source, &options),
        };
        if let Ok(mut ingested) = ingested {
            ingested.dataset.canonicalize_order();
            let hash = veridex_core::content_hash(&ingested.dataset);
            let engine = veridex_core::checks::default_engine().expect("standard checks");
            let _ = engine.run(&ingested.dataset, hash, &RunConfig::default());
        }
    }));
    match result {
        Ok(()) => Ok(()),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic".to_string());
            Err(message)
        }
    }
}

/// The committed binary fixtures, plus anything generated into `extra`.
fn fixtures() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("hdf5")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "h5") {
                out.push(path);
            }
        }
    }
    // The CLI's MCAP fixture: the one committed file for the format whose framing has produced the
    // most crashes in this repo's history.
    let mcap =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../veridex-cli/tests/fixtures/demo.mcap");
    if mcap.is_file() {
        out.push(mcap);
    }
    out.sort();
    out
}

/// The Zarr sweep: a store is a *directory*, so the damage goes inside it, one member file at a
/// time — a truncated chunk, a corrupt `.zarray`, a mangled `episode_ends`. The single-file sweep
/// above cannot reach any of that, and the path handling it exercises is where an adapter reads
/// something it should not.
#[test]
fn no_damaged_store_takes_the_process_down() {
    let source_store =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/zarr/dp_replay.zarr");
    assert!(source_store.is_dir(), "the Zarr fixture must exist");

    let members: Vec<PathBuf> = walk(&source_store);
    assert!(members.len() > 3, "the store has members to damage");

    let dir = tempfile::tempdir().expect("temp dir");
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for member in &members {
        let relative = member
            .strip_prefix(&source_store)
            .expect("inside the store");
        let bytes = std::fs::read(member).expect("read member");
        for (label, damaged) in mutations(&bytes) {
            let store = dir.path().join("damaged.zarr");
            let _ = std::fs::remove_dir_all(&store);
            copy_tree(&source_store, &store);
            std::fs::write(store.join(relative), &damaged).expect("write damaged member");
            checked += 1;
            for forced in [None, Some("zarr")] {
                if let Err(message) = survives_as(&store, forced) {
                    let how = forced.map_or("detected", |_| "forced");
                    failures.push(format!(
                        "{}: {label} ({how}) → panic: {message}",
                        relative.display()
                    ));
                }
            }
        }
    }

    assert!(checked > 20, "the sweep must actually run: {checked}");
    assert!(
        failures.is_empty(),
        "{} of {checked} damaged stores took the process down:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every file under `root`, depth-first, in a stable order.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Copy a directory tree, so each mutation starts from an intact store.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy file");
        }
    }
}

#[test]
fn no_damaged_file_takes_the_process_down() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for fixture in fixtures() {
        let bytes = std::fs::read(&fixture).expect("read fixture");
        let name = fixture.file_name().unwrap().to_string_lossy().to_string();
        let extension = fixture
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let format = match extension.as_str() {
            "h5" | "hdf5" => "hdf5".to_string(),
            "mcap" => "mcap".to_string(),
            other => panic!("no adapter name known for a `.{other}` fixture"),
        };
        for (label, damaged) in mutations(&bytes) {
            // Keep the original extension: format detection is by extension for these, and a
            // mutation that only changed the name would test nothing.
            let path = dir.path().join(format!("damaged.{extension}"));
            std::fs::write(&path, &damaged).expect("write damaged fixture");
            for forced in [None, Some(format.as_str())] {
                checked += 1;
                if let Err(message) = survives_as(&path, forced) {
                    let how = forced.map_or("detected", |_| "forced");
                    failures.push(format!("{name}: {label} ({how}) → panic: {message}"));
                }
            }
        }
    }

    assert!(checked > 100, "the sweep must actually run: {checked}");
    assert!(
        failures.is_empty(),
        "{} of {checked} damaged files took the process down:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
