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
      Task strings are resolved (`task_index` + `meta/tasks.jsonl` → `episode.task`). Video *files*
      are now resolved and their container headers read into `Stream.media`; per-frame pixel decoding
      remains deliberately out of scope.
- [x] MCAP adapter → CDM (channels→streams, message timestamps, schemas→modalities). Backed by the
      `mcap` crate; tests write real MCAP files and ingest them.
- [x] RLDS/TFDS adapter → CDM (`features.json` step leaves→streams, one TFRecord per episode, step
      index→frame ts, `language_instruction`→`episode.task`, `episode_metadata/file_path`→
      `provenance.upstream`, split `shardLengths`→declared episode count). TFRecord framing and
      `tf.train.Example` are parsed directly, with the masked CRC-32C verified on every record. An
      episode's step count is *derived* per feature (list length ÷ declared element size) and the
      answers must agree — a record contradicting its own schema is refused, never mapped into a
      short episode. RLDS records no wall clock, so the timeline is the step index on a clock named
      `rlds-step-index`, no rate is invented, and the ingest report states the omission.
- [~] Streaming/large-than-memory ingestion; remote (Hub) metadata-only ingestion. **Sampled
      ingestion is in** (`--sample-episodes` / `--sample-fraction` / `--sample-seed`, and the same
      arguments from Python): the LeRobot adapter resolves the draw from the declared episode set
      before reading any Parquet, so a skipped episode is never hashed, never accumulated into the
      statistics, and never charged to the frame budget — the bounded way to check a dataset larger
      than the budget allows. RLDS/TFDS resolves it the same way from the split `shardLengths`, and
      an unselected record is framed, its length prefix verified, and then seeked past without its
      payload being read — so drawing a few episodes from a 900 MB shard costs a few episodes of I/O,
      at the stated cost that a sampled run does not attest what it skipped. Single-episode formats (MCAP,
      CAN+DBC, MF4) refuse a sample rather than returning everything. Coverage rides in the verdict (and its hash), every report states it, and
      `certify` refuses a partial run. True streaming (never materializing the whole CDM) and remote
      Hub ingestion remain follow-ups; `metadata_only` and `Source::Remote` are **refused** with
      `IngestError::NotImplemented` rather than silently ignored.
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
- [x] Statistical: range/sanity + degenerate distributions over stored stats
      (`statistical.range-sanity`: inverted range, non-finite, negative std, mean-outside-range,
      Popoviciu-implausible std, integer-dtype range, constant); CDM carries `Stream.stats` from
      `meta/stats.json`. Value-based checks now ship too — the LeRobot adapter recomputes per-feature
      stats from the actual cells (fingerprinting the same pass), enabling stored-vs-recomputed
      (`STATISTICAL.STATS_STALE`), saturation (`STATISTICAL.SATURATED`), extreme outliers
      (`STATISTICAL.OUTLIER`), and non-finite values in the data (`STATISTICAL.NON_FINITE_OBSERVED`,
      scanned across every dimension). MCAP abstains from value-based checks (opaque payloads).
- [x] Video/media: the container against the data it is paired with. The LeRobot adapter resolves
      each video stream's `videos/**/<feature>/episode_<n>.mp4`, reads its ISO-BMFF headers (never a
      pixel), and carries both the manifest's declared encoding and the container's own into the CDM
      — so `video.media-readable` catches a missing or unparseable file (`VIDEO.MEDIA_MISSING` /
      `VIDEO.MEDIA_UNREADABLE`) and `video.media-conformance` catches the video/data desync and the
      re-export drift (`VIDEO.FRAME_COUNT_MISMATCH` / `RESOLUTION_MISMATCH` / `CODEC_MISMATCH` /
      `FPS_MISMATCH`). Per-frame decode analysis stays out of scope by design. An aggregated video
      layout, where no file can be attributed to an episode, is reported as unmapped rather than
      guessed at.
- [x] Provenance-completeness checks (presence + internal consistency).
- [x] Each check ships ID + documented risk + remedy.

## M5 — Provenance & Croissant
- [x] Provenance model + known/asserted/unknown classification (in `cdm.rs`).
- [~] Extract provenance from LeRobot v3 and MCAP sources. (MCAP now extracts source_format, the
      header library/profile, producer-written Metadata records — with well-known keys mapped to
      typed provenance — and Attachment summaries incl. calibration. LeRobot extracts robot_type as
      a sensor element and the SPDX license from the dataset card's (`README.md`) YAML frontmatter;
      richer LeRobot-side extraction remains a follow-up.)
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
- [x] Python bindings (`veridex-py`, pyo3/abi3) exposing the full operation set (10 functions: `check`, `content_hash`, `inspect`, `catalog`, `certify`, `verify`, `provenance`, `diff`, `keygen`, `version`);
      both front-ends call one shared `veridex_core` pipeline so parity is by construction.
      **Parity test** (`crates/veridex-py/tests/test_parity.py`) asserts CLI and Python produce
      byte-identical `check` and `inspect` output — run in CI (maturin build + pytest). The
      `veridex-data` wheel builds with maturin.

## M9 — Proof & anointer prep
- [ ] End-to-end demo on a real LeRobot v3 Hub dataset and a real MCAP recording. (Synthetic
      end-to-end demos work today for **both** formats via `examples/make_demo_mcap` and
      `examples/make_demo_lerobot` + `veridex check`; real Hub datasets need network access.)
- [~] Reproduce detection of a synthetic cross-stream skew (done: the demo MCAP triggers
      `TEMPORAL.CLOCK_SKEW` + `TEMPORAL.END_OFFSET`, and `make_demo_mcap -- <out> late-start` triggers
      `TEMPORAL.START_OFFSET`) and a corrupted episode boundary (covered by unit tests). A runnable
      structural-corruption fixture now exists: `make_demo_lerobot -- <dir> truncated` writes a
      cut-short export that `veridex check` flags as `STRUCTURAL.FRAME_COUNT_MISMATCH`, and
      `make_demo_lerobot -- <dir> short-episode` writes a five-episode dataset with one truncated
      capture that `veridex check` flags as `TEMPORAL.EPISODE_DURATION_OUTLIER`. The
      boundary-specific fixture now exists too: `make_demo_lerobot -- <dir> boundary` writes a
      two-episode dataset whose `meta/episodes.jsonl` declares the wrong `length` for one episode
      (the lerobot#4143 corrupted-cumulative-length class), which `veridex check` flags as
      `STRUCTURAL.EPISODE_BOUNDARY` — reproduced end-to-end through the real adapter, not just unit
      tests.
- [x] Draft the upstream proposal (LeRobot CI/Hub quality-and-provenance gate) citing lerobot#4143
      — [docs/adoption-lerobot-ci-gate.md](../../../docs/adoption-lerobot-ci-gate.md).
- [~] Quickstart docs written (README). Publishing `veridex-data` to PyPI and the crates to
      crates.io is a release step (not done here; needs registry credentials).
