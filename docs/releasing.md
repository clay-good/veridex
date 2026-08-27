# Releasing Veridex

Three artifacts ship from this repo, in this order, because each depends on the one before it:

1. **`veridex-core`** → crates.io — the library everything else calls.
2. **`veridex-cli`** → crates.io — installs the `veridex` binary (`cargo install veridex-cli`).
3. **`veridex-data`** → PyPI — the Python bindings (`crates/veridex-py`, built with maturin).

None of the three has been published yet; v0.1.0 is the first.

## Before you tag

Everything here is checkable locally, and CI runs all of it on every push.

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all --all-features
cargo publish --dry-run -p veridex-core            # packages, then compiles the packaged copy
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p veridex-core -p veridex-cli
```

CI runs all five on every push, including the publish dry-run and the docs build — a docs.rs failure
is invisible from crates.io, so it is caught here instead.

Then the CLI⇄Python parity suite, which CI runs in its own job and which needs the extension built:

```sh
maturin develop -m crates/veridex-py/Cargo.toml
python -m pytest crates/veridex-py/tests/test_parity.py
```

Checklist, in the order the things go wrong:

- [ ] `CHANGELOG.md`'s `[Unreleased]` section renamed to the version, with the date.
- [ ] Version bumped in `[workspace.package]` — all three crates inherit it, and
      `veridex-cli`/`veridex-py` pin `veridex-core = { version = "<the same>" }`. A mismatch is
      caught at publish time, not before.
- [ ] `CANONICAL_VERSION` bumped **if and only if** the canonical encoding changed. The golden vector
      in `tests/canonical_golden.rs` fails until the two agree, which is the point.
- [ ] `RUBRIC_VERSION` bumped if the trust-score rubric changed (`docs/rubric-v1.md`).
- [ ] The README's status section and `docs/checks.md` match what actually ships.

## Publishing

`veridex-cli` and `veridex-py` depend on `veridex-core` by path *and* version. Cargo drops the path
when publishing and resolves the version from crates.io — so **`cargo publish --dry-run -p
veridex-cli` cannot succeed until `veridex-core` is actually on crates.io**. That failure
(`no matching package named veridex-core found`) is expected, not a problem with the manifest.

```sh
cargo publish -p veridex-core
# wait for the index to update (usually seconds), then:
cargo publish -p veridex-cli
```

For PyPI, maturin builds an abi3 wheel per platform:

```sh
maturin publish -m crates/veridex-py/Cargo.toml
```

## What is in the published crate, and what is not

`veridex-core` excludes `tests/fixtures/**` — 3.5 MiB of binary Zarr, HDF5 and rosbag2 stores that
only this repo's own tests read. The tests themselves are packaged; they will not all pass from the
published crate alone, which is why the source of truth for "does this work" is CI over the
repository, not `cargo test` over the tarball. `veridex-cli`'s end-to-end rosbag2 test reads those
same fixtures across the workspace (`../veridex-core/tests/fixtures/rosbag2/`), so the caveat covers
it too — nothing a *user* of either crate compiles or runs depends on a fixture.

Check what a release would actually contain before sending it:

```sh
cargo package --list -p veridex-core | less
```

## After publishing

- [ ] Tag the commit (`git tag v0.1.0 && git push --tags`).
- [ ] Confirm docs.rs built (`https://docs.rs/veridex-core`); a docs.rs failure is invisible from
      crates.io and is usually a feature-flag or platform assumption in a doc example.
- [ ] Install from the registry into a clean environment and run the quickstart end to end —
      `cargo install veridex-cli && veridex check <a real dataset>` — because that path exercises the
      published manifest, not the workspace one.
