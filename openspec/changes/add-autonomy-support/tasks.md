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
- [x] ROS 2 **rosbag2** adapter (both storage plugins) → CDM. `adapter/rosbag2.rs`
      ingests a bag directory — `metadata.yaml` beside one or more `.db3` (the `sqlite3` plugin) or
      beside one or more `.mcap` (the plugin `ros2 bag record` writes by default from Jazzy on) — or
      a bare `.db3`,
      mapping `topics` → streams, `messages` → frames on the bag's single log clock, and the ROS type
      → the rig modality. Message bodies go through the same `adapter/cdr.rs` decoders MCAP uses, so
      `PointCloud2`/`CameraInfo`/`TFMessage`/`Odometry` populate point fields, intrinsics, the
      transform tree and the ego trajectory; payloads are fingerprinted, never decoded. Reads SQLite
      through `adapter/sqlite.rs` — a hand-written, bounds-checked table-b-tree reader (no new
      dependency) that refuses an out-of-range page, refuses a b-tree or overflow chain that revisits
      a page, and caps the payload one row may assemble. Columns are bound by name from each table's
      `CREATE TABLE`, so a bag version that adds a column does not shift what is read. Three
      disclosures rather than silence: a message on a topic `topics` never declares, a manifest shard
      that is absent or names a path out of the bag, and a `metadata.yaml` `message_count` the
      recording falls short of, all become `unread_sources` → `COVERAGE.SOURCE_UNREAD`. Fixtures are
      real Python-`sqlite3` output (`tests/fixtures/rosbag2/generate_fixtures.py`), with golden
      payload hashes pinning the overflow-chain reassembly. `--compression-mode file` bags
      (`.db3.zstd`) are decompressed under the ingest budget, bounded during the read; per-*message*
      compression is refused by name rather than read wrong. The topic's recorded QoS **durability** is
      read into `Stream::latched` (from a `.db3` and from an MCAP channel alike), which exempts a
      latched topic from the four checks that ask whether streams cover the same window —
      `CANONICAL_VERSION` bumped 7 → 8, since checks reach different verdicts on it.
      `--metadata-only` reads `topics_with_message_count` and refuses a bare `.db3` or an inventory
      that does not add up to the manifest's own total. An **MCAP-backed** bag takes the same path
      with one shard reader swapped: a channel carries what the `topics` table carries, so modality,
      latched, and every decoded AV header come out identical, and a test pins that the same
      recording through either plugin yields the same CDM. A directory of `.mcap` files with no
      manifest is not claimed (it could be unrelated recordings in one folder), and a manifest that
      disagrees with the shards beside it is refused rather than resolved either way. Follow-up: the rest of a QoS profile
      (reliability, history depth, deadline, lifespan, liveliness), which the CDM has no shape for.
- [~] ASAM MDF/MF4 adapter → CDM; record unmapped channels. `adapter/mdf4.rs` walks the MDF 4.x block
      graph (`##HD` → `##DG` → `##CG` → `##CN`), uses each channel group's time master as the
      timeline, and emits one stream per measured channel, applying identity/linear (`##CC` type 1)
      conversions; integer and float channels in both byte orders; values fingerprinted into the
      content hash; the identification block's program becomes `recorder` provenance and the `##SI`
      acquisition sources (`cg_si_acq_source`, `cn_si_source`) become `sensor`, each qualified by its
      bus or path — an MF4 scored 0/6 on provenance coverage until they were read. Every block read
      is bounds-checked and every chain walk loop-guarded (truncation/corruption fuzz tests). All four
      shapes a group's data arrives in are read: an uncompressed `##DT`, a deflated `##DZ` (plain or
      byte-column transposed), a `##DL` data list splitting the records across several of those, and
      an `##HL` header list wrapping such a list — which is what a real logger writes, so reading only
      `##DT` read nothing off the files the format is actually used for. Decompression is charged to
      the shared `DecompressionBudget` before a decompressor sees a stream and each read is capped at
      the length the block declares, so a forged expansion is refused rather than allocated. Recorded
      An **unsorted** data group — several rasters interleaved behind their `cg_record_id`s, the way a
      bus logger writes them — is demultiplexed into one contiguous stream per channel group at that
      group's own record length, and each group gets its own clock id, because two channel groups are
      two independent timelines. Recorded as **unread** rather than decoded — data that is there and
      nobody read it, so each raises `COVERAGE.SOURCE_UNREAD`: a `##DZ` holding something other than a
      `DT` record stream or using an undefined zip type, a data list whose elements do not all resolve
      (half a list is not a shorter measurement, it is a misaligned one), a record tagged with an id
      no channel group claims (a record's length is known only from its id, so the rest of the stream
      cannot be located), a variable-length signal-data group, a group with no usable time master, a
      channel declaring per-sample invalidation, and a group declaring more cycles than its data block
      holds. A `--metadata-only` run describes a measurement from its header tree without
      opening or decompressing a data block. Genuinely
      unmapped, because the CDM has no shape for them: bit-packed and non-numeric channels, other
      conversion types. Those — plus `##SR` sample
      reduction, attachments, and the `##FH`/`##MD` metadata comments — are the follow-ups.
      Per-channel statistics are recomputed from the converted physical values through the shared
      accumulator, so the statistical family grades a measurement rather than abstaining on it.
