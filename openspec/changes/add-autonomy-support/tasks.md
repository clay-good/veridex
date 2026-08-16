# Tasks — autonomy / world-model sensor-data support

Gated on the core being built and tested (bootstrap MVP). Ordered so each milestone is demoable.
No code until this change is approved; this is the build plan.

## A0 — CDM extensions
- [x] Extend CDM with point-cloud streams (per-point fields), the transform (TF) tree, timestamped
      per-sensor calibration (intrinsics/extrinsics with validity ranges), and ego-pose/trajectory.
      Added as optional extensions (design A1: extend, don't fork): `Modality::{PointCloud, Imu, Gnss,
      CanSignal, EgoPose}`, `Stream.point_fields`, `Dataset.calibration` (`Calibration` = TF
      `Transform`s + `CameraIntrinsics`, each with `valid_from`/`valid_to`), and `Episode.ego_poses`
      (`EgoPose` = ts + `Pose`). All content-bearing fields are bound into the content hash — the
      TF tree, intrinsics, and ego trajectory are canonicalized order-independently while a
      point-field layout is order-significant — with `CANONICAL_VERSION` bumped 2 → 3.
- [x] Confirm manipulation datasets still round-trip and validate identically (no regression).
      The full existing suite (LeRobot/MCAP ingest, checks, scoring, certificate, reports, Python
      parity) passes unchanged with the new optional fields defaulting to absent, and
      `tests/autonomy_cdm.rs` adds round-trip, order-independence, and hash-sensitivity coverage for
      the extensions. Adapters that don't populate them are unaffected.

## A1 — Adapters (phased per design A3)
- [x] Extend the MCAP/ROS path to AV message types (PointCloud2, Image/CameraInfo, Imu, NavSatFix,
      TF). Schema-name classification maps PointCloud2/LaserScan, Imu, NavSatFix, Odometry, and
      CAN-frame schema names to the rig modalities. Message *bodies* are now CDR-decoded too: a hand-
      rolled, bounds-checked ROS 2 CDR reader (`adapter/cdr.rs`, no new dependency, survives malformed
      input) decodes `PointCloud2` → `Stream.point_fields`, `CameraInfo` → `CameraIntrinsics`,
      `Odometry` → `Episode.ego_poses`, and `TFMessage` → the transform tree, all wired into the
      adapter and proven end-to-end (`ros_message_bodies_populate_the_autonomy_cdm_end_to_end`) plus
      per-decoder unit tests. Validity ranges on decoded transforms/intrinsics are left open (time-
      varying calibration from per-message stamps is a refinement).
- [~] ASAM MDF/MF4 adapter → CDM; record unmapped channels. `adapter/mdf4.rs` walks the MDF 4.x block
      graph (`##HD` → `##DG` → `##CG` → `##CN`), uses each channel group's time master as the
      timeline, and emits one stream per measured channel, applying identity/linear (`##CC` type 1)
      conversions; integer and float channels in both byte orders; values fingerprinted into the
      content hash; the identification block's program becomes `recorder` provenance. Every block read
      is bounds-checked and every chain walk loop-guarded (truncation/corruption fuzz tests). Recorded
      as unmapped rather than decoded: compressed (`##DZ`) / listed (`##DL`) data, unsorted data
      groups, bit-packed and non-numeric channels, other conversion types. Those — plus `##SR` sample
      reduction, attachments, and the `##FH`/`##MD` metadata comments — are the follow-ups.
- [~] CAN + DBC decoding → named signal streams; surface DBC-coverage gaps and decode errors.
      `adapter/candbc.rs`: ingests a directory holding a `.dbc` + candump `.log`/`.asc`, parses the
      DBC (`BO_`/`SG_`), decodes each frame's signals in both byte orders — little-endian (Intel,
      `@1`) and big-endian (Motorola, `@0`, walked over the sawtooth bit numbering from the signal's
      MSB) — with factor/offset and sign extension, into one `CanSignal` stream per `Message.Signal`,
      and reports DBC-coverage gaps (undefined CAN ids) as `unmapped` fields. A signal whose bits
      fall outside the frame is declined, never truncated. Registered in `default_registry`
      (autodetected). Decoded values fingerprinted into the content hash. Unit + integration + CLI
      e2e tests, including a Motorola signal over a byte-swapped copy of its Intel twin that must
      decode to identical samples. Recomputed signal stats (for statistical checks) remain a
      follow-up.
