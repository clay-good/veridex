# Autonomy quickstart

Check a multi-sensor rig log — LiDAR, cameras, IMU, GNSS, ego-odometry, CAN — and issue a signed
**world-model readiness** certificate. Five minutes, no dataset of your own required.

## 1. Make a rig log

The repo ships a synthetic five-sensor rig with a deliberately desynced IMU:

```sh
cargo run -p veridex-core --example make_demo_mcap -- /tmp/av.mcap av
```

## 2. See what Veridex read

```sh
cargo run -p veridex-cli -- inspect /tmp/av.mcap
```

`inspect` shows the streams and their modalities, provenance coverage, the scenario/map references
the log was recorded against, and the descriptive scenario coverage (abbreviated here):

```
      /camera/image [video] — 31 frame(s), clock `mcap-log`
      /lidar/points [point-cloud] — 11 frame(s), clock `mcap-log`
      /imu/data [imu] — 101 frame(s), clock `mcap-log`
      …
  scenario & map references:
      scenario: OpenSCENARIO 1.2 (version 1.2)
      map: maps/demo_town.xodr (version demo-hdmap-1.9)
  scenario coverage (descriptive):
      weather: rain (1)
      …
```

Scenario coverage is descriptive only — never a finding, never a score change.

## 3. Check it

```sh
cargo run -p veridex-cli -- check /tmp/av.mcap
```

```
  Status:   FAIL   Trust: 70 (C)  [data 73 · provenance 66%]
  [error] AUTONOMY.RIG_SYNC  episode 0
      rig sensors are out of sync — `/imu/data` spans 700.0 ms but `/odom` spans 1000.0 ms,
      a 300.0 ms drift across 5 sensors
      remedy: Re-synchronize the rig against a common time base, or record and apply per-sensor
              trigger/latency offsets before fusing.
```

The rig checks emit `AUTONOMY.RIG_SYNC`, `SEQUENCE_COMPLETE`, `EGO_POSE_CONTINUITY`,
`EGO_POSE_NON_FINITE`, `CALIBRATION_INCOMPLETE`, and `SENSOR_FRAME_UNKNOWN` /
`SENSOR_FRAME_UNRELATED` / `SENSOR_FRAME_UNDECLARED` — see
[checks.md](checks.md) for what each one catches and why it matters. On a rig, `RIG_SYNC` supersedes
the pairwise `TEMPORAL.CLOCK_SKEW`.

### The miscalibration a well-formed calibration hides

A rig can carry a complete, connected transform tree and still be unusable, because the chain from a
sensor to the camera does not exist. Nothing about the tree's own shape reveals it. The
`av-miscalibrated` variant is that rig — the LiDAR hangs off a `lidar_mount` frame nothing joins to
`base_link`:

```sh
cargo run -p veridex-core --example make_demo_mcap -- /tmp/av-bad.mcap av-miscalibrated
cargo run -p veridex-cli -- check /tmp/av-bad.mcap
```

```
  [error] AUTONOMY.SENSOR_FRAME_UNRELATED  episode 0 · stream `/lidar/points`
      episode 0: stream `/lidar/points` is in frame `lidar_top`, but no chain of transforms
      connects it to any camera frame (camera_front) — this sensor cannot be projected into the image
      remedy: Publish the missing link joining this sensor's subtree to the camera's
              (typically sensor → base_link → camera), and re-record the calibration.
```

The sibling code `AUTONOMY.SENSOR_FRAME_UNKNOWN` catches the other half: a sensor stamping a frame
name the tree never mentions at all — the calibration was recorded for `lidar_top` while the driver
publishes `lidar_top_v2`. Veridex never decodes point coordinates or pixels, so it does not compute a
reprojection *error*; it verifies the reprojection is defined at all.

## 4. Certify readiness

```sh
cargo run -p veridex-cli -- keygen /tmp/issuer
cargo run -p veridex-cli -- certify /tmp/av.mcap --key /tmp/issuer \
    --out /tmp/av.veridex.json --profile world-model-ready
```

```
certified av — grade C (70), bound to 87cbf54311e1f0c4
  world-model-ready profile: NOT READY
    ✗ autonomy.rig-sync — rig sensors within a 20 ms cross-sensor span drift
    ✓ autonomy.sequence-complete — no rig sensor dropping more than 5% of its frames
    ✓ autonomy.ego-pose-continuity — ego trajectory continuous (no step above 100 m/s implied speed)
    ✗ autonomy.calibration-completeness — connected transform (TF) tree and camera intrinsics present
    ✓ autonomy.sensor-frame-resolution — every sensor's own frame resolves through the tree to a camera
```

Anyone can read that back offline, with no network and no access to your data:

```sh
cargo run -p veridex-cli -- verify /tmp/av.mcap --certificate /tmp/av.veridex.json --key /tmp/issuer.pub
```

See [profiles.md](profiles.md) for the profile's criteria and the full verification output.

## What Veridex reads from a rig log

| Source | Becomes |
|---|---|
| ROS/ROS 2 MCAP topics (`PointCloud2`, `Image`/`CameraInfo`, `Imu`, `NavSatFix`, `Odometry`, TF) | streams with rig modalities, camera intrinsics, the transform tree, the ego trajectory |
| ROS 2 **rosbag2** (`.db3` or `.db3.zstd`, the `sqlite3` storage plugin) — a bag directory or a bare shard | the same, from the same message types; the bag's `metadata.yaml` also supplies the recording distribution and the message total the recording is reconciled against |
| CAN frames + a `.dbc` | one named signal stream per `Message.Signal`, both Intel and Motorola byte order (point the CLI at the directory) |
| ASAM MDF/MF4 (`.mf4`) | one stream per measured channel, on the group's time master, with linear conversions applied |
| Producer metadata | rig lineage (firmware, platform, drive, region, map, redaction/consent) as provenance |
| Scenario/map references (`.xosc`, `.xodr`, OSI, simulator) | provenance, with the version read from the sidecar's own ASAM header |

Veridex never decodes the bulk point or pixel payload — it fingerprints it. That is why calibration
is checked for *presence and coherence* rather than by reprojecting points, and why a huge log costs
little to check.

## Limits worth knowing

- **MF4 coverage is the uncompressed core.** Compressed (`##DZ`) or listed (`##DL`) data blocks,
  unsorted data groups, bit-packed channels, and lookup-table conversions are reported as unmapped
  rather than decoded — `veridex inspect` lists exactly what was skipped.
- **Per-sensor trigger/latency offsets** aren't modeled yet, so a rig with known constant offsets
  will report that drift as sync spread.
- **A latched topic is exempt only where the source says it is latched.** `AUTONOMY.RIG_SYNC`
  compares sensors only, so the topics recorded beside a rig (`/rosout`, `/parameter_events`,
  `/diagnostics`) do not fail it. A transform tree published once at startup is additionally exempt
  from `STRUCTURAL.SINGLE_FRAME_STREAM`, `TEMPORAL.START_OFFSET`, `TEMPORAL.END_OFFSET` and
  `TEMPORAL.CLOCK_SKEW` — but only when its recorded QoS declares transient-local durability
  (rosbag2's `topics.offered_qos_profiles`, or the same on an MCAP channel). A source that records
  no QoS still draws those warnings, and that is deliberate: a latched topic and a sensor that fired
  once and died are identical in the data, so silencing the second to spare the first is the error
  worth avoiding.
- **Coverage is never prescriptive.** Veridex reports scenario distribution and sparse cells; the
  right target balance is your call.