- [x] CAN + DBC decoding → named signal streams; surface DBC-coverage gaps and decode errors.
      `adapter/candbc.rs`: ingests a directory holding a `.dbc` + candump `.log`/`.asc`, parses the
      DBC (`BO_`/`SG_`), decodes each frame's signals in both byte orders — little-endian (Intel,
      `@1`) and big-endian (Motorola, `@0`, walked over the sawtooth bit numbering from the signal's
      MSB) — with factor/offset and sign extension, into one `CanSignal` stream per `Message.Signal`,
      and reports DBC-coverage gaps — frames on an id the DBC never defines, and log lines that are
      not candump frames — as `unread_sources` → `COVERAGE.SOURCE_UNREAD`, since that traffic was on
      the bus and went into no stream (the eight busiest ids are named, the rest counted). A signal whose bits
      fall outside the frame is declined, never truncated. Registered in `default_registry`
      (autodetected). Decoded values fingerprinted into the content hash. Unit + integration + CLI
      e2e tests, including a Motorola signal over a byte-swapped copy of its Intel twin that must
      decode to identical samples. Per-signal statistics are recomputed from the decoded values
      through the same accumulator LeRobot and HDF5 use, so the statistical family grades a CAN log
      rather than abstaining on it — a wheel speed pinned at its rail is `STATISTICAL.SATURATED`,
      which is the example the abstention finding was written around.
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
      out of scope). **The per-sensor frame→camera path check is now in**
      (`autonomy.sensor-frame-resolution`, `AUTONOMY.SENSOR_FRAME_UNKNOWN` /
      `AUTONOMY.SENSOR_FRAME_UNRELATED`): the CDM's new `Stream.frame_id` — decoded by the MCAP
      adapter from `header.frame_id`, bound into the content hash at `CANONICAL_VERSION` 5 — lets the
      check ask whether a chain of transforms exists from each sensor's own frame to a camera's. This
      catches what counting the tree's components cannot: a connected tree recorded for a frame name
      the sensor does not publish, and a sensor in a subtree nothing joins to the camera. Proven
      end-to-end through the adapter and by the `av-miscalibrated` demo variant.
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
      (`crate::profile`): tightens cross-sensor sync to 20 ms and names the autonomy criteria (five as of
      `autonomy.sensor-frame-resolution`).
      Applied via `veridex certify --profile world-model-ready`.
- [x] Certificate reports per-criterion pass/fail against that profile. A `readiness` block
      (`ReadinessReport`) records profile name, `applicable` (a rig that also carries a
      perception sensor and an ego trajectory — the things a world model is built from), overall
      `ready`, and each
      criterion's check id / threshold / passed / finding count — signed like every field, honest by
      construction (a non-rig is `N/A`, never a vacuous pass). Proven end-to-end (`certify --profile`
      on the `av` rig prints and signs NOT READY) + unit tests (`tests/profile.rs`). The sequence
      and ego-pose thresholds are now config-wired like every other family (`sequence_drop_fraction`,
      `ego_max_speed_mps` in `[tolerances]`); the profile still applies its own defaults.

## A5 — Proof
- [ ] End-to-end on a real multi-sensor rig log (MF4 and/or ROS bag): LiDAR + multi-camera + CAN +
      GNSS/IMU.
- [~] Reproduce detection of an injected single-sensor sync drift and a LiDAR-camera miscalibration.
      The sync-drift half is reproduced end-to-end on a synthetic rig: `make_demo_mcap -- <out> av`
      writes a five-sensor MCAP rig (camera/LiDAR/IMU/GNSS/odometry) with the IMU span cut ~0.30 s,
      which `veridex check` flags as `AUTONOMY.RIG_SYNC` naming the IMU as the tightest-spanning
      sensor (on a rig, `RIG_SYNC` supersedes the pairwise `TEMPORAL.CLOCK_SKEW`)
      (`an_injected_single_sensor_sync_drift_is_flagged_on_an_av_rig`). **The miscalibration half is
      now reproduced too**: `make_demo_mcap -- <out> av-miscalibrated` writes the same rig with the
      LiDAR parented to a `lidar_mount` frame nothing joins to `base_link`, and `veridex check` flags
      `AUTONOMY.SENSOR_FRAME_UNRELATED` naming `/lidar/points` — the transform tree is well-formed and
      the LiDAR is in it, yet no chain reaches the camera, so the reprojection is undefined
      (`a_lidar_stranded_from_the_camera_is_caught_end_to_end`). What remains out of scope by design is
      a reprojection *error* in pixels, which would require decoding point coordinates.
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
- **`declared_frame_count` drives the verdict but is not in the content hash** — *reversed*. The audit
  proved the consequence: two datasets differing only in that field, one passing and one failing,
  shared a hash, so the passing one's certificate verified against the failing one. A field a check
  reads has to be bound. It is now encoded and `CANONICAL_VERSION` is 4.

Both audit follow-ups are now built. `statistical.range-sanity` claims a stream only once it has
produced a finding, matching its sibling checks, so a later episode's corrupt stats can no longer be
masked by an earlier clean one. The decompressed-byte budget is built — expansion is capped at
100x the file's own size with a 64 MiB floor (`--max-decompression-ratio`), charged off the MCAP
chunk headers before the reader sees the file and again against the bytes that arrive, and per
record batch on the LeRobot Parquet path.
