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
  real datasets; the omission is reported honestly when no `meta/tasks.jsonl` is present. It reads
  the SPDX license from the dataset card's (`README.md`) YAML frontmatter — where LeRobot datasets
  actually record it — so a licensed dataset no longer trips `PROVENANCE.MISSING_LICENSE`. It also
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
- **Checks catalog** — 32 checks across six families (the sixth, **autonomy**, is described in its own
  entries below), each finding carrying a training **risk** and a **remedy** and located to the exact
  episode / stream / frame:
  - **Structural** — episode-boundary integrity (the lerobot#4143 class: a per-episode declared
    `length` from `meta/episodes.jsonl` that disagrees with the frames ingested, duplicate episode
    indices, or inverted `start_ts`/`end_ts`), degenerate
    episodes/streams (including a zero-episode dataset), episode-index continuity, declared-vs-actual
    episode/frame counts (truncated exports), cross-episode dtype/shape and stream-presence
    consistency, exact-duplicate episodes (`STRUCTURAL.DUPLICATE_EPISODE`, content-hash-gated so it
    never mis-flags same-length episodes), and a frozen-camera check (`STRUCTURAL.STUCK_STREAM`).
  - **Temporal** — monotonicity, declared-rate validity (`TEMPORAL.INVALID_RATE`), rate conformance,
    gaps, jitter, the headline cross-stream `TEMPORAL.CLOCK_SKEW`, shared-clock start/end offsets,
    cross-episode rate consistency (`TEMPORAL.RATE_INCONSISTENT`), and episode-duration outliers.
  - **Statistical** — stored-stats range and sanity (inverted range, non-finite, negative or
    Popoviciu-implausible std, mean-out-of-range, integer-dtype range, degeneracy). Where the adapter
    reads feature values (LeRobot), Veridex recomputes statistics from the actual cells and adds four
    data-facing checks: `STATISTICAL.STATS_STALE` flags a stored `meta/stats.json` whose range doesn't
    bound the data (stale stats poison normalization); `STATISTICAL.SATURATED` flags a clamped actuator
    whose values sit **exactly** pinned at one rail (exact-equality is the signal, so a noisy sensor is
    never mis-flagged); `STATISTICAL.OUTLIER` flags an extreme many σ from the mean, provably a rare
    spike by Chebyshev's inequality (≤1% of samples at 10σ); and `STATISTICAL.NON_FINITE_OBSERVED`
    flags a NaN or ±infinity in the cells that a clean or absent `stats.json` hides — a single one
    propagates to a NaN loss and silently kills a training run. All four scan **every dimension** of a
    multi-DoF feature and name the offending joint, so a stale stat, saturated gripper, spike, or NaN
    buried in element 6 of a 7-DoF `action` is caught, not just element 0.
  - **Semantic** — task-string quality and stream-key clarity (an exact-duplicate key is an error, a
    case/whitespace collision a warning); and language-annotation integrity
    (`SEMANTIC.ANNOTATION_UNALIGNED` / `SEMANTIC.ANNOTATION_CONFLICT` / `SEMANTIC.EMPTY_ANNOTATION`):
    timestamped language
    annotations are verified — in span, unique per instant, non-empty — never written or modified. The
    LeRobot adapter surfaces mid-episode `task_index` changes as timestamped `language` labels
    (single-task episodes carry none), so the check runs on real multi-task datasets.
  - **Provenance-completeness** — presence, internal consistency, and placeholder detection (a
    `license` of `"unknown"` is present in form but empty in substance, so it isn't counted as real).

  The full catalog — every check, its finding codes, default severity, and exactly when it fires —
  lives in [docs/checks.md](docs/checks.md), guarded against drift in both directions by tests.
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
- **Python bindings** (`import veridex`) exposing `check` / `content_hash` / `inspect` / `catalog` / `diff` / `keygen` / `certify` / `verify` /
  `provenance` / `version`, calling the same core pipeline as the CLI, with passing CLI ⇄ Python
  parity tests over `check`, `inspect`, `catalog`, `provenance`, `diff`, and `certify`/`verify` —
  each shares a single core render helper (`render_catalog_json`, `render_provenance`,
  `render_diff_json`) with the CLI, and because Ed25519 signing is deterministic the certificate a
  given key issues is byte-identical across CLI and Python.
- **Configuration** — a `veridex.toml` (auto-discovered, or `--config`) that selects categories,
  disables checks, overrides per-check severities, and sets the failure threshold and minimum trust
  score (`min_score`, overridable by `--min-score`); the effective config is recorded in every
  verdict. Unknown TOML keys are rejected, and a check id that names no real check (a typo in
  `disabled_checks`, `only_checks`, or a `severity_overrides` key) is a hard error rather than a
  silent no-op. A `[tolerances]` table tunes the temporal and statistical checks' numeric thresholds
  (`clock_skew_ms`, `start_offset_ms`, `end_offset_ms`, `rate_deviation`, `gap_factor`, `jitter_cv`,
  `episode_duration_factor`, `saturation_fraction`, `saturation_min_samples`, `outlier_z`,
  `sequence_drop_fraction`, `ego_max_speed_mps` — see
  [docs/veridex.toml.example](docs/veridex.toml.example)); each is optional, validated
  (finite, non-negative; positive `gap_factor`), and falls back to the check's default. The
  tolerances the run used are recorded in the verdict's effective config, so a result is fully
  reproducible from what it reports.
- **Runnable demos** — `examples/make_demo_mcap` (synthetic cross-stream clock skew) and
  `examples/make_demo_lerobot` (a LeRobot v3 dataset with an out-of-order timestamp, a
  `truncated` variant whose manifest over-declares its frame count → `STRUCTURAL.FRAME_COUNT_MISMATCH`,
  a `boundary` variant whose `meta/episodes.jsonl` declares the wrong length for one episode → the
  lerobot#4143 `STRUCTURAL.EPISODE_BOUNDARY`, and a `jitter` variant whose one episode has an
  irregular inter-frame spacing → `TEMPORAL.JITTER`; plus `short-episode`, `duplicate`, `saturated`,
  `spike`, `nan`, and `multi-joint` variants — the full list is in the README), each with a `clean`
  variant, so `veridex check` has something to find end-to-end in **both** formats.
- **CI** — GitHub Actions running fmt, clippy (`-D warnings`), and the full test suite, plus a
  Python job that builds the extension with maturin and runs the CLI ⇄ Python parity test on every
  push. The `veridex` binary has its own integration tests (`crates/veridex-cli/tests/cli.rs`)
  asserting command dispatch, argument validation, and the CI exit-code contract (0 · 10 · 20 · 2).
- **Autonomy sensor-rig CDM extensions (A0)** — the first slice of `autonomy-sensor-data`: the CDM now
  represents a multi-sensor rig as *extensions* of the existing model, not a fork (design A1). New
  modalities (`point-cloud`, `imu`, `gnss`, `can-signal`, `ego-pose`); a declared per-point field
  layout on a stream (`Stream.point_fields`); a rig `Calibration` on the dataset — the coordinate-frame
  transform (TF) tree plus per-camera intrinsics, each with a `valid_from`/`valid_to` validity range so
  a recalibration mid-log is representable; and a per-episode ego-vehicle trajectory (`Episode.ego_poses`).
  All are optional and absent for manipulation datasets, whose CDM and verdicts are unchanged. Every
  content-bearing field is bound into the content hash — the TF tree, intrinsics, and ego trajectory
  canonicalized order-independently, the point-field layout order-significant — with
  `CANONICAL_VERSION` bumped 2 → 3. The spatial/sequence checks that read these are still to come (A2).
- **MCAP autonomy message classification (A1, first slice)** — the MCAP adapter now recognizes the
  common ROS/ROS 2 autonomy message types by schema name and maps them to the new rig modalities
  (`PointCloud2`/`LaserScan` → point-cloud, `Imu` → imu, `NavSatFix` → gnss, `Odometry` → ego-pose,
  CAN frames → can-signal), instead of lumping them into `scalar-state`. So an AV rig log's streams
  are typed correctly at ingest. The message **bodies** are now CDR-decoded too: a hand-rolled,
  bounds-checked ROS 2 CDR reader (`adapter/cdr.rs` — no new dependency, `#![forbid(unsafe_code)]`,
  declines malformed/big-endian bodies without panicking) reads each AV message's structural *header*
  (never the bulk point/pixel payload) to populate the rig CDM: `PointCloud2` → `Stream.point_fields`,
  `CameraInfo` → camera intrinsics, `TFMessage` → the transform tree, `Odometry` → the ego trajectory.
  Proven end-to-end through the adapter and by per-decoder unit tests. A new `make_demo_mcap -- <out> av` variant
  writes a five-sensor rig (camera, LiDAR, IMU, GNSS, ego-odometry) with a single-sensor sync drift
  injected on the IMU; `veridex inspect` shows the typed rig and `veridex check` flags the drift.
- **`AUTONOMY.RIG_SYNC` — rig-wide time sync (A2)** — the first autonomy check and a new `autonomy`
  check family. It generalizes the pairwise `TEMPORAL.CLOCK_SKEW` to N sensors: on an episode that is
  a sensor rig (≥3 AV-native rig sensors), it reports the rig-wide sync spread — the widest sensor
  span minus the tightest — as a **single** error naming the tightest- and widest-spanning sensors,
  instead of O(n²) pairwise findings. On a rig it *supersedes* `CLOCK_SKEW` (which now skips rig
  episodes), so a drifting sensor no longer floods the report; a manipulation dataset has no rig
  sensors, so it never enters rig mode and `CLOCK_SKEW` behaves exactly as before. It shares the
  `clock_skew_ms` tolerance (same semantics, one knob). On the `av` demo this turns four pairwise
  `CLOCK_SKEW` errors into one clear `AUTONOMY.RIG_SYNC` finding.
- **`AUTONOMY.SEQUENCE_COMPLETE` — rig sequence completeness (A2)** — flags a rig sensor that quietly
  drops an aggregate fraction of its frames (default > 5%): its observed frame count against the count
  its own median inter-frame cadence implies over its active span. It catches many small drops that
  `TEMPORAL.GAP` (a single oversized interval) and `TEMPORAL.RATE` (which needs a declared rate MCAP
  rigs lack) both miss. Rig-only, median-baseline (robust to the drops it hunts, no declared rate or
  shared clock needed), and skips streams with too few frames for a stable estimate. Proven end-to-end
  through the MCAP adapter (`a_frame_dropping_sensor_is_flagged_incomplete_end_to_end`).
- **`AUTONOMY.EGO_POSE_CONTINUITY` — ego trajectory continuity (A2)** — flags an episode whose ego
  trajectory (`Episode.ego_poses`, decoded from Odometry) contains a step whose implied speed
  (distance / elapsed time) exceeds the plausible maximum (default 100 m/s ≈ 360 km/h): a GPS glitch,
  localization reset, or stitched log that teleports the ego frame, so every later sensor observation
  registers against a wrong world pose. Reports the worst jump and how many occurred. Runs end-to-end
  on the CDR-decoded ego trajectory (`a_teleporting_ego_trajectory_is_flagged_end_to_end`).
- **`AUTONOMY.CALIBRATION_INCOMPLETE` — rig calibration completeness (A2)** — the principle-respecting
  form of the LiDAR-camera reprojection check. Veridex never decodes the bulk point/pixel payload, so
  it cannot reproject actual points; instead it verifies the calibration needed to *is present and
  coherent*. On a rig with spatial sensors it flags: no transform (TF) tree at all; a TF tree split
  into disconnected components (sensors that can't be related, found by connected-components over the
  frame graph); or cameras with no intrinsics. Runs on the CDR-decoded TF tree + intrinsics, proven
  end-to-end (`a_rig_without_a_transform_tree_is_flagged_incomplete_end_to_end`).
- **`world-model-ready` profile + readiness certificate (A4)** — a named policy profile
  (`crate::profile`, applied with `veridex certify --profile world-model-ready`) that tightens
  cross-sensor sync to 20 ms and bundles the four autonomy criteria a world-model set needs. The
  certificate gains a signed `readiness` block reporting per-criterion pass/fail and the threshold
  each attests, plus `applicable` (is the dataset a sensor rig) and an overall `ready` flag. Honest by
  construction: a non-rig is `N/A`, never a vacuous pass, and the report claims nothing beyond the
  criteria listed. The block is signed like every other certificate field (verifies offline). See
  [docs/profiles.md](../docs/profiles.md).
- **Readiness certificates are readable offline, from both surfaces (A5)** — `veridex verify` now
  reports what a certificate *attests*, not merely that its signature checks out: the CDM hash it is
  bound to, the trust score and provenance coverage, and — for a certificate issued with
  `--profile` — the profile verdict and every readiness criterion. `--json` emits the same facts as a
  machine-readable summary, with the signed `readiness` block verbatim. Everything printed comes out
  of the signed document, so a doctored readiness block fails verification instead of being read back
  (covered by a test that flips `ready` to true and asserts the certificate no longer verifies).
  Python reaches parity: `veridex.certify(..., profile="world-model-ready")` issues the identical
  profiled certificate (byte-for-byte with the CLI, checked in the parity suite) and
  `veridex.verify(...)` returns the identical summary. Certify and verify share one core renderer, so
  the two surfaces can't drift.
- **Autonomy provenance lineage (A3)** — the MCAP adapter now extracts the sensor-rig lineage a
  producer records in Metadata: firmware, calibration session, platform/vehicle and drive/run IDs,
  capture region, HD-map version, and — acute for public-road capture — redaction and consent status.
  Each is classified `known` (read from the source bytes) and surfaced in both provenance emits: the
  Croissant `veridex:provenance` list and the PROV entity as `veridex:` properties. Extracted without
  changing the coverage denominator, so a manipulation dataset's coverage score is unchanged. The `av`
  demo carries the lineage end-to-end.
- **Scenario-dimension coverage (A3/A6)** — a **descriptive** report of the conditions a dataset was
  recorded under. `crate::scenario` recognizes scenario tags (weather, time-of-day, environment,
  lighting, season, traffic) from episode labels and reports each dimension's value distribution across
  episodes, marking a **sparse** cell (a value in under 10% of covered episodes). It is descriptive by
  design (A6): never a finding, never a score change, never a required balance — the target
  distribution is the training team's call. The MCAP adapter extracts recognized scenario metadata
  keys into episode labels, and `veridex inspect` shows a "scenario coverage" section.
- **Scenario / map / simulation references (A1)** — Veridex now records *what a log was recorded or
  replayed against*: the OpenSCENARIO scenario, the OpenDRIVE road network / HD map, the OSI version,
  and the simulator or replay tool. `crate::simref` recognizes the well-known metadata spellings and
  the MCAP adapter maps them to `scenario_ref` / `map_ref` / `osi_version` / `simulator` provenance,
  each `known`. Versions are extracted, never guessed: when the reference names a sidecar that really
  sits next to the log, the ASAM revision declared in that file's own header (`revMajor`/`revMinor`,
  the same shape in `.xosc` and `.xodr`) is read from its bytes; otherwise the version is whatever
  dotted version the recorded value itself carries, and a bare file name (`town10.xodr`) yields no
  version rather than a wrong one. A reference pointing outside the dataset (absolute, or with `..`)
  is recorded but never followed. An explicitly recorded `map_version` always wins over an OpenDRIVE
  header revision. References travel with both provenance emits and show in `veridex inspect` as a
  "scenario & map references" section; the `av` demo carries them. Reading the reference is the scope
  — Veridex does not parse scenario semantics, road geometry, or ground truth.

