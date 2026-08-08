# autonomy-sensor-data Specification

> **Status: Core — priority immediately after the initial core is built and tested.** This
> capability makes Veridex first-class for autonomous-systems and world-model training data
> (multi-sensor vehicle/robot rigs), not just manipulation datasets. It composes with the existing
> engine: it extends the CDM, adds adapters through the `dataset-ingestion` contract, and adds
> checks through the `validation-engine`/`checks-catalog` framework.

## Purpose

Autonomy and world-model pipelines train on **large, multi-sensor, time-synchronized logs** —
multiple cameras, LiDAR, radar, the vehicle bus (CAN), GNSS/IMU/odometry, and ego-pose — captured
from a calibrated sensor rig and often organized by driving scenario. This data is exactly the
multi-rate, spatially-calibrated, provenance-heavy form Veridex was built to verify, and it is
where silent time-sync, calibration, and ego-pose errors most quietly corrupt training. This
capability makes that world native: AV-native formats map into the CDM, and AV-specific checks and
provenance make a dataset's readiness for perception or world-model training a verifiable fact.

Veridex remains **model-agnostic and non-mutating** here: it verifies datasets used by any training
pipeline (perception, prediction, neural simulators / world models) and never trains, labels, or
alters them.

## Requirements

### Requirement: Multi-sensor rig model
The CDM SHALL represent an autonomy sensor rig: point-cloud streams (LiDAR, radar) with per-point
fields; multiple camera streams; vehicle-bus (CAN) signal streams; GNSS, IMU, wheel-odometry, and
ego-pose/trajectory streams; a coordinate-frame / transform (TF) tree relating sensors and the ego
frame; and per-sensor calibration (intrinsics, extrinsics, and their validity time ranges).

#### Scenario: A full rig is represented in the CDM
- **WHEN** a log with several cameras, a LiDAR, a radar, CAN, GNSS, and IMU is ingested
- **THEN** each appears in the CDM as a typed stream with its modality, rate, and clock, related
  through the rig's transform tree
- **AND** per-sensor calibration is associated with the corresponding streams

#### Scenario: Point-cloud fields are preserved
- **WHEN** a LiDAR stream encodes per-point coordinates, intensity, and timestamps
- **THEN** the CDM preserves those per-point fields
- **AND** downstream spatial checks can use them

### Requirement: AV-native format adapters
Veridex SHALL provide adapters that map common autonomy data formats into the CDM without loss of
CDM-representable fields, including: ASAM MDF/MF4 measurement recordings; CAN logs with DBC-based
signal decoding; ROS/ROS 2 bags and MCAP; and references to scenario/map standards (ASAM
OpenSCENARIO, OpenDRIVE) and simulation ground-truth (ASAM OSI) where present. Each adapter SHALL
declare supported versions and record unmapped fields.

#### Scenario: An MF4 recording is ingested and validated
- **WHEN** an ASAM MDF/MF4 recording is supplied
- **THEN** Veridex maps its channels into CDM streams and runs the applicable checks
- **AND** any channel it cannot map is recorded, not silently dropped

#### Scenario: CAN signals are decoded via a DBC
- **WHEN** a CAN log is provided with its DBC database
- **THEN** raw CAN frames are decoded into named signal streams in the CDM
- **AND** undecodable frames or DBC-coverage gaps are surfaced as findings

### Requirement: Rig-wide time synchronization checks
Veridex SHALL extend cross-stream synchronization to a full rig: verifying that all sensors are
coherently aligned on a common timeline within stated tolerances, accounting for per-sensor clocks,
trigger offsets, and declared latencies, and flagging any sensor whose alignment drifts out of
tolerance.

#### Scenario: One camera drifts out of rig sync
- **WHEN** one camera in a multi-sensor rig drifts beyond the sync tolerance relative to LiDAR and
  the other cameras
- **THEN** a check fails with an `error` naming that sensor, the measured offset, and the time range
- **AND** the finding notes the perception/world-model training risk

