"""Regenerate the Zarr fixtures.

Run from this directory:

    python3 -m pip install "zarr<3" numcodecs
    python3 generate_fixtures.py

These are **real `zarr` + `numcodecs` stores**, not files this repository's own code wrote: a reader
tested only against its own writer proves the two agree with each other, not that either agrees with
the format. The per-row SHA-256 values the script prints are the ones `tests/zarr_adapter.rs` pins,
so regenerating means updating those.

`v3_store.zarr` is the one exception — a hand-written four-line `zarr.json`, because zarr 2.x cannot
write a v3 store and the only thing the test needs is for it to be recognized and refused by version.

| Store | What it covers |
|---|---|
| `dp_replay.zarr` | The Diffusion Policy replay-buffer layout: `data/*` + `meta/episode_ends`, one codec per array, and an image chunked smaller than itself on the inner axes |
| `codec_zoo.zarr` | One array per codec over identical values, so every codec must decode to the same bytes |
| `blosclz.zarr`, `bitshuffle.zarr`, `fortran_order.zarr` | Encodings this reader refuses by name rather than mis-decoding |
| `ends_backwards.zarr`, `ends_past_rows.zarr` | Episode boundaries that contradict the arrays they index |
| `ends_short.zarr` | Rows past the last boundary, which belong to no episode |
| `timed.zarr`, `untimed.zarr` | A timeline with declared `units`, and one without |
| `sparse_fill.zarr` | An array only partly written, so unwritten chunks must read as the declared `fill_value` (`"NaN"`, and `-1`) rather than as zeros |
| `v3_store.zarr` | A Zarr v3 store (hand-written), refused by version |
| `blosc_real.zarr` | Blosc arrays large enough to be genuinely *compressed*, several forced to many blocks — the codec ids and the per-block shuffle, both of which were wrong while every fixture was small enough for blosc to store it verbatim |
| `dtype_edges.zarr` | A `<U5` array (whose size is characters, not bytes) and a `<f2` array whose values this reader does not decode |
| `group_layout.zarr` | Two episode groups that must not see each other's arrays |
| `group_time.zarr` | Two episode groups where only one records a timeline |
| `bare_array.zarr` | A store that *is* a single array |
| `ends_empty.zarr` | A boundary pair spanning nothing, which stays an empty episode |
| `zero_width.zarr`, `huge_dtype.zarr`, `bad_attrs.zarr`, `fill_bomb.zarr` | Hand-written hostile metadata: a zero-length dimension, an element width that overflows the arithmetic derived from it, an unparseable `.zattrs`, and gigabyte chunks with no chunk files at all |

When regenerating a `U`-dtype fixture, hash `array[i:i+1].tobytes()` rather than `array[i].tobytes()`:
extracting one element yields a NumPy scalar whose width shrinks to its own content, so the latter
hashes 12 bytes for `"abc"` where the array stores 20. The stored width is what a reader sees.
"""

import hashlib
import json
import os
import shutil

import numcodecs
import numpy as np
import zarr


def fresh(name):
    if os.path.exists(name):
        shutil.rmtree(name)
    return zarr.open(name, mode="w")


rng = np.random.default_rng(5)

# The Diffusion Policy replay-buffer layout, one codec per array, with `img` chunked smaller than
# itself on the inner axes so the chunk-to-row assembly is exercised on a ragged edge.
root = fresh("dp_replay.zarr")
data, meta = root.create_group("data"), root.create_group("meta")
action = rng.random((10, 2)).astype(np.float32)
state = rng.random((10, 5)).astype(np.float64)
img = (rng.random((10, 4, 6, 3)) * 255).astype(np.uint8)
data.create_dataset(
    "action", data=action, chunks=(4, 2),
    compressor=numcodecs.Blosc(cname="lz4", clevel=5, shuffle=1),
)
data.create_dataset("state", data=state, chunks=(4, 5), compressor=numcodecs.Zstd(level=3))
data.create_dataset("img", data=img, chunks=(3, 2, 4, 2), compressor=numcodecs.Zlib(level=5))
meta.create_dataset("episode_ends", data=np.array([4, 10], dtype=np.int64), chunks=(2,))
root.attrs["author"] = "veridex test fixture"
root.attrs["task"] = "push the block"
for name, array in [("action", action), ("state", state), ("img", img)]:
    print(name, [hashlib.sha256(array[i].tobytes()).hexdigest() for i in range(array.shape[0])])

