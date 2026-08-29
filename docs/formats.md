# The formats, one at a time

The claim behind Veridex is that one command reads eight formats into one shape and runs the same
checks over all of them. This page is that claim, demonstrated: what each adapter reads, what it
refuses to guess, and what it will not pretend to know.

Every example runs against a fixture or a generated demo in this repository, so you can follow it
without a dataset of your own. The general shape is always the same:

```sh
veridex check <dataset>
```

The same command works on a LeRobot dataset — proof of the cross-format claim. Both layouts are
read: **v3.0**, which packs many episodes into each Parquet and MP4, and **v2.0/2.1**, which writes
one of each per episode and is what most datasets published to date are. The difference is where the
bytes sit, not what they mean — the episode a row belongs to is the `episode_index` column either
way — so the same checks run over both. v2.1 keeps its statistics per episode in
`meta/episodes_stats.jsonl` rather than one dataset-wide `meta/stats.json`, and Veridex reads them
there; a run that looked only for the dataset-wide file would report a dataset that ships statistics
as shipping none, and silently skip every stored-vs-observed comparison. Generate a demo
one (its second episode carries an out-of-order timestamp) and check it the same way:

```sh
# generate a demo LeRobot v3 dataset; append `clean`, `truncated`, `boundary`, `jitter`,
# `short-episode`, `duplicate`, `saturated`, `spike`, `nan`, `multi-joint`, `video`,
# `video-desync`, `video-missing`, or `video-reencoded`
cargo run -p veridex-core --example make_demo_lerobot -- /tmp/demo-lerobot
cargo run -p veridex-cli -- check /tmp/demo-lerobot   # fires TEMPORAL.NON_MONOTONIC, exits 20
```

The `truncated` variant writes a dataset whose manifest declares more frames than were exported —
a realistic interrupted upload — and `check` catches it as `STRUCTURAL.FRAME_COUNT_MISMATCH`. The
`boundary` variant leaves the frames intact but corrupts one episode's declared `length` in
`meta/episodes.jsonl` — the lerobot#4143 failure, where wrong cumulative boundaries silently load
frames under the wrong episode — and `check` catches the declared-vs-actual disagreement as
`STRUCTURAL.EPISODE_BOUNDARY`. The
`jitter` variant spaces one episode's frames unevenly so its mean rate still looks right, and
`check` flags the irregular timeline as `TEMPORAL.JITTER`. The `short-episode` variant records five
episodes where one was cut short right after it began, and `check` flags it against the dataset
median as `TEMPORAL.EPISODE_DURATION_OUTLIER`. The `duplicate` variant re-uploads an episode
byte-for-byte, and `check` catches it as `STRUCTURAL.DUPLICATE_EPISODE` — Veridex fingerprints each
feature cell's bytes into a per-frame content hash, so the duplicate is proven by content, not
guessed from matching timestamps. The `saturated` variant pins the feature values exactly at their
maximum for most of the episode — a clamped actuator against its stop — and `check` flags it as
`STATISTICAL.SATURATED` from the values it recomputes as it fingerprints them. The `spike` variant
jumps a single frame far off the baseline — a sensor glitch or unit error — and `check` flags it as
`STATISTICAL.OUTLIER`, provably a rare value by Chebyshev's inequality. The `nan` variant writes one
NaN feature value and no `meta/stats.json`, so the stored-stats check has nothing to inspect — only
the recompute over the real cells sees it, flagged as `STATISTICAL.NON_FINITE_OBSERVED`. The
`multi-joint` variant is a 3-DoF `action` whose gripper (dimension 2) saturates while the arm joints
sweep freely; `check` flags `STATISTICAL.SATURATED` and **names the dimension** — the value-based
checks scan every joint, not just element 0, which is where real robot data hides its problems. Every
variant also ships a Hugging Face-style dataset card (`README.md`), so `veridex inspect` surfaces the
extracted `license` as covered provenance rather than a `PROVENANCE.MISSING_LICENSE` gap.

Four more variants add a real camera feature backed by `.mp4` files, because a video dataset is two
artifacts that nothing reconciles — a manifest and a data table on one side, a container on the
other, paired by frame index and never checked against each other. `video` is the clean baseline;
`video-desync` gives episode 1 a video three frames short of its rows (`VIDEO.FRAME_COUNT_MISMATCH`
— every pair past the shorter one is an action against an image from a different moment);
`video-missing` never uploads that file (`VIDEO.MEDIA_MISSING`); and `video-reencoded` ships 320x240
video against a declared 640x480 (`VIDEO.RESOLUTION_MISMATCH`, charged once for the stream rather
than once per episode). Veridex reads the container's **headers only** — it never decodes a pixel —
and it compares the codec across the names for one encoder, so a manifest saying `h264` against a
container stamped `avc1` is not reported as a mismatch.