- [x] Read scenario/map/sim references (OpenSCENARIO/OpenDRIVE/OSI) and versions. `crate::simref`
      maps the well-known metadata spellings to four reference kinds (scenario / map / OSI /
      simulator); the MCAP adapter records them as `scenario_ref` / `map_ref` / `osi_version` /
      `simulator` provenance, class Known. Versions are extracted, not guessed: the ASAM
      `revMajor`/`revMinor` revision is read from the referenced sidecar's own header when that file
      exists next to the log (bounded 64 KiB read, relative in-dataset paths only — an absolute or
      `..` reference is recorded but never followed), else from a dotted version in the value itself,
      else absent. An explicitly recorded `map_version` outranks an OpenDRIVE header revision.
      Surfaced in `veridex inspect`, both provenance emits, and the `av` demo; unit + e2e tests.
      Parsing scenario *semantics* (manoeuvres, road geometry, ground truth) stays out of scope.

## A2 — Autonomy checks
- [~] `AUTONOMY.RIG_SYNC` — rig-wide time sync. Implemented in `checks/autonomy.rs` as the N-sensor
      generalization of `TEMPORAL.CLOCK_SKEW`: on a rig episode (≥3 AV-native sensors) it reports the
      rig-wide span spread as one finding and suppresses the pairwise `CLOCK_SKEW`. New `autonomy`
      check family/category. Proven end-to-end on the `av` demo + unit tests. Explicit trigger/latency
      offsets (a per-sensor expected-offset table) remain a follow-up — needs decoded per-sensor
      metadata (A1).
- [~] `AUTONOMY.LIDAR_CAM_REPROJ` + extrinsic/TF consistency + missing-calibration checks. Realized as
      `AUTONOMY.CALIBRATION_INCOMPLETE` (`autonomy.calibration-completeness`): since Veridex never
      decodes the bulk point payload it cannot reproject actual points, so it verifies the calibration
      is present + coherent instead — flags a spatial rig with no TF tree, a disconnected TF tree
      (connected-components over the frame graph), or cameras without intrinsics. End-to-end + unit
      tests. True per-point reprojection error would require decoding point coordinates (deliberately
      out of scope); per-sensor frame→camera path checks are a refinement (needs per-stream frame_id).
- [~] `AUTONOMY.EGO_POSE_CONTINUITY` — implemented: flags an ego trajectory step whose implied speed
      (distance/elapsed) exceeds a plausible max (default 100 m/s), reading the CDR-decoded
      `Episode.ego_poses`. Runs end-to-end on a teleporting Odometry MCAP + unit tests. GNSS/IMU/odometry
      cross-agreement and GNSS geospatial sanity remain follow-ups (need those sources decoded/fused).
- [~] `AUTONOMY.SEQUENCE_COMPLETE` — per-tick sensor completeness + frame-drop tolerance. Implemented
      as an aggregate frame-drop check: per rig sensor, observed frame count vs the count its median
      inter-frame cadence implies over its active span, flagged beyond `max_drop_fraction` (default
      5%). Robust median baseline, no declared rate needed; rig-only. Proven end-to-end through the
      MCAP adapter + unit tests. Windowed cross-sensor per-tick alignment (all sensors present in each
      tick) is a richer follow-up once decoded per-sensor cadences/offsets exist.

## A3 — Provenance + coverage
- [x] Autonomy provenance elements (sensor/firmware, calibration session, platform/drive IDs,
      region, map version, redaction/consent), classified known/asserted/unknown. The MCAP adapter's
      `provenance_key_for` now recognizes the autonomy metadata-key spellings and maps them to typed
      provenance (class Known, from the source bytes); the Croissant emit lists every element and the
      PROV emit surfaces the rig lineage as `veridex:` entity properties. Extracted freely without
      changing the coverage denominator, so manipulation datasets are unaffected. The `av` demo carries
      the rig lineage; unit + emit tests cover it. (Rig-aware coverage scoring is a follow-up.)
- [x] Scenario-dimension coverage/balance reporting (descriptive only). `crate::scenario` recognizes
      scenario tags (weather, time_of_day, environment, lighting, season, traffic) from episode labels
      and reports each dimension's value distribution across episodes, marking sparse cells (a value in
      <10% of covered episodes) — descriptive only, never a finding or score change (design A6). The
      MCAP adapter extracts recognized scenario metadata keys into episode labels; `veridex inspect`
      shows a "scenario coverage" section. The `av` demo carries scenario tags. Unit + e2e tests.

