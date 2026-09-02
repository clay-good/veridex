# Contributing to Veridex

Thanks for looking. This page is about the parts of the codebase you cannot infer from reading it —
the conventions the tests actually enforce, and the places a change has to touch that the compiler
will not point at.

Nothing here is style preference. Every rule below exists because breaking it once produced a wrong
answer about someone's data.

## Getting set up

You need a Rust toolchain ([rustup](https://rustup.rs/)); the pinned minimum is in
`[workspace.package] rust-version`.

```sh
cargo build
cargo test
```

Before you push, run exactly what CI runs — it is the same five commands, and they catch different
things:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --all-features
cargo test -p veridex-core                          # default features: the no-TLS build
cargo publish --dry-run -p veridex-core
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features -p veridex-core -p veridex-cli
```

The fourth is not redundant. `--all-features` never compiles the `#[cfg(not(feature = "remote"))]`
arms, so a library user embedding `veridex-core` without a TLS stack is only covered there.

The Python bindings have their own job, because the parity test shells out to the built CLI:

```sh
maturin develop -m crates/veridex-py/Cargo.toml
python -m pytest crates/veridex-py/tests/test_parity.py
```

## The invariants

**Verdicts are deterministic.** The same dataset bytes and the same Veridex version always produce
the same verdict and the same content hash. Parallelism, filesystem order, and the order collections
happened to arrive in never change a result. Any sort used in canonicalization must be on a *total*
key — sorting episodes by index alone leaves ties in input order, and duplicate indices are a fault
Veridex reports, so the tie is reachable.

**Veridex never mutates a dataset.** It reads, verifies, reports and certifies. Repair is out of
scope by design; a verifier that edits is not a neutral one.

**A run that could not measure something says so.** The failure mode this whole tool exists to
prevent is a clean result that means "nothing was asked". A check with no evidence to work on emits
an informational finding naming what it could not measure, and that finding travels into the JSON,
the SARIF, the HTML and the certificate. If you add a path where a check quietly does nothing,
you have added the bug, not avoided it.

**Coverage and narrowing are refused, not disclosed away.** A sampled run, a `--metadata-only` run,
or a run narrowed by config cannot support a claim about the whole dataset, so the score gate refuses
them outright rather than passing on less evidence. Anything that stops a check from measuring
*raises* the score, which is exactly backwards from what a gate is for.

## Testing conventions

**Prove red before green.** Write the test, watch it fail against the unfixed code, then fix. A
regression test that passes with the fix reverted is not a regression test, and this repo has caught
several of those. Save a copy of the file and restore it rather than reaching for `git checkout`, and
judge the run by its exit code — a passing line scrolling past in a suite of forty binaries is not a
result.

**Prove each claim red on its own.** An `assert!` stops the test at the first failure, so mutating
three things at once proves only that one of them is guarded. Mutate them one at a time.

**Pin a threshold on its boundary, in both directions.** A test that exercises a threshold *near* its
limit passes just as well with the comparison moved by one — which silently turns honest data into a
finding, or the reverse. Land on the boundary: the largest value that passes and the smallest that
does not. Where the quantity is a ratio of measured floats and no real recording lands on the limit
exactly, say so rather than pin it with a fabricated value.

**Build the fixture a real recorder would write.** Several checks guard CDM invariants that today's
adapters sanitize away, so they fire only on hand-built CDMs and never end to end. Do not assume a
new check fires on real data — run it through the real adapter before saying it does. The generators
in `crates/veridex-demo` write each format from its own specification rather than through Veridex's
reader, so that a generator cannot inherit the reader's misunderstanding of a format.

**Do not shell out to `cargo run` from a test.** Several test binaries running at once contend on the
build lock, and an invocation fails inside an unrelated test, reading as a real regression. It
happened; that is why the demo generators are library functions you call directly.

## Changes that touch more than they look like

### Adding a check

The compiler catches almost none of this.

1. Implement it in `crates/veridex-core/src/checks/<family>.rs`, declaring every code it can emit in
   `finding_codes()`.
2. Register it in `checks/mod.rs::standard_checks_with`.
3. Add a row to `docs/checks.md`. Guarded both ways: a registered code missing from the page fails,
   and a `FAMILY.CODE` on the page that no check emits fails.
4. Bump the executed-check count, which is written out in two test files. This is the step people
   forget; the tests name themselves clearly when it happens.

Adding only a **new code to an existing check** is lighter — steps 1 and 3 — and must *not* bump the
count, which counts checks, not codes.

An autonomy check has two more doc surfaces that bite and that the compiler is silent about:
`docs/profiles.md` (the criteria table and its sample `verify` output) and
`docs/autonomy-quickstart.md`, whose readiness block is diffed against real CLI output line by line.

### Adding a configurable threshold

Six places; the compiler catches the first three. Miss one of the last three and the knob exists but
cannot be set from the environment, is never printed by `--print-config`, or — worst — is not
disclosed as a narrowing when someone loosens it. `docs/veridex.toml.example` is guarded: a settable
key missing from it fails the config tests.

A profile may only ever *tighten* a threshold. That is what makes `--profile` compatible with a score
gate, and why there is deliberately no `lenient`.

### Adding a field to the CDM

The content-hash encoder in `canonical.rs` is **hand-written** — there is no derive — so a new field
on `Stream`/`Episode`/`Dataset`/`Provenance`/`Frame` is silently absent from the hash until you add
it. Two datasets differing only in that field would then collide, and one's certificate would attest
the other.

The rule is *every content field is hashed*, not only the ones a check can fail on. Add it to that
type's `encode`, bump `CANONICAL_VERSION`, and re-pin the golden vector in
`tests/canonical_golden.rs`. Two compile-time guards in `lib.rs`'s
`every_stream_field_binds_into_the_hash` are on your side: an exhaustive destructuring that breaks
when a field appears, and a mutator table whose length is written out. A field that can carry an
operator's own naming also belongs in `redact.rs` — a report meant to leave the building must not
carry it out.

Deliberate exclusions (manifest assertions, the reason a container would not parse) are documented as
excluded, with a test saying why.

## Documentation is tested

Claims in the README and `docs/` are held to the code that owns them, because they had drifted:

- every `cargo install` a page hands a reader must be a form that works today;
- the demo variant lists — bullets, both `Usage:` lines, the prose offering them, and the line the
  generator prints — must be the generator's own `VARIANTS`;
- spelled-out counts ("seven formats support `--metadata-only`") must be the count the registry or
  the engine reports;
- the check catalog page must match the registered catalog, both ways.

If one of these fails, the page is wrong, or the code changed and the page has not caught up. Fix the
page in the same commit. And write a claim against a *mechanism* rather than a number where you can —
a sentence naming the guard that enforces it does not go stale.

These guards read the page with its whitespace collapsed, because prose wraps and a claim's number
and its noun routinely land on different lines. A line-by-line guard silently stops guarding.

## Specs

Behavioral changes are designed in `openspec/` before they are built: `specs/` holds the ratified
north star for each capability, `changes/<id>/` holds the slice being built now, with its proposal,
design and task list. Read `openspec/AGENTS.md` for the layout and the requirement/scenario format.
A change big enough to alter what Veridex promises should arrive as a delta there, not only as code.

## Commits and pull requests

Titles follow [Conventional Commits](https://www.conventionalcommits.org/) — `type(scope): imperative
summary`, lowercase, no trailing period. The types in use are `feat`, `fix`, `docs`, `refactor`,
`chore`, `test`, `perf`, `build`, `ci` — match how the log already scopes them.

Say what was wrong and how you know it is fixed. The commit log here is written to be read later by
someone deciding whether to trust a verdict, so a message that states the old behavior, the new one,
and the proof — the failing-then-passing test, the before/after output — is worth the extra minute.

User-visible changes get a `CHANGELOG.md` entry under `[Unreleased]`.

## Reporting a security issue

Do not open a public issue. `SECURITY.md` has the process, along with the threat model and the list
of what Veridex does and does not guarantee.

## License

By contributing you agree your work is licensed under the [MIT License](LICENSE).
