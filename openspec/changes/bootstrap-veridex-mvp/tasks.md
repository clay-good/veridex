# Tasks — Veridex v0.1 MVP

Ordered so each milestone is demoable. No code is written until this change is approved; this is
the build plan.

## M0 — Scaffolding
- [x] Create the Rust workspace: `veridex-core`, `veridex-cli`. (`veridex-py` pyo3/maturin: deferred to M8.)
- [ ] Wire CI: build, test, clippy/fmt gates; `#![forbid(unsafe_code)]` where feasible. (`forbid(unsafe_code)` set on `veridex-core`; CI config still to add.)
- [ ] Decide crypto substrate sharing with Invariant (shared crate vs. mirrored module); record it.

## M1 — Canonical Dataset Model
- [x] Define CDM types (Dataset/Episode/Stream/Frame/Provenance/Label) per design D2.
- [x] Implement CDM canonicalization + content hashing (D5); property-test determinism.
- [x] Define the adapter trait (populate CDM; declare supported versions; record unmapped fields).

## M2 — Ingestion adapters (the neutrality proof)
- [ ] LeRobot v3 adapter → CDM (episodes, streams, timestamps, tasks, language annotations present
      as data, provenance extraction).
- [x] MCAP adapter → CDM (channels→streams, message timestamps, schemas→modalities). Backed by the
      `mcap` crate; tests write real MCAP files and ingest them.
- [ ] Streaming/large-than-memory ingestion; remote (Hub) metadata-only ingestion. (MCAP currently
      reads the whole local file; streaming is a follow-up.)
- [ ] **Gate test:** the same logical dataset as LeRobot v3 and MCAP yields equivalent CDMs.
      (Blocked on the LeRobot adapter.)

## M3 — Validation engine
- [x] Check registry, severities, categories, selection/config, run metadata (per spec).
- [x] Deterministic ordered verdict; fault isolation for errored checks.

## M4 — Checks catalog (MVP families)
- [~] Structural: episode-boundary integrity (covers lerobot#4143 class via duplicate-index /
      inverted-bounds), degenerate episodes/streams. (Missing-shards + dtype/shape consistency need
      adapter-populated shape metadata — deferred to M2.)
- [x] Temporal: monotonicity, rate conformance, gaps — and **`TEMPORAL.CLOCK_SKEW`** cross-stream
      alignment (D4, headline).
- [ ] Statistical: stored-vs-recomputed stats, range/sanity, saturation, degenerate distributions.
      (Needs CDM to carry stored/recomputed stats — deferred until adapters populate them.)
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
- [~] `veridex check | inspect | certify | verify | keygen` implemented end-to-end. `provenance`
      awaits Croissant emit.
- [x] Format autodetect + `--format` override; ambiguity is `IngestError::AmbiguousFormat`, not a
      silent guess.
- [~] CI exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error). Configurable failure threshold
      is a follow-up.
- [ ] Python bindings; **parity test:** CLI and Python produce identical verdicts/certificates.

## M9 — Proof & anointer prep
- [ ] End-to-end demo on a real LeRobot v3 Hub dataset and a real MCAP recording.
- [ ] Reproduce detection of a synthetic cross-stream skew and a corrupted episode boundary.
- [ ] Draft the upstream proposal (LeRobot CI/Hub quality-and-provenance gate) citing lerobot#4143.
- [ ] Write quickstart docs; publish `veridex-data` to PyPI (import module `veridex`) and the
      `veridex` CLI / crates.io crates.
