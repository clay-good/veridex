# Autonomy quickstart

Check a multi-sensor rig log — LiDAR, cameras, IMU, GNSS, ego-odometry, CAN — and issue a signed
**world-model readiness** certificate. Five minutes, no dataset of your own required.

## 1. Make a rig log

The repo ships a synthetic five-sensor rig with a deliberately desynced IMU:

```sh
cargo run -p veridex-demo --example make_demo_mcap -- /tmp/av.mcap av
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
  Status:   FAIL   Trust: 73 (C)  [data 77 · provenance 66%]
  [error] AUTONOMY.RIG_SYNC  episode 0
      rig sensors are out of sync — `/imu/data` spans 700.0 ms but `/odom` spans 1000.0 ms,
      a 300.0 ms drift across 5 sensors
      remedy: Re-synchronize the rig against a common time base, or record and apply per-sensor
              trigger/latency offsets before fusing.
```

Three findings, all about the same rig fault: the IMU stops 300 ms early, so `RIG_SYNC` reports the
spread and `TEMPORAL.END_OFFSET` reports the tail, and the rig has cameras but no `CameraInfo` to
project points into them. Nothing here is about `/tf_static`, which this demo publishes latched —
exactly as a real ROS 2 stack does — so the checks that ask whether a stream covers the recording's
window leave it alone.

The rig checks emit `AUTONOMY.RIG_SYNC`, `SEQUENCE_COMPLETE`, `EGO_POSE_CONTINUITY`,
`EGO_POSE_NON_FINITE`, `CALIBRATION_INCOMPLETE` / `CALIBRATION_AMBIGUOUS`,
`GNSS_IMPLAUSIBLE` / `GNSS_UNSET`, and
`SENSOR_FRAME_UNKNOWN` /
`SENSOR_FRAME_UNRELATED` / `SENSOR_FRAME_UNDECLARED` — see
[checks.md](checks.md) for what each one catches and why it matters. On a rig, `RIG_SYNC` supersedes
the pairwise `TEMPORAL.CLOCK_SKEW`.

### The miscalibration a well-formed calibration hides

A rig can carry a complete, connected transform tree and still be unusable, because the chain from a
sensor to the camera does not exist. Nothing about the tree's own shape reveals it. The
`av-miscalibrated` variant is that rig — the LiDAR hangs off a `lidar_mount` frame nothing joins to
`base_link`:

```sh
cargo run -p veridex-demo --example make_demo_mcap -- /tmp/av-bad.mcap av-miscalibrated
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

### The tree that is connected and still not a tree

Every question above — is the tree in one piece, is the LiDAR in it, does a chain reach the camera —
walks the frame graph **undirected**. All three answer whether two sensors *can* be related, and none
says whether the answer is **unique**. The `av-ambiguous-tf` variant is a rig where it is not: two
nodes each publish a transform for `lidar_top`, one from `base_link` and one from a `lidar_mount`
that is itself on `base_link`.

```sh
cargo run -p veridex-demo --example make_demo_mcap -- /tmp/av-amb.mcap av-ambiguous-tf
cargo run -p veridex-cli -- check /tmp/av-amb.mcap
```

```
  [error] AUTONOMY.CALIBRATION_AMBIGUOUS  dataset
      frame `lidar_top` is given 2 different parents at the same time (base_link, lidar_mount)
      — its place on the rig depends on which chain a consumer happens to resolve
      remedy: Publish exactly one parent per frame: remove the duplicate broadcaster, or re-parent
              the sensor under the mount it is actually measured against.
```

Reported once, at dataset scope: the calibration is one document, so what is wrong *with* it is one
defect however many episodes the rig recorded.

Nothing else moves: the tree is one connected component and every sensor still reaches the camera, so
`CALIBRATION_INCOMPLETE` and every `SENSOR_FRAME_*` code stay silent — which is exactly why this
needed its own rule. The same code covers a **cycle** (`base_link` → `lidar` → `radar` →
`base_link`), where every frame has one parent and the rig has no root. A re-parenting across
*disjoint* validity windows is a recalibration, not an ambiguity, and is not reported.

### A LiDAR that recorded nothing

A driver that lost its sensor does not stop publishing. It keeps emitting a well-formed
`PointCloud2` at the configured rate, in the right coordinate frame, declaring the same four fields
— with `width` of zero. Every other family passes on it: the structural checks see frames, the
temporal checks see a clean 10 Hz with no jitter, and `autonomy.sensor-frame-resolution` places the
sensor in the transform tree. The `av-dead-lidar` variant is that rig.

```sh
cargo run -p veridex-demo --example make_demo_mcap -- /tmp/av-dead.mcap av-dead-lidar
cargo run -p veridex-cli -- check /tmp/av-dead.mcap
```

```
  [error] AUTONOMY.POINT_CLOUD_EMPTY  episode 0 · stream `/lidar/points`
      episode 0: stream `/lidar/points` published 11 point cloud(s) and every one of them was empty
      — the messages have the schema, the rate and the coordinate frame of a working sensor and none
      of its data
      remedy: Check the sensor and its driver for the recording (power, network, the driver's own
              diagnostics) and re-record; the segment holds no point data to recover.
```

The count comes from each message's own `height × width`, stated in the header ahead of the bulk
blob — no point is decoded. A count is believed only when the body's own length invariants hold, so
a mislabelled topic or a truncated write abstains rather than reporting a fabricated one. When only
*some* sweeps are empty the sensor cut out mid-recording, which is `AUTONOMY.POINT_CLOUD_DROPPED` at
warning: the recording holds real data either side of the dropout.

## 4. Certify readiness

```sh
cargo run -p veridex-cli -- keygen /tmp/issuer
cargo run -p veridex-cli -- certify /tmp/av.mcap --key /tmp/issuer \
    --out /tmp/av.veridex.json --profile world-model-ready
