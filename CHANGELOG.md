# Changelog

All notable changes to Veridex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/); versions use [SemVer](https://semver.org/).

## [Unreleased] — v0.1 MVP (in progress)

The first shippable slice of the [`bootstrap-veridex-mvp`](openspec/changes/bootstrap-veridex-mvp/)
change. Runs end-to-end: ingest → validate → score → report → sign.

### Added

- **Canonical Dataset Model (CDM)** — the cross-format neutrality substrate
  (dataset / episode / stream / frame / provenance / label), with deterministic canonicalization
  streamed into SHA-256 and property-tested determinism.
- **Adapters** — LeRobot v3 (Parquet) and MCAP, each populating the CDM with a fidelity report of
  mapped / unmapped / omitted fields. A cross-format gate test proves the same logical dataset
  yields equivalent CDMs in both formats.
- **Validation engine** — check registry with duplicate-id rejection, category/id selection,
  severity overrides, deterministic stably-ordered verdicts with a result content hash, fault
  isolation for panicking checks, and reproducibility metadata.
- **Checks catalog** — structural (episode-boundary integrity covering the lerobot#4143 class,
  degenerate episodes/streams), temporal (monotonicity, rate conformance, gaps, and the headline
  `TEMPORAL.CLOCK_SKEW`), statistical (range/sanity and degeneracy over stored stats), and
  provenance-completeness. Every finding carries a training risk and a remedy.
- **Trust certificate** — a deterministic v1 score and A–F grade (provenance weighted as a separate
  30% axis), a content-bound certificate document, and Ed25519 signing with offline verification
  that rejects tampering, transplantation, and untrusted issuers.
- **Provenance emit** — MLCommons Croissant (JSON-LD) and minimal W3C PROV, preserving
  known / asserted / unknown classes without fabrication.
- **Reporting** — human-readable terminal output with worst-episodes-first rollups, a versioned
  JSON envelope (`veridex.report/1`), SARIF 2.1.0 (`veridex check --sarif`) for CI code-scanning, a
  self-contained HTML report (`veridex check --html`), and verdict diffing (`veridex diff`) that
  reports findings introduced / resolved / unchanged and the trust-score movement between two
  reports.
- **CLI** — `veridex check | inspect | certify | verify | provenance | keygen | diff`, with format
  autodetection (`--format` override, ambiguity is refused), a configurable failure threshold
  (`--fail-on`), and documented exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error).
- **Python bindings** (`import veridex`) that call the same core pipeline as the CLI, with a passing
  CLI ⇄ Python parity test.
- **Configuration** — a `veridex.toml` (auto-discovered, or `--config`) that selects categories,
  disables checks, overrides per-check severities, and sets the failure threshold; the effective
  config is recorded in every verdict.
- **CI** — GitHub Actions running fmt, clippy (`-D warnings`), and the full test suite.

### Not yet included

Streaming / large-than-memory and remote Hub ingestion; stored-vs-recomputed statistics and
actuator saturation; and publishing to PyPI / crates.io.
