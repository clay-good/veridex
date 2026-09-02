#!/usr/bin/env python3
"""Regenerate the rosbag2 fixtures under this directory.

The `.db3` files are written by **Python's own `sqlite3` module** — real SQLite, not this
repository's reader spelled backwards. That is the whole point: `crates/veridex-core/src/adapter/
sqlite.rs` is a hand-written reader, and a reader tested only against a writer from the same head
proves the two agree, not that either matches the format. Every fixture here is third-party output,
exactly as `tests/fixtures/hdf5` is real h5py output.

The message bodies are ROS 2 CDR, encoded here by a small writer that mirrors the alignment rules in
`adapter/cdr.rs`. Only the *headers* Veridex decodes are meaningful; the bulk payload of a cloud or
an image is filler, because Veridex never reads it.

Run:  python3 generate_fixtures.py
"""

import os
import shutil
import sqlite3
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))

# rosbag2's sqlite3 storage schema (rosbag2_storage_sqlite3), schema version 4 and later.
SCHEMA = """
CREATE TABLE topics(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  serialization_format TEXT NOT NULL,
  offered_qos_profiles TEXT NOT NULL);
CREATE TABLE messages(
  id INTEGER PRIMARY KEY,
  topic_id INTEGER NOT NULL,
  timestamp INTEGER NOT NULL,
  data BLOB NOT NULL);
CREATE INDEX timestamp_idx ON messages (timestamp ASC);
"""