- **ASAM MDF 4.x (MF4) adapter** — the dominant automotive measurement format, read into the CDM
  (`adapter/mdf4.rs`, no new dependency). Walks the block graph (`##HD` → `##DG` → `##CG` → `##CN`),
  takes each channel group's **time master** as the timeline, and emits one stream per measured
  channel with a frame per record, applying identity and linear (`##CC` type 1) conversions to get
  physical values. Integer and float channels decode in both byte orders; values are fingerprinted
  into the CDM content hash, so an altered measurement no longer hashes the same. The writing program
  from the identification block becomes `recorder` provenance, and a non-4.x file is rejected as an
  unsupported version rather than mis-parsed. Everything outside that core — compressed (`##DZ`) or
  listed (`##DL`) data, unsorted data groups, bit-packed or non-numeric channels, other conversion
  types, an over-declared cycle count — is reported as an `unmapped` field and contributes no frames,
  so a reader always knows what the verdict covered. Every block read is bounds-checked and every
  chain walk is loop-guarded: a truncated or byte-corrupted file yields an error or an empty result,
  never a panic (tested against file prefixes and corrupted bytes). Autodetected by the registry from
  the file's own identification block, not its extension. Fixtures are assembled byte by byte, so the
  adapter is tested against the on-disk layout rather than a writer sharing its assumptions.