# Every codec this reader implements, over identical content: they must all decode to the same rows.
root = fresh("codec_zoo.zarr")
values = rng.random((6, 3)).astype(np.float32)
for label, compressor in [
    ("none", None),
    ("zlib", numcodecs.Zlib(level=6)),
    ("gzip", numcodecs.GZip(level=6)),
    ("zstd", numcodecs.Zstd(level=5)),
    ("lz4", numcodecs.LZ4()),
    ("blosc_lz4_shuffle", numcodecs.Blosc(cname="lz4", shuffle=1)),
    ("blosc_zstd_noshuffle", numcodecs.Blosc(cname="zstd", shuffle=0)),
    ("blosc_zlib_shuffle", numcodecs.Blosc(cname="zlib", shuffle=1)),
]:
    root.create_dataset(label, data=values, chunks=(2, 3), compressor=compressor)
print("codec_zoo", [hashlib.sha256(values[i].tobytes()).hexdigest() for i in range(values.shape[0])])

# Encodings this reader refuses rather than mis-decoding.
fresh("blosclz.zarr").create_dataset(
    "action", data=values, chunks=(2, 3),
    compressor=numcodecs.Blosc(cname="blosclz", shuffle=1),
)
fresh("bitshuffle.zarr").create_dataset(
    "action", data=values, chunks=(2, 3),
    compressor=numcodecs.Blosc(cname="lz4", shuffle=2),
)
fortran = fresh("fortran_order.zarr")
array = fortran.zeros("action", shape=(4, 3), chunks=(2, 3), dtype="<f4", order="F")
array[:] = values[:4]


def replay(name, ends, rows=8):
    root = fresh(name)
    data, meta = root.create_group("data"), root.create_group("meta")
    data.create_dataset(
        "action", data=rng.random((rows, 2)).astype(np.float32), chunks=(4, 2)
    )
    meta.create_dataset("episode_ends", data=np.array(ends, dtype=np.int64), chunks=(4,))


replay("ends_backwards.zarr", [4, 2, 8])
replay("ends_past_rows.zarr", [4, 12])
replay("ends_short.zarr", [4, 6])  # two rows past the last boundary

# A timeline with declared units, and one without.
for name, units in [("timed.zarr", "s"), ("untimed.zarr", None)]:
    root = fresh(name)
    data, meta = root.create_group("data"), root.create_group("meta")
    n = 6
    data.create_dataset("action", data=rng.random((n, 2)).astype(np.float32), chunks=(3, 2))
    stamps = data.create_dataset(
        "timestamp", data=(np.arange(n) * 0.05).astype(np.float64), chunks=(3,)
    )
    if units:
        stamps.attrs["units"] = units
    meta.create_dataset("episode_ends", data=np.array([3, 6], dtype=np.int64), chunks=(2,))

# An array written only in part: the chunks covering the gap are not in the store at all, and read as
# the declared fill value. Zarr writes `"NaN"` for an unwritten float array by default, so reading a
# missing chunk as zeros would turn missing data into plausible data.
root = fresh("sparse_fill.zarr")
data, meta = root.create_group("data"), root.create_group("meta")
state = data.create_dataset(
    "state", shape=(6, 2), chunks=(2, 2), dtype="<f8",
    fill_value=float("nan"), compressor=numcodecs.Zstd(level=3),
)
state[0:2] = np.array([[1.0, 2.0], [3.0, 4.0]])
state[4:6] = np.array([[5.0, 6.0], [7.0, 8.0]])
count = data.create_dataset("count", shape=(6,), chunks=(2,), dtype="<i4", fill_value=-1)
count[0:2] = np.array([7, 8], dtype=np.int32)
meta.create_dataset("episode_ends", data=np.array([6], dtype=np.int64), chunks=(1,))
for name, array in [("sparse state", state[:]), ("sparse count", count[:])]:
    print(name, [hashlib.sha256(array[i].tobytes()).hexdigest() for i in range(array.shape[0])])

# Blosc arrays large enough that blosc actually compresses them, several forced to many blocks. The
# first fixture set was all small arrays, which blosc stores verbatim (the MEMCPYED flag) — so the
# codec ids, block offsets, split streams and shuffle were never exercised, and two were wrong.
root = fresh("blosc_real.zarr")
data, meta = root.create_group("data"), root.create_group("meta")
for label, cname, shuffle, blocksize, dtype in [
    ("lz4_shuf_multi", "lz4", 1, 1024, "<i4"),
    ("zstd_shuf_multi", "zstd", 1, 1024, "<f8"),
    ("zlib_shuf_multi", "zlib", 1, 1024, "<f8"),
    ("lz4hc_shuf", "lz4hc", 1, 0, "<i2"),
    ("zstd_noshuf", "zstd", 0, 1024, "<f4"),
    ("lz4_u1_multi", "lz4", 1, 512, "|u1"),
]:
    rows, width = 8, 2048
    array = (np.arange(rows * width) % 97).astype(dtype).reshape(rows, width)
    data.create_dataset(
        label, data=array, chunks=(rows, width),
        compressor=numcodecs.Blosc(cname=cname, clevel=5, shuffle=shuffle, blocksize=blocksize),
    )
    print(label, [hashlib.sha256(array[i].tobytes()).hexdigest() for i in range(rows)])
