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
- [ ] Extend the MCAP/ROS path to AV message types (PointCloud2, Image/CameraInfo, Imu, NavSatFix,
      TF).
- [ ] ASAM MDF/MF4 adapter → CDM; record unmapped channels.
- [ ] CAN + DBC decoding → named signal streams; surface DBC-coverage gaps and decode errors.
- [ ] Read scenario/map/sim references (OpenSCENARIO/OpenDRIVE/OSI) and versions.

## A2 — Autonomy checks
- [ ] `AUTONOMY.RIG_SYNC` — rig-wide time sync with trigger/latency offsets.
- [ ] `AUTONOMY.LIDAR_CAM_REPROJ` + extrinsic/TF consistency + missing-calibration checks.
- [ ] `AUTONOMY.EGO_POSE_CONTINUITY` + GNSS/IMU/odometry agreement + GNSS geospatial sanity.
- [ ] `AUTONOMY.SEQUENCE_COMPLETE` — per-tick sensor completeness + frame-drop tolerance.

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
- [ ] Reproduce detection of an injected single-sensor sync drift and a LiDAR-camera miscalibration.
- [ ] Issue and offline-verify a world-model-readiness certificate.
- [ ] Docs: an autonomy quickstart and the readiness-profile reference.
