# The formats, one at a time

The claim behind Veridex is that one command reads seven formats into one shape and runs the same
checks over all of them. This page is that claim, demonstrated: what each adapter reads, what it
refuses to guess, and what it will not pretend to know.

Every example runs against a fixture or a generated demo in this repository, so you can follow it
without a dataset of your own. The general shape is always the same:

```sh
veridex check <dataset>
```

The same command works on a LeRobot v3 dataset — proof of the cross-format claim. Generate a demo
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


## What no adapter does

None of them decode pixels, and none infer a clock a format does not record. Where a format cannot
express something, the CDM says so and the checks that would have needed it abstain out loud rather
than passing quietly — see [checks.md](checks.md) for the disclosures that carry that.
