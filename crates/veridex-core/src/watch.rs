//! Change detection for `veridex watch`: a cheap fingerprint of a dataset on disk, so a watch loop
//! can tell whether re-validating would say anything new.
//!
//! A watch runs against a dataset that is *being recorded*, so it must decide "has this changed?"
//! without reading the data — re-ingesting a growing multi-gigabyte log every second is not a
//! change detector, it is a load generator. The fingerprint is over directory structure and file
//! metadata (name, kind, size, mtime), which is what a recorder moves when it appends an episode,
//! rotates a file, or finishes a shard.
//!
//! Two properties it deliberately has:
//!
//! - **Symlinks are recorded, never followed.** Every adapter refuses to read through a symlink out
//!   of the dataset directory ([`crate::adapter`]); a change detector that followed them would
//!   re-validate on activity in a file that is not part of the dataset, and could be walked in a
//!   cycle forever.
//! - **The walk is bounded.** A watched directory is as untrusted as a checked one, and
//!   [`MAX_WATCH_ENTRIES`] is the ceiling on entries a single fingerprint will visit, so a pathological
//!   tree fails loudly instead of pinning a CPU on every tick.

use std::io;
use std::path::Path;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

/// Ceiling on the number of filesystem entries one fingerprint will visit.
///
/// Far above any real dataset (LeRobot's largest published datasets are in the low thousands of
/// files) and far below what would make a tick expensive.
pub const MAX_WATCH_ENTRIES: usize = 200_000;

/// A fingerprint of what is on disk at `path`, as lowercase hex.
///
/// Equal fingerprints mean nothing a watch cares about moved: same tree, same file sizes, same
/// modification times. Any difference — a new shard, a growing log, a rewritten manifest — changes
/// it. It is *not* a content hash: it never opens a file, and it says nothing about whether two
/// datasets are the same dataset. [`crate::content_hash`] is that.
///
/// Errors if `path` does not exist, or if the tree exceeds [`MAX_WATCH_ENTRIES`].
pub fn fingerprint(path: &Path) -> io::Result<String> {
    fingerprint_within(path, MAX_WATCH_ENTRIES)
}

/// [`fingerprint`], with an explicit ceiling on entries visited.
///
/// The ceiling is the parameter so it is testable at a size a test can actually build: a guard that
/// can only be exercised by creating [`MAX_WATCH_ENTRIES`] files is a guard nothing proves.
pub fn fingerprint_within(path: &Path, max_entries: usize) -> io::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"veridex.watch.v1\0");
    let mut visited = 0usize;
    // Depth-first, each directory's entries sorted by name, so the digest is independent of the
    // order the filesystem happens to hand them back.
    let mut stack = vec![(path.to_path_buf(), String::new())];
    // The root itself, so watching a single file (an `.mcap`, an `.mf4`) works the same way.
    let root = path.symlink_metadata()?;
    absorb(&mut hasher, "", &root);
    while let Some((dir, prefix)) = stack.pop() {
        if !dir.symlink_metadata().is_ok_and(|m| m.is_dir()) {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .collect::<io::Result<Vec<_>>>()?
            .into_iter()
            .map(|e| (e.file_name(), e.path()))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, child) in entries {
            visited += 1;
            if visited > max_entries {
                return Err(io::Error::other(format!(
                    "watched tree has more than {max_entries} entries: {}",
                    path.display()
                )));
            }
            let rel = format!("{prefix}/{}", name.to_string_lossy());
            // `symlink_metadata` describes the link itself: a symlink is part of the tree's shape
            // and is recorded as such, but is never walked or read through.
            let meta = child.symlink_metadata()?;
            absorb(&mut hasher, &rel, &meta);
            if meta.is_dir() {
                stack.push((child, rel));
            }
        }
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest {
        hex.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        hex.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    Ok(hex)
}

/// Fold one entry's identity into the digest: relative path, kind, size, and modification time.
fn absorb(hasher: &mut Sha256, rel: &str, meta: &std::fs::Metadata) {
    let kind = if meta.file_type().is_symlink() {
        b'l'
    } else if meta.is_dir() {
        b'd'
    } else {
        b'f'
    };
    // An unreadable mtime (some filesystems, some platforms) folds in as 0 rather than failing the
    // tick: size and structure still move when a recorder writes.
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.update((rel.len() as u64).to_le_bytes());
    hasher.update(rel.as_bytes());
    hasher.update([kind]);
    hasher.update(meta.len().to_le_bytes());
    hasher.update(mtime_ns.to_le_bytes());
}
