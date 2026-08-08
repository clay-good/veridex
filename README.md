# Veridex

[![CI](https://github.com/clay-good/veridex/actions/workflows/ci.yml/badge.svg)](https://github.com/clay-good/veridex/actions/workflows/ci.yml)

**Know if your robot data is safe to train on — in one command.**

Physical-AI teams lose weeks (and models) to bad data: mismatched clocks, corrupted episodes,
and datasets with no record of where they came from. As little as **0.3% poisoned data can
backdoor a policy**, and pooling the wrong data can make your model *worse*. Today there's no
fast, neutral way to check.

Veridex is that check. Point it at a dataset and it tells you — across any format — whether the
data is clean, correctly time-synchronized, and traceable to its origin, then stamps it with a
portable, signed **trust certificate** you can hand to anyone.

```sh
veridex check my-dataset/
```

```
Veridex Trust Report
  Score      82 / 100   (B)
  Structure  ✓ episodes intact, timestamps monotonic
  Temporal   ⚠ TEMPORAL.CLOCK_SKEW  camera vs. arm drift 41ms  → resync before training
  Provenance ⚠ missing sensor + license metadata on 3 streams
```

## Why it's useful

- **One command, any format.** LeRobot, MCAP, RLDS, HDF5/Zarr all map into one Canonical Dataset
  Model, so you check them the same way — no per-format tooling.
- **Catches the failures that quietly ruin training.** Clock skew across sensors, broken episode
  boundaries, timeline gaps, duplicate frames — each reported with the *training risk* it creates
  and a *remedy*.
- **Proves where data came from.** Which sensor, clock, calibration, annotator, license, and
  upstream dataset produced each segment — surfaced, scored, and emitted as a signed certificate
  (Croissant + W3C PROV underneath).
- **A number you can trust and share.** A deterministic 0–100 trust score and A–F grade. Same
  dataset always yields the same result, and the signed certificate verifies **offline**.
- **Never touches your data.** Veridex only reads and reports. It never mutates your dataset.

## Why it's different

Every major player — Hugging Face/LeRobot, Rerun, LanceDB, NVIDIA — is a *destination* that wants
your data in *their* format. None can be the neutral verifier *across* formats. Veridex takes the
one position an incumbent structurally can't: **Switzerland** — cross-format, and the only one that
also captures **provenance**.

## How it works

```mermaid
flowchart LR
    A[Your dataset<br/>LeRobot · MCAP · RLDS · HDF5] --> B[Adapter]
    B --> C[Canonical Dataset Model<br/>one neutral shape]
    C --> D[Validation engine<br/>structural · temporal · provenance checks]
    D --> E[Trust score<br/>0–100 · A–F grade]
    E --> F[Signed certificate<br/>portable · verifiable offline]
    D --> G[Human + JSON report]
```

A single flow: whatever format your data arrives in, an **adapter** maps it into the Canonical
Dataset Model. The **validation engine** runs the same checks over that neutral shape, a **trust
score** summarizes the result, and a **signed certificate** makes it portable — anyone can verify
it later without re-running Veridex.

```mermaid
sequenceDiagram
    participant You
    participant CLI as veridex
    participant Core as veridex-core
    participant Cert as Certificate

    You->>CLI: veridex check dataset/
    CLI->>Core: load via adapter → CDM
    Core->>Core: run structural + temporal + provenance checks
    Core-->>CLI: verdict + trust score + report
    CLI-->>You: report (terminal + JSON)

    You->>CLI: veridex certify dataset/ --key issuer.key
    CLI->>Cert: sign verdict
    Cert-->>You: portable trust certificate

    Note over You,Cert: Later, anywhere — no re-check needed
    You->>CLI: veridex verify dataset/ --certificate c.json
    CLI-->>You: ✓ trusted (offline)
```

## Commands

```sh
veridex check      <dataset>                                      # validate + report
veridex certify    <dataset> --key issuer.key                     # issue a signed trust certificate
veridex verify     <dataset> --certificate c.json --key pub.key   # verify offline
veridex provenance <dataset> --emit croissant                     # extract + emit provenance
veridex inspect    <dataset>                                      # summarize the dataset
```

Built on a Rust core (`veridex-core`) with a `veridex` CLI and a Python package
(`pip install veridex-data`, then `import veridex`) that produce identical verdicts.

## Quickstart

```sh
cargo build

# generate a demo MCAP with a synthetic cross-stream clock skew
cargo run -p veridex-core --example make_demo_mcap -- /tmp/demo.mcap

# validate it — prints a report and exits non-zero on failure
cargo run -p veridex-cli -- check /tmp/demo.mcap

# summarize the Canonical Dataset Model
cargo run -p veridex-cli -- inspect /tmp/demo.mcap

# machine-readable output
cargo run -p veridex-cli -- check --json /tmp/demo.mcap

# issue a signed, content-bound trust certificate and verify it offline
cargo run -p veridex-cli -- keygen /tmp/issuer
cargo run -p veridex-cli -- certify /tmp/demo.mcap --key /tmp/issuer --out /tmp/demo.veridex.json
cargo run -p veridex-cli -- verify  /tmp/demo.mcap --certificate /tmp/demo.veridex.json --key /tmp/issuer.pub
```

`check` catches the headline `TEMPORAL.CLOCK_SKEW` (the camera and robot clocks drift 210 ms apart),
reports the training risk and remedy, and exits `20` (fail). Exit codes: `0` pass · `10`
pass-with-warnings · `20` fail · `2` tool-error.

The certificate binds to the dataset's CDM content hash and is Ed25519-signed: `verify` succeeds
offline, and rejects a tampered certificate (signature mismatch) or one presented against a
different dataset (content-hash mismatch).

## Python

The same core is available from Python (`pip install veridex-data`, then `import veridex`), with
verdicts identical to the CLI:

```python
import veridex, json
report = json.loads(veridex.check("my-dataset.mcap"))
print(report["trust_score"]["grade"], report["verdict"]["status"])
```

Build the extension locally with [maturin](https://github.com/PyO3/maturin):
`maturin develop -m crates/veridex-py/Cargo.toml`.

## Build & test

```sh
cargo build            # build the workspace
cargo test             # unit + property + integration tests
cargo clippy --all-targets
```

## Status

**Early implementation, runs end-to-end.** Against a full [OpenSpec](openspec/) design, these are
in and tested: the Canonical Dataset Model with deterministic content hashing; the validation
engine; the structural / temporal / provenance check catalog (including the headline
`TEMPORAL.CLOCK_SKEW`); the v1 trust-score rubric; terminal + JSON reporting; **LeRobot v3 and MCAP
adapters** with a passing cross-format neutrality gate (the same logical dataset yields equivalent
CDMs in both formats); Croissant + W3C PROV provenance emit; Ed25519 **certificate signing with
offline verification** (tamper + transplant rejection); a working CLI (`check`, `inspect`,
`certify`, `verify`, `provenance`, `keygen`) — see the [Quickstart](#quickstart); and **Python
bindings** (`import veridex`) that call the same core pipeline, with a passing CLI⇄Python parity
test. Next up: streaming/remote ingestion and statistical checks.

Start with [openspec/project.md](openspec/project.md) for the design, or track progress in
[openspec/changes/bootstrap-veridex-mvp/tasks.md](openspec/changes/bootstrap-veridex-mvp/tasks.md).

## Relationship to Invariant

Veridex is a separate product from [Invariant](https://github.com/clay-good/invariant), a runtime
command-validation firewall — different lifecycle stage (training-time vs. runtime), different
users. Veridex reuses Invariant's COSE/JWS signed-verdict and audit substrate rather than
reinventing it.

## License

MIT — see [LICENSE](LICENSE).
