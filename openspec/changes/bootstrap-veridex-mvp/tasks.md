# Tasks — Veridex v0.1 MVP

Ordered so each milestone is demoable. No code is written until this change is approved; this is
the build plan.

## M0 — Scaffolding
- [x] Create the Rust workspace: `veridex-core`, `veridex-cli`. (`veridex-py` pyo3/maturin: deferred to M8.)
- [x] Wire CI: build, test, clippy/fmt gates (`.github/workflows/ci.yml`, `-D warnings`);
      `#![forbid(unsafe_code)]` on `veridex-core`.
- [ ] Decide crypto substrate sharing with Invariant (shared crate vs. mirrored module); record it.

## M1 — Canonical Dataset Model
- [x] Define CDM types (Dataset/Episode/Stream/Frame/Provenance/Label) per design D2.
- [x] Implement CDM canonicalization + content hashing (D5); property-test determinism.
- [x] Define the adapter trait (populate CDM; declare supported versions; record unmapped fields).

## M2 — Ingestion adapters (the neutrality proof)
- [x] LeRobot v3 adapter → CDM (features→streams, Parquet `timestamp`→frame ts, `episode_index`
      grouping, fps→rate, robot_type→provenance). Reads only timestamps/structure, not payloads.
      (Task-string resolution and video decoding are follow-ups.)
- [x] MCAP adapter → CDM (channels→streams, message timestamps, schemas→modalities). Backed by the
      `mcap` crate; tests write real MCAP files and ingest them.
- [ ] Streaming/large-than-memory ingestion; remote (Hub) metadata-only ingestion. (Adapters
      currently read the whole local file; streaming is a follow-up.)
- [x] **Gate test:** the same logical dataset as LeRobot v3 and MCAP yields equivalent CDMs
      (`tests/lerobot_adapter.rs::same_logical_dataset_yields_equivalent_cdms_across_formats`).

## M3 — Validation engine
- [x] Check registry, severities, categories, selection/config, run metadata (per spec).
- [x] Deterministic ordered verdict; fault isolation for errored checks.

## M4 — Checks catalog (MVP families)
- [~] Structural: episode-boundary integrity (covers lerobot#4143 class via duplicate-index /
      inverted-bounds), degenerate episodes/streams, and **cross-episode dtype/shape consistency**
      (`STRUCTURAL.SHAPE_MISMATCH`) — the CDM `Stream` now carries declared `dtype`/`shape`, which the
      LeRobot adapter reads from `meta/info.json`. (Missing-shards still needs adapter-populated shard
      metadata — deferred to M2.)
- [x] Temporal: monotonicity, rate conformance, gaps — and **`TEMPORAL.CLOCK_SKEW`** cross-stream
      alignment (D4, headline).
- [~] Statistical: range/sanity + degenerate distributions over stored stats
      (`statistical.range-sanity`: inverted range, non-finite, negative std, mean-outside-range,
      constant). CDM now
      carries `Stream.stats`; the LeRobot adapter reads `meta/stats.json`. Stored-vs-recomputed and
      saturation need streamed values — a follow-up.
- [x] Provenance-completeness checks (presence + internal consistency).
- [x] Each check ships ID + documented risk + remedy.

## M5 — Provenance & Croissant
- [x] Provenance model + known/asserted/unknown classification (in `cdm.rs`).
- [~] Extract provenance from LeRobot v3 and MCAP sources. (MCAP records source_format; rich
      extraction — metadata records, calibration — and the LeRobot side land with those adapters.)
- [x] Emit Croissant (JSON-LD) + minimal W3C PROV lineage from the CDM (honest classes, no
      fabrication). Wired to `veridex provenance --emit croissant|prov`.

## M6 — Trust certificate
- [x] Certificate schema (content, content-hash binding, rubric_version, issuance metadata) —
      `veridex.certificate/1`, records checks run + categories skipped + provenance coverage.
- [x] v1 scoring rubric (D7): deterministic score + grade; ship rubric doc (docs/rubric-v1.md).
      Provenance coverage (known/asserted/unknown) is a separate 30% axis so a clean check score
      can't mask missing provenance.
- [x] Ed25519 signing (JWS-style detached signature over canonical bytes, D6); offline `verify`;
      tamper + transplant + wrong-issuer rejection tests. (COSE envelope is a later refinement.)

## M7 — Reporting (MVP)
- [x] Terminal report + rollup summaries (dataset/episode/stream, worst episodes first).
- [x] Versioned JSON output (`veridex.report/1`).

## M8 — CLI + Python parity
- [x] `veridex check | inspect | checks | certify | verify | provenance | keygen | diff` implemented
      end-to-end (`checks` lists the built-in catalog as text or `--json`).
- [x] Format autodetect + `--format` override; ambiguity is `IngestError::AmbiguousFormat`, not a
      silent guess.
- [x] CI exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error) with a configurable failure
      threshold (`--fail-on error|warning`, default error).
- [x] Python bindings (`veridex-py`, pyo3/abi3) exposing `check`/`content_hash`/`inspect`/`version`;
      both front-ends call one shared `veridex_core` pipeline so parity is by construction.
      **Parity test** (`crates/veridex-py/tests/test_parity.py`) asserts CLI and Python produce
      byte-identical `check` and `inspect` output — run in CI (maturin build + pytest). The
      `veridex-data` wheel builds with maturin.

## M9 — Proof & anointer prep
- [ ] End-to-end demo on a real LeRobot v3 Hub dataset and a real MCAP recording. (Synthetic
      end-to-end demo works today via `examples/make_demo_mcap` + `veridex check`; real Hub datasets
      need network access.)
- [~] Reproduce detection of a synthetic cross-stream skew (done: the demo MCAP triggers
      `TEMPORAL.CLOCK_SKEW`) and a corrupted episode boundary (covered by unit tests; a runnable
      corrupted-boundary fixture is a follow-up).
- [x] Draft the upstream proposal (LeRobot CI/Hub quality-and-provenance gate) citing lerobot#4143
      — [docs/adoption-lerobot-ci-gate.md](../../../docs/adoption-lerobot-ci-gate.md).
- [~] Quickstart docs written (README). Publishing `veridex-data` to PyPI and the crates to
      crates.io is a release step (not done here; needs registry credentials).
