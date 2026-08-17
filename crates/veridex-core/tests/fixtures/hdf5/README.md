# HDF5 test fixtures

These files are **real `h5py` output**, not files this repository's own code wrote. A reader tested
only against its own writer proves the two agree with each other, not that either agrees with the
format — so the HDF5 adapter is tested against the reference implementation's bytes, and
`tests/hdf5_adapter.rs` pins per-row SHA-256 values taken from `h5py` itself.

They are small on purpose (a few kilobytes each) and are committed so the suite needs no HDF5
toolchain, no Python, and no network.

Regenerate them with `python3 -m pip install h5py` and the scripts below, run from this directory
(the first script writes the first five files; `generate_audit_fixtures.py` in the same directory
writes the rest).
Regenerating changes the row hashes pinned in the tests (the arrays are drawn from a seeded RNG, but
`h5py` also stamps its own version into the file), so update those alongside.

| File | What it covers |
|---|---|
| `robomimic_small.h5` | The `robomimic` layout: `/data/demo_N` episodes, nested `obs/`, a gzip-chunked `uint8` image array, `num_samples` / `total` / `env_args` attributes, a variable-length string attribute (global heap) |
| `timed_rig.h5` | A timestamp array with declared `units`, plus `shuffle` + `deflate` + `fletcher32` filters and a big-endian `float32` array |
| `untimed_units.h5` | A `time` array with **no** `units` attribute — which must not become a clock |
| `flat_single_episode.h5` | Arrays directly at the root: one episode, no `/data` group |
| `libver_latest.h5` | `libver='latest'`: a version-3 superblock and version-2 (`OHDR`) object headers |
| `chunked_ragged.h5` | Chunks smaller than the dataset on *every* axis, with a ragged edge on each — the case that catches a wrong stride or a bad odometer carry in the chunk-to-row copy |
| `units_zoo.h5` | One episode per unit spelling (`ms`, `us`, `ns`, `" S "`) plus one declaring a unit that means nothing |
| `nan_timestamp.h5` | A non-finite value in a timeline that declares units |
| `mismatched_timeline.h5` | A timeline covering only some of the episode's arrays, and a declared count with no single actual count to match |
| `disclosure.h5` | Everything that must be *named* rather than dropped: a soft link, a hard link back to an ancestor, a variable-length array, a scalar array, a zero-row array, an array attribute, an oversized attribute, a negative `int32` attribute, an array beside the episode groups, a committed datatype |
| `unsorted_names.h5` | Link order ≠ name order (`demo_10`, `demo_2`, `demo_1`; arrays written in reverse) |
| `colliding_names.h5` | Group names whose trailing numbers collide (`run_1`, `other_1`), forcing positional indices |
| `dense_links.h5` | 40 links in one group, which moves them into fractal-heap (dense) storage |
| `bomb.h5` | 100 KB on disk declaring a 98 MB chunk — refused on what it declares, before anything is inflated |

```python
import h5py, numpy as np, json
rng = np.random.default_rng(7)

with h5py.File("robomimic_small.h5", "w") as f:
    f.attrs["author"] = "veridex test fixture"
    f.attrs["date"] = "2026-08-16"
    d = f.create_group("data")
    d.attrs["total"] = 9
    d.attrs["env_args"] = json.dumps({"env_name": "Lift", "type": 1})
    for i, n in enumerate([5, 4]):
        g = d.create_group(f"demo_{i}")
        g.attrs["num_samples"] = n
        g.attrs["language_instruction"] = "lift the cube"
        g.create_dataset("actions", data=rng.random((n, 7)).astype(np.float32))
        g.create_dataset("rewards", data=np.zeros(n, dtype=np.float64))
        g.create_dataset("dones", data=np.array([0] * (n - 1) + [1], dtype=np.int64))
        obs = g.create_group("obs")
        obs.create_dataset("robot0_eef_pos", data=rng.random((n, 3)).astype(np.float32))
        obs.create_dataset("agentview_image",
                           data=(rng.random((n, 6, 8, 3)) * 255).astype(np.uint8),
                           chunks=(2, 6, 8, 3), compression="gzip")

rng = np.random.default_rng(11)
with h5py.File("timed_rig.h5", "w") as f:
    d = f.create_group("data")
    for i, n in enumerate([4, 4]):
        g = d.create_group(f"episode_{i}")
        t = g.create_dataset("timestamps", data=(np.arange(n) * 0.05 + i).astype(np.float64))
        t.attrs["units"] = "s"
        g.create_dataset("actions", data=rng.random((n, 2)).astype(np.float32),
                         chunks=(2, 2), shuffle=True, compression="gzip", fletcher32=True)
        g.create_dataset("joint_pos", data=rng.random((n, 3)).astype(">f4"))

with h5py.File("untimed_units.h5", "w") as f:
    g = f.create_group("data").create_group("demo_0")
    g.create_dataset("time", data=(np.arange(3) * 0.1).astype(np.float64))
    g.create_dataset("actions", data=np.zeros((3, 2), dtype=np.float32))

with h5py.File("flat_single_episode.h5", "w") as f:
    f.create_dataset("actions", data=rng.random((3, 2)).astype(np.float32))
    f.create_dataset("observations", data=rng.random((3, 5)).astype(np.float32))

with h5py.File("libver_latest.h5", "w", libver="latest") as f:
    g = f.create_group("data").create_group("demo_0")
    g.create_dataset("actions", data=np.arange(6, dtype=np.float32).reshape(3, 2))
```