It works on an **RLDS/TFDS** dataset too — the layout Open X-Embodiment and most TFDS-published
robot datasets ship in, and the third format behind the same command:

```sh
# generate a demo RLDS dataset in the TFDS layout; append `truncated`, `desynced`, or `corrupt`
cargo run -p veridex-core --example make_demo_rlds -- /tmp/demo-rlds
cargo run -p veridex-cli -- check /tmp/demo-rlds
```

RLDS stores one episode per TFRecord, with every step's values concatenated into a single
`tf.train.Example` — so an episode's step count is never written down, it is *derived* by dividing
each feature's list length by the element size `features.json` declares. Veridex does that division
for every step feature and requires the answers to agree. The `desynced` variant makes them
disagree (19 camera images against 20 actions) and is refused by name, rather than mapped into a
19-step episode that would read as sound. The `truncated` variant declares four episodes in its
shard lengths and ships three (`STRUCTURAL.EPISODE_COUNT_MISMATCH`), and `corrupt` flips one bit
inside a record — only the TFRecord CRC-32C notices, and Veridex verifies it on every record rather
than parsing past it.

One honesty note this format forces: **RLDS records no wall clock.** There is no per-step timestamp
in it, so Veridex stamps frames with their step index, records in the CDM that those timestamps are
an index rather than measured time, and never invents a rate. The checks that need measured time —
rate, gap, jitter, clock skew, start/end offset, episode duration — then skip those streams instead
of grading a dataset against a period Veridex made up.

They *say* they skipped, which is the part that matters. A step index is flawlessly monotonic,
perfectly regular, and identical across every stream of an episode, so a check that graded it would
pass — and a clean temporal result is exactly what a report and a signed certificate carry forward,
where it reads as "these sensors were synchronized." So a run over such a dataset emits
`TEMPORAL.UNMEASURED_CLOCK`, and it travels: into the JSON, the SARIF, the HTML, and the
certificate's findings summary. A passing verdict on an RLDS dataset means the structure and the
content are sound, and that nobody measured the timing.

It works on an **HDF5** file too — what `robomimic`, MimicGen, RoboTurk, and most hand-rolled lab
collectors write, and the fourth format behind the same command:

```sh
# a real h5py-written robomimic-layout file, committed as a test fixture
cargo run -p veridex-cli -- check crates/veridex-core/tests/fixtures/hdf5/robomimic_small.h5
```

The mapping is the file's own structure: a **group of arrays is an episode** (`/data/demo_0`), every
array under it is a **stream** (`actions`, `obs/agentview_image`, nested paths included), and an
array's first dimension is that stream's frame count. Types and shapes come from the file — a
`float32 [T, 7]` action stream stays exactly that — and the attributes a collector writes become
metadata, provenance, and the counts a check can test against (`num_samples` per episode, `/data`'s
`total` frames). Values are read, so the statistical checks are live: a gripper pinned at its limit,
a NaN buried in joint 6, or a lone 250x spike is caught **per dimension** and named. Veridex reads the HDF5 container directly, with no libhdf5 dependency: superblocks
v0–v3, old- and new-style groups, contiguous, compact, and chunked storage, and the `deflate`,
`shuffle`, and `fletcher32` filters. A structure it does not read is named rather than skipped past.

An HDF5 file is usually more than its episodes, and the report says so. `robomimic` writes a `/mask`
group of filter keys beside `/data`; collectors park reward tables and raw logs at the root the same
way. None of that sits under an episode group, so none of it is read — and anything there holding
rows is disclosed as `COVERAGE.SOURCE_UNREAD`, a warning in the verdict rather than a note only
`inspect` prints, because a clean result over the episodes is not a clean result over the file. An
object with no rows under it — an empty group, a scalar array, a zero-row array, a committed
datatype — is named as unmapped instead: there is nothing there to have read.

