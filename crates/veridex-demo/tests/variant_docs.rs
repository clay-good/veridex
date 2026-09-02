//! The variant lists a reader copies from must be the ones the generators accept.
//!
//! Every generator has one authoritative list — its `VARIANTS` — and three prose restatements of
//! it: the module doc's bullets, the module doc's `Usage:` line, and the `examples/` wrapper's
//! `Usage:` line. Plus the README, for the MCAP demo the quickstart tells a reader to run first.
//! Nothing watched any of them, and all four drifted: the LeRobot docs named the default variant
//! `broken` (the generator calls it `non-monotonic` and refuses `broken`), left `near-duplicate`
//! undocumented, and told a reader to run it out of `-p veridex-core`, which holds no such example;
//! the MCAP wrapper and the README each stopped four or five variants short.
//!
//! A stale list is not a cosmetic problem here. The generator refuses an unknown variant rather
//! than substituting one, so a name that has drifted is a copied command that fails — which, for a
//! reader whose first contact with the tool is the quickstart, is the tool not working.

use veridex_demo::{lerobot, mcap, mf4, rlds};

const ROOT: &str = env!("CARGO_MANIFEST_DIR");

fn read(rel: &str) -> String {
    let path = format!("{ROOT}/{rel}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path} is readable: {e}"))
}

/// The names backticked in a `//! - ` bullet, in the order they are documented. An optional
/// `(default) ` marker precedes the name of the variant `write` falls back to.
fn bulleted_names(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("//! - ")?;
            let rest = rest.strip_prefix("(default) ").unwrap_or(rest);
            let name = rest.strip_prefix('`')?.split('`').next()?;
            Some(name.to_string())
        })
        .collect()
}

/// The names inside the `[a|b|c]` list of a `Usage:` line.
fn usage_names(source: &str, line_prefix: &str) -> Vec<String> {
    let line = source
        .lines()
        .find(|l| l.starts_with(line_prefix))
        .unwrap_or_else(|| panic!("a line starting `{line_prefix}`"));
    let inner = line
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .unwrap_or_else(|| panic!("`{line}` lists its variants in brackets"))
        .0;
    inner.split('|').map(|s| s.trim().to_string()).collect()
}

/// One generator: its authoritative list, its module source, and its `examples/` wrapper.
struct Generator {
    name: &'static str,
    variants: &'static [&'static str],
    module: &'static str,
    example: &'static str,
}

fn generators() -> Vec<Generator> {
    vec![
        Generator {
            name: "mcap",
            variants: mcap::VARIANTS,
            module: "src/mcap.rs",
            example: "examples/make_demo_mcap.rs",
        },
        Generator {
            name: "lerobot",
            variants: lerobot::VARIANTS,
            module: "src/lerobot.rs",
            example: "examples/make_demo_lerobot.rs",
        },
        Generator {
            name: "mf4",
            variants: mf4::VARIANTS,
            module: "src/mf4.rs",
            example: "examples/make_demo_mf4.rs",
        },
        Generator {
            name: "rlds",
            variants: rlds::VARIANTS,
            module: "src/rlds.rs",
            example: "examples/make_demo_rlds.rs",
        },
    ]
}

/// Every variant is documented, and every documented variant exists. A bullet naming a variant the
/// generator would refuse is worse than no bullet: it tells a reader the fault it demonstrates.
#[test]
fn every_variant_has_a_doc_bullet_and_every_bullet_is_a_variant() {
    for g in generators() {
        let documented = bulleted_names(&read(g.module));
        for v in g.variants {
            assert!(
                documented.iter().any(|d| d == v),
                "`{}::VARIANTS` has `{v}`, which {} documents in no bullet",
                g.name,
                g.module
            );
        }
        for d in &documented {
            assert!(
                g.variants.contains(&d.as_str()),
                "{} documents `{d}`, which `{}::VARIANTS` does not accept — a reader who runs it \
                 is refused",
                g.module,
                g.name
            );
        }
    }
}

