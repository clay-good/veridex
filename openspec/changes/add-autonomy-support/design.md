# Design — autonomy / world-model sensor-data support

## Sequencing

Gated on: core proven on LeRobot v3 + MCAP (bootstrap MVP done and tested). This is the **first
major post-core expansion**. Rationale: the CDM's neutrality and the calibration/transform
representation must be real and exercised before extending to a full sensor rig, or the AV work will
silently re-introduce format-specific assumptions.

## Key decisions

### A1 — Extend the CDM, do not fork it
Autonomy support is CDM *extensions* (point-cloud fields, transform tree, timestamped calibration,
ego-pose), not a parallel model. Manipulation and autonomy datasets share one representation; a
LiDAR stream is just another stream with a modality and clock. This keeps the "one verdict across
formats" promise intact across domains.

### A2 — Transform tree and calibration are first-class, time-varying
Calibration (intrinsics/extrinsics) and the coordinate-frame/TF tree are represented with validity
time ranges, because rigs are recalibrated and transforms can change within a log. Spatial checks
resolve the transform valid at each timestamp, not a single static calibration.

### A3 — Format priority order
1. **ROS/ROS 2 bag + MCAP** — already in core via MCAP; extend to the AV message types (PointCloud2,
   CameraInfo/Image, Imu, NavSatFix, TF).
2. **ASAM MDF/MF4** — the dominant automotive measurement format; highest-leverage new adapter.
3. **CAN + DBC** — decode raw bus frames into named signals; DBC is the standard signal database.
4. **Scenario/map/sim references** — read ASAM OpenSCENARIO/OpenDRIVE/OSI references and versions;
   full semantic parsing is later.

### A4 — Reuse the existing check framework
AV checks register through `validation-engine` like any others, with namespaced IDs (e.g.
`AUTONOMY.RIG_SYNC`, `AUTONOMY.LIDAR_CAM_REPROJ`, `AUTONOMY.EGO_POSE_CONTINUITY`,
`AUTONOMY.SEQUENCE_COMPLETE`). Rig-wide sync generalizes the core's `TEMPORAL.CLOCK_SKEW` from a pair
to N sensors with trigger/latency offsets.

### A5 — World-model readiness is a profile, not a new engine
"World-model ready" is a named `configuration` profile bundling sync + calibration + ego-pose +
sequence-completeness thresholds. The certificate reports per-criterion results. No bespoke
world-model logic beyond composing existing checks at the right thresholds.

### A6 — Coverage is descriptive, never prescriptive
Coverage/diversity reporting over scenario tags is informational. Veridex surfaces distribution and
sparse cells; it never declares a required balance or blocks on coverage, because the right target is
the training team's call.

### A7 — Privacy is acute here
Public-road capture implies faces and license plates. The existing privacy/PII checks apply, and
autonomy provenance records redaction/consent status. Veridex flags likely PII; it never redacts or
alters frames.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| CDM bends toward AV and hurts manipulation | A1: extensions only; the "same verdict across formats/domains" tests must still pass for manipulation datasets. |
| MF4/DBC parsing surface is large | Phase adapters (A3); record unmapped channels rather than block; land ROS/MCAP AV messages first. |
| Calibration math is subtle | A2: resolve time-valid transforms; make reprojection tolerance explicit and configurable; test against known-good and known-bad calibration fixtures. |
| Scope creep into simulation / labeling | Proposal's out-of-scope: verify only; no generation, no coverage mandates. |
| Huge logs blow up runtime | Reuse core streaming, sampling, and incremental validation. |
