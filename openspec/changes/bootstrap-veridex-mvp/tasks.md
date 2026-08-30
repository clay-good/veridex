# Tasks — Veridex v0.1 MVP

Ordered so each milestone is demoable. No code is written until this change is approved; this is
the build plan.

## M0 — Scaffolding
- [x] Create the Rust workspace: `veridex-core`, `veridex-cli`. (`veridex-py` pyo3/maturin: deferred to M8.)
- [x] Wire CI: build, test, clippy/fmt gates (`.github/workflows/ci.yml`, `-D warnings`);
      `#![forbid(unsafe_code)]` on `veridex-core`.
- [x] Decide crypto substrate sharing with Invariant (shared crate vs. mirrored module); record it.
      Decided: **mirrored module**, and a raw domain-separated Ed25519 detached signature rather than
      a COSE or JWS envelope. Recorded as design D6a with the consequences accepted; the specs,
      `project.md`, and the README now describe what ships instead of claiming COSE/JWS.

## M1 — Canonical Dataset Model
- [x] Define CDM types (Dataset/Episode/Stream/Frame/Provenance/Label) per design D2.
- [x] Implement CDM canonicalization + content hashing (D5); property-test determinism.
- [x] Define the adapter trait (populate CDM; declare supported versions; record unmapped fields).

## M2 — Ingestion adapters (the neutrality proof)
- [x] LeRobot adapter → CDM (features→streams, Parquet `timestamp`→frame ts, `episode_index`
      grouping, fps→rate, robot_type→provenance). Reads only timestamps/structure, not payloads.
      Task strings are resolved (`task_index` + `meta/tasks.jsonl` → `episode.task`, falling back to
      the task each `meta/episodes.jsonl` line states). Video *files* are resolved and their
      container headers read into `Stream.media`; per-frame pixel decoding remains deliberately out
      of scope. **v2.0/2.1 as well as v3.0**: v2 writes one Parquet and one MP4 per episode where v3
      packs many into each, but the episode a row belongs to is the `episode_index` column either
      way and the metadata files are the same — the one real difference is v2.1's per-episode
      statistics in `meta/episodes_stats.jsonl`, which are read in the full, metadata-only and
      remote paths alike.
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
- [x] HDF5 adapter → CDM (group of arrays→episode, array→stream, first dimension→frames, attributes
      →metadata/provenance/declared counts). The container is parsed directly with no libhdf5
      dependency: superblocks v0–v3, object headers v1/v2 with continuation chunks, old-style
      symbol-table groups and new-style link messages, compact/contiguous/chunked storage, global-heap
      variable-length strings, and the deflate, shuffle, and fletcher32 filters. HDF5 records no
      clock, so the timeline is the step index on `hdf5-step-index` unless the file both stores a
      timestamp array and declares its `units`; a structure the reader does not implement is refused
      by name rather than read as absent. Tested against real `h5py` output committed as fixtures.
- [x] Zarr v2 adapter → CDM (the Diffusion Policy / UMI replay buffer: `data/*` arrays sliced at
      `meta/episode_ends`, plus the group-of-arrays layouts). Chunk files are read directly with the
      zlib, gzip, zstd, lz4, and blosc (lz4/zstd/zlib + byte shuffle) codecs; an encoding this reader
      cannot apply — blosclz, snappy, the bit shuffle, Fortran order, a filter, a v3 store — is
      refused by name rather than mis-decoded, because a wrongly-decoded array is plausible numbers
      rather than an error. Boundaries that run backwards or past the arrays they index are refused;
      rows past the last boundary are disclosed. Tested against real `zarr`/`numcodecs` stores.