HDF5 records no clock either, so the same honesty rule applies: frames carry a step index, and the
temporal checks abstain and say so. A file that *does* record time gets measured time — but only if
it also declares its units (a `units` attribute on the timestamp array). Whether a bare `time`
column is seconds or nanoseconds is not something Veridex will guess: guess wrong and every rate,
duration, and skew verdict derived from it is fiction.

And on a **Zarr** store — the replay-buffer layout Diffusion Policy, UMI, and the tooling around them
ship in, and the fifth format behind the same command:

```sh
cargo run -p veridex-cli -- check crates/veridex-core/tests/fixtures/zarr/dp_replay.zarr
```

A replay buffer is one flat array per key with every episode concatenated end to end, and the episode
boundaries kept beside it in `meta/episode_ends`. Those boundaries *are* the episode structure:
`[4, 10]` means episode 0 is rows 0..4 and episode 1 is rows 4..10, and Veridex slices every `data/`
array accordingly. Rows past the last boundary belong to no episode, and the report says so rather
than attaching them to the last one — an off-by-one in a replay buffer is exactly the corruption this
tool exists to catch, and a boundary that runs backwards or past the end of the arrays it indexes is
refused outright.

Zarr's chunks are plain files, so there is no index to trust — but there is a codec to get right, and
a compressed array read through the wrong one does not fail, it yields plausible numbers. Veridex
reads `zlib`, `gzip`, `zstd`, `lz4`, and `blosc` (with `lz4`, `zstd`, or `zlib` inside it, byte
shuffle included), and refuses anything else by name with what to re-save it as. Every codec is tested
by decoding the same values through all of them and requiring identical bytes.


And on a **ROS 2 rosbag2** recording — what a ROS 2 robot writes by default, and the format most
existing robot logs are sitting in:

```sh
# a bag directory: metadata.yaml beside its .db3, written by real SQLite as a test fixture
cargo run -p veridex-cli -- check crates/veridex-core/tests/fixtures/rosbag2/clean_rig

# or the one .db3 on its own, when that is all you were handed
cargo run -p veridex-cli -- inspect crates/veridex-core/tests/fixtures/rosbag2/bare.db3
```

A topic's recorded QoS is read for one thing: whether it declares **transient-local** (latched)
durability — published once and retained for late subscribers, which is how every ROS 2 stack
publishes `/tf_static`. A latched stream is exempt from the four checks that ask whether streams
cover the same window (`STRUCTURAL.SINGLE_FRAME_STREAM`, `TEMPORAL.START_OFFSET`,
`TEMPORAL.END_OFFSET`, `TEMPORAL.CLOCK_SKEW`), because it is not trying to. Read conservatively:
only the four unambiguous spellings rosbag2 has written across bag versions count, two publishers
disagreeing count for nothing, and nothing is inferred from the frames — a latched topic and a
sensor that fired once and died are identical in the data.

rosbag2's `sqlite3` storage plugin keeps the recording in two tables: `topics` (one row per recorded
topic) and `messages` (one row per message, with its receive timestamp and its serialized body). Each
topic becomes a stream, each message a frame on the bag's single log clock, and the ROS type names
the modality. The AV message *headers* are CDR-decoded exactly as they are from MCAP — rosbag2's
other storage plugin — so a `PointCloud2` supplies the per-point field layout, `CameraInfo` and
`TFMessage` the intrinsics and the transform tree, and `Odometry` the ego trajectory. The bulk
payload is fingerprinted, never decoded.

One exception, and it is the one that lets the statistical family grade a bag at all: a
`sensor_msgs/msg/JointState` carries nothing *but* the measurement — a handful of joint angles — so
its `position` array is read and summarized per joint, exactly as LeRobot's or HDF5's values are.
Without it, an arm recording whose elbow sat pinned against its stop scored a clean `data 100` with
every statistical check listed as run. Every other topic's payload stays opaque and says so, through
`STATISTICAL.UNMEASURED_VALUES`. The same is true of a plain MCAP file.

Three things it will not do. **Columns are bound by name**, from each table's own `CREATE TABLE`
statement, because rosbag2 has added columns across bag versions and reading position 3 because that
is where `serialization_format` used to sit would report a type-description hash as a serialization
format. **A message on a topic the `topics` table never declares** is not filed under an invented
stream and not dropped in silence — it is disclosed as unread coverage, because a bag with half its
rows unattributed must not produce the verdict an intact one does. And **`relative_file_paths` is
content, so it is never followed out of the bag**: the `.db3` files read are the ones in the bag
directory, and a manifest entry naming a path with a directory component, or naming a shard that is
not there, is recorded as unread.