class Cdr:
    """A CDR (XCDR1, little-endian) writer: what a ROS 2 publisher puts on the wire."""

    def __init__(self):
        # Encapsulation header: CDR_LE, options 0.
        self.buf = bytearray(b"\x00\x01\x00\x00")

    def _pos(self):
        return len(self.buf) - 4

    def align(self, n):
        while self._pos() % n:
            self.buf.append(0)

    def u8(self, v):
        self.buf.append(v & 0xFF)

    def u32(self, v):
        self.align(4)
        self.buf += struct.pack("<I", v & 0xFFFFFFFF)

    def i32(self, v):
        self.align(4)
        self.buf += struct.pack("<i", v)

    def f64(self, v):
        self.align(8)
        self.buf += struct.pack("<d", v)

    def string(self, s):
        raw = s.encode("utf-8") + b"\x00"
        self.u32(len(raw))
        self.buf += raw

    def header(self, frame_id, ts_ns):
        self.i32(ts_ns // 1_000_000_000)
        self.u32(ts_ns % 1_000_000_000)
        self.string(frame_id)

    def raw(self, b):
        self.buf += b

    def bytes(self):
        return bytes(self.buf)


def point_cloud2(frame_id, ts_ns, npoints=8):
    c = Cdr()
    c.header(frame_id, ts_ns)
    c.u32(1)  # height
    c.u32(npoints)  # width
    fields = [("x", 0, 7), ("y", 4, 7), ("z", 8, 7), ("intensity", 12, 7), ("ring", 16, 4)]
    c.u32(len(fields))
    for name, offset, datatype in fields:
        c.string(name)
        c.u32(offset)
        c.u8(datatype)
        c.u32(1)
    c.u8(0)  # is_bigendian
    c.u32(18)  # point_step
    c.u32(18 * npoints)  # row_step
    # `data` — filler. Veridex never reads the points, but it does check that the message's own
    # length invariants hold (`row_step` covers a row of `width` points, `data` is
    # `row_step * height` bytes and those bytes are present) before believing the point count,
    # so the blob has to actually be here and be the right size.
    c.u32(18 * npoints)  # data (sequence<uint8>)
    c.raw(bytes(18 * npoints))
    c.u8(1)  # is_dense
    return c.bytes()


def camera_info(frame_id, ts_ns, width=1920, height=1080):
    c = Cdr()
    c.header(frame_id, ts_ns)
    c.u32(height)
    c.u32(width)
    c.string("plumb_bob")
    d = [-0.31, 0.09, 0.0, 0.0, 0.0]
    c.u32(len(d))
    for v in d:
        c.f64(v)
    fx, fy, cx, cy = 1080.5, 1080.5, 960.0, 540.0
    for v in [fx, 0.0, cx, 0.0, fy, cy, 0.0, 0.0, 1.0]:
        c.f64(v)
    return c.bytes()


def odometry(frame_id, ts_ns, x, y):
    c = Cdr()
    c.header(frame_id, ts_ns)
    c.string("base_link")
    for v in [x, y, 0.0, 0.0, 0.0, 0.0, 1.0]:
        c.f64(v)
    return c.bytes()


def tf_message(ts_ns, edges):
    c = Cdr()
    c.u32(len(edges))
    for parent, child, (tx, ty, tz) in edges:
        c.header(parent, ts_ns)
        c.string(child)
        for v in [tx, ty, tz, 0.0, 0.0, 0.0, 1.0]:
            c.f64(v)
    return c.bytes()


def joint_state(ts_ns, names, positions):
    """A `sensor_msgs/msg/JointState`: header, name[], position[], velocity[], effort[]."""
    c = Cdr()
    c.header("", ts_ns)
    c.u32(len(names))
    for n in names:
        c.string(n)
    c.u32(len(positions))
    for v in positions:
        c.f64(v)
    c.u32(0)  # velocity[]
    c.u32(0)  # effort[]
    return c.bytes()


def header_only(frame_id, ts_ns, filler=64):
    """Any header-first message whose body Veridex does not decode (Image, …).

    Not for `Imu` or `JointState` any more: Veridex decodes both in full, so a stub body there
    leaves the bag path's decoder unexercised and the statistical family abstaining on a sensor the
    fixture calls clean. See `imu()`.
    """
    c = Cdr()
    c.header(frame_id, ts_ns)
    c.raw(bytes(filler))
    return c.bytes()


def imu(frame_id, ts_ns, phase):
    """A real `sensor_msgs/msg/Imu` body: orientation, angular velocity and linear acceleration,
    each followed by its 9-element covariance.

    Veridex decodes this in full, so the bag's IMU carries measured values and the statistical
    family grades it — which is the point of a fixture called `clean_rig`. A leading `-1` in a
    covariance is ROS's "not provided"; these are zero, so every value is measured.
    """
    import math

    c = Cdr()
    c.header(frame_id, ts_ns)
    # Level and driving straight: identity orientation, a little sway, 1 g down.
    groups = [
        [0.0, 0.0, 0.0, 1.0],
        [0.0, 0.0, 0.02 * math.sin(phase)],
        [0.0, 0.0, 9.81 + 0.05 * math.sin(phase * 2.0)],
    ]
    for values in groups:
        for v in values:
            c.f64(v)
        for _ in range(9):
            c.f64(0.0)
    return c.bytes()


def write_bag(path, topics, messages):
    """topics: [(id, name, type, serialization_format, qos)]; messages: [(topic_id, ts, blob)]."""
    if os.path.exists(path):
        os.remove(path)
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    db.executemany("INSERT INTO topics VALUES (?,?,?,?,?)", topics)
    db.executemany(
        "INSERT INTO messages(topic_id, timestamp, data) VALUES (?,?,?)",
        [(t, ts, sqlite3.Binary(blob)) for t, ts, blob in messages],
    )
    db.commit()
    db.close()


def zstd_compress(path):
    """Compress `path` in place to `path + '.zstd'` with the real `zstd` CLI, as rosbag2 does.

    Shelled out rather than done in Python because the point of every fixture here is that a
    third-party writer produced it — the same reason the `.db3` files come from Python's `sqlite3`
    and not from this repository's reader run backwards. Requires `zstd` on PATH; the fixtures are
    committed, so only regenerating them needs it.
    """
    # `-o` because the CLI defaults to `.zst` while rosbag2 writes `.zstd`.
    subprocess.run(["zstd", "-q", "-f", "-o", path + ".zstd", path], check=True)
    os.remove(path)
    return path + ".zstd"


def metadata_yaml(relative_paths, message_count, per_topic, duration_ns, start_ns,
                  compression_format="", compression_mode=""):
    topics = "\n".join(
        f"""    - topic_metadata:
        name: {name}
        type: {typ}
        serialization_format: cdr
        offered_qos_profiles: ""
      message_count: {count}"""
        for name, typ, count in per_topic
    )
    files = "\n".join(f"    - {p}" for p in relative_paths)
    return f"""rosbag2_bagfile_information:
  version: 5
  storage_identifier: sqlite3
  relative_file_paths:
{files}
  duration:
    nanoseconds: {duration_ns}
  starting_time:
    nanoseconds_since_epoch: {start_ns}
  message_count: {message_count}
  topics_with_message_count:
{topics}
  compression_format: "{compression_format}"
  compression_mode: "{compression_mode}"
  ros_distro: humble
"""


START = 1_700_000_000_000_000_000

# What rosbag2 writes into `topics.offered_qos_profiles`: a YAML sequence of the publisher's QoS.
# `durability: 1` is TRANSIENT_LOCAL (latched); `2` is VOLATILE.
QOS_VOLATILE = "- history: 3\n  depth: 0\n  reliability: 1\n  durability: 2\n"
QOS_LATCHED = "- history: 3\n  depth: 0\n  reliability: 1\n  durability: 1\n"

RIG_TOPICS = [
    (1, "/lidar/points", "sensor_msgs/msg/PointCloud2", "cdr", QOS_VOLATILE),
    (2, "/camera/front/image_raw", "sensor_msgs/msg/Image", "cdr", QOS_VOLATILE),
    (3, "/camera/front/camera_info", "sensor_msgs/msg/CameraInfo", "cdr", QOS_VOLATILE),
    (4, "/imu/data", "sensor_msgs/msg/Imu", "cdr", QOS_VOLATILE),
    (5, "/odom", "nav_msgs/msg/Odometry", "cdr", QOS_VOLATILE),
    # Latched, as every real ROS 2 stack publishes it: once at startup, retained for late
    # subscribers.
    (6, "/tf_static", "tf2_msgs/msg/TFMessage", "cdr", QOS_LATCHED),
]

# The topics every real `ros2 bag record -a` also captures: node logging, parameter events, and
# diagnostics. None of them observes the world, and each keeps its own arbitrary cadence.
HOUSEKEEPING_TOPICS = [
    (7, "/rosout", "rcl_interfaces/msg/Log", "cdr", QOS_VOLATILE),
    (8, "/parameter_events", "rcl_interfaces/msg/ParameterEvent", "cdr", QOS_VOLATILE),
    (9, "/diagnostics", "diagnostic_msgs/msg/DiagnosticArray", "cdr", QOS_VOLATILE),
]

# An arm recording: one JointState topic, nothing else. The only ROS message whose whole payload is
# the measurement, and the one that makes the statistical family reachable over a bag.
ARM_TOPICS = [
    (1, "/joint_states", "sensor_msgs/msg/JointState", "cdr", QOS_VOLATILE),
]

TF_EDGES = [
    ("base_link", "lidar_link", (0.0, 0.0, 1.8)),
    ("base_link", "camera_front", (1.2, 0.0, 1.5)),
    ("base_link", "imu_link", (0.0, 0.0, 0.4)),
]


def rig_messages(n_lidar=20, camera_end_scale=1.0):
    """A 2-second rig recording at 10 Hz LiDAR / 20 Hz camera / 100 Hz IMU.

    `camera_end_scale` stretches the camera's inter-frame period, so its stream spans a different
    duration than the others on the same clock — a cross-stream skew.
    """
    msgs = []
    lidar_dt = 100_000_000
    for i in range(n_lidar):
        ts = START + i * lidar_dt
        msgs.append((1, ts, point_cloud2("lidar_link", ts)))
    cam_dt = int(50_000_000 * camera_end_scale)
    for i in range(n_lidar * 2):
        ts = START + i * cam_dt
        msgs.append((2, ts, header_only("camera_front", ts, 256)))
        # Most drivers publish CameraInfo alongside every frame. (Some latch it instead; either
        # way it is not graded as a sensor — see `Modality::is_sensor`.)
        msgs.append((3, ts, camera_info("camera_front", ts)))
    for i in range(n_lidar * 10):
        ts = START + i * 10_000_000
        msgs.append((4, ts, imu("imu_link", ts, i * 0.05)))
    for i in range(n_lidar * 5):
        ts = START + i * 20_000_000
        msgs.append((5, ts, odometry("odom", ts, i * 0.2, 0.0)))
    msgs.append((6, START, tf_message(START, TF_EDGES)))
    msgs.sort(key=lambda m: m[1])
    return msgs


def housekeeping_messages():
    """Node chatter on its own schedule: two log lines early, a parameter event at startup, and
    diagnostics at 1 Hz — none of it covering the recording's full window."""
    msgs = []
    for i in range(2):
        ts = START + 200_000_000 + i * 150_000_000
        msgs.append((7, ts, header_only("", ts, 48)))
    ts = START + 5_000_000
    msgs.append((8, ts, header_only("", ts, 32)))
    for i in range(2):
        ts = START + 100_000_000 + i * 1_000_000_000
        msgs.append((9, ts, header_only("", ts, 40)))
    return msgs


def per_topic_counts(msgs):
    counts = {}
    for topic_id, _, _ in msgs:
        counts[topic_id] = counts.get(topic_id, 0) + 1
    by_id = {t[0]: (t[1], t[2]) for t in RIG_TOPICS + HOUSEKEEPING_TOPICS}
    return [(by_id[i][0], by_id[i][1], counts[i]) for i in sorted(counts)]


def bag_dir(name, msgs, declared_count=None, topics=None, compress=None):
    """A rosbag2 *directory*: one `.db3` plus the `metadata.yaml` a recording always ships.

    `compress` mirrors `--compression-mode`: `"FILE"` compresses the finished shard to `.db3.zstd`
    (what rosbag2 does), `"MESSAGE"` only *declares* per-message compression, which is enough to
    prove Veridex refuses it rather than reading the bag wrong.
    """
    d = os.path.join(HERE, name)
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    db = f"{name}_0.db3"
    write_bag(os.path.join(d, db), topics if topics else RIG_TOPICS, msgs)
    if compress == "FILE":
        zstd_compress(os.path.join(d, db))
        db += ".zstd"
    span = max(m[1] for m in msgs) - min(m[1] for m in msgs)
    with open(os.path.join(d, "metadata.yaml"), "w") as f:
        f.write(
            metadata_yaml(
                [db],
                declared_count if declared_count is not None else len(msgs),
                per_topic_counts(msgs),
                span,
                min(m[1] for m in msgs),
                compression_format="zstd" if compress else "",
                compression_mode=compress or "",
            )
        )


def arm_messages(n=40, pinned=30):
    """A 40-sample, 2-DoF arm recording whose elbow sits hard against its stop for `pinned` of them.

    A saturated actuator: the joint cannot move, so a policy trained on it learns to command a limit
    it can never leave. Nothing about the bag's *structure* is wrong, which is the point — only the
    values say so.
    """
    msgs = []
    for i in range(n):
        ts = START + i * 10_000_000
        shoulder = i * 0.01
        elbow = 2.0 if i < pinned else 2.0 - i * 0.01
        msgs.append((1, ts, joint_state(ts, ["shoulder", "elbow"], [shoulder, elbow])))
    return msgs


def main():
    # A clean five-sensor rig recording, as a full bag directory.
    bag_dir("clean_rig", rig_messages())

    # An arm whose elbow is pinned at its limit — a defect only the values can show.
    bag_dir("pinned_arm", arm_messages(), topics=ARM_TOPICS)

    # The same rig, but the camera runs 1.4x slow on the shared clock: its stream ends well before
    # the others, which is the cross-stream drift TEMPORAL.CLOCK_SKEW exists to find.
    bag_dir("skewed_rig", rig_messages(camera_end_scale=0.6))

    # The same synchronized rig with the topics every `ros2 bag record -a` also captures. Nothing
    # about the rig changed, so nothing about the rig's verdict should.
    noisy = sorted(rig_messages() + housekeeping_messages(), key=lambda m: m[1])
    bag_dir("housekeeping", noisy, topics=RIG_TOPICS + HOUSEKEEPING_TOPICS)

    # The same rig, stored the way any recording large enough to care about is: `ros2 bag record
    # --compression-mode file --compression-format zstd`, which compresses the finished shard and
    # deletes the original.
    bag_dir("compressed_rig", rig_messages(), compress="FILE")

    # A bag that declares per-message compression. Veridex must refuse it by name: the tables are
    # plain, so it *would* read — and would fingerprint compressed bodies and decode no headers,
    # returning an empty rig from a full bag.
    bag_dir("message_compressed", rig_messages(), compress="MESSAGE")

    # A shard that unpacks to 96 MiB of nothing from a few kilobytes on disk. It is not a database
    # and never gets as far as being read as one: the point is that the decompression budget stops
    # the unpacking, rather than charging for it once the memory is already gone.
    d = os.path.join(HERE, "zstd_bomb")
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    bomb = os.path.join(d, "zstd_bomb_0.db3")
    with open(bomb, "wb") as f:
        for _ in range(96):
            f.write(bytes(1024 * 1024))
    zstd_compress(bomb)
    with open(os.path.join(d, "metadata.yaml"), "w") as f:
        f.write(
            metadata_yaml(
                ["zstd_bomb_0.db3.zstd"], 0, [], 0, START,
                compression_format="zstd", compression_mode="FILE",
            )
        )

    # A split recording: `ros2 bag record --max-bag-size` rolls a long bag into `_0`, `_1`, … `_11`.
    # Twelve shards is the smallest count that separates recording order from name order, because a
    # lexicographic sort puts `_10` and `_11` ahead of `_2`.
    d = os.path.join(HERE, "split")
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    split_topics = [RIG_TOPICS[0], RIG_TOPICS[3]]
    names = []
    total = 0
    for k in range(12):
        base = START + k * 1_000_000_000
        msgs = [
            (1, base + i * 100_000_000, point_cloud2("lidar_link", base + i * 100_000_000))
            for i in range(10)
        ] + [
            (4, base + i * 10_000_000, header_only("imu_link", base + i * 10_000_000, 96))
            for i in range(100)
        ]
        msgs.sort(key=lambda m: m[1])
        total += len(msgs)
        name = f"split_{k}.db3"
        names.append(name)
        write_bag(os.path.join(d, name), split_topics, msgs)
    with open(os.path.join(d, "metadata.yaml"), "w") as f:
        f.write(
            metadata_yaml(
                names,
                total,
                [
                    ("/lidar/points", "sensor_msgs/msg/PointCloud2", 120),
                    ("/imu/data", "sensor_msgs/msg/Imu", 1200),
                ],
                12_000_000_000,
                START,
            )
        )

    # A bag caught mid-recording: no `metadata.yaml` yet (rosbag2 writes it when the recorder
    # closes) and a real, uncheckpointed write-ahead log holding 50 committed messages the `.db3`
    # itself does not carry. Generated in a child process that exits without closing the connection,
    # because a clean close checkpoints the WAL away — which is the whole thing being reproduced.
    d = os.path.join(HERE, "recording")
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    shard = os.path.join(d, "recording_0.db3")
    write_bag(shard, RIG_TOPICS, rig_messages())
    child = f"""
import os, sqlite3
db = sqlite3.connect({shard!r}, isolation_level=None)
db.execute("PRAGMA journal_mode=WAL")
db.execute("BEGIN")
for i in range(50):
    db.execute("INSERT INTO messages(topic_id, timestamp, data) VALUES (?,?,?)",
               (1, 1700000002000000000 + i * 1000,
                sqlite3.Binary(b"\\x00\\x01\\x00\\x00" + bytes(64))))
db.execute("COMMIT")
os._exit(0)
"""
    subprocess.run([sys.executable, "-c", child], check=True)
    # The shared-memory index is a runtime artifact, not data; only the log itself is kept.
    shm = shard + "-shm"
    if os.path.exists(shm):
        os.remove(shm)
    assert os.path.getsize(shard + "-wal") > 0, "the WAL must survive for this fixture to mean anything"

    # A bag whose manifest lists only some of its topics, so the per-topic counts do not add up to
    # its own total. Veridex must refuse a metadata-only run over it rather than present a partial
    # inventory as the bag's contents.
    msgs = rig_messages()
    d = os.path.join(HERE, "partial_inventory")
    if os.path.exists(d):
        shutil.rmtree(d)
    os.makedirs(d)
    db = "partial_inventory_0.db3"
    write_bag(os.path.join(d, db), RIG_TOPICS, msgs)
    span = max(m[1] for m in msgs) - min(m[1] for m in msgs)
    with open(os.path.join(d, "metadata.yaml"), "w") as f:
        f.write(
            metadata_yaml(
                [db], len(msgs), per_topic_counts(msgs)[:2], span, min(m[1] for m in msgs)
            )
        )

    # A recording whose `.db3` lost its tail (the process was killed) while metadata.yaml still
    # claims every message it meant to write.
    msgs = rig_messages()
    bag_dir("interrupted", msgs[: len(msgs) - 40], declared_count=len(msgs))

    # A bare `.db3` with no metadata.yaml beside it — what you get when someone hands you the one
    # file out of the bag directory.
    write_bag(os.path.join(HERE, "bare.db3"), RIG_TOPICS, rig_messages())

    # A bag whose messages reference a topic id the `topics` table never declares.
    write_bag(
        os.path.join(HERE, "orphan_topic.db3"),
        RIG_TOPICS[:1],
        [(1, START, point_cloud2("lidar_link", START)), (99, START + 1, b"\x00\x01\x00\x00")],
    )

    # A database that is valid SQLite but is not a rosbag2 bag at all.
    p = os.path.join(HERE, "not_a_bag.db3")
    if os.path.exists(p):
        os.remove(p)
    db = sqlite3.connect(p)
    db.execute("CREATE TABLE notes(id INTEGER PRIMARY KEY, body TEXT)")
    db.execute("INSERT INTO notes(body) VALUES ('nothing to do with robots')")
    db.commit()
    db.close()

    # A message blob far larger than one page, so the reader's overflow-chain path is exercised
    # against a chain real SQLite laid out.
    write_bag(
        os.path.join(HERE, "overflow.db3"),
        RIG_TOPICS[:1],
        [(1, START + i, point_cloud2("lidar_link", START + i, npoints=2_000)) for i in range(2)],
    )

    print("wrote fixtures under", HERE)


if __name__ == "__main__":
    main()
