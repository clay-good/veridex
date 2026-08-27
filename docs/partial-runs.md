# Partial runs: budgets, sampling, and manifest-only checks

A run that looked at less than the whole dataset is not a smaller version of a full run — it is a
different claim. This page covers the three ways a run can be partial, and the one rule they share:
**a partial run is never presented as a whole-dataset one**, and cannot be used to gate on a score or
to issue a certificate.

Veridex reads untrusted files, so ingestion carries two budgets. A **frame budget** (20M by default):
a dataset that would materialize more frames is refused with a clear error rather than exhausting
memory, because the frame count is a product of two numbers the file itself controls. And a
**decompression budget** for compressed containers (MCAP chunks and LeRobot Parquet), capping
expansion at 100x the file's own size with a 64 MiB floor — so a small file cannot unpack into a
gigabyte, while a genuinely large log keeps a proportionate allowance. Raise either with `--max-frames <n>` /
`--max-decompression-ratio <n>`, or remove it with `0`.

For a dataset too large to check in full on every commit, `check` and `inspect` can validate a subset
of its episodes:

```sh
veridex check my-dataset/ --sample-episodes 20             # the first 20 episodes by index
veridex check my-dataset/ --sample-fraction 0.1 --sample-seed 7   # a deterministic 10% draw
```

The draw is resolved from the dataset's declared episode set *before* any data is read, so the
episodes you skipped cost nothing — a sample of a dataset over the frame budget succeeds where the
full ingest is refused. The same seed always draws the same episodes. Sampling applies to LeRobot,
RLDS/TFDS, HDF5, and Zarr (which have an episode axis); MCAP, ROS 2 rosbag2, CAN+DBC, and MF4 ingest a recording as one episode and refuse the request rather than handing
back everything labelled as a sample.

A sampled run is never presented as a whole-dataset one. The verdict carries a `coverage` field
(bound into its hash), every report states the sample and the episode count, and **`certify` refuses
to issue a certificate from a partial run** — a certificate speaks for a dataset, and the episodes a
sample never read are exactly where the problem would be.

For a dataset too large to read on every commit at all, `check` and `inspect` can also skip the data
entirely and check what the dataset *says about itself*:

```sh
veridex check my-dataset/ --metadata-only     # LeRobot; reads meta/, opens no Parquet or video
```

This is a real check, not a smoke test: the declared episode set and per-episode lengths, every
feature's dtype/shape/rate, the stored statistics in `meta/stats.json` (an inverted range or a mean
outside its own bounds is caught here), and the whole provenance family. What it cannot see — every
timestamp, value, content hash, and media header — it says it cannot see. The frame-dependent checks
**abstain rather than firing**: "declares 120 frames, ingested 0" is true of every sound dataset read
this way, and reporting it would fail them all. The verdict carries
`coverage: {"kind": "metadata_only"}`, every report prints a `METADATA-ONLY` banner, and `certify`
refuses it — a sound manifest says nothing about the data. One nice detail: when the episode set is
derived from `info.json`'s `total_episodes` alone, the declared-episode-count check is *withheld*
rather than run, because comparing that number against a set built from it could not fail, and a
check that cannot fail must not be reported as having passed.


## Why the refusals exist

The data score starts at 100 and only deducts, so anything that stops a check from measuring *raises*
it. That makes every form of partiality a way to score better by looking less — which is why
`--min-score` is refused over a sampled or metadata-only run, and why `certify` refuses one outright.
The coverage note travels in the verdict, in every report shape, and under the certificate's
signature.
