//! The counts the prose spells out must be the counts the code produces.
//!
//! Several user-facing sentences state a number the registry or the catalog owns — "Seven formats
//! support `--metadata-only`", "the seven checks that answer by comparing episodes against each
//! other" — and each is restated on more than one page, and in a source comment besides. The
//! sensor-rig count in the README's status table is already held to the catalog
//! (`the_readmes_engine_picture_names_every_family`, in `checks.rs`) because it had already gone
//! stale once. These two were held to nothing.
//!
//! A count that has drifted is worse than no count: it reads as precision. A reader told that seven
//! formats support a mode, who then finds an eighth adapter that does, has no way to know which
//! sentence is wrong.
//!
//! Every match here is made against the page with its whitespace collapsed, because these sentences
//! wrap: the README states this count with the numeral ending one line and the noun beginning the
//! next, so a line-by-line guard sees a claim with no number in it and passes.

use std::path::{Path, PathBuf};

use veridex_core::adapter::mcap::McapAdapter;
use veridex_core::adapter::{default_registry, Adapter, IngestOptions, Source};

const SPELLED: [&str; 13] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    "eleven", "twelve",
];

fn spelled(n: usize) -> &'static str {
    SPELLED.get(n).copied().unwrap_or("?")
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A page with every run of whitespace collapsed to one space, lowercased — so a sentence that
/// wraps reads as one sentence.
fn flowed(rel: &str) -> String {
    let path = root().join(rel);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// A window of `text` around `at`, for an error message a reader can act on.
fn around(text: &str, at: usize) -> String {
    let start = text[..at]
        .char_indices()
        .rev()
        .nth(70)
        .map_or(0, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .nth(70)
        .map_or(text.len(), |(i, _)| at + i);
    format!("…{}…", &text[start..end])
}

/// Every claim of the form "<number> <anchor>" in `page` states `want`.
///
/// An occurrence whose preceding word is not a spelled number ("the checks that answer by
/// comparing…") is a sentence making no count, and is passed over rather than failed. Returns how
/// many counted claims were checked, so a caller can refuse a guard that found nothing.
fn counts_before(page: &str, anchor: &str, want: &str) -> usize {
    let text = flowed(page);
    let mut checked = 0;
    for (at, _) in text.match_indices(anchor) {
        let Some(word) = text[..at].split_whitespace().next_back() else {
            continue;
        };
        if !SPELLED.contains(&word) {
            continue;
        }
        checked += 1;
        assert!(
            word == want,
            "{page} says `{word} {anchor}`, but the code reports {want}:\n  {}",
            around(&text, at)
        );
    }
    checked
}

/// Every claim of the form "<anchor> <number>" in `page` states `want`.
fn counts_after(page: &str, anchor: &str, want: &str) -> usize {
    let text = flowed(page);
    let mut checked = 0;
    for (at, _) in text.match_indices(anchor) {
        let Some(word) = text[at + anchor.len()..].split_whitespace().next() else {
            continue;
        };
        if !SPELLED.contains(&word) {
            continue;
        }
        checked += 1;
        assert!(
            word == want,
            "{page} says `{anchor} {word}`, but the code reports {want}:\n  {}",
            around(&text, at)
        );
    }
    checked
}

/// Every page that says how many formats support `--metadata-only` says the number that do.
///
/// The registry is the authority: an adapter opts in with `supports_metadata_only`, and the CLI
/// already lists them from that same call in its refusal message. The prose was written by hand.
#[test]
fn the_metadata_only_format_count_is_the_registry_count() {
    let want = spelled(default_registry().formats_supporting_metadata_only().len());
    let checked: usize = ["README.md", "docs/partial-runs.md"]
        .iter()
        .map(|p| counts_before(p, "formats support", want))
        .sum();
    assert!(
        checked >= 2,
        "expected the sentence in README.md and docs/partial-runs.md, found {checked}"
    );
}

/// Every place that says how many checks a single-episode run cannot ask states the number it skips.
///
/// The engine names them in `STRUCTURAL.UNCOMPARED_EPISODES` — "too few for N check(s)" — which is
/// what a reader actually sees, so that is the authority here rather than a list private to the
/// check. An MCAP recording is one episode by construction, which is why the demo rig produces the
/// finding at all.
#[test]
fn the_cross_episode_check_count_is_the_one_the_finding_reports() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("av.mcap");
    veridex_demo::mcap::write(&path, "av").expect("write the demo rig");
    let dataset = McapAdapter
        .ingest(&Source::Local(path), &IngestOptions::default())
        .expect("ingest")
        .dataset;
    assert_eq!(
        dataset.episodes.len(),
        1,
        "an MCAP recording is one episode by construction; the finding depends on it"
    );
    let engine = veridex_core::checks::default_engine().expect("the standard catalog");
    let hash = veridex_core::content_hash(&dataset);
    let message = engine
        .run(&dataset, hash, &veridex_core::RunConfig::default())
        .findings
        .into_iter()
        .find(|f| f.code == "STRUCTURAL.UNCOMPARED_EPISODES")
        .expect("a one-episode run discloses the comparisons it could not make")
        .message;
    // "this run covers 1 episode(s), too few for 7 check(s) that answer by comparing …"
    let n: usize = message
        .split("too few for ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the finding states how many checks it skipped:\n  {message}"));
    let want = spelled(n);

    // Every restatement, including the one in the source that explains why the finding exists.
    let pages = [
        "README.md",
        "docs/checks.md",
        "crates/veridex-core/src/checks/structural.rs",
    ];
    let checked: usize = pages
        .iter()
        .map(|p| {
            counts_before(p, "checks that answer by comparing", want)
                + counts_after(p, "silently skipped", want)
        })
        .sum();
    assert!(
        checked >= 3,
        "expected the count on both pages and in structural.rs, found {checked}"
    );
}

