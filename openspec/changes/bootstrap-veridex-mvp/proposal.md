# Bootstrap Veridex — v0.1 MVP

## Why

Veridex only matters if it is **differentiated at launch**. The nearest tool (Trajlens) is a
capable LeRobot-only linter with repair. If v0.1 is "a worse LeRobot linter," it dies. So the MVP
must ship the two things Trajlens structurally lacks — **cross-format** validation and
**provenance + a signed certificate** — from day one, even if narrow.

The MVP also has a second job: **earn the anointer conversation.** It should be good enough, on the
LeRobot Hub's own datasets, that a PR proposing Veridex as a quality/provenance gate is credible.
The strongest evidence in our research is that Hugging Face filed the very bug this addresses
([lerobot#4143](https://github.com/huggingface/lerobot/issues/4143)) and never shipped a general
validator.

## What changes

Introduces the first working slice of Veridex:

- **Cross-format from day one:** ingestion adapters for **LeRobot v3** and **MCAP**, both mapping
  into the Canonical Dataset Model. Two formats — not one — is the point.
- **The wedge checks:** structural integrity + temporal/synchronization + statistical checks, with
  **cross-stream clock-skew detection** as the headline capability no single-format tool offers.
- **Provenance, minimally but really:** extract whatever lineage the source encodes, classify
  known/asserted/unknown, and emit **Croissant**.
- **The flagship artifact:** a signed, offline-verifiable **trust certificate** bound to the CDM
  content hash, with a documented v1 scoring rubric, reusing Invariant's COSE/JWS substrate.
- **CLI:** `veridex check | certify | verify | provenance | inspect`, terminal + JSON output,
  CI-friendly exit codes, plus a Python binding with verdict parity.

## Explicitly out of scope for v0.1 (deferred to later changes)

- Additional adapters: RLDS/TFDS, HDF5, Zarr.
- Semantic and video **deep** checks (basic structural media checks stay in; VLM-grade semantic
  analysis is later). Language-annotation *verification* is deferred to the annotation-verify
  change.
- HTML and SARIF reporting; verdict diffing.
- Producer attestation signing UX beyond the minimum needed for the certificate.
- Any hosted registry / monitoring / commercial layer.

## Impact

- Establishes: `dataset-ingestion`, `validation-engine`, `checks-catalog`, `provenance-lineage`,
  `trust-certificate`, `reporting`, `cli` (all at MVP depth).
- New repo scaffolding: `veridex-core` (Rust), Python bindings (`veridex-data` on PyPI, import
  `veridex`), `veridex` CLI.
- Dependency on Invariant's attestation approach (shared or mirrored COSE/JWS signing + audit).
- Success criteria: runs end-to-end on a real LeRobot v3 Hub dataset **and** an MCAP recording;
  detects a synthetic cross-stream skew and a corrupted episode boundary; emits a certificate that
  `veridex verify` validates offline; produces valid Croissant.