The manifest's `message_count` is reconciled against what the recording actually yielded. A recorder
killed mid-flush leaves a `.db3` short of the total `metadata.yaml` closed with, and the shortfall is
reported as unread coverage rather than read as a complete bag:

```sh
cargo run -p veridex-cli -- check crates/veridex-core/tests/fixtures/rosbag2/interrupted
#   [warning] COVERAGE.SOURCE_UNREAD — 1 source(s) the dataset declares were not read
#             (metadata.yaml message_count), so every result below speaks for the part that was

cargo run -p veridex-cli -- inspect crates/veridex-core/tests/fixtures/rosbag2/interrupted
#   UNREAD: metadata.yaml message_count (the manifest declares 401 message(s) but 361 were
#           read — 40 are missing from the bag's .db3 file(s))
```

That total is deliberately *not* mapped to the CDM's declared frame count: it counts every topic's
messages, while that field is what each of an episode's streams should hold, and comparing the two
would fail a sound bag. A bare `.db3` has no manifest at all, so there is nothing to reconcile
against and no recording distribution to record — and `inspect` says exactly that rather than
leaving you to assume it was checked.

A bag that is **still being recorded** is read too, which is what makes `veridex watch` useful on
one: point it at the directory while the robot is driving and each tick re-validates what has landed.
rosbag2 writes `metadata.yaml` when the recorder *closes*, so a bag in progress is a directory with a
growing `.db3` and nothing else — and Veridex says what the missing manifest would have supplied
rather than assuming it. If SQLite is running in WAL mode, the `.db3-wal` beside the shard holds
committed messages the shard itself does not carry; Veridex reads the shard's own pages and does not
replay a write-ahead log, so those messages are disclosed as unread coverage rather than quietly
missed:

```sh
cargo run -p veridex-cli -- watch crates/veridex-core/tests/fixtures/rosbag2/recording --iterations 1
#   COVERAGE.SOURCE_UNREAD — recording_0.db3-wal: a SQLite write-ahead log sits beside this
#   shard, holding transactions the `.db3` itself does not carry
```

A **split** recording — `ros2 bag record --max-bag-size`, which rolls a long bag into
`bag_0.db3` … `bag_11.db3` — is read as one recording, in the order it was written. That order is
not name order: a lexicographic sort puts `_10` and `_11` ahead of `_2`, and since frames keep the
order their shards were read in (reordering them would hide the out-of-order timestamps this tool
exists to find), a sound twelve-shard bag came back with two `TEMPORAL.NON_MONOTONIC` errors. Shards
are ordered by their number, then by the order the manifest lists them — the bag's own record of how
it wrote them. Taking an ordering from the manifest follows no path: only the files already found in
the bag directory are ever opened.

A bag recorded through the **MCAP storage plugin** — what `ros2 bag record` writes by default from
Jazzy on — is read as the bag it is, not as a loose file: point Veridex at the bag *directory* and it
reads every `.mcap` shard in the order the manifest lists them, then reconciles the result against
the bag's own `message_count` exactly as it does for `sqlite3`. Which plugin a team picked does not
change what Veridex sees: an MCAP channel carries what the `topics` table carries — topic name,
schema, encoding, and the publisher's QoS — so the same recording through either plugin yields the
same streams, modalities, timestamps and rig calibration. What the storage does change is what the
report *names*: an MCAP-backed bag's mapped fields speak of channels and log times, never of SQLite
tables the bag does not have.

The manifest is required for a directory of `.mcap` files, and only for that case. A directory
holding a `.db3` is unambiguously one bag; a directory of `.mcap` files could as easily be a folder
someone dropped three unrelated recordings into, and reading those as one bag would concatenate three
timelines into one episode and report the seams as defects. `metadata.yaml` is what makes the
directory a bag. A bag still being recorded has not written one yet — point Veridex at the `.mcap`
file itself, which the MCAP adapter reads. A directory holding both `.db3` and `.mcap` shards is
refused: one recording uses one plugin, and whichever half was picked, the other half's messages
would go unread while the verdict named the directory.