/// Both `Usage:` lines list every variant, in the order `VARIANTS` declares them.
#[test]
fn both_usage_lines_list_every_variant() {
    for g in generators() {
        let expected: Vec<String> = g.variants.iter().map(|s| s.to_string()).collect();
        for (file, source) in [(g.module, read(g.module)), (g.example, read(g.example))] {
            assert_eq!(
                usage_names(&source, "//! Usage:"),
                expected,
                "the `Usage:` line in {file} does not list `{}::VARIANTS`",
                g.name
            );
        }
    }
}

/// Each `Usage:` line invokes the crate that actually holds the example. The LeRobot one named
/// `veridex-core`, where these generators used to live; `cargo run -p veridex-core --example
/// make_demo_lerobot` finds no such target.
#[test]
fn every_usage_line_names_this_crate() {
    for g in generators() {
        for (file, source) in [(g.module, read(g.module)), (g.example, read(g.example))] {
            let line = source
                .lines()
                .find(|l| l.starts_with("//! Usage:"))
                .expect("a Usage: line");
            assert!(
                line.contains("-p veridex-demo"),
                "the `Usage:` line in {file} does not run out of `veridex-demo`:\n  {line}"
            );
        }
    }
}

/// The prose that offers a reader a variant to append: the README's quickstart for the MCAP demo,
/// and `docs/formats.md` for the other three. These are the lists someone copies from, so a name
/// missing here is a fault nobody knows they can reproduce, and a name here that `write` refuses is
/// a command that fails on the first thing a reader tries.
#[test]
fn the_prose_that_offers_variants_offers_exactly_the_real_ones() {
    for (page, generator, variants) in [
        ("README.md", "mcap", mcap::VARIANTS),
        ("docs/formats.md", "lerobot", lerobot::VARIANTS),
        ("docs/formats.md", "rlds", rlds::VARIANTS),
        ("docs/formats.md", "mf4", mf4::VARIANTS),
    ] {
        let text = std::fs::read_to_string(format!("{ROOT}/../../{page}"))
            .unwrap_or_else(|e| panic!("{page} is readable: {e}"));
        let block = offer_block(&text, generator)
            .unwrap_or_else(|| panic!("{page} introduces `make_demo_{generator}` with a comment"));
        // `skew`/`non-monotonic`/`clean`/`saturated` — whichever the argument-less command writes —
        // is what the block is already describing, so it is offered by running the command at all.
        let default = variants[0];
        for v in variants.iter().filter(|v| **v != default) {
            assert!(
                block.contains(&format!("`{v}`")),
                "{page} offers the `{generator}` variants but not `{v}`"
            );
        }
        for token in block.split('`').skip(1).step_by(2) {
            // Ignore backticks around anything that could not be a variant name at all (`##DZ`).
            if !token
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                continue;
            }
            assert!(
                variants.contains(&token),
                "{page} offers `{token}` as a `{generator}` variant, which `write` refuses"
            );
        }
    }
}

/// The run of `#` comment lines directly above the first `make_demo_<generator>` invocation.
fn offer_block(page: &str, generator: &str) -> Option<String> {
    let lines: Vec<&str> = page.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(&format!("--example make_demo_{generator} --")))?;
    let start = lines[..at]
        .iter()
        .rposition(|l| !l.starts_with('#'))
        .map(|i| i + 1)
        .unwrap_or(0);
    (start < at).then(|| lines[start..at].join("\n"))
}

/// `describe` opens with the name that selects it. It is the one restatement a reader sees at
/// *runtime* — the generator prints it on every successful write — and it named the default variant
/// `broken`, so the tool reported a name it would then refuse.
#[test]
fn every_description_opens_with_its_own_variant_name() {
    for v in lerobot::VARIANTS {
        let described = lerobot::describe(v).expect("a known variant is described");
        assert!(
            described.starts_with(&format!("{v} ")) || described == *v,
            "`describe(\"{v}\")` opens with a different name than the one that selects it:\n  \
             {described}"
        );
    }
}
