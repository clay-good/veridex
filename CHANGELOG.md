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
  yields equivalent CDMs in both formats. The LeRobot adapter resolves task strings
  (`task_index` + `meta/tasks.jsonl` → `episode.task`), so the semantic task-quality check runs on
  real datasets; the omission is reported honestly when no `meta/tasks.jsonl` is present. It also
  fingerprints each feature cell's raw value bytes into `frame.value_ref.content_hash` (a SHA-256,
  never a decode of the values), so — like MCAP below — the CDM hash is content-sensitive (a tampered
  export no longer verifies against the original's certificate) and exact-duplicate episode detection
  works end-to-end; cells whose type isn't a hashable numeric feature (e.g. images stored outside the
  Parquet) are left unhashed, honestly. The MCAP
  adapter extracts the file header's writing `library` (as a `recorder` provenance element) and
  `profile`, every producer-written **Metadata** record (preserved in dataset metadata, with
  well-known keys — license/sensor/calibration/operator/upstream — mapped to typed provenance), and
  **Attachment** summaries (a calibration-looking attachment supplies the `calibration` element), so
  provenance reflects who produced the recording and how. Each message's raw bytes are fingerprinted
  into `frame.value_ref.content_hash` (a SHA-256 of the bytes, not a decode), so the CDM content hash
  — and thus certificate binding — is sensitive to actual frame content: a tampered recording with
  identical topics and timestamps no longer hashes the same, and content-level checks (duplicate
  episodes) have something exact to compare.
- **Validation engine** — check registry with duplicate-id rejection, category/id selection,
  severity overrides, deterministic stably-ordered verdicts with a result content hash, fault
  isolation for panicking checks, and reproducibility metadata.
