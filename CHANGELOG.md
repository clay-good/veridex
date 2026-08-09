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
  degenerate episodes/streams, and cross-episode dtype/shape consistency so a stream that changes
  tensor shape between episodes — an un-batchable dataset — is caught), temporal (monotonicity,
  rate conformance, gaps, and the headline
  `TEMPORAL.CLOCK_SKEW`), statistical (range/sanity — inverted range, non-finite, negative std, mean-outside-range, and
  degeneracy — over stored stats), semantic
  (task-string quality — present-but-empty and placeholder tasks — and stream-key clarity, which
  flags camera/stream keys that collide by case or whitespace; verified, never modified), and
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
- **CLI** — `veridex check | inspect | checks | certify | verify | provenance | keygen | diff`
  (`checks` lists the built-in catalog — id, category, default severity, scope — as text or
  `--json`), with format
  autodetection (`--format` override, ambiguity is refused), a configurable failure threshold
  (`--fail-on`), and documented exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error).
- **Python bindings** (`import veridex`) exposing `check` / `content_hash` / `inspect` / `version`,
  calling the same core pipeline as the CLI, with a passing CLI ⇄ Python parity test over both
  `check` and `inspect`.
- **Configuration** — a `veridex.toml` (auto-discovered, or `--config`) that selects categories,
  disables checks, overrides per-check severities, and sets the failure threshold; the effective
  config is recorded in every verdict.
- **Runnable demos** — `examples/make_demo_mcap` (synthetic cross-stream clock skew) and
  `examples/make_demo_lerobot` (a LeRobot v3 dataset with an out-of-order timestamp), each with a
  `clean` variant, so `veridex check` has something to find end-to-end in **both** formats.
- **CI** — GitHub Actions running fmt, clippy (`-D warnings`), and the full test suite, plus a
  Python job that builds the extension with maturin and runs the CLI ⇄ Python parity test on every
  push.

### Fixed

- The `veridex-data` wheel could not build: `pyproject.toml` was missing a `version` (now taken
  dynamically from the crate) and referenced a nonexistent package `README.md` (now added). The
  wheel builds and the parity test passes under pyo3 0.29.
- A mistyped or missing dataset path was misreported as `unsupported format: no adapter recognized
  the source`. Ingestion now checks a local path exists first and returns a clear
  `no such file or directory` (`IngestError::SourceNotFound`), distinct from an unrecognized format.
- `veridex verify --key <path>` with a missing/invalid key file was silently reinterpreting the path
  string as the key, then reporting `untrusted issuer` (a verification *failure*, exit 20) instead of
  a tool error. The `--key` value is now resolved unambiguously — a 64-char hex key inline, otherwise
  a file path — and an unreadable or non-hex key file is a clear exit-2 error, not a false mismatch.
- `veridex keygen` silently overwrote an existing key file — an unrecoverable loss of a signing key.
  It now refuses to clobber an existing secret or public key unless `--force` is passed.
- `veridex check --fail-on <typo>` silently fell back to the default threshold, quietly disabling the
  strictness a CI user asked for. An unrecognized `--fail-on` value is now an exit-2 error.
- The temporal checks (rate, gaps, clock-skew) computed timestamp intervals with plain `i64`
  subtraction, which overflowed on corrupt timestamps spanning the full `i64` range — a panic in
  debug builds (isolated to an errored check) or a wrapped value in release. They now use saturating
  subtraction, so pathological timestamps are reported rather than crashing the check.

### Security

- Upgraded `pyo3` 0.22 → 0.29, clearing three advisories (RUSTSEC out-of-bounds read in
  `PyList`/`PyTuple` `nth`/`nth_back`, the missing `Sync` bound on `PyCFunction::new_closure`, and
  the `PyString::from_object` buffer-overflow risk). The bindings' API surface was already on the
  `Bound` API, so the bump is source-compatible.

### Not yet included

Streaming / large-than-memory and remote Hub ingestion; stored-vs-recomputed statistics and
actuator saturation; and publishing to PyPI / crates.io.
