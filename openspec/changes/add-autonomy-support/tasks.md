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
- [~] Extend the MCAP/ROS path to AV message types (PointCloud2, Image/CameraInfo, Imu, NavSatFix,
      TF). Schema-name classification lands first: the MCAP adapter now maps PointCloud2/LaserScan,
      Imu, NavSatFix, Odometry, and CAN-frame schema names to the `PointCloud`/`Imu`/`Gnss`/`EgoPose`/
      `CanSignal` modalities (`av_rig_message_types_map_to_the_autonomy_modalities`). Decoding message
      *bodies* to populate `point_fields` (PointCloud2 fields), `CameraIntrinsics` (CameraInfo), and
      the TF tree / `ego_poses` (TF, Odometry) — which needs CDR/schema decoding — remains a follow-up.
- [ ] ASAM MDF/MF4 adapter → CDM; record unmapped channels.
- [ ] CAN + DBC decoding → named signal streams; surface DBC-coverage gaps and decode errors.
- [ ] Read scenario/map/sim references (OpenSCENARIO/OpenDRIVE/OSI) and versions.

## A2 — Autonomy checks
- [~] `AUTONOMY.RIG_SYNC` — rig-wide time sync. Implemented in `checks/autonomy.rs` as the N-sensor
      generalization of `TEMPORAL.CLOCK_SKEW`: on a rig episode (≥3 AV-native sensors) it reports the
      rig-wide span spread as one finding and suppresses the pairwise `CLOCK_SKEW`. New `autonomy`
      check family/category. Proven end-to-end on the `av` demo + unit tests. Explicit trigger/latency
      offsets (a per-sensor expected-offset table) remain a follow-up — needs decoded per-sensor
      metadata (A1).
- [ ] `AUTONOMY.LIDAR_CAM_REPROJ` + extrinsic/TF consistency + missing-calibration checks.
- [ ] `AUTONOMY.EGO_POSE_CONTINUITY` + GNSS/IMU/odometry agreement + GNSS geospatial sanity.
- [~] `AUTONOMY.SEQUENCE_COMPLETE` — per-tick sensor completeness + frame-drop tolerance. Implemented
      as an aggregate frame-drop check: per rig sensor, observed frame count vs the count its median
      inter-frame cadence implies over its active span, flagged beyond `max_drop_fraction` (default
      5%). Robust median baseline, no declared rate needed; rig-only. Proven end-to-end through the
      MCAP adapter + unit tests. Windowed cross-sensor per-tick alignment (all sensors present in each
      tick) is a richer follow-up once decoded per-sensor cadences/offsets exist.

## A3 — Provenance + coverage
- [ ] Autonomy provenance elements (sensor/firmware, calibration session, platform/drive IDs,
      region, map version, redaction/consent), classified known/asserted/unknown.
- [ ] Scenario-dimension coverage/balance reporting (descriptive only).

## A4 — World-model readiness
- [ ] `world-model-ready` policy profile bundling sync/calibration/ego-pose/sequence thresholds.
- [ ] Certificate reports per-criterion pass/fail against that profile.

## A5 — Proof
- [ ] End-to-end on a real multi-sensor rig log (MF4 and/or ROS bag): LiDAR + multi-camera + CAN +
      GNSS/IMU.
- [~] Reproduce detection of an injected single-sensor sync drift and a LiDAR-camera miscalibration.
      The sync-drift half is reproduced end-to-end on a synthetic rig: `make_demo_mcap -- <out> av`
      writes a five-sensor MCAP rig (camera/LiDAR/IMU/GNSS/odometry) with the IMU span cut ~0.30 s,
      which `veridex check` flags as `TEMPORAL.CLOCK_SKEW` naming the IMU
      (`an_injected_single_sensor_sync_drift_is_flagged_on_an_av_rig`). The LiDAR-camera
      miscalibration half needs the reprojection check (A2) and decoded calibration (A1).
- [ ] Issue and offline-verify a world-model-readiness certificate.
- [ ] Docs: an autonomy quickstart and the readiness-profile reference.