meta.create_dataset("episode_ends", data=np.array([8], dtype=np.int64), chunks=(1,))

# A `U` dtype (sized in characters) and a `f2` dtype (whose values this reader does not decode).
root = fresh("dtype_edges.zarr")
data, meta = root.create_group("data"), root.create_group("meta")
labels = np.array(["abc", "de", "fghij", "x"], dtype="<U5")
data.create_dataset("labels", data=labels, chunks=(2,))
data.create_dataset("half", data=np.array([1.0, 2.0, np.nan, np.inf], dtype="<f2"), chunks=(2,))
data.create_dataset("action", data=np.arange(8, dtype="<f4").reshape(4, 2), chunks=(2, 2))
meta.create_dataset("episode_ends", data=np.array([4], dtype=np.int64), chunks=(1,))
print("labels", [hashlib.sha256(labels[i : i + 1].tobytes()).hexdigest() for i in range(4)])

# Two episode groups, which must not see each other's arrays.
root = fresh("group_layout.zarr")
for index, rows in [(0, 3), (1, 4)]:
    group = root.create_group(f"ep_{index}")
    action = (np.arange(rows * 2) + index * 100).astype(np.float32).reshape(rows, 2)
    group.create_dataset("action", data=action, chunks=(2, 2))
    state = (np.arange(rows * 3) + index * 100).astype(np.float32).reshape(rows, 3)
    group.create_group("obs").create_dataset("state", data=state, chunks=(2, 3))
    print(f"ep_{index} action", [hashlib.sha256(action[i].tobytes()).hexdigest() for i in range(rows)])

# Two episode groups where only one records a timeline.
root = fresh("group_time.zarr")
first = root.create_group("ep_0")
first.create_dataset("action", data=np.zeros((3, 2), dtype=np.float32), chunks=(3, 2))
stamps = first.create_dataset("timestamp", data=np.array([10.0, 11.0, 12.0]), chunks=(3,))
stamps.attrs["units"] = "s"
root.create_group("ep_1").create_dataset(
    "action", data=np.ones((3, 2), dtype=np.float32), chunks=(3, 2)
)

# A store that is a single array.
shutil.rmtree("bare_array.zarr", ignore_errors=True)
zarr.open("bare_array.zarr", mode="w", shape=(5, 2), chunks=(2, 2), dtype="<f4")[:] = (
    np.arange(10, dtype=np.float32).reshape(5, 2)
)

# A boundary pair that spans nothing.
root = fresh("ends_empty.zarr")
data, meta = root.create_group("data"), root.create_group("meta")
data.create_dataset("action", data=np.arange(12, dtype=np.float32).reshape(6, 2), chunks=(3, 2))
meta.create_dataset("episode_ends", data=np.array([3, 3, 6], dtype=np.int64), chunks=(3,))

# Hand-written hostile metadata. zarr will not write any of these.
def hand_written(name, zarray):
    shutil.rmtree(name, ignore_errors=True)
    os.makedirs(f"{name}/data/values")
    open(f"{name}/.zgroup", "w").write('{"zarr_format": 2}')
    open(f"{name}/data/.zgroup", "w").write('{"zarr_format": 2}')
    open(f"{name}/data/values/.zarray", "w").write(json.dumps(zarray))


base = {"zarr_format": 2, "compressor": None, "fill_value": 0, "order": "C", "filters": None}
hand_written("zero_width.zarr", {**base, "shape": [3, 0], "chunks": [1, 1], "dtype": "<f4"})
hand_written("huge_dtype.zarr", {**base, "shape": [2, 1], "chunks": [1, 1], "dtype": "<f4294967295"})
hand_written(
    "fill_bomb.zarr",
    {**base, "shape": [2, 1], "chunks": [1, 1000000000], "dtype": "<f4", "fill_value": 1.5},
)
shutil.rmtree("bad_attrs.zarr", ignore_errors=True)
shutil.copytree("bare_array.zarr", "bad_attrs.zarr")
open("bad_attrs.zarr/.zattrs", "w").write("{not json at all")

print(sorted(n for n in os.listdir(".") if n.endswith(".zarr")))