- **CAN + DBC adapter** — a new AV-native ingestion path (`adapter/candbc.rs`). It ingests a directory
  holding a `.dbc` signal database and one or more candump ASCII logs (`.log`/`.asc`), parses the DBC
  (`BO_` messages, `SG_` signals), and decodes each CAN frame's signals in **both DBC byte orders** —
  little-endian (Intel, `@1`) and big-endian (Motorola, `@0`, walking the sawtooth bit numbering from
  the signal's most-significant bit) — applying the factor/offset and sign-extension, into one
  `CanSignal` stream per `Message.Signal`. A signal whose bits fall outside the frame is declined
  rather than truncated. DBC-coverage gaps (CAN ids seen in the log with no DBC definition) are
  surfaced as `unmapped` fields. Decoded values are fingerprinted into the CDM content hash.
  Autodetected by the registry (a directory with a `.dbc`). Dependency-free text parsing; unit,
  integration, and CLI end-to-end tests — including a Motorola signal laid over a byte-swapped copy
  of its Intel twin, which must decode to identical samples. Recomputed signal stats (to feed the
  statistical checks) remain a follow-up.
- **Sampled ingestion** — `check` / `inspect` can validate a subset of a dataset's episodes:
  `--sample-episodes <n>` takes the first *n* by index, and `--sample-fraction <f> [--sample-seed
  <s>]` draws a deterministic fraction (episodes ordered by `SHA-256(seed, index)`, so the same seed
  always draws the same episodes and a positive fraction never draws none). The same requests are
  available from Python as `sample_episodes=` / `sample_fraction=` / `sample_seed=` on
  `veridex.check()` and `veridex.inspect()`.

  Sampling is resolved from the declared episode set (`meta/episodes.jsonl`, else `info.json`'s
  `total_episodes`) *before* any Parquet is read, so an unselected episode is never fingerprinted,
  never accumulated into the recomputed statistics, and never charged to the frame budget — a
  sample of a dataset that exceeds the budget succeeds where the full ingest is refused. Only
  LeRobot has an episode axis; MCAP, CAN+DBC, and MF4 ingest a recording as a single episode and
  **refuse** a sampling request rather than returning everything labelled as a sample.

  A sampled run cannot be mistaken for a full one. The verdict carries a `coverage` field, digested
  into `result_content_hash`; the terminal, JSON, and HTML reports all state the sample and the
  episode count; `veridex inspect` says so next to the hash it produced; and `certify` **refuses**
  to issue a certificate from a partial run, because a certificate is a claim about a dataset and
  the episodes it never read are exactly where the problem would be. `verify` and `provenance`
  reject the sampling flags outright. Under a sample the adapter also drops the dataset-level
  declared totals from the CDM, so a deliberate subset is never reported as a truncated export —
  while the per-episode declared lengths (the lerobot#4143 check) still apply to the episodes that
  *were* read.

### Fixed

- **One shared timeline produced a finding per stream.** Several streams in an episode routinely share
  a timeline — an MF4 channel group samples every channel on one raster, a CAN message decodes into
  many signals off the same frames — so `TEMPORAL.GAP` and `TEMPORAL.JITTER` re-reported one root
  cause once per stream. A normal 8-channel event-driven log produced 32 warnings for 4 real facts,
  deducting enough to floor the data score at 0. The timeline checks now report once and name how
  many streams share it.
- **`AUTONOMY.SEQUENCE_COMPLETE` called a complete event-driven log 88% dropped.** Its baseline is the
  frame count a stream's own median cadence implies over its span — meaningless for a change-triggered
  signal that arrives in bursts with long idles, which never aimed at a cadence. It now abstains when
  the intervals are far from uniform (that shape is `TEMPORAL.JITTER`'s to report); a genuinely
  dropping steady stream stays well inside the bound.
- **A few hundred KB of crafted input could exhaust memory.** Every adapter materializes
  *streams × samples* frames and both factors come from the file — a CAN log's signals-per-id against
  its frame count, an MF4 group's channels against its records, a LeRobot `info.json`'s declared
  features (which need no matching Parquet column) against its rows. Measured: 344 KB of crafted CAN
  produced 6.4M frames and 900 MB, doubling with each doubling of input, so a ~10 MB file projects to
  tens of GB and an OOM-killed CI gate. Ingestion now charges a **frame budget** (default 20M, well
  above real datasets — a one-hour ten-sensor 100 Hz rig is 3.6M) *before* allocating, and refuses
  with a clear error naming the limit rather than being killed. `--max-frames <n>` raises it;
  `--max-frames 0` removes it.
- **Python had no SARIF or HTML binding**, so the two CI-facing render formats were CLI-only despite
  the stated parity. `veridex.check_sarif` and `veridex.check_html` now expose them through the same
  shared render helpers, so their output is byte-identical to `--sarif` and `--html`.
- **Commands accepted gate flags they could not honor.** `inspect --min-score 90` looked like a gate
  and was none, and `--fail-on` was equally inert on `inspect`, `provenance`, and `verify`. Each now
  refuses the flag by name rather than ignoring it.
- **The `av` demo's ego trajectory never decoded, so the flagship readiness demo said N/A.** Its
  Odometry topic carried an 8-byte dummy payload like every other sensor, so `Episode.ego_poses` came
  back empty — and the `world-model-ready` profile, which applies only to a rig carrying a perception
  sensor *and* an ego trajectory, correctly abstained. The generator now writes a real CDR Odometry
  body (a ~10 m/s drive down +x), so the demo exercises ego-pose decoding and prints the NOT READY
  report the quickstart documents. A test pins profile applicability in both directions.
- **`veridex diff` skipped flag validation, so a typo turned the CI gate off.** It scanned argv for
  the flags it recognized and dropped everything else, so `--fail-on-regresion` (one letter short)
  silently disabled the regression gate and exited 0 — the exact failure the shared parser exists to
  prevent. `diff` now goes through it, and unknown options are a tool error like everywhere else.
- **`veridex diff` read a wrong-shaped file as "no findings".** An empty `{}`, a truncated artifact,
  or a SARIF file passed by mistake produced "all resolved, no regression" and passed the gate.
  Both inputs must now carry a findings array, and a diff between reports bound to different dataset
  content says so.
- **`check --profile` was parsed and thrown away.** The run silently used the default, looser
  thresholds while the user believed the profile's applied, and an unknown profile name passed
  without a word. `check` now resolves the profile, applies its tolerances, and rejects an unknown
  name — matching `certify`.
- **`certify --config` was accepted and ignored**, including its validation. A signed certificate
  could disagree with the `check` just run on the same data in the same directory (`check` also
  auto-discovers `veridex.toml`; `certify` did not), and a config naming a nonexistent check was
  silently accepted here while `check` rejected it. `certify` now loads, validates, and applies the
  same configuration, with a profile's tolerances taking precedence.
- **A crashed check rendered as a clean pass in HTML and SARIF.** Only the terminal report listed
  `errored_checks`, so a CI job gating on SARIF or a human reading the shareable HTML artifact saw
  green while a check never ran. HTML gains an "Errored checks" section, SARIF a
  `VERIDEX.CHECK_ERRORED` result per errored check. The HTML report now also discloses non-default
  tolerances, as the terminal one already did.
- **`verify --json` printed plain text on failure**, leaving a machine consumer nothing to parse.
- **`veridex --help` omitted four real flags**, including `--allow-any-issuer`, the documented way to
  skip issuer trust.
- **Python could not see a config, so it disagreed with the CLI.** `veridex.check` now takes
  `config=` (the contents of a `veridex.toml`), validated the same way; Python still never
  auto-discovers a config file, since an import should not pick up behavior from the working
  directory.
- **The LeRobot/Parquet path had no expansion bound at all.** Every row of a Parquet file was decoded
  into memory before the frame budget was charged, and the decompression budget was never consulted:
  a 50 KB zstd file measured **1.26 GB** resident and a 149 KB file **3.76 GB**, in both cases raising
  the budget error only after the memory was spent. Both budgets are now charged per record batch as
  it decodes, and the per-row cost is the larger of what `info.json` declares and what the Parquet
  actually holds — a manifest declaring zero features no longer rides a 50,000-column file for free.
- **A crafted MF4 block length could panic or be silently accepted.** The `at + length` containment
  check in the block-header reader used unchecked arithmetic on a file-declared `u64`: a header
  claiming `u64::MAX - 8` bytes panicked in debug (the mode the test suite runs in) and, in release,
  wrapped into a header that passed validation — so a corrupt file was accepted as a clean, signable,
  zero-episode dataset instead of being refused.
- **Duplicate MF4 channel names were disambiguated quadratically.** Each collision restarted its
  suffix counter at zero and re-probed from scratch, so *N* identically-named channels cost O(N²):
  16,000 of them in a 1.3 MB file measured 18 seconds, and a 100 MB file extrapolated to hours of CPU
  inside a CI gate. Each collision is now one probe.
- **A certificate could verify against a dataset it was not issued for.** `declared_frame_count` was
  deliberately left out of the content hash as an assertion *about* content rather than content — but
  `structural.episode-boundary` reads it and fails on it, so two datasets differing only there (one
  passing, one failing) hashed identically and the clean one's certificate verified against the
  corrupt one. It is now encoded; `CANONICAL_VERSION` is **4**.
- **The hash depended on input order for exactly the datasets Veridex exists to catch.** Episodes were
  ordered by `index` alone and streams by `name` alone — neither a total order, and duplicates of both
  are faults the catalog reports. A stable sort left ties in `Vec` order, so two datasets holding the
  same duplicate-index episodes in different orders produced different content hashes and different
  `result_content_hash`es. Both now break ties on the item's own canonical encoding (computed only for
  items that actually tie, so an ordinary dataset pays nothing). `canonicalize_order` also now sorts
  episode labels and the calibration transform/intrinsics sets, which the encoder already treated as
  sets — closing the gap before a reader resolves "the transform valid at time t" by first match.
- **A signed certificate had no canonical byte form.** Hex decoding and the algorithm check were
  case-insensitive, so uppercasing `signature`, `public_key`, or `algorithm` produced a different file
  that still verified. Verification now requires the canonical spelling, so a consumer that pins or
  de-duplicates certificates by file digest cannot be handed two files that both verify.
- **Every honest multi-rate rig was reported as clock-skewed.** `TEMPORAL.CLOCK_SKEW` and
  `AUTONOMY.RIG_SYNC` compare stream *spans*, but a stream observing a window at period `T` spans a
  whole number of `T`s — so two perfectly synchronized sensors at different rates differ by up to one
  period with no drift at all. The 50 ms tolerance was therefore smaller than the intrinsic bias of
  any sensor slower than 20 Hz: a zero-drift rig of 10 Hz LiDAR + 100 Hz IMU + 5 Hz GNSS was measured
  reporting a 70 ms "drift" (500 ms with a 1 Hz GNSS), and a 30 fps camera beside a 10 Hz state stream
  scored F. Both checks now widen the tolerance by the larger of the two streams' own sampling
  periods. A real 500 ms drift on a 10 Hz sensor is still flagged.
- **`AUTONOMY.SEQUENCE_COMPLETE` still called complete event-driven data dropped.** Dividing the span
  by the median cadence charges idle stretches as missing frames, and the interval-uniformity guard
  did not bound that (a stream of 40 x 80 ms and 10 x 200 ms intervals — every event present — sat
  under the guard and was reported ~23% dropped). It now counts the frames that gaps at *multiples* of
  the cadence actually swallowed, so an idle burst costs nothing and a steady sensor's real drops are
  still found.
- **One root cause could be deducted many times.** `TEMPORAL.NON_MONOTONIC` had no shared-timeline
  guard, so a single stuck timestamp on an 8-channel CAN group cost eight Errors and floored the data
  score; it now reports once per timeline and names the rest, as `TEMPORAL.GAP` and `TEMPORAL.JITTER`
  already did. `SEMANTIC.AMBIGUOUS_STREAM_KEY` and `SEMANTIC.DUPLICATE_STREAM_KEY` were emitted per
  episode, so one naming mistake across 50 episodes cost 100 warnings; a naming mistake is a property
  of the schema, so each collision is now reported once, naming the first episode it appears in.
- **A constant stream's float-noise `std` was either missed or called impossible.** `DEGENERATE`
  required `std == 0.0` exactly, and the Popoviciu tolerance scaled with the *range* rather than the
  magnitude of the values. A constant channel at 0.7 with a `std` of 1e-12 escaped entirely; at 1e-8 —
  what naive `E[x²] − E[x]²` cancellation produces at that magnitude — it became a
  `STATISTICAL.STD_IMPLAUSIBLE` **error**, as did a near-constant channel at 300.0 with an f32-computed
  `std`. Both now use one magnitude-scaled rounding tolerance. A genuinely impossible `std` is still an
  error.
- **`STATISTICAL.SATURATED` claimed a stream before testing it**, the same defect just fixed in
  `range-sanity`: a clean episode-0 copy of a stream masked a saturated one in a later episode, and
  the finding depended on episode order.
- **Three thresholds were unreachable from config.** `STATISTICAL.OUTLIER`'s sigma and the two
  autonomy tolerances (`AUTONOMY.SEQUENCE_COMPLETE`'s tolerated drop fraction,
  `AUTONOMY.EGO_POSE_CONTINUITY`'s maximum plausible speed) were hardcoded to their defaults while
  every other family's were tunable — so a rig with a legitimately faster platform, or a
  deliberately sparse sensor, had no way to say so. They are now `outlier_z`,
  `sequence_drop_fraction`, and `ego_max_speed_mps` under `[tolerances]`: validated on parse
  (a sigma at or below 1.0, a drop fraction outside `[0, 1)`, or a non-positive speed is rejected,
  not silently accepted), snapshotted into the signed effective config, and listed in the report's
  non-default-tolerances note.
- **A clean episode could mask a later episode's corrupt statistics.** `statistical.range-sanity`
  reports each stream once (stored stats are dataset-level), but it claimed the stream name *before*
  evaluating it — so only the first episode carrying a stream was ever examined. Exact today, wrong
  the moment an adapter attaches per-episode stats. It now claims the stream when it produces a
  finding, like its sibling checks: a clean episode 0 followed by a corrupt episode 1 is reported,
  and attributed to the episode it was found in. Findings still never scale with episode count.
- **The frame budget bounded frames, not the bytes they arrive in.** An MCAP chunk header declares how
  much it unpacks into, and nothing checked that figure: a few hundred bytes claiming 8 GiB of chunk
  contents sent the reader into an unbounded read loop, and a chunk full of oversized messages costs
  one frame each — cheap by the frame budget, ruinous in memory. Ingestion now also charges a
  **decompression budget**, sized at 100x the file's own size (with a 64 MiB floor) so it scales with
  genuinely large logs while refusing bomb-scale ratios. It is charged off the chunk headers *before*
  the file reaches the reader, and again against the message bytes that actually arrive, so a header
  that understates its expansion buys nothing. `--max-decompression-ratio <n>` raises it; `0` removes
  it.
- **A scenario/map version could be read from the wrong place and recorded as extracted.** The ASAM
  `revMajor`/`revMinor` scan searched the whole file for each attribute independently, so a templated
  `.xodr` whose comment or `description` mentioned `revMajor="0"` had that read as its declared
  version — class `known`, i.e. presented as read from the file's bytes. Both attributes are now read
  from the same header element, comments are skipped, and the element is walked as `name="value"`
  pairs, so a mention inside another attribute's value or a longer name ending in `revMajor` no
  longer matches. Empty values no longer yield the version `"."`, and a bare `name=` at a truncated
  buffer's end no longer abandons the scan.
- **Two datasets could share a content hash and disagree on the verdict.** The canonical encoder
  treats several collections as *sets* — the ego trajectory, dataset metadata, provenance records and
  their elements — but `canonicalize_order` sorted only episodes and streams, and some checks read
  those collections as sequences or by first match. Verified: the same six ego poses in two Vec orders
  hashed identically while one reported five 200 m/s teleports and the other passed; duplicate
  metadata keys and provenance records behaved the same way. Since a certificate binds the content
  hash, it could attest a hash that also matches a dataset that fails. `canonicalize_order` now sorts
  every collection the encoder canonicalizes, with the encoder's own sort keys so the two cannot
  drift, and a property test permutes all of them at once and asserts both the content hash and the
  verdict are unchanged.
- **Provenance emit could contradict itself across permutations.** Elements were sorted by `key`
  alone (ties left in Vec order) and mapped fields like `license` took the first match, so two
  datasets with an identical content hash emitted different attribution. Both now use the encoder's
  full content key, and `inspect`/`provenance` canonicalize before rendering on both surfaces.
- **A decoded value's fingerprint could differ between x86 and ARM.** The CAN+DBC and MF4 adapters
  hashed `f64::to_bits` of an *arithmetic result*, and a DBC or `##CC` coefficient of `inf` makes
  `0.0 * inf` a NaN whose default sign is platform-specific (`-0.0` was likewise distinguishable from
  `+0.0`). Both now route through the encoder's canonical float bits, so the same bytes hash the same
  everywhere — which is what the determinism contract promises.
- **A 33 KB MF4 file could allocate 1.35 GB.** The block-graph walk kept a visited set per parent
  chain, but MF4 links may legally point at shared blocks — so *n* data groups each re-walking the
  same *n* channel groups each re-walking the same *n* channels was O(n³) streams. One visited set
  now spans the whole walk, making it linear in file size.
- **MF4 could produce plausible-but-wrong data instead of reporting it.** An unapplied `##CC`
  conversion was reported for a signal but ignored on the **time master**, silently shifting every
  timestamp in the group (it now stops the group); channels declaring per-sample invalidation bits
  were decoded as if every sample were valid (they are now skipped and reported); a second channel
  group inside a sorted data group was decoded against the same records from offset 0; and a
  three-way name collision emitted two streams with the same name.
- **MF4 rasters were compared as if they shared a clock.** Every stream got one `mf4-master` clock id,
  so a 1 Hz group and a 100 Hz group over the same measurement tripped start/end-offset checks. Each
  channel group is now its own timeline.
- **A bus-only measurement was treated as a sensor rig.** Rig detection counted AV-native streams, and
  a CAN or MF4 log is dozens of `CanSignal` streams off one bus — so ordinary raster differences read
  as rig-wide clock drift (an *error*), and the pairwise `TEMPORAL.CLOCK_SKEW` was suppressed on those
  datasets. A rig now also requires two distinct AV-native modalities, which every real rig has.
- **`veridex verify` implied trust it had not checked.** With no `--key`, verification confirmed only
  that a certificate was internally consistent and bound to the presented dataset — so a certificate
  forged about *real* data and signed with an attacker's own key verified cleanly, exit 0, reporting
  whatever score it claimed. `verify` now requires a trust decision: name the issuer with `--key`, or
  pass `--allow-any-issuer` for the self-consistency check alone, which prints a warning and reports
  `issuer_verified: false` in `--json`. Python's `veridex.verify` mirrors this (`allow_any_issuer=`).
- **Certificates tolerated fields the signature never covered.** The signature is computed over the
  parsed structure, so an injected `trust_score_override` (or anything else) survived verification
  and would be read as authentic by any consumer parsing the JSON directly. Every certificate type
  now rejects unknown fields.
- **A symlink could lead the reader outside the dataset.** `simref`'s sidecar lookup rejected `..`
  and absolute paths but still followed symlinks, and the CAN+DBC adapter's input discovery did not
  check at all. Both now refuse: the sidecar path is canonicalized and re-checked for containment
  under the dataset root, and a symlinked CAN log is skipped.
- **A corrupt element count could reserve gigabytes.** The ROS CDR decoder bounded a declared element
  count against the message's *byte* length, but each element is far larger than a byte — a 100 MB
  TFMessage claiming 100M transforms reserved ~13 GB before the first read failed. Counts are now
  bounded by the smallest each element can encode.
- **`keygen --force` left a pre-existing key file world-readable.** The `0600` mode applies only at
  creation, so overwriting an existing path wrote a fresh secret seed into it without tightening
  permissions. It now sets the mode explicitly after the write.
- **A readiness criterion could pass without its check ever running.** `ReadinessReport::evaluate`
  derived `passed` from "this check produced no findings" — but a check disabled in `veridex.toml`,
  filtered out by `categories`/`only_checks`, or one that failed internally also produces none. A
  dataset that genuinely failed `autonomy.rig-sync` could be certified `READY` by disabling that
  check. Each criterion now records whether its check actually **ran** (executed and did not error);
  silence from a check that never ran blocks `ready` and prints as `? … [check did not run]`. The
  field is omitted when the check ran, so certificates issued before it existed still verify
  byte-identically.
- **`world-model-ready` applied to datasets its criteria couldn't judge.** Applicability was "is this
  a sensor rig", and a rig is ≥3 AV-native sensors — which a bus-only CAN or MF4 log satisfies. With
  no perception sensor and no ego trajectory, calibration completeness and ego-pose continuity abstain,
  so such a log was certified ready on two criteria that examined nothing. A profile now carries an
  explicit `applies_to` predicate, and `world-model-ready` demands a rig **with** a perception sensor
  and an ego trajectory; anything else is `N/A`.

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
- The content hash silently omitted four `Stream` stats fields — the stored per-dimension stats
  (`dim_stats`) and the recomputed `observed_*` fields — because the hand-written canonical encoder
  had drifted from the struct. Two datasets differing only in a corrupted per-joint stat vector
  hashed identically. The encoder now binds every content-bearing stream field, a regression test
  guards each one, and `CANONICAL_VERSION` bumps 1 → 2.
- Provenance canonicalization sorted records by scope alone and elements by key alone — neither a
  total order — so a scope with more than one record, or two elements sharing a key, could hash
  differently under a mere reordering. Both now sort on full content (permutation-independent).
- The LeRobot adapter read Arrow bookkeeping cells (`timestamp`, `episode_index`, `task_index`)
  without consulting the null bitmap, so a null cell read as a fabricated `0` — inventing a
  mid-stream `ts = 0`, misattributing frames to episode 0, or mislabeling a task. Null cells now
  abstain, so a null timestamp correctly falls back to `frame_index / fps`.
- `STATISTICAL.MEAN_OUT_OF_RANGE` compared the stored mean against min/max with no float tolerance,
  so a source's independently-rounded mean landing one ULP past a bound on a near-constant stream
  raised a hard error on honest data. It now allows the same small tolerance as the Popoviciu std
  check.
- The LeRobot per-dimension statistics silently misaligned when a multi-DoF cell had a **null leaf**:
  a dropped joint contributed nothing, sliding every later dimension down one and polluting their
  min/max/mean/std (false `STATS_STALE`/`SATURATED`, misattributed dimensions). A null leaf now holds
  its dimension slot (absent, not shifted), matching the content-hash path; a regression test covers it.
- The verdict and human/JSON/SARIF reports were **input-order-dependent** while the content hash was
  order-independent, so two datasets that hashed identically but were built with their episodes/streams
  in a different order could produce different `result_content_hash` and report bytes. The pipeline now
  canonicalizes episode order (by index) and stream order (by name) before validating, so the verdict
  matches the hash's order-independence.
- A non-finite tolerance (`NaN`/`inf`) constructed via the library/Python API serialized to JSON
  `null` — a signed certificate embedding it could never be re-verified — and silently disabled the
  checks that guard on it. Tolerances are now sanitized to their finite defaults before the run and in
  the recorded config.
- `veridex check --min-scor 90` (any mistyped or unknown flag) was silently ignored, quietly dropping
  the CI gate the user asked for; a value-flag could also swallow the next flag as its value
  (`--key --format`). Unknown options and missing flag values are now exit-2 errors.
- The LeRobot adapter never reconciled the Parquet data columns against the `meta/info.json` feature
  declarations, so an undeclared data column was silently dropped and a declared-but-absent feature
  became a phantom stream with no content — neither disclosed. The fidelity report now lists an
  undeclared column as `unmapped` and a declared-but-absent feature as `omitted`.
- The LeRobot adapter never validated `codebase_version`, so a v2.x export (which still has
  `meta/info.json`) was misparsed as v3. A recognized-but-unsupported version is now rejected cleanly
  with `IngestError::UnsupportedVersion`.
- Recomputed per-dimension variance used the one-pass `E[x²]−E[x]²` formula, which loses precision
  (and can clamp a real variance to 0 → spurious `DEGENERATE`) for signals riding a large DC offset.
  It now uses Welford's numerically stable online algorithm. Integer index columns stored as an
  unsigned or narrower Arrow type are now accepted instead of falsely rejecting the dataset, and the
  Parquet directory walk no longer follows symlinks (a self-referential link could recurse unbounded).
- Robustness: MCAP `log_time` above `i64::MAX` now saturates instead of wrapping negative and
  corrupting frame ordering; `STREAM_ABSENT` no longer lists a duplicate episode index twice; and the
  saturation check skips a zero-sample summary rather than emitting a `NaN%` finding, while the score's
  penalty arithmetic saturates so a pathological finding count cannot overflow.
- `SEMANTIC.ANNOTATION_UNALIGNED` treated a declared episode window as authoritative even when it was
  *narrower* than the recorded frames, so a `language` annotation on a genuinely recorded frame outside
  that window raised a false Error (flipping the episode to FAIL). The alignment span is now the union
  of the declared bounds and the actual frame extent; a genuinely out-of-range annotation still fires.

### Security

- Certificate verification now uses Ed25519 `verify_strict`, rejecting non-canonical signatures and
  small-order keys so a given certificate has exactly one valid signature (no malleability).

- `veridex keygen` wrote the secret signing key world/group-readable (default umask), so another local
  user on a shared host or CI runner could read it and forge certificates. On Unix the secret key is
  now created `0600` (owner-only); the public `.pub` file is unchanged.

- Upgraded `pyo3` 0.22 → 0.29, clearing three advisories (RUSTSEC out-of-bounds read in
  `PyList`/`PyTuple` `nth`/`nth_back`, the missing `Sync` bound on `PyCFunction::new_closure`, and
  the `PyString::from_object` buffer-overflow risk). The bindings' API surface was already on the
  `Bound` API, so the bump is source-compatible.

### Not yet included

Streaming / large-than-memory and remote Hub ingestion (both are *refused* with a clear error rather
than silently ignored — `metadata_only` and `Source::Remote` return `IngestError::NotImplemented`);
and publishing to PyPI / crates.io.
