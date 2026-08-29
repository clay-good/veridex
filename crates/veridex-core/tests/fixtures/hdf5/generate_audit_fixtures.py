"""Regenerate the HDF5 fixtures added after the multi-agent audit of the adapter.

Run from this directory with h5py installed:

    python3 -m pip install h5py
    python3 generate_audit_fixtures.py

Each file exists to exercise one thing the audit found untested. See README.md for the table, and
note that regenerating changes the per-row SHA-256 values pinned in tests/hdf5_adapter.rs — the
script prints the ones the tests assert.
"""

import hashlib
import os

import h5py
import numpy as np

# Chunks smaller than the dataset on every axis, with a ragged edge on each: the case that catches a
# wrong destination stride, a bad odometer carry, or missing edge clipping in the chunk-to-row copy.
rng = np.random.default_rng(23)
with h5py.File("chunked_ragged.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    image = (rng.random((5, 6, 8, 3)) * 255).astype(np.uint8)
    g.create_dataset("obs/agentview_image", data=image, chunks=(2, 3, 5, 2), compression="gzip")
    grid = rng.random((5, 7, 5)).astype(np.float32)
    g.create_dataset("grid", data=grid, chunks=(2, 3, 2))
    series = rng.random(7).astype(np.float64)
    g.create_dataset("scalar_series", data=series, chunks=(3,))
    for name, array in [("image", image), ("grid", grid), ("series", series)]:
        digests = [hashlib.sha256(array[i].tobytes()).hexdigest() for i in range(array.shape[0])]
        print(f"chunked_ragged {name}:", digests)

rng = np.random.default_rng(31)

# Every unit spelling the adapter accepts, plus one it must refuse.
with h5py.File("units_zoo.h5", "w") as f:
    d = f.create_group("data")
    for i, (unit, values) in enumerate(
        [
            ("ms", [0.0, 50.0, 100.0, 150.0]),
            ("us", [0.0, 50000.0, 100000.0, 150000.0]),
            ("ns", [0.0, 5e7, 1e8, 1.5e8]),
            (" S ", [0.0, 0.05, 0.10, 0.15]),
            ("furlongs", [0.0, 1.0, 2.0, 3.0]),
        ]
    ):
        g = d.create_group(f"demo_{i}")
        t = g.create_dataset("timestamps", data=np.array(values, dtype=np.float64))
        t.attrs["units"] = unit
        g.create_dataset("actions", data=rng.random((4, 2)).astype(np.float32))

# A non-finite value in a timeline that does declare its units.
with h5py.File("nan_timestamp.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    t = g.create_dataset("timestamps", data=np.array([0.0, 0.1, np.nan, 0.3], dtype=np.float64))
    t.attrs["units"] = "s"
    g.create_dataset("actions", data=np.zeros((4, 2), dtype=np.float32))

# A timeline that describes only part of the episode, and declared counts with no single actual
# count to compare against.
with h5py.File("mismatched_timeline.h5", "w") as f:
    d = f.create_group("data")
    d.attrs["total"] = 6
    g = d.create_group("demo_0")
    g.attrs["num_samples"] = 6
    t = g.create_dataset("timestamps", data=(np.arange(4) * 0.1).astype(np.float64))
    t.attrs["units"] = "s"
    g.create_dataset("actions", data=rng.random((6, 2)).astype(np.float32))

# Everything the reader must name rather than drop.
with h5py.File("disclosure.h5", "w") as f:
    f.attrs["offset_steps"] = np.int32(-7)
    f.attrs["notes_blob"] = "x" * 5000 + "é" + "y" * 100
    f.attrs["shape_attr"] = np.array([1, 2, 3], dtype=np.int32)
    d = f.create_group("data")
    g = d.create_group("demo_0")
    g.create_dataset("actions", data=rng.random((3, 2)).astype(np.float32))
    g.create_dataset("empty_rows", data=np.zeros((0, 2), dtype=np.float32))
    g.create_dataset("scalar_meta", data=np.float32(1.5))
    g.create_dataset("labels", data=np.array(["a", "bb", "ccc"], dtype=h5py.special_dtype(vlen=str)))
    g.create_dataset("channels_7", data=(rng.random((3, 4, 4, 7)) * 255).astype(np.uint8))
    g.create_dataset("float_image_shaped", data=rng.random((3, 6, 8, 3)).astype(np.float32))
    g["self_ref"] = g  # a hard link back to the episode group
    g["alias"] = h5py.SoftLink("/data/demo_0/actions")
    d.create_dataset("sibling_array", data=np.zeros((2,), dtype=np.float32))
    f["a_committed_type"] = np.dtype("f4")

# Link order differs from name order, in both groups and arrays.
with h5py.File("unsorted_names.h5", "w") as f:
    d = f.create_group("data")
    for name, n in [("demo_10", 3), ("demo_2", 3), ("demo_1", 3)]:
        g = d.create_group(name)
        g.create_dataset("rewards", data=np.zeros(n, dtype=np.float64))
        g.create_dataset("actions", data=rng.random((n, 2)).astype(np.float32))
        obs = g.create_group("obs")
        obs.create_dataset("z_last", data=rng.random((n, 1)).astype(np.float32))
        obs.create_dataset("a_first", data=rng.random((n, 1)).astype(np.float32))

# Trailing numbers that collide, so they cannot be the episode index.
with h5py.File("colliding_names.h5", "w") as f:
    d = f.create_group("data")
    for name in ["run_1", "other_1"]:
        g = d.create_group(name)
        g.create_dataset("actions", data=rng.random((2, 2)).astype(np.float32))

# 100 KB on disk declaring a 98 MB chunk.
with h5py.File("bomb.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    g.create_dataset(
        "actions",
        data=np.zeros((24000, 4096), dtype=np.uint8),
        chunks=(24000, 4096),
        compression="gzip",
    )

# Enough links in one group to move them into fractal-heap (dense) storage.
with h5py.File("dense_links.h5", "w", libver="latest") as f:
    g = f.create_group("data").create_group("demo_0")
    for i in range(40):
        g.create_dataset(f"channel_{i:02d}", data=np.zeros((2,), dtype=np.float32))

# Values carrying the faults the statistical checks exist to catch, each in a *non-first* dimension
# so an element-0-only recompute would miss it. n has to be large enough for one spike to clear the
# z >= 10 threshold: with a single outlier the best achievable z is about (n - 1) / sqrt(n).
rng = np.random.default_rng(41)
n = 140
with h5py.File("statistical_faults.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    actions = rng.normal(0, 0.1, (n, 7)).astype(np.float64)
    actions[:, 6] = 1.0   # a gripper pinned at its limit...
    actions[:6, 6] = 0.0  # ...but not constant, so DEGENERATE is not the finding
    actions[70, 3] = 250.0
    g.create_dataset("actions", data=actions)
    state = rng.normal(0, 1.0, (n, 4)).astype(np.float64)
    state[17, 2] = np.nan
    g.create_dataset("obs/joint_state", data=state)
    # 400 values per frame: too wide for per-position statistics. Constant apart from one NaN pixel,
    # so gzip keeps the committed fixture small.
    depth = np.zeros((n, 20, 20), dtype=np.float32)
    depth[5, 4, 4] = np.float32("nan")
    g.create_dataset("obs/depth", data=depth, chunks=(10, 20, 20), compression="gzip")

# What a `robomimic` file holds beside `/data`: the `/mask` group of filter keys, plus a few shapes
# that decide whether a root object is a coverage hole (rows nothing read) or only a note.
rng = np.random.default_rng(53)
with h5py.File("root_siblings.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    g.create_dataset("actions", data=rng.random((3, 2)).astype(np.float32))
    mask = f.create_group("mask")
    mask.create_dataset("train", data=np.array([b"demo_0"]))
    mask.create_dataset("valid", data=np.zeros((4,), dtype=np.int64))
    f.create_dataset("reward_model", data=rng.random((5, 3)).astype(np.float32))
    f.create_group("notes")  # a group holding no arrays
    f.create_dataset("zero_rows", data=np.zeros((0, 2), dtype=np.float32))
    f.create_dataset("scalar_at_root", data=np.float32(2.5))
    f.create_group("logs").create_group("run_0").create_dataset(
        "events", data=np.zeros((2,), dtype=np.int32)
    )  # arrays two levels down: the walk has to recurse to find them

for name in sorted(n for n in os.listdir(".") if n.endswith(".h5")):
    print(name, os.path.getsize(name))
