# Add check-packs: third-party checks that a verdict can account for

## Why

The plugin surface already exists and nobody can be held to it. `Check` is a public, object-safe
trait and `EngineBuilder::register` is public, so a third party can add a check to a run **today** —
and when they do, the verdict cannot say it happened. A finding from a lab's own rule is
indistinguishable from a built-in one; nothing records which extra checks ran or at what version;
and two runs that disagree because one had a pack loaded look like two runs that disagree about the
data.

That is the opposite of what this project sells. A Veridex verdict is meant to be reproducible from
what it states about itself — the effective config, the tolerances, the executed checks — and an
unrecorded check breaks that more quietly than a narrowed threshold does, because a narrowing is at
least disclosed. Closing this turns "you can register a check" into "you can register a check and
the result still means something".

## What changes

A **check-pack**: a named, versioned set of checks registered together.

- **Namespaced ids.** A pack declares a name, and every check it registers is addressed as
  `pack/check-id`. The registry refuses a pack whose checks collide with the built-in catalog or
  with another pack — the same refusal `register` already makes for a duplicate id, extended to the
  namespace.
- **The verdict records the packs.** Name and version for each, beside `executed_checks`, and inside
  the result content hash. Two runs of the same core with different packs then have different
  verdicts by construction rather than by coincidence.
- **Findings are attributable.** A finding already carries its `check_id`; with namespaced ids that
  id names the pack, so every renderer, the SARIF output and the certificate attribute it without
  further change.
- **The certificate states what it attested under.** A certificate issued from a run with packs
  loaded names them, so a reader can tell a pass under the standard catalog from a pass under a
  catalog someone extended.

## What this change deliberately does **not** do

- **No dynamic loading.** A pack is a Rust crate the caller links and registers. Loading arbitrary
  code at runtime is a different risk surface (a plugin that can read the dataset can also read the
  signing key), and the spec's "untrusted plugins cannot forge trust" requirement is far easier to
  keep when a pack cannot run before the caller chose to link it. Dynamic loading, if it is ever
  wanted, is its own change with its own threat model.
- **No Python registration yet.** The Rust side first; the pyo3 binding follows the same shape once
  the accountability model is settled, and CLI/Python parity is a tested invariant here.
- **No pack registry or distribution mechanism.** A pack is a crate on crates.io. The hosted
  registry is a separate, explicitly roadmap capability.

## Impact

- `checks/mod.rs` and `engine.rs`: a `CheckPack` type, namespaced registration, and pack records in
  the verdict.
- `CANONICAL_VERSION` / the result hash: the pack set joins what a verdict is computed from.
- `docs/checks.md` gains a section on what a pack may and may not do, and `docs/` gains a short
  authoring guide with a worked example.
- No adapter change, no CDM change, no new built-in check.
