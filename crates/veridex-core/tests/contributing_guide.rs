//! The contributing guide names real things.
//!
//! `CONTRIBUTING.md` tells a newcomer where a change has to reach — the function a check registers
//! itself in, the compile-time census that catches a new CDM field, the pages whose claims are
//! tested. Those are the parts of the guide that are worth anything, and they are exactly the parts
//! a rename makes silently false. A guide that sends someone to a symbol that no longer exists is
//! worse than one that says nothing, because they will go looking.
//!
//! This holds it to two cheap facts: every Rust identifier it names still exists somewhere in the
//! workspace, and every repository path it names is still there. It deliberately does not check
//! prose — that is what review is for.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The guide with fenced code blocks removed: those are shell commands, not references.
fn prose() -> String {
    let text = std::fs::read_to_string(root().join("CONTRIBUTING.md"))
        .expect("CONTRIBUTING.md is readable");
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push(line);
        }
    }
    out.join("\n")
}

/// Everything the guide sets in backticks, one span at a time.
fn backticked(text: &str) -> Vec<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter(|s| !s.contains('\n'))
        .map(|s| s.to_string())
        .collect()
}

/// Every workspace source file, concatenated — the haystack for a symbol.
fn all_source() -> String {
    let mut out = String::new();
    let mut stack = vec![root().join("crates")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // `target/` can appear inside a crate; nothing there is a source of truth.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Every Rust identifier the guide names is one the workspace defines.
///
/// Only multi-word identifiers — a `snake_case` function or a `SCREAMING_CASE` constant — because
/// those are the ones that name something specific enough to go stale. A trailing `()` and any
/// module path in front of it are stripped: the guide writes `checks/mod.rs::standard_checks_with`.
#[test]
fn every_symbol_the_guide_names_exists() {
    let source = all_source();
    let mut checked = 0usize;
    for token in backticked(&prose()) {
        let name = token
            .trim_end_matches("()")
            .rsplit("::")
            .next()
            .unwrap_or_default();
        let identifier = !name.is_empty()
            && name.contains('_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
        if !identifier {
            continue;
        }
        checked += 1;
        assert!(
            source.contains(name),
            "CONTRIBUTING.md sends a contributor to `{name}`, which nothing in the workspace \
             defines any more"
        );
    }
    assert!(
        checked >= 3,
        "the guide should still point at the symbols that make it useful, found {checked}"
    );
}

/// Every repository path the guide names is still there.
#[test]
fn every_path_the_guide_names_exists() {
    let mut checked = 0usize;
    for token in backticked(&prose()) {
        // Only paths written from the repository root; the guide also uses crate-relative shorthand
        // (`tests/canonical_golden.rs`), which has no single place to resolve against.
        if !["docs/", "crates/", "openspec/"]
            .iter()
            .any(|p| token.starts_with(p))
        {
            continue;
        }
        // A `path::to::symbol` reference is a symbol, checked by the test above.
        let path = token.split("::").next().unwrap_or(&token);
        // `checks/<family>.rs` is a shape, not a file. A placeholder is the guide being general on
        // purpose, and the reader can see that it is.
        if path.contains('<') {
            continue;
        }
        checked += 1;
        assert!(
            root().join(path).exists(),
            "CONTRIBUTING.md names `{path}`, which is not in the repository"
        );
    }
    assert!(
        checked >= 5,
        "the guide should still point at the pages and crates it explains, found {checked}"
    );
}