#[test]
fn the_abstention_check_counts_on_the_page_are_the_catalog_counts() {
    // `docs/checks.md` states both halves of the split: the checks that exist *only* to report what
    // their family could not do, and the ones that do their own work and disclose their own silence.
    // Both drifted before this guard existed — the page said "three checks" while six more had since
    // gained an abstention code, and a reader was told the disclosure surface was half its size.
    let page = flowed("docs/checks.md");
    let engine = veridex_core::checks::default_engine().unwrap();
    let catalog = engine.catalog();

    let only = catalog
        .iter()
        .filter(|c| {
            !c.abstention_codes.is_empty() && c.abstention_codes.len() == c.finding_codes.len()
        })
        .count();
    let also = catalog
        .iter()
        .filter(|c| {
            !c.abstention_codes.is_empty() && c.abstention_codes.len() != c.finding_codes.len()
        })
        .count();

    let only_claim = format!(
        "{} checks in the catalog exist only to report",
        spelled(only)
    );
    assert!(
        page.contains(&only_claim),
        "docs/checks.md must say `{only_claim}` — {only} checks have nothing but abstention codes"
    );
    let also_claim = format!("{} further checks do their own work", spelled(also));
    assert!(
        page.contains(&also_claim),
        "docs/checks.md must say `{also_claim}` — {also} checks disclose their own silence \
         alongside real findings"
    );
}

#[test]
fn the_readmes_abstaining_check_count_is_the_catalog_count() {
    // The README states how many checks in the catalog raise an abstention. It is the same class of
    // claim as the two in `docs/checks.md`, and drifts the same way: the paragraph enumerated six
    // situations while nine checks declared one.
    let page = flowed("README.md");
    let engine = veridex_core::checks::default_engine().unwrap();
    let n = engine
        .catalog()
        .iter()
        .filter(|c| !c.abstention_codes.is_empty())
        .count();
    let claim = format!("{} checks in the catalog raise one", spelled(n));
    assert!(
        page.contains(&claim),
        "README.md must say `{claim}` — {n} checks declare an abstention code"
    );
}

#[test]
fn the_readme_names_every_source_that_shares_the_provenance_key_table() {
    // The README's promise is that one curated table makes `sensor` mean the same thing whatever
    // format it arrives in, and it makes that promise by *naming* the sources. An adapter that
    // starts using the table and is not named there shrinks the claim to a reader — which is what
    // happened when the MF4 adapter began routing its header comment through it.
    //
    // Keyed on the call, not on a count: the adapters that route free-form metadata are exactly the
    // ones that call `provenance_key_for`.
    // The *sentence* making the promise, not the whole page: every one of these formats is named
    // somewhere in a README this long, so a page-wide search passes however the sentence is worded
    // and guards nothing. (Verified: deleting MF4 from the sentence left a page-wide check green,
    // because `MF4` appears eight other times.)
    let page = flowed("README.md");
    const ANCHOR: &str = "one curated key table shared by every source that carries free-form";
    let at = page
        .find(ANCHOR)
        .expect("README.md still makes the shared-key-table promise");
    let sentence = &page[at..page[at..].find(" | ").map_or(page.len(), |e| at + e)];
    let dir = root().join("crates/veridex-core/src/adapter");
    // Adapter module → the name the README calls that format.
    let names = [
        ("mcap", "MCAP"),
        ("rosbag2", "rosbag2"),
        ("hdf5", "HDF5"),
        ("zarr", "Zarr"),
        ("mdf4", "MF4"),
        ("lerobot", "LeRobot"),
        ("rlds", "RLDS"),
        ("candbc", "CAN+DBC"),
    ];
    let mut missing: Vec<&str> = Vec::new();
    for (module, shown) in names {
        let src = [
            dir.join(format!("{module}.rs")),
            dir.join(module).join("mod.rs"),
        ]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| panic!("adapter `{module}` has a source file"));
        // `flowed` lowercases, so compare in that case — the page names these formats in their own
        // capitalization (`MCAP`, `HDF5`, `MF4`) and a case-sensitive test reports every one of them
        // as absent from a page that names them all.
        if src.contains("provenance_key_for") && !sentence.contains(&shown.to_ascii_lowercase()) {
            missing.push(shown);
        }
    }
    assert!(
        missing.is_empty(),
        "these adapters route free-form metadata through the shared key table but the README's \
         sentence about it does not name them: {missing:?}"
    );
}
