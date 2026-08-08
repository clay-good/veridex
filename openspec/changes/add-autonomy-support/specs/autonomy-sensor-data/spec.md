# autonomy-sensor-data — change delta

Introduces the `autonomy-sensor-data` capability as the first major post-core expansion. These
requirements are the ratified target for this capability; the north-star spec at
`openspec/specs/autonomy-sensor-data/spec.md` holds the full statement.

## ADDED Requirements

### Requirement: Multi-sensor rig CDM extensions
The CDM SHALL represent an autonomy sensor rig — point-cloud (LiDAR/radar), multi-camera, CAN
signal, GNSS/IMU/odometry, and ego-pose streams; a coordinate-frame/transform tree; and timestamped
per-sensor calibration with validity ranges — as extensions of the existing model, with no
regression to manipulation-dataset handling.

#### Scenario: Rig log ingested without breaking manipulation datasets
- **WHEN** a multi-sensor rig log is ingested and, separately, a manipulation dataset is re-ingested
- **THEN** the rig is fully represented in the CDM
- **AND** the manipulation dataset's CDM and verdict are unchanged from before the extension

### Requirement: AV-native adapters
Veridex SHALL ingest ASAM MDF/MF4, CAN-with-DBC, and ROS/ROS 2 bag (via the MCAP path) into the
CDM, recording unmapped fields, and SHALL read scenario/map/sim references (OpenSCENARIO/OpenDRIVE/
OSI) and versions where present.

#### Scenario: MF4 and CAN+DBC ingest into one CDM
- **WHEN** an MF4 recording and its CAN log with DBC are ingested
- **THEN** channels and decoded CAN signals appear as CDM streams
- **AND** unmapped channels and DBC-coverage gaps are recorded as findings

### Requirement: Autonomy checks
Veridex SHALL run rig-wide time-sync, spatial-calibration-consistency (including LiDAR-camera
reprojection), ego-motion/pose-consistency, and sequence-completeness checks, each with configurable
tolerances and precise findings.

#### Scenario: Injected sync drift and miscalibration are both caught
- **WHEN** a rig log has an injected single-sensor sync drift and a LiDAR-camera calibration error
- **THEN** the sync check and the reprojection check each fail with `error` findings naming the
  sensor(s), measured offset/error, and time range

### Requirement: World-model readiness profile and certificate
Veridex SHALL provide a `world-model-ready` policy profile bundling the relevant sync, calibration,
ego-pose, and sequence-completeness thresholds, and the certificate SHALL report per-criterion
pass/fail and the thresholds used.

#### Scenario: Readiness certificate is per-criterion and honest
- **WHEN** a dataset is certified under the `world-model-ready` profile
- **THEN** the certificate lists each criterion's result and threshold
- **AND** it claims no readiness beyond the criteria checked
