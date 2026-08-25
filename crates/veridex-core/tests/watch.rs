//! `veridex watch`'s change detector: what it must notice, what it must not follow, and what it
//! refuses.
//!
//! The fingerprint is the whole basis for "should I re-validate?", so a miss here is a watch that
//! sits quietly while the recording it is watching goes wrong.

use std::path::{Path, PathBuf};

use veridex_core::watch::{fingerprint, fingerprint_within};

/// A unique, per-test temp directory (created), so parallel test runs don't collide.
fn temp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("veridex-watch-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("create temp dir");
    p
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(path, bytes).expect("write");
}

#[test]
fn an_unchanged_dataset_fingerprints_the_same_every_time() {
    let dir = temp_dir("stable");
    write(&dir.join("data/chunk-000/file-000.parquet"), b"one");
    write(&dir.join("meta/info.json"), b"{}");

    let first = fingerprint(&dir).expect("fingerprint");
    let second = fingerprint(&dir).expect("fingerprint");
    assert_eq!(
        first, second,
        "a tree nobody touched must fingerprint identically, or a watch re-validates on every tick"
    );
}

#[test]
fn a_growing_file_and_a_new_shard_both_change_the_fingerprint() {
    let dir = temp_dir("growing");
    let shard = dir.join("data/chunk-000/file-000.parquet");
    write(&shard, b"one");
    let before = fingerprint(&dir).expect("fingerprint");

    // The recording appends — the case a watch exists for.
    write(&shard, b"one-two-three");
    let grown = fingerprint(&dir).expect("fingerprint");
    assert_ne!(
        before, grown,
        "a file that grew must change the fingerprint: this is a recorder writing"
    );

    // A new episode lands as a new file.
    write(&dir.join("data/chunk-000/file-001.parquet"), b"two");
    let added = fingerprint(&dir).expect("fingerprint");
    assert_ne!(grown, added, "a new file must change the fingerprint");
}

#[test]
fn a_file_a_watch_never_reads_does_not_wake_it() {
    // A symlink pointing out of the dataset is not the dataset's data — every adapter refuses to
    // read through one. A change detector that followed it would re-validate on activity in a file
    // whose contents can never appear in the verdict, and could be walked in a cycle forever.
    let dir = temp_dir("symlink");
    let dataset = dir.join("dataset");
    write(&dataset.join("meta/info.json"), b"{}");
    let outside = dir.join("outside.parquet");
    write(&outside, b"payroll");

    std::fs::create_dir_all(dataset.join("data")).expect("create data dir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dataset.join("data/link.parquet")).expect("symlink");
    #[cfg(not(unix))]
    return;

    let before = fingerprint(&dataset).expect("fingerprint");
    // The file at the far end of the link changes size and mtime.
    write(&outside, b"payroll-and-then-some-more-bytes");
    let after = fingerprint(&dataset).expect("fingerprint");
    assert_eq!(
        before, after,
        "the fingerprint must not follow a symlink out of the dataset"
    );

    // The link itself, however, is part of the tree's shape: removing it is a change.
    std::fs::remove_file(dataset.join("data/link.parquet")).expect("remove link");
    assert_ne!(
        before,
        fingerprint(&dataset).expect("fingerprint"),
        "removing the link changes the tree, even though its target was never read"
    );
}

#[test]
fn a_pathological_tree_is_refused_rather_than_walked() {
    // A watched directory is as untrusted as a checked one, and this walk runs on every tick.
    let dir = temp_dir("bounded");
    for i in 0..4 {
        write(&dir.join(format!("f{i}")), b"x");
    }
    let err = fingerprint_within(&dir, 2).expect_err("must refuse a tree past the ceiling");
    assert!(
        err.to_string().contains("more than 2 entries"),
        "the refusal must name the ceiling: {err}"
    );
    // The same tree under a ceiling that fits is fine.
    assert!(fingerprint_within(&dir, 100).is_ok());
}

#[test]
fn a_path_that_is_not_there_is_an_error_not_a_fingerprint() {
    // "No dataset" must not fingerprint as a stable empty tree: that would read as "nothing
    // changed" for as long as the path stays missing.
    let dir = temp_dir("missing");
    let err = fingerprint(&dir.join("no-such-dataset")).expect_err("missing path must error");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn a_single_file_dataset_is_fingerprinted_like_a_directory_one() {
    // MCAP, MF4 and HDF5 datasets are one file, not a tree.
    let dir = temp_dir("singlefile");
    let file = dir.join("log.mcap");
    write(&file, b"header");
    let before = fingerprint(&file).expect("fingerprint");
    assert_eq!(before, fingerprint(&file).expect("fingerprint"));
    write(&file, b"header-plus-a-recorded-message");
    assert_ne!(
        before,
        fingerprint(&file).expect("fingerprint"),
        "a growing single-file log must change the fingerprint"
    );
}
