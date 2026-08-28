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
veridex check my-bag/      --metadata-only    # rosbag2; reads metadata.yaml, opens no .db3
veridex check oxe-dataset/ --metadata-only    # RLDS/TFDS; reads the two manifest files, opens no shard
veridex check buffer.zarr/ --metadata-only    # Zarr; reads .zarray/.zattrs + meta/, opens no data chunk
veridex check drive.mcap   --metadata-only    # MCAP; reads the summary section at the end, opens no chunk
veridex check demos.h5     --metadata-only    # HDF5; reads the group tree and array headers, opens no chunk
veridex check hf://lerobot/svla_so101_pickplace --metadata-only   # the Hub, without downloading it
```

The third form is the same check over a manifest fetched from the Hugging Face Hub — a few hundred
kilobytes beside a repository that is routinely hundreds of gigabytes. Only the manifest is ever
requested: `meta/info.json`, `meta/episodes.jsonl`, `meta/stats.json`, `meta/tasks.jsonl` and the
dataset card, a list fixed in Veridex's own source rather than discovered from anything the server
says. Requests and any redirect they follow are restricted to the Hub's own hosts over HTTPS, and no
credential is sent — so a private or gated dataset answers 401 and is reported as private rather than
having a token quietly forwarded on your behalf. A remote run is a metadata-only run and carries
every refusal that comes with one, so it can neither pass a score gate nor be certified. Nothing else
about Veridex touches a network: a certificate still verifies offline, which is the property the
whole trust chain rests on.

Six formats support it, because six state their structure somewhere other than in their data. For a **rosbag2**
bag it reads `topics_with_message_count` — every topic's name, its ROS type and so its modality, the
declared message total, the recording distribution, the storage and any compression — without
opening a shard, which is the difference between seconds and a terabyte. What it cannot see is
everything a shard would answer: no timestamps, no message bytes, no content hashes, and no decoded
rig calibration or ego trajectory, since those come from message *bodies*. It says all of that
rather than leaving it inferred, and it refuses two cases outright rather than approximating them: a
bare `.db3` has no manifest at all, and a manifest whose per-topic counts do not add up to its own
total means Veridex did not read the whole inventory — presenting three topics out of twelve as the
bag's contents is invisible to the caller, so the run is refused naming both numbers.

For **LeRobot** this is a real check, not a smoke test: the declared episode set and per-episode lengths, every
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

For **RLDS/TFDS** it is the mode the Open X-Embodiment corpus asks for: those directories run to
hundreds of gigabytes and their manifest is two files of a few kilobytes. `dataset_info.json` gives
the per-split shard lengths — so the declared episode count — plus the file format, version,
citation and licence; `features.json` gives every per-step feature with its dtype and shape. Those
become the episode set and one empty stream per feature.

What it cannot see is everything inside a record: no steps, no values, no content hashes, no
TFRecord CRC verification, no language instruction or episode task, and no
`episode_metadata/file_path`, which is where an RLDS episode's upstream provenance lives — so a
metadata-only run of a dataset scores *lower* on provenance than a full one, honestly, rather than
claiming lineage it never read. Two things are refused instead of approximated: a manifest with no
shard lengths, since there is then no episode set and an empty dataset would score a clean 100 on a
catalog that measured nothing; and per-episode step counts, which RLDS simply never declares — every
episode carries no declared frame count rather than one derived from a shard length that does not
mean that. As with LeRobot, the declared-episode-count check is withheld, because the episode set
came from that very number.

For **Zarr** the structure is already outside the data by construction: `.zarray` states every
array's dtype, per-row shape and row count, `.zattrs` carries the store's own metadata and
provenance, and the `meta/` group holds the episode boundaries. Those are a few kilobytes in front
of a replay buffer that may be hundreds of gigabytes of chunks, and this mode opens none of the
`data/` ones — a test corrupts every one of them and the run is unchanged. Each episode carries the
length its boundaries declare, and one empty stream per array, with the same dtype, shape and
modality a full run reports.

Two details are worth stating. The `meta/` group *is* read: the episode boundaries are the store's
manifest, a few bytes saying where each episode starts and ends, and without them there is no
episode set at all. And the clock is the store's own — whether a timeline array exists and declares
its units is knowable from `.zattrs` alone, so a timed store reports its measured clock here rather
than a step index. Reporting the step index would be this run's abstention dressed up as a fact
about the source, and it would then be bound into the content hash as one.

For **MCAP** the structure is inside the container, but at a known offset: an MCAP writes its own
index at the *end* of the file — a Channel and a Schema record per topic, and a Statistics record
carrying the message total, the per-channel totals and the recording's log-time span. Reading it is
three seeks in front of a recording that is routinely tens of gigabytes, and no chunk is opened or
decompressed. You get the topic inventory with each topic's modality, the message encodings the file
declares, the declared counts, and the library that wrote it — and, by following the summary's own
Metadata and Attachment indexes, the same **provenance** a full read extracts: the licence, sensor,
clock and annotator a producer wrote into Metadata records, and the calibration attachment's name.
That matters because provenance is 30% of the trust score, and reporting none of it would have been
a claim about the file rather than about the read. The one difference is the version of a
scenario/map sidecar, which a full read prefers to take from the referenced file's own header:
opening that file is reading a second recording, so only a version the recorded value itself carries
is used here, and the difference is disclosed.

Every offset used comes out of the file itself, so every one is checked against the file's real
length before it is followed. Three cases are refused rather than approximated: a file whose footer
says `summary_start = 0` — a streaming writer that never finalized, whose topics exist only in the
records — is refused by name, because there is genuinely nothing to read without reading the file; a
footer pointing outside the file is refused rather than followed; and a summary whose per-channel
counts do not add up to its own total is refused, for the same reason a rosbag2 manifest is, since
presenting three channels out of twelve as the recording's contents is invisible to the caller.


## Why the refusals exist

The data score starts at 100 and only deducts, so anything that stops a check from measuring *raises*
it. That makes every form of partiality a way to score better by looking less — which is why
`--min-score` is refused over a sampled or metadata-only run, and why `certify` refuses one outright.
The coverage note travels in the verdict, in every report shape, and under the certificate's
signature.
