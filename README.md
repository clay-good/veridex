# Veridex

**The open, cross-format trust layer for physical-AI data.**

Veridex verifies that a robot or sensor dataset is clean, correctly time-synchronized, and
traceable to its origin, then stamps it with a portable, signed **provenance certificate** — so any
team can tell in seconds whether the data they're about to train on will improve their model or
silently poison it.

> One line: *the neutral layer that tells you, across any format, whether the data you're about to
> train on is clean and where it came from.*

## Status

**Early implementation** — building the v0.1 MVP against the full [OpenSpec](openspec/) design.

Done so far:

- **Rust workspace** (`veridex-core`, `veridex-cli`) with `#![forbid(unsafe_code)]`.
- **Canonical Dataset Model (CDM)** — the cross-format neutrality substrate
  (`dataset`/`episode`/`stream`/`frame`/`provenance`/`label`), in
  [`crates/veridex-core/src/cdm.rs`](crates/veridex-core/src/cdm.rs).
- **Deterministic content hashing** — canonicalization streamed straight into SHA-256, with
  property tests proving the same dataset always yields the same hash regardless of the ordering
  of order-insensitive collections
  ([`canonical.rs`](crates/veridex-core/src/canonical.rs)).
- **Adapter contract** — the `Adapter` trait + registry every format plugs into, with
  fidelity reporting (mapped/unmapped/omitted fields) and clear rejection of unsupported formats
  ([`adapter.rs`](crates/veridex-core/src/adapter.rs)).
- **Validation engine** — a check registry (duplicate-id rejection, category/id selection,
  severity overrides), deterministic stably-ordered verdicts with a result content hash, fault
  isolation for panicking checks, and full reproducibility metadata
  ([`engine.rs`](crates/veridex-core/src/engine.rs)).
- **Checks catalog (structural + temporal)** — episode-boundary integrity (the lerobot#4143
  corrupted-boundary class), degenerate episodes/streams, timestamp monotonicity, declared-rate
  conformance, timeline gaps, and the headline **`TEMPORAL.CLOCK_SKEW`** cross-stream drift check
  (design D4). Plus **provenance-completeness** checks that surface missing/inconsistent
  license, sensor, clock, calibration, annotator, and lineage provenance. Every check ships an id,
  a documented training **risk**, and a **remedy** ([`checks/`](crates/veridex-core/src/checks/)).
- **Trust score (v1 rubric)** — a deterministic 0–100 score and A–F grade from the verdict and
  provenance coverage, with provenance weighted as a separate 30% axis so a clean check score can't
  mask missing provenance. Rubric documented in [`docs/rubric-v1.md`](docs/rubric-v1.md)
  ([`certificate/`](crates/veridex-core/src/certificate/)).
- **`veridex` CLI skeleton** — the command surface is wired; subcommands land as their core
  capabilities are implemented.

Next: the format adapters (LeRobot v3, MCAP) that populate the CDM from real files. Track progress in
[openspec/changes/bootstrap-veridex-mvp/tasks.md](openspec/changes/bootstrap-veridex-mvp/tasks.md).
For the design, start at [openspec/project.md](openspec/project.md).

## Build & test

```sh
cargo build            # build the workspace
cargo test             # unit + property tests (determinism of the CDM hash)
cargo run -p veridex-cli -- --help
```

## Why Veridex exists

In physical AI / vision-language-action robotics, **data quality — not architecture or compute — is
the binding constraint**, and today it is unmeasurable across formats:

- A curated ~5% coreset can recover 85–90% of full-dataset performance; pooling heterogeneous robot
  data can cause measurable *negative transfer*; ~0.3% poisoned episodes can backdoor a policy.
- Datasets arrive in incompatible containers (LeRobot, MCAP, RLDS, HDF5/Zarr) with no shared way to
  check timestamp alignment, episode integrity, or **where any of it came from**.

Teams still train on unvetted data because checking it is manual, per-format, and provenance simply
isn't captured. Veridex makes dataset trust a one-command, cross-format, machine-readable fact.

## The wedge

Every funded player here — Hugging Face/LeRobot, Rerun, LanceDB, NVIDIA — is a **destination**, each
wanting your data in *their* format. None can be the neutral verifier *across* formats. Veridex owns
the one position an incumbent structurally cannot: **Switzerland**. Its two differentiators are the
flanks the nearest tool (Trajlens, LeRobot-only, no provenance) leaves open:

1. **Cross-format** — one validation model over a Canonical Dataset Model (CDM) that MCAP, RLDS,
   LeRobot, HDF5/Zarr all map into.
2. **Provenance / lineage / attestation** — which sensor, clock, calibration, annotator, license,
   and upstream dataset produced each segment, emitted as a signed, portable certificate
   (Croissant + W3C PROV underneath).

Veridex **interoperates with** Trajlens and LeRobot rather than duplicating them, and it **never
mutates** your dataset (repair is not its lane).

## Shape of the tool (aspirational CLI)

```sh
veridex check      <dataset>                      # validate + report
veridex certify    <dataset> --key issuer.key     # issue a signed trust certificate
veridex verify     <dataset> --certificate c.json --key issuer.pub   # offline verification
veridex provenance <dataset> --emit croissant     # extract + emit provenance
veridex inspect    <dataset>                       # summarize the CDM
```

A Rust core (`veridex-core`) powers a `veridex` CLI and a Python package
(`pip install veridex-data`, then `import veridex`) with identical verdicts. The PyPI distribution
is `veridex-data` (the bare `veridex` name is taken on PyPI); the CLI, import module, and
GitHub/crates.io project are all `veridex`.

## Specs

The complete design lives under [`openspec/`](openspec/):

- North-star capabilities: [`openspec/specs/`](openspec/specs/)
- First build (v0.1 MVP): [`openspec/changes/bootstrap-veridex-mvp/`](openspec/changes/bootstrap-veridex-mvp/)

## Relationship to Invariant

Veridex is a separate product from [Invariant](../invariant) (a runtime command-validation
firewall). Different lifecycle stage (training-time vs. runtime), different users. Veridex reuses
Invariant's COSE/JWS signed-verdict and audit substrate rather than reinventing it.

## License

MIT — see [LICENSE](LICENSE).