- **Checks catalog** — structural (episode-boundary integrity covering the lerobot#4143 class,
  degenerate episodes/streams — including a dataset with zero episodes, which would otherwise pass
  silently — episode-index continuity (a gap like `0, 1, 3` means a dropped episode, caught without
  any manifest), declared-vs-actual episode- and frame-count checks that catch a truncated export
  (LeRobot `total_episodes` / `total_frames` vs what was ingested), and cross-episode dtype/shape
  consistency so a stream
  that changes
  tensor shape between episodes — an un-batchable dataset — is caught, and cross-episode stream
  presence so a stream that appears in only some episodes — a sensor that dropped out, or two
  exports pooled together — is flagged (`STRUCTURAL.STREAM_PRESENCE_INCONSISTENT`), and exact-duplicate
  episode detection (`STRUCTURAL.DUPLICATE_EPISODE`) that groups episodes with identical frame content
  — a re-upload or a bad merge that over-weights the repeated trajectories; sound-only, comparing an
  episode only when every frame carries a `content_hash`, so it never mis-flags two different
  same-length episodes that merely share a time base and dataset-global stats, and a frozen-sensor
  check (`STRUCTURAL.STUCK_STREAM`) that flags a `Video` stream repeating a byte-identical frame for
  ≥5 consecutive frames while timestamps advance — a stuck camera that every timestamp-based temporal
  check passes over; scoped to video, where byte-identical frames are physically implausible, so a
  constant scalar stream isn't mistaken for one), temporal (monotonicity,
  rate conformance, gaps, the headline
  a declared-rate validity check (`TEMPORAL.INVALID_RATE`) that flags a corrupt declared rate
  (`0`, negative, `NaN`, `inf`) which the rate- and gap-conformance checks would otherwise skip
  silently, the headline
  `TEMPORAL.CLOCK_SKEW`, a shared-clock start-offset check that catches a stream which comes
  online late — a misalignment the duration-based skew check can miss — its mirror
  `TEMPORAL.END_OFFSET` that catches a stream which drops out early or runs long (a truncated tail;
  because `end = start + duration`, a tail misalignment can slip past both the start-offset and
  clock-skew checks), a timeline-jitter check
  (`TEMPORAL.JITTER`) that flags an irregular inter-frame spacing (coefficient of variation above a
  configurable tolerance) even when the mean rate looks correct and no single interval is a gap, and
  a cross-episode declared-rate consistency check (`TEMPORAL.RATE_INCONSISTENT`) that flags a stream
  whose declared sampling rate changes between episodes — differently-configured sources pooled under
  one key — which every per-episode check passes over, and a cross-episode duration-outlier check
  (`TEMPORAL.EPISODE_DURATION_OUTLIER`) that flags an episode whose length is a large multiple away
  from the dataset's median — a truncated capture or a stuck recorder that still counts as a full
  labeled trajectory — using a median baseline robust to the outliers it hunts),
  statistical (range/sanity — inverted range, non-finite, negative std, mean-outside-range, and
  degeneracy, an implausibly-large std that violates Popoviciu's bound `(max−min)/2`, and stored
  min/max outside the declared integer dtype's representable range — over stored stats), semantic
  (task-string quality — present-but-empty and placeholder tasks — and stream-key clarity, which
  flags an exact-duplicate stream key within an episode as an error (`SEMANTIC.DUPLICATE_STREAM_KEY`,
  a uniqueness-invariant violation) and camera/stream keys that merely collide by case or whitespace
  as a warning (`SEMANTIC.AMBIGUOUS_STREAM_KEY`); verified, never modified), and
  provenance-completeness (presence, internal consistency, and placeholder values — a `license` of
  `"unknown"` is present in form but empty in substance, so it's flagged and not counted as real
  provenance). Every finding carries a training risk and a remedy.
- **Trust certificate** — a deterministic v1 score and A–F grade (provenance weighted as a separate
  30% axis), a content-bound certificate document, and Ed25519 signing with offline verification
  that rejects tampering, transplantation, untrusted issuers, and unsupported signature algorithms.
- **Provenance emit** — MLCommons Croissant (JSON-LD) and minimal W3C PROV, preserving
  known / asserted / unknown classes without fabrication. The PROV graph attributes the dataset to
  every known agent (recorder as a `prov:SoftwareAgent`, annotator as a `prov:Person`, sensor as a
  `prov:Agent`) and derives it from a known upstream, with each agent resolvable as a graph node.
- **Reporting** — human-readable terminal output with worst-episodes-first rollups (and a note of
  any non-default tolerance the run applied, so a "no findings" result is read against the right
  thresholds), a versioned
  JSON envelope (`veridex.report/1`), SARIF 2.1.0 (`veridex check --sarif`) for CI code-scanning
  (rules carry a description and a link to the check catalog), a
  self-contained HTML report (`veridex check --html`), and verdict diffing (`veridex diff`) that
  reports findings introduced / resolved / unchanged and the trust-score movement between two
  reports — with `--fail-on-regression` to fail CI when the new report introduces findings or a
  lower score.
- **CLI** — `veridex check | inspect | checks | certify | verify | provenance | keygen | diff`
  (`inspect` summarizes the CDM structure — including each episode's wall-clock span, so a
  duration outlier is visible at a glance — and the provenance coverage — known/asserted/unknown per
  expected element, with placeholders shown as missing; `checks` lists the built-in catalog — id,
  category, default severity, scope, and the finding
  codes each check can emit — as text or
  `--json`; tests guard those codes against the doc catalog in both directions, so a code can't
  ship undocumented and a stale doc row can't outlive its code),
  with format
  autodetection (`--format` override, ambiguity is refused), a configurable failure threshold
  (`--fail-on`), a trust-score gate for CI (`--min-score 0-100`, fails below the threshold), and
  documented exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error). An end-to-end integration
  test drives the real binary over the whole trust flow — `check` (terminal + JSON) then
  `keygen → certify → verify` against a committed dataset fixture, including rejection of an
  untrusted issuer key.
- **Python bindings** (`import veridex`) exposing `check` / `content_hash` / `inspect` / `catalog` /
  `provenance` / `version`, calling the same core pipeline as the CLI, with a passing CLI ⇄ Python
  parity test over `check`, `inspect`, `catalog`, and `provenance` — each shares a single core render
  helper (`render_catalog_json`, `render_provenance`) with the CLI, so the machine-readable catalog
  and the Croissant / PROV provenance documents are byte-identical across CLI and Python.
- **Configuration** — a `veridex.toml` (auto-discovered, or `--config`) that selects categories,
  disables checks, overrides per-check severities, and sets the failure threshold and minimum trust
  score (`min_score`, overridable by `--min-score`); the effective config is recorded in every
  verdict. Unknown TOML keys are rejected, and a check id that names no real check (a typo in
  `disabled_checks`, `only_checks`, or a `severity_overrides` key) is a hard error rather than a
  silent no-op. A `[tolerances]` table tunes the temporal checks' numeric thresholds
  (`clock_skew_ms`, `start_offset_ms`, `rate_deviation`, `gap_factor`); each is optional, validated
  (finite, non-negative; positive `gap_factor`), and falls back to the check's default. The
  tolerances the run used are recorded in the verdict's effective config, so a result is fully
  reproducible from what it reports.
- **Runnable demos** — `examples/make_demo_mcap` (synthetic cross-stream clock skew) and
  `examples/make_demo_lerobot` (a LeRobot v3 dataset with an out-of-order timestamp, a
  `truncated` variant whose manifest over-declares its frame count → `STRUCTURAL.FRAME_COUNT_MISMATCH`,
  and a `jitter` variant whose one episode has an irregular inter-frame spacing → `TEMPORAL.JITTER`),
  each with a `clean` variant, so `veridex check` has something to find end-to-end in **both** formats.
- **CI** — GitHub Actions running fmt, clippy (`-D warnings`), and the full test suite, plus a
  Python job that builds the extension with maturin and runs the CLI ⇄ Python parity test on every
  push. The `veridex` binary has its own integration tests (`crates/veridex-cli/tests/cli.rs`)
  asserting command dispatch, argument validation, and the CI exit-code contract (0 · 10 · 20 · 2).

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