### Requirement: Spatial calibration consistency checks
Veridex SHALL check spatial calibration coherence across sensors: extrinsics and the transform tree
are internally consistent; LiDAR points project into camera frames within tolerance; and calibration
is present and valid for the streams that require it. Findings SHALL name the sensor pair and the
measured error.

#### Scenario: LiDAR-to-camera projection is misaligned
- **WHEN** projecting LiDAR points into a camera using the recorded calibration exceeds the
  configured reprojection tolerance
- **THEN** a calibration check fails and reports the sensor pair and the reprojection error
- **AND** the finding flags likely extrinsic or timing miscalibration

#### Scenario: Missing calibration for a required sensor
- **WHEN** a camera or LiDAR stream lacks the calibration needed to place it in the rig
- **THEN** a check reports the missing calibration and the affected stream

### Requirement: Ego-motion and pose consistency checks
Veridex SHALL check ego-state coherence: GNSS, IMU, wheel-odometry, and ego-pose agree within
tolerance; the pose trajectory is continuous (no impossible jumps or teleports); and GNSS values are
geospatially sane (within valid bounds, no discontinuities beyond configured limits).

#### Scenario: Ego-pose teleports between frames
- **WHEN** the ego-pose trajectory contains a discontinuity implying an impossible velocity
- **THEN** a check fails and reports the frames and the implied motion
- **AND** the finding notes the risk to prediction and world-model training

#### Scenario: GNSS and odometry disagree
- **WHEN** GNSS-derived motion and wheel-odometry disagree beyond tolerance over a window
- **THEN** a check emits a finding describing the disagreement and window

### Requirement: Sequence completeness for world-model training
Veridex SHALL check temporal-sequence readiness for sequence/world-model training: for each time
tick in a declared training window, the required sensor set is present and complete, sequences are
continuous with no missing sensors mid-sequence, and frame-drop rates per sensor stay within
tolerance. Coverage SHALL be reported per sequence.

#### Scenario: A sensor drops out mid-sequence
- **WHEN** a required sensor is missing for part of a training sequence
- **THEN** a check reports the sequence, the sensor, and the affected tick range
- **AND** the report states the sequence is incomplete for sensor-complete training

### Requirement: Autonomy provenance extensions
The provenance model SHALL cover autonomy-specific elements at rig and session scope: sensor
make/model/firmware, calibration session identity and date, vehicle/platform identifier, drive or
recording session identifier, geographic region, map/OpenDRIVE version referenced, and
redaction/consent status for public-road capture — each classified known/asserted/unknown.

#### Scenario: Calibration session and map version are captured
- **WHEN** a log records the calibration session and the map version it was collected against
- **THEN** provenance records both, classified as known, at the appropriate scope
- **AND** absent elements (e.g. consent status) are reported unknown, not assumed

### Requirement: Scenario and coverage metadata
Where scenario descriptors are present or attested (e.g. OpenSCENARIO tags, maneuver/condition
labels), Veridex SHALL represent them and SHALL be able to report dataset coverage and balance
across scenario dimensions, so training-set gaps and skew are visible. Veridex SHALL report, not
prescribe, target coverage.

#### Scenario: Coverage skew across conditions is surfaced
- **WHEN** a dataset carries scenario/condition tags (e.g. weather, time-of-day, maneuver)
- **THEN** a coverage report shows the distribution across those dimensions
- **AND** it highlights sparsely covered combinations without imposing a required target

### Requirement: World-model readiness profile
Veridex SHALL provide a named policy profile that bundles the sync, calibration, ego-pose, and
sequence-completeness thresholds relevant to world-model / neural-simulator training, so a dataset
can be checked and certified against a single "world-model ready" standard, with the certificate
stating exactly which criteria were met.

#### Scenario: Certifying against the world-model profile
- **WHEN** a dataset is certified under the world-model readiness profile
- **THEN** the certificate reports pass/fail against each bundled criterion and the thresholds used
- **AND** it does not claim readiness beyond the criteria the profile checked