A bag recorded with `--compression-mode file --compression-format zstd` — which is how any
recording large enough to care about is stored — is read directly. rosbag2 compresses the finished
shard to `<shard>.db3.zstd` and deletes the original; Veridex unpacks it under the same
decompression budget that bounds every other container here, and bounded *during* the read rather
than charged after it, so a bomb is stopped instead of billed for once the memory is gone. The same
recording compressed and uncompressed produces identical streams, frames and content hashes; only
the dataset's name differs. Per-*message* compression is refused by name: those tables are plain, so
the bag would read — and every frame's fingerprint would be of a zstd frame rather than the message,
and no AV header would decode, so a full rig would come back with no point fields, no calibration
and no ego trajectory. A wrong answer is worse than a refusal.

The SQLite reader is Veridex's own, hand-written and bounds-checked for the same reason the HDF5 and
Zarr readers are: a `.db3` is an untrusted file, and a general-purpose database engine will follow a
page chain a corrupt header points into with allocations no ingest budget can charge. This one
refuses a page outside the file, refuses a b-tree or overflow chain that revisits a page, and caps
the payload it will assemble before the bytes are copied. It is tested against fixtures written by
Python's own `sqlite3` — a reader proven only against a writer from the same repository proves the
two agree with each other, not that either matches the format.

## Reading a dataset you have not downloaded

One source is not a file at all. `veridex check hf://org/name --metadata-only` reads a dataset's
manifest straight from the Hugging Face Hub — `meta/` and the dataset card, a few hundred
kilobytes — and runs the manifest half of the catalog over it. It is the fastest way to answer "is
this the dataset I think it is, and does it declare what I need" for a repository too large to pull.

Two layouts are read: a **LeRobot** dataset (`meta/info.json` and what sits beside it) and an
**RLDS/TFDS** export (`dataset_info.json` + `features.json`). Which one a repository holds is settled
by asking for each layout's first required file in turn — one request per layout, against a path
fixed in the source. A TFDS export is usually published one version directory deep, so the directory
can be named in the reference: `veridex check hf://org/name/my_dataset/1.0.0 --metadata-only`. It is
named by you and never discovered, because a path the server chose would undo the fixed file list —
and two directories in one repository are two datasets, so the id carries the path.

The list of files requested is fixed in Veridex's source, not discovered from anything the server
returns, so a hostile repository cannot enlarge it. Requests and any redirect they follow are
restricted to the Hub's own hosts over HTTPS. No credential is read from your environment or your
filesystem, so a private dataset answers 401 and is reported as private — forwarding a token you
happen to have to a host you did not name in the command is not a validator's decision to make.

The manifest is staged in a temporary directory and read by the ordinary local adapter, so a remote
check and a local check of the same manifest are the same code reading the same bytes. Nothing is
written outside that directory, and it is removed when the command returns. Two things follow:

- The dataset is identified as `org/name` — the repository, not the temporary directory. A local
  copy of the same dataset is identified by its directory instead, so the two are deliberately
  different datasets to the content hash: one is "this Hub repository", the other "this directory".
- A remote run is a metadata-only run, with every refusal that comes with one. It cannot pass a score
  gate and cannot be certified.

### Which commit was read

`hf://org/name` names a branch, and a branch moves. The commit the Hub actually served the manifest
from is recorded as the dataset's `hub_commit` metadata, so it binds into the content hash and is
printed by `veridex inspect` beside the hash it produced:

```
Dataset: lerobot/pickplace
  format:   lerobot
  CDM hash: 6b1f…
  source:   hf://lerobot/pickplace@main (commit 0c1e9f…)
```

`veridex check` prints the same line as a footer under its report, so the commit sits with the
verdict it produced.

Re-run against that commit — `veridex check hf://org/name@0c1e9f… --metadata-only` — and the read is
pinned to those exact bytes. Two reads of one repository at two commits are two datasets to the
content hash, which is the point: a hash that could not tell them apart would let yesterday's result
stand for today's data.

A manifest is several requests, so a branch can move part-way through one. Veridex refuses a read
whose responses name two different commits rather than stitching half of each into a dataset that
never existed, and the refusal names the commit to pin to. If the Hub names no commit at all, nothing
is recorded and the run says so — an invented commit would be worse than none.

Anything past the manifest is refused rather than downloaded — `veridex check hf://org/name` without
`--metadata-only` says so and names the option that works. Veridex validates; it is not a downloader.
This is also the only network path in the tool: a certificate still verifies with no network at all.


## What no adapter does

None of them decode pixels, and none infer a clock a format does not record. Where a format cannot
express something, the CDM says so and the checks that would have needed it abstain out loud rather
than passing quietly — see [checks.md](checks.md) for the disclosures that carry that.
