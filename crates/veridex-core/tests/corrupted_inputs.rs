//! Every adapter, over deliberately damaged versions of its own valid fixtures.
//!
//! Veridex reads files it did not write, and the failure that matters here is not "the verdict is
//! wrong" — a corrupt file has no right verdict — but "the process died". A panic inside an adapter
//! is not a finding, not an exit code, and not something a CI gate can read: the prior audits found
//! four of them (an MCAP length prefix inside a chunk, two HDF5 sizes that overflowed the arithmetic
//! reading them, a vacuous `all()` over an empty collection), each a real file away from a real
//! crash.
//!
//! So this sweep truncates and flips bytes in every committed binary fixture — files, Zarr stores,
//! and rosbag2 bags under either storage plugin — and asserts one thing
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
///
/// `metadata_only` chooses whether the ingest reads the data or only what the source declares about
/// itself. Both matter, and they are different code: a metadata-only run follows offsets and lengths the
/// file states about *itself* — an MCAP's summary pointer, an HDF5 object header, a Zarr `.zarray` —
/// and does so without the frame reading that would otherwise trip over the same corruption first.
/// A hostile file must not be able to panic either one.
fn survives_with(path: &Path, format: Option<&str>, metadata_only: bool) -> Result<(), String> {
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
            metadata_only,
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
                for metadata_only in [false, true] {
                    if let Err(message) = survives_with(&store, forced, metadata_only) {
                        let how = forced.map_or("detected", |_| "forced");
                        let mode = if metadata_only {
                            "metadata-only"
                        } else {
                            "full"
                        };
                        failures.push(format!(
                            "{}: {label} ({how}, {mode}) → panic: {message}",
                            relative.display()
                        ));
                    }
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
                for metadata_only in [false, true] {
                    checked += 1;
                    if let Err(message) = survives_with(&path, forced, metadata_only) {
                        let how = forced.map_or("detected", |_| "forced");
                        let mode = if metadata_only {
                            "metadata-only"
                        } else {
                            "full"
                        };
                        failures.push(format!(
                            "{name}: {label} ({how}, {mode}) → panic: {message}"
                        ));
                    }
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

/// The rosbag2 sweep: a bag is a *directory* too, and there are two of them — the `sqlite3` plugin's
/// `.db3` shards and the `.mcap` ones `ros2 bag record` writes by default from Jazzy on. Each is a
/// different reader over a different container behind one adapter, and neither was reachable from
/// the single-file sweep above: a bag is only recognized as a directory, so damage has to go inside
/// one. `metadata.yaml` is damaged along with the shards, because it is content like everything else
/// — a manifest is exactly where a hostile bag would put a length or a path it wants followed.
#[test]
fn no_damaged_bag_takes_the_process_down() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mcap_bag = dir.path().join("mcap_bag");
    write_mcap_bag(&mcap_bag);

    let bags = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rosbag2/clean_rig"),
        // The same recording zstd-compressed, so the decompression path meets damaged input too: a
        // corrupt frame there is decoded before any of the SQLite reader's own bounds checks run.
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rosbag2/compressed_rig"),
        mcap_bag,
    ];

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for source_bag in &bags {
        assert!(source_bag.is_dir(), "{} must exist", source_bag.display());
        let members = walk(source_bag);
        assert!(members.len() > 1, "a bag has a manifest and a shard");
        let bag_name = source_bag
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();

        for member in &members {
            let relative = member.strip_prefix(source_bag).expect("inside the bag");
            let bytes = std::fs::read(member).expect("read member");
            for (label, damaged) in mutations(&bytes) {
                let bag = dir.path().join("damaged_bag");
                let _ = std::fs::remove_dir_all(&bag);
                copy_tree(source_bag, &bag);
                std::fs::write(bag.join(relative), &damaged).expect("write damaged member");
                checked += 1;
                for forced in [None, Some("rosbag2")] {
                    for metadata_only in [false, true] {
                        if let Err(message) = survives_with(&bag, forced, metadata_only) {
                            let how = forced.map_or("detected", |_| "forced");
                            let mode = if metadata_only {
                                "metadata-only"
                            } else {
                                "full"
                            };
                            failures.push(format!(
                                "{bag_name}/{}: {label} ({how}, {mode}) → panic: {message}",
                                relative.display()
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(checked > 20, "the sweep must actually run: {checked}");
    assert!(
        failures.is_empty(),
        "{} of {checked} damaged bags took the process down:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A minimal MCAP-storage bag: one shard of a two-topic rig, and the manifest that describes it.
fn write_mcap_bag(bag: &Path) {
    std::fs::create_dir_all(bag).expect("create the bag directory");
    let mut out = Vec::new();
    {
        let mut writer = mcap::Writer::new(std::io::Cursor::new(&mut out)).expect("writer");
        for (schema, topic) in [
            ("sensor_msgs/msg/Imu", "/imu/data"),
            ("sensor_msgs/msg/PointCloud2", "/lidar/points"),
        ] {
            let schema_id = writer
                .add_schema(schema, "ros2msg", b"")
                .expect("add schema");
            let channel_id = writer
                .add_channel(schema_id, topic, "cdr", &std::collections::BTreeMap::new())
                .expect("add channel");
            for i in 0..8u64 {
                writer
                    .write_to_known_channel(
                        &mcap::records::MessageHeader {
                            channel_id,
                            sequence: i as u32,
                            log_time: 1_000_000_000 + i * 10_000_000,
                            publish_time: 1_000_000_000 + i * 10_000_000,
                        },
                        b"payload",
                    )
                    .expect("write message");
            }
        }
        writer.finish().expect("finish");
    }
    std::fs::write(bag.join("rec_0.mcap"), &out).expect("write shard");
    std::fs::write(
        bag.join("metadata.yaml"),
        "rosbag2_bagfile_information:\n  version: 9\n  storage_identifier: mcap\n  \
         relative_file_paths:\n    - rec_0.mcap\n  message_count: 16\n  \
         topics_with_message_count:\n    - topic_metadata:\n        name: /imu/data\n        \
         type: sensor_msgs/msg/Imu\n        serialization_format: cdr\n        \
         offered_qos_profiles: \"\"\n      message_count: 8\n    - topic_metadata:\n        \
         name: /lidar/points\n        type: sensor_msgs/msg/PointCloud2\n        \
         serialization_format: cdr\n        offered_qos_profiles: \"\"\n      message_count: 8\n  \
         compression_format: \"\"\n  compression_mode: \"\"\n  ros_distro: jazzy\n",
    )
    .expect("write manifest");
}