- [~] Streaming/larger-than-memory ingestion. **Remote (Hub) metadata-only ingestion is in**
      (`veridex check hf://org/name --metadata-only`, and the same from Python): `crate::remote`
      fetches a fixed list of manifest paths over HTTPS to allowlisted Hub hosts, sends no
      credential, caps each response and the total, host-checks every redirect, stages the manifest
      in a temporary directory and hands it to the ordinary local adapter — so a remote and a local
      read of one manifest are the same code on the same bytes. The dataset is identified by its
      repository (`org/name`), and the run carries every metadata-only refusal. The socket sits
      behind a `remote` cargo feature (on for the binary and the Python package, off for library
      users) and everything above it behind a `FetchFile` trait, so the whole path is tested against
      a fake Hub with no network. Downloading a remote dataset's *data* is refused by name; Veridex
      validates rather than downloads. **Sampled
      ingestion is in** (`--sample-episodes` / `--sample-fraction` / `--sample-seed`, and the same
      arguments from Python): the LeRobot adapter resolves the draw from the declared episode set
      before reading any Parquet, so a skipped episode is never hashed, never accumulated into the
      statistics, and never charged to the frame budget — the bounded way to check a dataset larger
      than the budget allows. RLDS/TFDS resolves it the same way from the split `shardLengths`, and
      an unselected record is framed, its length prefix verified, and then seeked past without its
      payload being read — so drawing a few episodes from a 900 MB shard costs a few episodes of I/O,
      at the stated cost that a sampled run does not attest what it skipped. Single-episode formats (MCAP,
      CAN+DBC, MF4) refuse a sample rather than returning everything. Coverage rides in the verdict (and its hash), every report states it, and
      `certify` refuses a partial run.
      **Metadata-only ingestion is in too** (`--metadata-only`, `veridex.check(..., metadata_only=True)`):
      the LeRobot adapter builds the CDM from `meta/` alone — the declared episode set and per-episode
      lengths, every feature's dtype/shape/rate, `meta/stats.json`, the dataset card's license — and
      opens no Parquet or video file. Every episode therefore carries zero frames *by request*, so the
      engine hands each check a `CheckContext { frames_read: false }` and the frame-dependent arms
      abstain instead of reading that absence as a defect (`FRAME_COUNT_MISMATCH`, the declared-length
      arm of `EPISODE_BOUNDARY`, and `EMPTY_STREAM`/`SINGLE_FRAME_STREAM` would otherwise fire on every
      sound dataset). What remains live is the whole stored-statistics family, the provenance family,
      shape/stream-presence consistency, and — only when `meta/episodes.jsonl` makes it an independent
      second assertion — the declared episode count; derived from `total_episodes` alone the comparison
      could not fail, so it is **withheld and reported as omitted** rather than passed. Coverage rides
      in the verdict as `metadata_only` (and in its hash), every report prints it, and `certify` refuses
      it. An adapter must claim `Adapter::supports_metadata_only()` to be handed the option, so the six
      formats that keep their structure inside the container are refused by name, not silently degraded.
      True streaming (never materializing the whole CDM) remains a follow-up;
      a remote source that is not a readable Hub manifest is **refused** by name rather than silently ignored.
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
      a sensor element and, from the dataset card's (`README.md`) YAML frontmatter, the SPDX license,
      `source_datasets` as `upstream` and `annotations_creators` as `annotator` — the Hub's two
      "none" values (`original`, `no-annotation`) deliberately excluded, since they answer the
      question the same way a missing element does. Still open on the LeRobot side: nothing in
      `meta/` names a clock or a calibration, so those two stay honestly unknown.)
- [x] Emit Croissant (JSON-LD) + minimal W3C PROV lineage from the CDM (honest classes, no
      fabrication). Wired to `veridex provenance --emit croissant|prov`.

## M6 — Trust certificate
- [x] Certificate schema (content, content-hash binding, rubric_version, issuance metadata) —
      `veridex.certificate/1`, records checks run + categories skipped + provenance coverage.
- [x] v1 scoring rubric (D7): deterministic score + grade; ship rubric doc (docs/rubric-v1.md).
      Provenance coverage (known/asserted/unknown) is a separate 30% axis so a clean check score
      can't mask missing provenance.
- [x] Ed25519 signing (detached signature over domain-separated canonical bytes, D6/D6a); offline
      `verify`; tamper + transplant + wrong-issuer rejection tests. Not a JWS or COSE envelope —
      an envelope format stays a possible later refinement, and would be a new `algorithm` value
      plus a schema bump rather than a rewrite.

## M7 — Reporting (MVP)
- [x] Terminal report + rollup summaries (dataset/episode/stream, worst episodes first).
- [x] Versioned JSON output (`veridex.report/1`).

## M8 — CLI + Python parity
- [x] `veridex check | inspect | checks | certify | verify | provenance | keygen | diff | watch`
      implemented end-to-end (`checks` lists the built-in catalog as text or `--json`; `watch`
      re-validates a dataset as it is recorded, printing the delta after the first pass and bounded
      by `--iterations` so it is a CI step as well as an interactive one).
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
- [~] End-to-end demo on a real LeRobot v3 Hub dataset and a real MCAP recording. **The LeRobot half
      is done**, against `lerobot/svla_so101_pickplace` pulled from the Hub (50 episodes, 4 streams
      each, 2 video features, 86 MB): `inspect` maps it to 50 episodes without a warning, `check`
      returns `PASS (warnings)` / trust 74 (C) / `data 100 · provenance 16%`, and `certify` +
      `verify` round-trip offline. The content hash was identical across a relative path, an
      absolute path, and a run from *inside* the dataset directory; editing one field of
      `meta/info.json` moved it and `verify` refused the certificate.

      The run is notable for what it *declines* to say: the dataset ships one shared `.mp4` per
      camera rather than one per episode, so the video checks abstain by name
      (`VIDEO.MEDIA_UNATTRIBUTED`) instead of passing, and the video streams carry no per-frame
      fingerprints, so `STRUCTURAL.UNFINGERPRINTED_CONTENT` records that the duplicate-episode and
      stuck-stream checks had nothing to compare. Both are reported as unmeasured, not as clean —
      which is the behavior the abstention work exists to produce, confirmed here on data nobody
      wrote for us.

      The **remote** path is now proven against that same repository too, from the Hub with nothing
      downloaded: `veridex check hf://lerobot/svla_so101_pickplace --metadata-only` returns
      `PASS` / trust 79 (C) / `data 100 · provenance 33%` over 50 declared episodes and 4 streams
      each, in about a second. Running it is what found the one defect a fake Hub could not: the
      real Hub answers a manifest read with a *relative* redirect (`/api/resolve-cache/…`), which
      the host allowlist refused because it was not an absolute URL.

      Still open: a **real MCAP recording** (the synthetic `veridex-demo` MCAP rig covers the
      format end-to-end, including the `av` five-sensor variant).
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