```

```
certified av — fail, grade C (73), bound to ceaae7feeb2b0c09
  world-model-ready profile: NOT READY
    ✗ autonomy.rig-sync — rig sensors within a 20 ms cross-sensor span drift
    ✓ autonomy.sequence-complete — no rig sensor dropping more than 5% of its frames
    ✓ autonomy.ego-pose-continuity — ego trajectory continuous (no step above 100 m/s implied speed)
    ✗ autonomy.calibration-completeness — a connected, unambiguous transform (TF) tree and camera intrinsics present, and arithmetically usable
    ✓ autonomy.sensor-frame-resolution — every sensor's own frame resolves through the tree to a camera
    ✓ autonomy.gnss-plausibility — every satellite fix is a possible place, and the receiver actually had one
    ✓ autonomy.point-cloud-density — every point-cloud sensor actually recorded points
```

A certificate is issued for a failing dataset too — it records what is true, and what is true here is
`fail`. It is the *readiness* verdict, not the issuance, that a consumer gates on.

Anyone can read that back offline, with no network and no access to your data:

```sh
cargo run -p veridex-cli -- verify /tmp/av.mcap --certificate /tmp/av.veridex.json --key /tmp/issuer.pub
```

See [profiles.md](profiles.md) for the profile's criteria and the full verification output.

## What Veridex reads from a rig log

| Source | Becomes |
|---|---|
| ROS/ROS 2 MCAP topics (`PointCloud2`, `Image`/`CameraInfo`, `Imu`, `NavSatFix`, `Odometry`, TF) | streams with rig modalities, camera intrinsics, the transform tree, the ego trajectory |
| ROS 2 **rosbag2** — a bag directory (either storage plugin: `sqlite3` `.db3`/`.db3.zstd`, or the `.mcap` shards Jazzy records by default) or a bare `.db3` | the same, from the same message types; the bag's `metadata.yaml` also supplies the recording distribution and the message total the recording is reconciled against |
| CAN frames + a `.dbc` | one named signal stream per `Message.Signal`, both Intel and Motorola byte order (point the CLI at the directory); each `BO_` line's transmitting ECU becomes `provenance.sensor` |
| ASAM MDF/MF4 (`.mf4`) | one stream per measured channel, on that channel group's time master, sliced at any bit offset and width — how bus signals are actually stored — with every numeric `##CC` conversion applied (linear, rational, both value-to-value tables, value-range-to-value); the converted values are measured, so a channel at its end-stop is `STATISTICAL.SATURATED`; the `##SI` acquisition sources become `provenance.sensor` |
| Producer metadata | rig lineage (firmware, platform, drive, region, map, redaction/consent) as provenance — from an MCAP `Metadata` record or a rosbag2 `custom_data` map, through one shared key table |
| Scenario/map references (`.xosc`, `.xodr`, OSI, simulator) | provenance, with the version read from the sidecar's own ASAM header |

CAN traffic the `.dbc` does not define is not silently dropped: frames on an undefined id, and log
lines that are not candump frames, are disclosed as unread and raise `COVERAGE.SOURCE_UNREAD` — a
partial DBC otherwise reads as a clean pass over whichever fraction of the bus it happened to cover.

Veridex never decodes the bulk point or pixel payload — it fingerprints it. That is why calibration
is checked for *presence and coherence* rather than by reprojecting points, and why a huge log costs
little to check.

## Limits worth knowing

- **MF4 coverage is the record stream, sorted or not.** All four shapes a group's data arrives in are read:
  an uncompressed `##DT`, a deflated `##DZ` (plain or byte-column transposed), a `##DL` data list
  splitting the records across several of those, and an `##HL` header list wrapping such a list.
  Unsorted data groups — several rasters interleaved behind their record ids, the way a bus logger
  writes them — are demultiplexed and decoded too. What is still declined: a `##DZ` holding something
  other than a `DT` record stream, a data list whose elements do not all resolve, a record tagged
  with an id no channel group claims, and a variable-length signal-data group. Because those samples
  *are* in the file, they are disclosed as **unread**: a `COVERAGE.SOURCE_UNREAD` warning in the
  verdict, not a note only `inspect` prints. Bit-packed *little-endian* signals are decoded at any
  offset and any width up to 64 bits, which is how bus traffic is stored; the big-endian equivalent is
  declined, since MDF's bit numbering for a straddling Motorola field is not the DBC sawtooth.
  Lookup-table conversions stay unmapped: the CDM has no shape for them. A `--metadata-only` run
  describes a measurement from its header tree without decompressing anything, which is the cheapest
  way to inventory a large one.
- **An MF4 names its own hardware.** The `##SI` acquisition source on a channel group or channel —
  the ECU, bus, I/O device or tool the samples came from — becomes `provenance.sensor`, qualified by
  its bus or path. It is the one provenance element the format carries natively; the other five come
  from `veridex certify` inputs or a richer source.
- **Per-sensor trigger/latency offsets aren't modeled yet**, and it is worth being precise about what
  that costs. A sensor triggered a constant 200 ms after the rest is *shifted*, not drifting: every
  span is still the same length, so `AUTONOMY.RIG_SYNC` is silent — correctly. What the rig does
  report is `TEMPORAL.START_OFFSET` and its mirror `TEMPORAL.END_OFFSET`, which are true statements:
  that sensor does start and end later than its peers on the same clock. Veridex has no way to tell a
  known, deliberate latency from an accidental one, so it reports what it measured; a declared
  offset table is a follow-up, and a run that simply disables those two checks loses everything else
  they catch.
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