## A4 — World-model readiness
- [x] `world-model-ready` policy profile bundling sync/calibration/ego-pose/sequence thresholds
      (`crate::profile`): tightens cross-sensor sync to 20 ms and names the four autonomy criteria.
      Applied via `veridex certify --profile world-model-ready`.
- [x] Certificate reports per-criterion pass/fail against that profile. A `readiness` block
      (`ReadinessReport`) records profile name, `applicable` (is it a rig), overall `ready`, and each
      criterion's check id / threshold / passed / finding count — signed like every field, honest by
      construction (a non-rig is `N/A`, never a vacuous pass). Proven end-to-end (`certify --profile`
      on the `av` rig prints and signs NOT READY) + unit tests (`tests/profile.rs`). Tolerances for
      the sequence/ego-pose/calibration criteria are their defaults (config-wiring them is a follow-up).

## A5 — Proof
- [ ] End-to-end on a real multi-sensor rig log (MF4 and/or ROS bag): LiDAR + multi-camera + CAN +
      GNSS/IMU.
- [~] Reproduce detection of an injected single-sensor sync drift and a LiDAR-camera miscalibration.
      The sync-drift half is reproduced end-to-end on a synthetic rig: `make_demo_mcap -- <out> av`
      writes a five-sensor MCAP rig (camera/LiDAR/IMU/GNSS/odometry) with the IMU span cut ~0.30 s,
      which `veridex check` flags as `AUTONOMY.RIG_SYNC` naming the IMU as the tightest-spanning
      sensor (on a rig, `RIG_SYNC` supersedes the pairwise `TEMPORAL.CLOCK_SKEW`)
      (`an_injected_single_sensor_sync_drift_is_flagged_on_an_av_rig`). The LiDAR-camera
      miscalibration half needs the reprojection check (A2) and decoded calibration (A1).
- [x] Issue and offline-verify a world-model-readiness certificate. `certify --profile` issues it and
      `verify` now reads it back: the bound hash, trust score, profile verdict, and each criterion,
      with `--json` for the machine-readable form. Everything reported comes from the signed
      document — a test flips `ready` to true and asserts the certificate no longer verifies, and a
      non-rig verifies as `N/A`, never a vacuous pass. Python reaches parity
      (`veridex.certify(..., profile=…)` is byte-identical to the CLI; `veridex.verify` returns the
      identical summary), enforced by the CI parity suite. Proven on a rig (`tests/profile.rs`) and
      end-to-end through the binary (`tests/cli.rs`).
- [x] Docs: an autonomy quickstart and the readiness-profile reference.
      [docs/autonomy-quickstart.md](../../../docs/autonomy-quickstart.md) walks the rig demo end to
      end (generate → inspect → check → certify readiness → verify offline) with real output, a table
      of what Veridex reads from a rig log, and the honest limits (MF4 coverage is the uncompressed
      core, no trigger/latency offsets, coverage never prescriptive). The readiness-profile reference is
      [docs/profiles.md](../../../docs/profiles.md), now including how to read a certificate back.

## Audit follow-ups (deliberately not changed)

Two findings from the audit pass are documented rather than fixed, because changing them is a policy
call, not a defect fix:

- **Narrowing a run costs no score.** An *errored* check is charged −10 as a coverage gap; a check
  disabled in config, deselected, or severity-overridden is charged nothing. Charging it would change
  the rubric (scores are comparable only within a `rubric_version`) and would penalize legitimate
  configuration. Recorded instead: the effective config and executed checks are signed into every
  certificate, and readiness criteria now require their check to have run. See `docs/rubric-v1.md`.
- **`declared_frame_count` drives the verdict but is not in the content hash** — by design, since it
  is an assertion *about* content rather than content. Documented in `SECURITY.md` so a certificate
  reader knows what the binding does and does not cover; folding it in would need a
  `CANONICAL_VERSION` bump to 4.

Still open from the audit, unblocked but unbuilt: `statistical.range-sanity` claiming a stream name
before evaluating it (exact today because stored stats are dataset-level; wrong the moment an adapter
attaches per-episode stats). The MCAP decompressed-byte budget is now built — expansion is capped at
100x the file's own size (`--max-decompression-ratio`), charged off the chunk headers before the
reader sees the file and again against the bytes that arrive.
