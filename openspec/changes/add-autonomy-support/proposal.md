# Add autonomy / world-model sensor-data support

## Why

The largest and most demanding physical-AI datasets are **multi-sensor autonomy logs** — fleets of
cameras plus LiDAR, radar, CAN, GNSS/IMU, and ego-pose, captured from calibrated rigs and consumed
by perception, prediction, and **world-model / neural-simulator** training. This is precisely the
multi-rate, spatially-calibrated, provenance-heavy data Veridex is built to verify, and it is where
silent time-sync, calibration, and ego-pose errors most quietly corrupt training. Serving this world
well multiplies Veridex's reach beyond manipulation datasets and lands it in the pipelines with the
most acute data-trust pain and the deepest budgets.

This work is sequenced deliberately **after the core is built and tested** (ingestion/CDM,
validation-engine, checks-catalog, provenance, certificate, CLI proven on LeRobot v3 + MCAP). The
core's neutrality abstractions must be real before we extend them to a full sensor rig — but this is
the **first major expansion after core**, not a someday item.

## What changes

Introduces the `autonomy-sensor-data` capability:

- **CDM extensions** for a sensor rig: point clouds (LiDAR/radar), multi-camera, CAN signals,
  GNSS/IMU/odometry, ego-pose/trajectory, a transform (TF) tree, and timestamped per-sensor
  calibration.
- **AV-native adapters:** ASAM MDF/MF4, CAN + DBC decoding, ROS/ROS 2 bag (building on MCAP), and
  reading scenario/map/sim references (ASAM OpenSCENARIO, OpenDRIVE, OSI) where present.
- **AV checks:** rig-wide time sync, spatial calibration consistency (incl. LiDAR-camera
  reprojection), ego-motion/pose consistency, and sequence-completeness for world-model training.
- **Autonomy provenance:** sensor/firmware, calibration session, platform/drive IDs, region, map
  version, redaction/consent status.
- **Coverage reporting** across scenario dimensions, and a **world-model readiness profile** with a
  matching certification.

## Explicitly out of scope

- Generating or auto-labeling scenarios, ground truth, or annotations (Veridex verifies, never
  produces).
- Simulation execution or synthetic-data generation.
- Prescribing coverage targets (Veridex reports distribution; it does not dictate a required
  balance).
- HD-map correctness beyond referencing/version consistency.

## Impact

- New capability `autonomy-sensor-data`; extensions consumed by `dataset-ingestion`,
  `checks-catalog`, `provenance-lineage`, `configuration` (the new profile), and `trust-certificate`.
- Depends on: the core proven end-to-end, and the CDM's calibration/transform representation.
- Success criteria: ingest a real multi-sensor rig log (MF4 and/or ROS bag) with LiDAR + multi-camera
  + CAN + GNSS/IMU; detect an injected single-sensor sync drift and a LiDAR-camera miscalibration;
  produce a world-model-readiness certificate stating per-criterion results.
