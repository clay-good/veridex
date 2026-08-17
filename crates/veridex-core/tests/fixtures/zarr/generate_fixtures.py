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
"""

import hashlib
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

print(sorted(n for n in os.listdir(".") if n.endswith(".zarr")))
