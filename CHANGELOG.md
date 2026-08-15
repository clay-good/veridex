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
  `episode_duration_factor`, `saturation_fraction`, `saturation_min_samples` — see
  [docs/veridex.toml.example](docs/veridex.toml.example)); each is optional, validated
  (finite, non-negative; positive `gap_factor`), and falls back to the check's default. The
  tolerances the run used are recorded in the verdict's effective config, so a result is fully
  reproducible from what it reports.
- **Runnable demos** — `examples/make_demo_mcap` (synthetic cross-stream clock skew) and
  `examples/make_demo_lerobot` (a LeRobot v3 dataset with an out-of-order timestamp, a
  `truncated` variant whose manifest over-declares its frame count → `STRUCTURAL.FRAME_COUNT_MISMATCH`,
  a `boundary` variant whose `meta/episodes.jsonl` declares the wrong length for one episode → the
  lerobot#4143 `STRUCTURAL.EPISODE_BOUNDARY`, and a `jitter` variant whose one episode has an
  irregular inter-frame spacing → `TEMPORAL.JITTER`),
  each with a `clean` variant, so `veridex check` has something to find end-to-end in **both** formats.
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
- **Autonomy provenance lineage (A3)** — the MCAP adapter now extracts the sensor-rig lineage a
  producer records in Metadata: firmware, calibration session, platform/vehicle and drive/run IDs,
  capture region, HD-map version, and — acute for public-road capture — redaction and consent status.
  Each is classified `known` (read from the source bytes) and surfaced in both provenance emits: the
  Croissant `veridex:provenance` list and the PROV entity as `veridex:` properties. Extracted without
  changing the coverage denominator, so a manipulation dataset's coverage score is unchanged. The `av`
  demo carries the lineage end-to-end.

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

Streaming / large-than-memory and remote Hub ingestion; and publishing to PyPI / crates.io.
