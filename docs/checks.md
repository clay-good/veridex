# Veridex check catalog

Every finding Veridex can emit, by check. Run `veridex checks` for the live list — each check lists
the finding codes it can emit (add `--json` for a machine-readable catalog) — and
`veridex check <dataset>` to run them. Each finding carries a training
**risk** and a **remedy**; this page is the quick reference for the codes you'll see.

Checks are selected, disabled, or re-severitied via [`veridex.toml`](veridex.toml.example).
Severities below are the defaults. Every numeric threshold named in this catalog has a
[`veridex.toml`](veridex.toml.example) key — the *configurable* marker on some rows is redundant, not
exclusive — **except two fixed constants**: `structural.stuck-stream`'s 5-frame run and
`temporal.episode-duration`'s 4-episode abstention floor. `TolerancesConfig` has twelve fields and
neither is among them, so a config naming one is rejected.

## Structural — is the dataset shaped like trainable data?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `structural.episode-boundary` | `STRUCTURAL.EPISODE_BOUNDARY` | error | A per-episode declared `length` (e.g. LeRobot `meta/episodes.jsonl`) that disagrees with the frames ingested, duplicate episode indices, or an episode whose `start_ts > end_ts` — all signatures of corrupt cumulative boundaries (the lerobot#4143 class), where frames load under the wrong episode. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_DATASET` | error | The dataset has no episodes at all. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_EPISODE` | error | An episode has no streams. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_STREAM` | error | A stream has no frames. |
| `structural.degenerate-episode` | `STRUCTURAL.SINGLE_FRAME_STREAM` | warning | A stream has a single frame (no temporal signal). A stream the source **declares latched** — published once and retained for late subscribers, which is how every ROS 2 stack publishes `/tf_static` — is exempt: one frame at the recording's first instant is what a latched topic is *for*, not a trajectory cut short. Only a recorded QoS profile (rosbag2's `topics.offered_qos_profiles`, or the same on an MCAP channel) sets that; it is never inferred from the frames, because a latched topic and a sensor that fired once and stopped are identical in the data and opposite in meaning. |
| `structural.episode-continuity` | `STRUCTURAL.EPISODE_INDEX_GAP` | warning | Episode indices are non-contiguous (e.g. `0, 1, 3`) — an episode was dropped between export and ingest. Needs no manifest, unlike the declared-count check. |
| `structural.declared-episode-count` | `STRUCTURAL.EPISODE_COUNT_MISMATCH` | error | The manifest's declared episode count (e.g. LeRobot `total_episodes`) differs from the episodes ingested (a truncated export). |
| `structural.declared-frame-count` | `STRUCTURAL.FRAME_COUNT_MISMATCH` | error | The manifest's declared frame count (e.g. LeRobot `total_frames`) differs from the frames ingested (episodes present but cut short). |
| `structural.shape-consistency` | `STRUCTURAL.SHAPE_MISMATCH` | error | A stream keeps a different declared dtype/shape across episodes (un-batchable). dtype and shape carry **independent** baselines, each taken from the first episode that declared *that* axis — a stream declaring a dtype in one episode and a shape only in a later one is still compared on both. (HDF5 and Zarr write no `shape` for a 1-D dataset, so a single such episode used to disable shape-drift detection for that stream permanently.) |
| `structural.stream-presence` | `STRUCTURAL.STREAM_PRESENCE_INCONSISTENT` | warning | A stream key is present in some episodes but missing from others — a heterogeneous feature set (a sensor that dropped out, or two exports pooled together). |
| `structural.step-alignment` | `STRUCTURAL.STEP_COUNT_MISMATCH` | error | Two streams in one episode indexed by the same **step counter** disagree about how many steps the episode has. A step index is a row index, so `action[i]` and `observation.state[i]` are the same moment by construction — the only thing that can break the pairing is the arrays holding different numbers of rows. Nothing else looks: the whole temporal family abstains on a step index (correctly — an index is flawlessly monotonic and perfectly regular), and `structural.declared-frame-count` needs a count these formats rarely declare, so an `action` of 100 rows beside an `observation.state` of 50 came back clean with every pair past row 50 built from the wrong observation. On measured time the same defect is `TEMPORAL.CLOCK_SKEW`. Reachable for HDF5 and Zarr, both proven end-to-end. RLDS stamps step indices too but cannot reach this check: a TFRecord holds one `steps` sequence, so the adapter refuses a record whose features disagree about its length before a CDM exists. MCAP, rosbag2, LeRobot, CAN+DBC and MF4 carry measured time, where the same defect is `TEMPORAL.CLOCK_SKEW`. **A difference of one is tolerated**: several collectors store the terminal observation a trajectory ends in, giving `observation` one row more than `action` — a deliberate convention, and flagging it would fire on sound robomimic data. Two rows is no convention. Empty streams are `STRUCTURAL.EMPTY_STREAM`'s concern and are excluded. |
| `structural.stuck-stream` | `STRUCTURAL.STUCK_STREAM` | warning | A `Video` stream repeats a byte-identical frame (same `content_hash`) for ≥5 consecutive frames while timestamps advance — a frozen/stuck camera the timestamp-based temporal checks can't see. Real camera frames are never byte-identical, so this is unambiguous. Only frames carrying a `content_hash` are compared (MCAP images are fingerprinted; LeRobot video lives outside the Parquet, so the check abstains there). Constant *scalar* streams are not this check's concern: one constant across the statistics its source summarizes is `STATISTICAL.DEGENERATE`, and one constant through a single episode of a dataset where it moves in the others is `STRUCTURAL.FROZEN_EPISODE` — the case DEGENERATE cannot see where those statistics are dataset-wide. |
| `structural.frozen-episode` | `STRUCTURAL.FROZEN_EPISODE` | warning | Every frame of an actuator or proprioception stream is byte-identical through one episode, while that stream changes in most other episodes — a recording where the robot never moved. The commonest failure in a teleoperated dataset, and one that fell exactly between two checks that each defer to the other: `structural.stuck-stream` looks only at `Video` (a frozen *scalar* is the statistical family's business), and `STATISTICAL.DEGENERATE` reads summary statistics, which for LeRobot are **dataset-wide** — one dead episode among fifty does not move them. Fifty good episodes plus one where nothing moved scored the same as fifty-one good ones. The evidence is frame `content_hash`, not values, so it applies to every format that fingerprints frames. Three guards keep it off honest data: only streams carrying **more than one scalar per frame** (an `action` or joint-state vector — a single column is as likely to be a `reward` or `done` that is legitimately constant through a failed demonstration), judged from the declared shape or the dimension names; only when the frozen episodes are a strict **minority** (frozen in most episodes is how the dataset is built); and only on evidence — ≥8 frames, ≥3 episodes, and every frame fingerprinted in every episode, since an unfingerprinted stream is `STRUCTURAL.UNFINGERPRINTED_CONTENT`'s disclosure rather than this check's finding. |
| `structural.content-measurability` | `STRUCTURAL.UNFINGERPRINTED_CONTENT` | info | A stream's frames carry no content fingerprint, so the two checks that prove things by comparing frame bytes — `structural.duplicate-episode` and `structural.stuck-stream` — could not inspect it. The abstention is not a corner case: a LeRobot video feature's pixels live in `.mp4` files outside the Parquet, and the duplicate check aborts the whole episode signature if *any* frame of *any* stream lacks a hash — so one video feature, the ordinary layout of a real LeRobot dataset, made two byte-identical episodes undetectable. `stuck-stream` only looks at `Video` streams, which on LeRobot are exactly the hashless ones, so the frozen-camera check never ran there at all. The finding states separately whether *no* episode was fully fingerprinted, since that disables the duplicate check dataset-wide. |
| `structural.content-measurability` | `STRUCTURAL.UNCOMPARED_EPISODES` | info | The run covers too few episodes for the checks that answer by comparing one episode against another, so they had nothing to compare: `structural.duplicate-episode`, `near-duplicate-episode`, `stream-presence`, `shape-consistency` and `episode-continuity` need two, `structural.frozen-episode` needs three, `temporal.episode-duration` needs four. Worded as the *run*, not the dataset, because both cause it: an MCAP file and a bare rosbag2 recording are one episode by construction, and `--sample-episodes 1` over a five-hundred-episode dataset leaves one episode in the CDM. Not a corner case — every run over a single recording silently skipped seven checks while the certificate listed them as executed with no categories skipped. The third axis of the same reasoning as `TEMPORAL.UNMEASURED_CLOCK` and `STATISTICAL.UNMEASURED_VALUES`: not "no clock", not "no values", but "nothing to compare against". Informational — a dataset is not worse for being one recording; what changes is what its passing verdict is evidence of. |
| `structural.duplicate-episode` | `STRUCTURAL.DUPLICATE_EPISODE` | warning | Two or more episodes have identical frame **content** (same schema, timestamps, and per-frame `content_hash`) — a re-upload or a bad merge that over-weights the repeated trajectories. Sound-only: an episode is compared solely when every frame carries a `content_hash`, so it never mis-flags two different same-length episodes that merely share a time base and dataset-global stats. Fires once adapters populate per-frame content hashes; a *partial* copy is `structural.near-duplicate-episode`'s. |
| `structural.near-duplicate-episode` | `STRUCTURAL.NEAR_DUPLICATE_EPISODE` | warning | Two or more episodes are built largely from the **same frames** without being exact copies — a re-upload with its tail trimmed, a merge that pulled one recording in twice, an episode contained inside a longer one. Evidence is set overlap over per-frame `content_hash`es (no payload is decoded), reported when the weakest shared stream still clears `near_duplicate_fraction` (default 0.80, over `min(\|a\|, \|b\|)` so containment counts as full overlap). Three guards keep it off honest data: only streams where every frame is hashed, at least 8 frames long, and at least 80% distinct within the episode count as evidence (an arm at rest repeats values across every episode); every stream both episodes carry must agree; and a hash held by more than 512 episodes is boilerplate, not evidence — a ceiling set far above any plausible duplication group, because a recording ingested forty times shares every frame with thirty-nine others and must not be mistaken for boilerplate. Pairs the exact check reports are suppressed, using its own signature. A **re-encoded or perturbed** copy shares no bytes and is still out of scope. |
| `structural.near-duplicate-episode` | `STRUCTURAL.NEAR_DUPLICATE_UNCHECKED` | info | The check abstained, and says on how much: either more episode pairs share frames than it will hold at once (200,000), or every frame of some episodes is held by more than 512 others and was treated as boilerplate, leaving those episodes uncompared. Abstention is reported rather than left silent, because a check that said nothing looks exactly like a check that found nothing — and it is reported *alongside* whatever the check did find, never instead of it. |

## Temporal — is the time base sound?

Every check in this family except `temporal.rate-validity` and `temporal.rate-consistency` (which
grade a *declared* rate, not a timeline) reads only streams whose timestamps are **measured time**.
A source that records no clock carries a step index instead: RLDS/TFDS has no per-step timestamp at
all, and HDF5 and Zarr have one only when the file both stores a timestamp array and declares that
array's units (see "What is *not* covered" below). An index
and an index satisfies all of them trivially: flawlessly monotonic, perfectly regular, identical
across every stream of an episode. Grading it would put a clean temporal result in a report and a
signed certificate on the strength of a timeline nobody measured, so those streams are skipped and
`temporal.clock-measurability` reports that they were.

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `temporal.clock-measurability` | `TEMPORAL.UNMEASURED_CLOCK` | info | A stream's timestamps are a positional step index rather than measured time, so the rate, gap, jitter, clock-skew, start/end-offset and episode-duration checks had nothing to grade. Reported once per clock for the dataset (the clock is a property of the source format, not of an episode), naming the streams. Not a defect in the data — what it changes is what a passing temporal result is *evidence of*: the absence of a measurement, not good timing. |
| `temporal.clock-measurability` | `TEMPORAL.UNCOMPARED_STREAMS` | info | No two streams in an episode shared a clock with a measurable span, so `CLOCK_SKEW`, `START_OFFSET` and `END_OFFSET` — the three checks that answer whether a dataset's sensors are aligned — had nothing to compare. Their silence is indistinguishable from three checks that ran and found everything in order, and it reaches the certificate's list of executed checks looking exactly like that. The sharp case is a ROS bag holding only latched topics (a transform tree and a robot description, both published once): no sensor data at all, and `data 100` with no temporal finding. A single-stream dataset, and one whose streams each sit on their own clock, have the same shape. Reported once for the dataset, naming how many episodes of how many. Informational — the dataset is not worse for having one stream. Withheld under `--metadata-only`, where no episode has frames by request and `COVERAGE.METADATA_ONLY` already says so in full. |
| `temporal.monotonicity` | `TEMPORAL.NON_MONOTONIC` | error | Timestamps within a stream do not strictly increase (out-of-order or duplicated frames). Streams sharing one timeline (a CAN or MF4 group off a single clock) are reported once, naming the others. |
| `temporal.rate-validity` | `TEMPORAL.INVALID_RATE` | error | A stream declares a sampling rate that isn't a positive, finite number (`0`, negative, `NaN`, `inf`) — corrupt metadata the rate and gap checks would otherwise skip silently. |
| `temporal.rate-conformance` | `TEMPORAL.RATE` | warning | The observed mean rate deviates from the declared rate beyond tolerance. |
| `temporal.gap` | `TEMPORAL.GAP` | warning | An inter-frame interval is far larger than expected (dropped frames). |
| `temporal.jitter` | `TEMPORAL.JITTER` | warning | A stream's inter-frame intervals are badly irregular (coefficient of variation above tolerance) even though the mean rate can look correct — a jittery timeline that `RATE` and `GAP` both miss. |
| `temporal.clock-skew` | `TEMPORAL.CLOCK_SKEW` | error | Two streams in an episode span materially different durations — the headline cross-stream drift check. The tolerance (`clock_skew_ms`, default 50 ms) is widened by the larger of the two streams' own sampling periods: a stream observing a window at period `T` spans a whole number of `T`s, so two synchronized sensors at different rates differ by up to one period with no drift at all. A stream with too few intervals to take a median — a slow sensor catching two samples in a short episode — takes its period from `declared_rate_hz`, bounded by the one interval observed, so it is neither charged for its own sampling quantum nor able to buy a huge allowance out of a single gap. |
| `temporal.start-offset` | `TEMPORAL.START_OFFSET` | warning | Two streams sharing a `clock_id` start at materially different absolute times (a sensor that came online late) — a misalignment `CLOCK_SKEW`'s duration comparison can miss. Streams the source declares *latched* are excluded (see `END_OFFSET`). |
| `temporal.end-offset` | `TEMPORAL.END_OFFSET` | warning | Two streams sharing a `clock_id` end at materially different absolute times (a sensor that dropped out early, or a truncated tail) — the mirror of `START_OFFSET`; because `end = start + duration`, a tail misalignment can slip past both `START_OFFSET` and `CLOCK_SKEW`. Latched streams are excluded here too, and from `CLOCK_SKEW`: all three ask whether streams cover the same window, and a latched topic is not trying to. |
| `temporal.rate-consistency` | `TEMPORAL.RATE_INCONSISTENT` | warning | A stream declares one sampling rate in some episodes and a materially different one in others — differently-configured sources pooled under one key, or wrong rate metadata. Every per-episode check passes, but a global fixed-rate assumption is wrong for part of the data. The temporal sibling of `STRUCTURAL.SHAPE_MISMATCH`. |
| `temporal.episode-duration` | `TEMPORAL.EPISODE_DURATION_OUTLIER` | warning | An episode's total duration is a large multiple (default 10×, configurable) away from the dataset's *median* episode duration — a truncated capture cut short, or a recorder left running, not natural task variation. The median baseline is robust to the outliers it hunts; the check abstains below 4 measurable-duration episodes (no stable "typical"). |

## Statistical — do the stored per-stream statistics hold together?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `statistical.value-measurability` | `STATISTICAL.UNMEASURED_VALUES` | info | A stream carries neither stored nor recomputed statistics, so every check in this family had nothing to measure on it. Reported for the same reason `TEMPORAL.UNMEASURED_CLOCK` is: MCAP and rosbag2 fingerprint payload bytes without interpreting them — every topic but a `JointState` or an `Imu`, which carry nothing *but* their measurements — as does any leaf a TFRecord stores as a `bytes_list` (an image, an instruction string), so a recording whose actuator is pinned at its rail reported `data 100` with no statistical findings — while the certificate listed all five statistical checks as run with no categories skipped. (CAN+DBC was the original example, and neither it nor MF4 abstains any more: a DBC decodes each frame into named signal *values*, an MF4 applies its `##CC` conversion to each sample, and both results are measured like any other values.) A saturated actuator, a buried NaN, and a stale stored statistic all signed as checked-and-clean. Informational: a dataset is not worse for the container it was published in; what changes is what a passing verdict is evidence of. **Withheld under `--metadata-only`**, where no adapter reads values by request: there the finding would describe the flag rather than the dataset, and `COVERAGE.METADATA_ONLY` already states that no value was examined. |
| `statistical.value-measurability` | `STATISTICAL.UNMEASURABLE_VALUES` | info | A stream holds values that are **not numbers** — a text feature (an RLDS `language_instruction`) has no minimum, maximum, mean or standard deviation in this format or any other. Distinct from `UNMEASURED_VALUES`, which means *this source* did not hand over values another one would: no re-run and no other source changes this, so a remedy pointing the reader elsewhere would send them after a summary that does not exist. Judged from the dtype the source declares, never guessed from a name. **Imagery is deliberately not here**: an HDF5 or Zarr image feature is a plain `uint8` array Veridex reads and summarizes per dimension, while an RLDS `bytes_list` leaf or a bag's camera topic is an encoded frame it fingerprints — so whether a picture's values are measurable is a property of how the source stores them, and it belongs to `UNMEASURED_VALUES`, where the remedy is real advice. What can go wrong in a text stream is the semantic family's to report. |
| `statistical.value-measurability` | `STATISTICAL.NO_STORED_STATS` | info | A stream's values *were* read and summarized, but the source published no summary statistics of its own — so `statistical.stored-vs-observed` and the stored-range rules of `statistical.range-sanity` had nothing to compare against. This is HDF5 and Zarr, which recompute but carry no stored stats. The recomputed checks still apply; the source-agreement ones did not run. |
| `statistical.range-sanity` | `STATISTICAL.NON_FINITE` | error | A stored min/max/mean/std is NaN or infinite. |
| `statistical.range-sanity` | `STATISTICAL.RANGE_INVERTED` | error | Stored `min > max`. |
| `statistical.range-sanity` | `STATISTICAL.NEGATIVE_STD` | error | Stored standard deviation is negative. |
| `statistical.range-sanity` | `STATISTICAL.MEAN_OUT_OF_RANGE` | error | The stored mean lies outside `[min, max]`. |
| `statistical.range-sanity` | `STATISTICAL.STD_IMPLAUSIBLE` | error | The stored `std` contradicts the stored range, in either direction: above `(max − min) / 2`, the largest value possible for that range (Popoviciu's inequality), or exactly zero while `min` and `max` differ, which no set of values has. Everything between is admissible — a distribution sitting almost entirely at its mean has a small std over a wide range, and that is data, not corruption. Zero matters on its own because `statistical.extreme-outlier` divides by this std: a stored zero makes every z-score infinite, and that check steps aside for corrupt stats expecting this one to report them. The slack allowed scales with the magnitude of the values, so honest float cancellation on a near-constant channel at 300.0 isn't called impossible. |
| `statistical.range-sanity` | `STATISTICAL.DTYPE_RANGE` | error | The stored min/max falls outside what the stream's declared integer dtype can represent (e.g. a `uint8` with max `300`) — the dtype or the stats are wrong. |
| `statistical.range-sanity` | `STATISTICAL.DEGENERATE` | warning | The stream is constant (`min == max`, and `std` zero within a rounding tolerance that scales with the magnitude of the values — an exporter computing `std` in floating point reports a constant channel's spread as noise, not exactly zero). |
| `statistical.stored-vs-observed` | `STATISTICAL.STATS_STALE` | error | The stored stats and the values Veridex recomputed from the data disagree: a real value falls outside the stored `[min, max]`. The stored `meta/stats.json` is stale or was computed on different data, so normalization built from it clips/distorts the true inputs. Only min/max are compared (convention-free). For a multi-DoF feature it compares **per dimension** — LeRobot's `stats.json` stores per-element arrays and normalization is per dimension, so a stale stat in one joint is caught and named, where an element-0-only check would falsely report a match. Reachable where the adapter reads feature values *and* the source stores its own (LeRobot); HDF5 stores no statistics, so there is nothing to compare its recompute against. |
| `statistical.saturation` | `STATISTICAL.SATURATED` | warning | A large fraction (default ≥50%) of a stream's recomputed values sit **exactly** at one extreme — a clamped/saturated actuator or a state pinned against a rail. The controller can't tell "at the limit" from "wants to go further," so the policy imitates an observation that no longer tracks intent. Exact-equality is the signal (a noisy sensor never lands on the same float repeatedly), so it's false-positive-free; fully constant streams are `STATISTICAL.DEGENERATE`'s concern. Saturation is judged **per dimension** — a gripper pinned at element 6 of a 7-DoF `action` vector is caught, not just element 0, and the finding names the saturating dimension, by the source's own word for it (`dimension 6 \`gripper\``) wherever the source names one per element: a LeRobot feature's `names`, a `JointState`'s `name[]`, an IMU's axes. Abstains below 20 samples, and where the adapter doesn't read feature values (any MCAP or rosbag2 topic other than a `JointState` or an `Imu`). Reachable for LeRobot, HDF5, Zarr, CAN+DBC, MF4, RLDS, and a bag's `JointState` / `Imu` topics. A **boolean** channel is skipped: it has two states and nothing between them, so "sits at its rail" describes every value it will ever hold — RLDS carries `is_first`/`is_last` on every step and LeRobot writes `next.done` the same way. What would be a defect on such a channel is being constant, which `STATISTICAL.DEGENERATE` reports. |
| `statistical.declared-range` | `STATISTICAL.OUT_OF_DECLARED_RANGE` | warning | The values fall outside the range the **source itself declares** for the stream — a DBC signal's `[min\|max]`. A declaration is a fact about the data separate from any summary of it, and comparing the two answers what neither a checksum nor a statistic can: whether this log was decoded against the database that describes it. A CAN log read with the wrong DBC does not error — every signal produces a number and the timeline is intact — but the numbers stop fitting the declared spans, wrong in every stream at once. A warning rather than an error because the narrower reading is real too: a sensor operating out of spec. Silent where the source declares no range, and where no values were read (`STATISTICAL.UNMEASURED_VALUES` owns that silence). |
| `statistical.non-finite-observed` | `STATISTICAL.NON_FINITE_OBSERVED` | error | The recorded **data** holds a NaN or ±infinity value. Distinct from `STATISTICAL.NON_FINITE`, which inspects the source's stored `stats.json`: a clean or absent summary can still hide non-finite values in the actual feature cells, and only a recompute over real values sees them. Veridex holds these out of the recomputed summary (a NaN would poison every stat) and counts them separately, scanning **every dimension** of a multi-DoF cell (a NaN buried in one joint of a 7-DoF arm is still caught). A single NaN propagates to a NaN loss and silently kills a training run. Reachable where the adapter reads feature values (LeRobot, HDF5, Zarr, CAN+DBC, MF4, and a bag's `JointState` / `Imu` topics); every other MCAP or rosbag2 payload is fingerprinted, not decoded, and abstains. An integer array cannot hold a NaN, so reading it is enough to report it clean — a different answer from never having read it. |
| `statistical.extreme-outlier` | `STATISTICAL.OUTLIER` | warning | A stream's extreme (min or max) sits many standard deviations from the mean (≥10σ, configurable via `outlier_z`). By Chebyshev's inequality at most `1/z²` of samples can be that far out (≤1% at 10σ), so the flagged value is provably a rare spike — a sensor glitch or unit error — not a wide-but-normal distribution. A lone extreme dominates min/max normalization and destabilizes training. Reads only summary stats (recomputed when available, else stored); for a multi-DoF feature it scans **every dimension** and names the outlying one, so a spike in a non-first joint is caught. Corrupt/degenerate stats are `statistical.range-sanity`'s concern and are skipped. |

## Semantic — are labels and keys usable?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `semantic.task-quality` | `SEMANTIC.EMPTY_TASK` | warning | An episode has a present-but-empty task string. |
| `semantic.task-quality` | `SEMANTIC.PLACEHOLDER_TASK` | info | An episode's task is a low-information placeholder (e.g. "Hold"). |
| `semantic.stream-key-clarity` | `SEMANTIC.DUPLICATE_STREAM_KEY` | error | A stream key appears more than once in one episode — a violation of the "unique within an episode" invariant that makes the name an unusable identifier. |
| `semantic.stream-key-clarity` | `SEMANTIC.AMBIGUOUS_STREAM_KEY` | warning | Two *distinct* stream keys differ only by case or whitespace. A naming mistake is a property of the dataset's schema, so each collision is reported once, naming the first episode it appears in — not once per episode. |
| `semantic.annotation-integrity` | `SEMANTIC.ANNOTATION_UNALIGNED` | error | A timestamped `language` annotation falls outside its episode's time span — it would attach to a moment the episode never recorded, so per-frame language conditioning built from it aligns to the wrong frame. |
| `semantic.annotation-integrity` | `SEMANTIC.ANNOTATION_CONFLICT` | warning | Two `language` annotations at the same timestamp carry different values — contradictory supervision for one instant. |
| `semantic.annotation-integrity` | `SEMANTIC.EMPTY_ANNOTATION` | warning | A `language` annotation is present but its value is empty/whitespace. Veridex verifies annotations, never writes or edits them. LeRobot surfaces mid-episode `task_index` changes as timestamped `language` labels; single-task episodes carry none. |

## Video — does the media match the data it is paired with?

A video dataset is two artifacts nothing reconciles: a manifest and a data table on one side, an
`.mp4` on the other. A loader pairs video frame *i* with data row *i* and asks no questions. These
checks read the container's **headers only** — Veridex never decodes a pixel — so "decodable" is
answered as "does this container parse and describe a video track", which is what catches the
truncated, half-uploaded, and re-encoded files.

They apply to a feature the manifest declares as `dtype: "video"` — the declaration that its pixels
live in video files. A feature stored inline, or as individual images, or one whose videos are
concatenated into shared files rather than written one per episode, is reported as unmapped coverage
rather than accused of anything. Today the resolution is LeRobot's `videos/**/<feature>/episode_<n>`
layout, in any of `.mp4` / `.m4v` / `.mov`.

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `video.media-readable` | `VIDEO.MEDIA_UNATTRIBUTED` | info | The manifest declares a stream's pixels live in video files, but they are not laid out one file per episode (`episode_<n>.<ext>`) — the v3 layout that concatenates episodes into shared files — so no container can be paired with the rows it belongs to. Reported once per stream. The pixels may all be present and perfectly sound; what cannot be done is the pairing these checks verify. Before this existed such a stream carried no `media` at all, which is exactly what a non-video feature carries, so the whole video family iterated past it and emitted nothing — even for a file holding no container. |
| `video.media-readable` | `VIDEO.MEDIA_ABSENT` | error | The manifest declares a stream's pixels live in video files (`dtype: "video"`) and **no** media file for it was found at all — the signature of an un-pulled LFS pointer or an interrupted download. Charged once for the stream: one tree that never arrived is one gap, not one per episode. Nothing in the manifest or the data table records this, so without the check the dataset reads as complete until a loader tries to open a video. |
| `video.media-readable` | `VIDEO.MEDIA_MISSING` | error | An episode's media file is absent, though the dataset stores that stream's video one file per episode. The episode's rows claim imagery the dataset does not hold. |
| `video.media-readable` | `VIDEO.MEDIA_UNREADABLE` | error | The file exists but is not a readable container — the finding names the structure that was wrong (no `moov` box, a truncated header, a box declaring more bytes than remain). A container that will not parse will not decode, and training fails at that episode hours into a run. |
| `video.media-conformance` | `VIDEO.FRAME_COUNT_MISMATCH` | error | The container's sample count differs from the frames the paired data stream carries. Every video/data pair past the shorter of the two is wrong, so the policy learns actions against images from a different moment. Reported per episode — one bad video is one bad episode — **except** when every episode of a stream whose video could be *measured* is off by the same amount, and at least two could be, which is one systematic export defect (an encoder dropping a leading frame, a converter counting from one) and is charged once. An episode whose file is missing, whose container would not parse, or whose container declares no sample count was never weighed, so it neither supports nor defeats the pattern — and a stream with only one measured episode is reported per episode, naming the file and both counts. Rolling it up changes how often it is reported, never its severity. |
| `video.media-conformance` | `VIDEO.RESOLUTION_MISMATCH` | warning | The container's encoded resolution differs from the manifest's. The declared resolution comes from `info.video.width`/`height`, falling back to the feature's `shape` — read through the manifest's own axis `names`, or, absent those, only when the shape is unambiguously channel-last. Charged **once per stream per distinct value**, naming the first episode and how many share it: a re-export is one defect however many episodes it touched, but two episodes at two different wrong resolutions are two. |
| `video.media-conformance` | `VIDEO.CODEC_MISMATCH` | warning | The container's codec differs from the declared one. Both names must resolve through the alias table (`h264`/`libx264`/`h264_videotoolbox`/`avc1`, `hevc`/`hvc1`, `av1`/`av01`, `vp9`/`vp09`, `vp8`, `mpeg4`, `mjpeg`, `prores`) — encoder names are an open namespace, so a spelling Veridex does not recognize means *cannot tell*, and the check abstains rather than calling it a difference. Charged once per stream, per distinct value. |
| `video.media-conformance` | `VIDEO.FPS_MISMATCH` | warning | The container's frame rate (its sample count over its media duration) differs from the declared rate beyond `rate_deviation` — the same relative tolerance `temporal.rate-conformance` uses. Video time drifts against the action timeline, worsening through the episode. Charged once per stream. |

## Provenance — do we know where the data came from?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `provenance.completeness` | `PROVENANCE.INCONSISTENT` | warning | A provenance element's class and value disagree (known/asserted without a value, or unknown with one). |
| `provenance.completeness` | `PROVENANCE.PLACEHOLDER_VALUE` | info | A known/asserted element's value is a low-information placeholder (`unknown`, `n/a`, `none`, …) — present in form but empty in substance, so it doesn't count as real provenance. |
| `provenance.completeness` | `PROVENANCE.PARTIAL` | info | An expected element is recorded for **some** episodes and no others. It is not *missing*, so none of the `MISSING_…` codes fires; and the provenance coverage percentage counts the strongest class found anywhere, so a lineage recorded on one episode of a thousand reads as lineage for the dataset — in the report and in the certificate. An Open X-Embodiment conversion where only part of the shards carried `episode_metadata/file_path` is exactly that. The denominator is the episodes **in this run**: a sampled run says nothing about episodes it did not read (that narrowing is disclosed by the run's own coverage note), so the finding never reports a partiality the request created. A dataset-scoped record covers every episode and is silent here, and so is an element a verified producer **attestation** supplied — signing for an element is a claim about the whole dataset, which is what makes the score count it as covered; what it *is* stays disclosed by `PROVENANCE.ATTESTED`. Note this reports the fact; it does not change the coverage arithmetic, which is rubric-defined. |
| `provenance.completeness` | `PROVENANCE.MISSING_LICENSE` | warning | No license is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_SENSOR` | info | No sensor/device is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_CLOCK` | info | No clock source is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_CALIBRATION` | info | No calibration is known. Silent on a run that opened no stream payload (`--metadata-only`): a bag's calibration lives in its message bodies, so a run that declined to read them cannot tell an uncalibrated dataset from one it did not look at — the same reason `AUTONOMY.CALIBRATION_INCOMPLETE` abstains there. `PROVENANCE.MISSING_UPSTREAM` is silent there for the same reason (RLDS records lineage inside the TFRecord); every other expected element comes from a manifest, header or dataset card, which such a run does read, so its absence means the same thing in either mode. A recording that carries its own extrinsics and camera intrinsics — a ROS transform tree and `CameraInfo`, from MCAP or a rosbag2 bag — supplies this element itself (`Known`, "recorded in-band"), because the calibration is *in* the dataset and in its content hash, which answers the question better than a reference to a file the reader cannot check. |
| `provenance.completeness` | `PROVENANCE.MISSING_ANNOTATOR` | info | No annotator/operator is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_UPSTREAM` | info | No upstream lineage is known. |

An element a verified producer attestation supplies is **not** reported missing. The trust score
counts it as covered — that is what an attestation is for — so reporting it missing would have the
same report say both, and its remedy ("attest this element") would be advice the reader had already
followed. What it is stays disclosed: `PROVENANCE.ATTESTED` names every attested element and the key
that signed it, and `PROVENANCE.ATTESTATION_CONFLICT` names any whose value contradicts what the
dataset records.

Provenance findings do not lower the data-quality sub-score; provenance coverage is a separate 30%
axis of the [trust score](rubric-v1.md).

### What each format can supply

Coverage depends on what the format records about itself, and the honest answer differs per format.
Everything below is **extracted** — read out of the source bytes, class `known`, never asserted —
and anything a source does not carry stays missing rather than being invented. The six expected
elements are `license`, `sensor`, `clock`, `calibration`, `annotator`, `upstream`; `recorder` and the
autonomy rig-lineage keys are recorded too but sit outside the coverage denominator.

| Format | What it supplies, and from where |
|---|---|
| LeRobot | `sensor` from `meta/info.json`'s `robot_type`; `license`, `upstream` (`source_datasets`) and `annotator` (`annotations_creators`) from the dataset card's YAML frontmatter — one element per value, so a dataset merged from two parents records two upstreams. The Hub's "none" values — `original`, `no-annotation` — name nothing and are deliberately not counted |
| RLDS/TFDS | `license` from `dataset_info.json`'s `redistributionInfo`; `upstream` per episode, from the raw file each record was converted from (plus the TFDS split, which keeps eval data from being sampled as training data) |
| HDF5 | `sensor` from a `robomimic` `env_args`' `env_kwargs.robots`; `license` and any [well-known key](#well-known-metadata-keys) among the root attributes; `upstream` per episode, from the group it came from. `author` stays its own element — who wrote the file is not who labelled the data |
| Zarr | the same well-known keys from `.zattrs`, and `upstream` per episode from the group path |
| MCAP | the richest: every well-known key a producer wrote into a `Metadata` record, `recorder` from the header's library/profile, an attachment summary including calibration — and `calibration` **in-band** from a recorded transform tree and `CameraInfo`, which is stronger than a reference to a file nobody can check |
| ROS 2 rosbag2 | `recorder` and storage identity from `metadata.yaml`, every well-known key from its `custom_data` map (what `ros2 bag record --custom-data k=v` writes), plus the same in-band `calibration` as MCAP |
| CAN+DBC | `sensor` from the `BO_` transmitter of every message the log actually carried, one element per node. A node the database declares but that never transmitted is not claimed, and `Vector__XXX` names nobody |
| ASAM MF4 | `sensor` from the `##SI` acquisition sources a channel group or channel points at, one element each, qualified by bus or path; `recorder` from the identification block's writing program |

No format in this set records a clock *source* (as opposed to timestamps) as part of its own layout,
so `clock` is unknown unless a producer wrote it: a `clock`, `clock_source`, `time_source` or
`timebase` key in an MCAP `Metadata` record, an HDF5 attribute or a Zarr `.zattrs` entry is
recognised and read. Otherwise `veridex certify` inputs are the way to fill it.

### Well-known metadata keys

Several formats carry free-form key/value metadata: an MCAP `Metadata` record, a rosbag2
`metadata.yaml` `custom_data` entry, an HDF5 root attribute, a Zarr `.zattrs` entry. One curated table maps the well-known spellings onto typed
provenance for all of them, so a key called `sensor` means the same thing whichever container it was
written in — `device` / `camera_model` / `lidar_model` / `hardware` are `sensor`, `derived_from` /
`source_dataset` / `parent_dataset` are `upstream`, `spdx` / `license_id` are `license`,
`time_source` / `clock_source` / `timebase` are `clock`, and so on,
plus the autonomy rig-lineage keys (firmware, platform, drive, region, map version, redaction,
consent). A producer's arbitrary key is preserved as metadata and never promoted to provenance:
guessing at what an unknown key means is how a verifier starts inventing lineage.

## Autonomy — is the sensor rig internally consistent?

These checks run only on an **autonomy sensor rig** — an episode carrying at least three AV-native
rig sensor streams (point cloud, IMU, GNSS, CAN signal, or ego-pose) drawn from **at least two
distinct modalities**. The second half matters: a CAN or MF4 measurement is dozens of signal streams
off one bus, not several sensors observing the world from different places, so a bus-only log stays
out of rig mode. A manipulation dataset has no rig sensors at all, so these never fire on it. Note
that camera streams are `Video`, not a rig modality — they are checked, but they do not by themselves
make an episode a rig.

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `autonomy.rig-sync` | `AUTONOMY.RIG_SYNC` | error | The rig's sensors span materially different durations over an episode — the widest sensor span minus the tightest exceeds the tolerance (default 50 ms, shares `clock_skew_ms`), widened by the slower sensor's own sampling period — a rig is multi-rate by construction, and each span quantizes to its sensor's period, so a synchronized 10 Hz LiDAR and 100 Hz IMU differ by up to 100 ms with no drift. The N-sensor generalization of `TEMPORAL.CLOCK_SKEW`: one finding names the tightest- and widest-spanning sensors and the drift, and on a rig it *replaces* the pairwise clock-skew report to avoid O(n²) findings for one drifting sensor. Compares **sensors only** — LiDAR/radar, camera, IMU, GNSS, CAN, ego-pose, audio, tactile. A rig log carries more than its rig (`/rosout`, `/parameter_events`, `/diagnostics`, a latched transform tree, a `CameraInfo` channel), and none of those samples the world or keeps a sensor's cadence; grading them here failed a synchronized rig at error severity naming a log topic as the sensor that drifted. Their timing is still reported, by `TEMPORAL.START_OFFSET` / `TEMPORAL.END_OFFSET`. |
| `autonomy.sequence-complete` | `AUTONOMY.SEQUENCE_COMPLETE` | warning | A rig sensor dropped an aggregate fraction of its frames (> 5%, configurable via `sequence_drop_fraction`): its inter-frame gaps sitting at *multiples* of its own median cadence account for the frames those gaps swallowed. Counting multiples rather than dividing the span by the cadence is what keeps an idling event-driven signal from being called incomplete. Catches many small drops that `TEMPORAL.GAP` (single large gap) and `TEMPORAL.RATE` (needs a declared rate MCAP lacks) miss. Robust median baseline; skips streams with too few frames for a stable estimate, and abstains on an event-driven signal whose intervals are far from uniform (no cadence to fall short of — that shape is `TEMPORAL.JITTER`'s). |
| `autonomy.ego-pose-continuity` | `AUTONOMY.EGO_POSE_NON_FINITE` | error | An ego-trajectory step has a non-finite (NaN or infinite) position, so the distance travelled over it cannot be computed. Reported rather than skipped because the skip was worse than silent: `NaN > max_speed` is *false*, so the step was not flagged — and the NaN poisons both pairs it touches, so a trajectory of `(0, NaN, 10000)` over two seconds hid a genuine 10 km/s teleport and certified clean. The continuity of those steps is unverifiable, not verified. |
| `autonomy.ego-pose-continuity` | `AUTONOMY.EGO_POSE_CONTINUITY` | error | The ego trajectory (`Episode.ego_poses`, decoded from Odometry) has a step whose implied speed (distance / elapsed time) exceeds the plausible maximum (100 m/s ≈ 360 km/h, configurable via `ego_max_speed_mps`) — a GPS glitch, localization reset, or stitched log that teleports the ego frame, so every later observation registers against a wrong pose. Reports the worst jump and how many occurred. |
| `autonomy.calibration-completeness` | `AUTONOMY.CALIBRATION_INCOMPLETE` | warning | A rig with spatial sensors (point-cloud or camera) is missing the calibration needed to fuse them: no transform (TF) tree at all, a TF tree split into disconnected components (sensors that can't be related), cameras with no intrinsics (CameraInfo) at all, or **fewer sets of intrinsics than the rig has cameras** — a six-camera surround rig that published one `CameraInfo` (one driver configured, five not) satisfies every *presence* test while five cameras cannot be projected into. That shortfall is counted, never name-matched: a `CameraInfo` names its own topic (`/camera_front/camera_info`), never the image stream it calibrates, so pairing them would mean guessing at the ROS namespace convention and accusing whichever camera the guess missed. Cameras are counted by distinct **coordinate frame**, not by topic, so a bag republishing one camera as `image_raw` plus `compressed` is one camera; when any camera declares no frame the count would be a guess and this abstains (that stream is already reported by `AUTONOMY.SENSOR_FRAME_UNDECLARED`). The principle-respecting form of the LiDAR-camera reprojection check — Veridex never decodes the bulk point/pixel payload, so it verifies the calibration is *present and coherent* rather than reprojecting actual points. The disconnected-tree case is left to `autonomy.sensor-frame-resolution` only when that check can actually name the stranded sensors — every spatial sensor declares a frame and a camera names one the tree knows. Otherwise this reports it, so a broken tree is never silent. Abstains entirely under `--metadata-only`: on a rig log the transform tree and intrinsics are decoded from message *bodies*, which such a run never opens, so it would read the absence it created itself and report a fully calibrated bag as having no TF tree. |
| `autonomy.calibration-completeness` | `AUTONOMY.CALIBRATION_IMPLAUSIBLE` | error | The rig's calibration is *present* and cannot be used: a camera whose focal length is not positive and finite, a principal point that is not a finite non-negative pixel coordinate **or falls outside the image the calibration itself declares**, a non-finite distortion coefficient, a distortion coefficient list whose **length** does not match the model it is declared under, or a transform holding a non-finite value, an all-zero rotation quaternion, or a rotation quaternion whose norm is more than 1% away from 1. This is what an uncalibrated ROS camera driver publishes by default — a `CameraInfo` of zeros — and it satisfies every *presence* test, so the rig scored a clean pass and the `world-model-ready` calibration criterion reported green over a camera that can project nothing. Present is not usable. A quaternion is a rotation only when it is a *unit* quaternion, and the standard quaternion-to-matrix conversion does not renormalize — norm `n` composes a uniform scale of `n²` into the transform, so a 90° yaw written as `[0.707, 0, 0, 0]` (the `w` dropped) places every point at half its real distance from the rig. A `sensor_msgs/msg/CameraInfo` states its image `width` and `height` in the same message as the intrinsic matrix, and `cx`/`cy` are pixel coordinates *in that image* — so a principal point at or past the edge is arithmetic with no answer, not a judgement: the intrinsics were calibrated at one resolution and applied to a stream recorded at another (a `cx` of 960 on a 640-wide image), or `cx`/`cy` were transposed with `fx`/`fy`. Judged only where the source declares the dimension; a format that carries no image size (MF4, HDF5, a driver publishing `width: 0`) is not measured against an assumed one. The distortion coefficients themselves are never interpreted — their meaning is model-specific — but the model names how many there should be: `plumb_bob` takes 5, `rational_polynomial` 8, the fisheye models 4, and a list of a different length cannot be applied under the model it claims. That is what a calibration copied between two models, or a hand-edited YAML with a coefficient deleted, leaves behind. The model namespace is **open**, so an unrecognized name abstains rather than being read as a disagreement (the same rule `canonical_codec` follows for codecs), and an **empty** `d` is what `CameraInfo` specifies for a camera with no distortion, so a rectified stream naming a model and writing no coefficients is not accused. Only **impossibilities** are judged, never implausibility: a long lens, an off-centre principal point, a strong distortion coefficient and a quaternion off unit by rounding are all legitimate, and telling sensible from silly is not attempted. The 1% norm tolerance sits in the wide gap between the two — honest producers miss unit by parts in ten thousand (three-decimal serialization, an unnormalized least-squares fit), the real defects by tens of percent. Error rather than warning — a focal length of zero is arithmetic with no answer. Reported once at **dataset** scope: the calibration is one document, so what is wrong with it is one defect however many episodes the rig recorded, unlike the presence rules above which genuinely ask a per-episode question. At most **eight** unusable elements are named, followed by a sentence counting the rest — these defects are systematic (a producer that drops the `w` component drops it on every edge) and nothing caps how many transforms a file may declare, so the overflow is counted rather than dropped. |
| `autonomy.calibration-completeness` | `AUTONOMY.CALIBRATION_AMBIGUOUS` | error | The rig's calibration is present, connected, well-formed edge by edge — and not a *tree*. Two shapes: a frame given **two different parents** whose validity ranges overlap (two nodes both broadcasting a transform for `lidar_top`, one from `base_link` and one from a mount frame — tf2's `TF_MULTIPLE_PARENT`), or a **cycle** that leaves the tree with no root. Both are invisible to every other calibration check, because `AUTONOMY.CALIBRATION_INCOMPLETE` and `autonomy.sensor-frame-resolution` walk the frame graph **undirected**: they answer whether two sensors *can* be related and nothing about whether the answer is unique. A consumer resolves the sensor's pose through whichever chain it happens to walk, so two tools fusing the same log place the same sensor differently and neither is flagged. A re-parenting across **disjoint** validity windows is a recalibration, not an ambiguity, and is not reported; nor is a disagreement in the numbers between two chains, which is a calibration-quality judgment this does not make. Error rather than warning — which of two chains places the sensor is a question the log does not answer. Reported once at **dataset** scope, like `AUTONOMY.CALIBRATION_IMPLAUSIBLE`. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.SENSOR_FRAME_UNDECLARED` | warning | A spatial sensor on a rig that **does** carry a transform tree declares no coordinate frame at all, so it cannot be located in that tree — what an unconfigured ROS driver publishing an empty `header.frame_id` produces. A warning rather than an error because the recording may be fine for non-geometric use; what is certain is that the sensor's calibration is *unverifiable*, and passing over it silently made this check find nothing, which made the `world-model-ready` criterion it backs read as satisfied. Reported once per stream. Silent when the rig has no transform tree at all — that single defect belongs to `autonomy.calibration-completeness`, which is why an MF4 or CAN rig (no frames, no tree) is not flagged per sensor. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.SENSOR_FRAME_UNKNOWN` | error | A spatial sensor (point-cloud, camera, IMU, GNSS) stamps its data with a coordinate frame (`header.frame_id`) that the transform tree never mentions, so that sensor has no extrinsics. Reported once per stream however many episodes it spans — the calibration is dataset-level, so it is one defect. Invisible to any check on the tree itself: a rig can carry a perfectly connected TF tree recorded for `lidar_top` while the LiDAR publishes `lidar_top_v2`, and every geometric operation involving it silently has no transform. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.SENSOR_FRAME_UNRELATED` | error | A rig sensor's frame is in the transform tree, but no chain of transforms connects it to any camera frame — the extrinsics exist for part of the rig and the link to the image frame is missing, so the sensor's observations cannot be projected into the image. This is the LiDAR-camera miscalibration class, verified as "is the reprojection *defined*"; Veridex never decodes point coordinates or pixels, so it does not compute a reprojection error. Abstains when no camera names a frame the tree knows (nothing to measure against) — in which case `autonomy.calibration-completeness` reports the break instead. Bus signals and ego-pose streams are out of scope: a CAN scalar is never projected into an image, and an ego frame is joined to the body dynamically rather than by static TF. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.EGO_FRAME_UNKNOWN` | error | The ego trajectory is recorded **for** a coordinate frame the transform tree never names — a rig publishing odometry for `base_footprint` while its tree roots at `base_link`. This is a different question from the one the per-stream rules ask, and the one they deliberately do not: an ego-pose stream's own `frame_id` is the *reference* frame (`odom`, `map`), joined to the body dynamically rather than by the static tree, which is why it is excluded from them. The body frame the trajectory is *of* — a `nav_msgs/msg/Odometry`'s `child_frame_id` — is the static question: every sensor's extrinsics hang off it, so a trajectory expressed for a frame outside the tree cannot place a single observation along the drive. Nothing else reports it: the tree is well-formed, every sensor resolves through it, and the trajectory itself is continuous. Silent where the source names no body frame — a trajectory that does not say what it is of is not a trajectory of the wrong thing. |
| `autonomy.gnss-plausibility` | `AUTONOMY.GNSS_IMPLAUSIBLE` | error | A GNSS stream records a latitude outside ±90° or a longitude outside ±180° — not a place. The receiver, the unit conversion, or the field order is wrong, and every downstream use of the trajectory is wrong with it *silently*, because the numbers still look like coordinates. Judged from the per-dimension statistics the `NavSatFix` decode produces, so the flagged value is a real recorded one. Reachable for MCAP and ROS 2 rosbag2, the two sources that carry `sensor_msgs/msg/NavSatFix`; silent where the GNSS was never decoded, which `STATISTICAL.UNMEASURED_VALUES` reports instead — a check that cannot see the values must not report them plausible. |
| `autonomy.gnss-plausibility` | `AUTONOMY.GNSS_UNSET` | warning | A GNSS stream reports latitude **and** longitude of exactly 0 for every frame: a receiver that never acquired a fix. Null Island is a real point in the Gulf of Guinea, so this is judged by exact equality across the whole stream — a vehicle that genuinely drove there would not hold six decimal places of zero for an entire recording, and a receiver that never got a fix reports precisely zero. The same reasoning `STATISTICAL.SATURATED` rests on, and what keeps it free of false positives. A trajectory anchored at Null Island aligns the drive to the wrong point on Earth for anything that fuses it with a map. |
| `autonomy.point-cloud-density` | `AUTONOMY.POINT_CLOUD_EMPTY` | error | Every `PointCloud2` message on a LiDAR or radar stream carried **zero points**. This is the autonomy fault every other check passes: a driver that lost its sensor keeps publishing perfectly-formed clouds at the configured rate, with the right schema, the right `frame_id`, monotonic timestamps and no jitter — so the structural family sees frames, the temporal family sees a clean 10 Hz, `autonomy.sensor-frame-resolution` places the sensor in the tree, and the rig certifies as world-model-ready on a LiDAR that recorded nothing. Read from each message's own `height × width`, which a `PointCloud2` states in its header ahead of the bulk blob — no point payload is decoded. A count is believed only when the body proves it is a `PointCloud2`: the decode walks the field list to the `point_step`/`row_step`/`data` values behind it and requires the message's own invariants to hold (at least one declared field, a non-zero point stride, a record layout that fits that stride — every field's `offset + width × count` inside the point, no two fields sharing a byte, and no datatype tag the message definition does not define — a `row_step` covering a row of `width` points, `data` of exactly `row_step × height` bytes, and those bytes actually present). A channel's declared schema is not proof of its bodies — a mislabelled topic, a truncated write or a stubbed payload would otherwise yield a fabricated count and a finding about honest data. Reported once per stream per episode. Silent where no point counts were read at all (every non-ROS format, and a `--metadata-only` run): a stream whose density was never measured is not a stream measured and found empty. |
| `autonomy.point-cloud-density` | `AUTONOMY.POINT_CLOUD_DROPPED` | warning | Some — not all — of a point-cloud stream's messages carried zero points: the sensor cut out during the recording rather than never starting. Invisible to every timing check, because the empty messages keep the stream's rate and continuity intact, so the timeline says the sensor was present over a stretch it did not see. Warning rather than error: the recording holds real data on either side of the dropout and may be usable once the affected span is cut. The finding names how many messages were empty, out of how many, and how many points the fullest sweep held. |
| `autonomy.point-cloud-density` | `AUTONOMY.POINT_CLOUD_UNMEASURED` | info | A point-cloud stream carries no per-message point count, so the density rules had nothing to measure on it — the sensor was never asked whether it recorded anything, which is indistinguishable in a report from being asked and coming back clean. Reported once for the dataset, naming the streams. **Withheld under `--metadata-only`**: a run that did not open the message bodies read no counts for any stream of any format, so the finding would blame the data for a silence the request caused — `COVERAGE.METADATA_ONLY` states that instead. Informational, not a defect: a dataset is not worse for the container it was published in. What it changes is what a passing autonomy result is evidence of. |

## What the catalog does not check

Two capabilities the [checks-catalog spec](../openspec/specs/checks-catalog/spec.md) describes have
no implementation, and neither is a gap Veridex intends to close by decoding data. A third limit is
not a missing check at all but a property of one source format, recorded here for the same reason.

**Privacy and safety (likely faces, readable text).** Detecting a face in a camera frame requires
decoding pixels and running a model over them. Veridex's design commitment is the opposite — it
reads structure and metadata and fingerprints bytes, never interprets them — so it says nothing
about the content of your imagery. Treat a Veridex pass as evidence that a dataset is *structurally*
sound and traceable, never as a review for personally identifiable information. Run a dedicated PII
tool before publishing.

**Perturbed copies.** Exact duplicates are detected by content (`STRUCTURAL.DUPLICATE_EPISODE`), and
*partial* copies — a re-upload with its tail trimmed, an episode contained in a longer one — by frame
overlap (`STRUCTURAL.NEAR_DUPLICATE_EPISODE`). What neither catches is a copy whose bytes were
changed: a re-encoded video, a trajectory with noise added. Every frame hash differs, so a
similarity measure over frame *payloads* is the only evidence, and that means decoding them. A
dataset can therefore hold two perceptually-identical episodes and pass.

**Measured time, on a format that records none.** RLDS/TFDS has no per-step timestamp, and an HDF5
file has one only when it both stores a timestamp array and declares that array's units. Veridex
stamps the rest of those frames with their step index, marks the stream's `clock_kind` as `step-index`, and
never invents a rate — so the checks that need measured time (`TEMPORAL.RATE`, `NON_MONOTONIC`,
`GAP`, `JITTER`, `CLOCK_SKEW`, `START_OFFSET`, `END_OFFSET`, `EPISODE_DURATION_OUTLIER`) skip those
streams rather than grading them.

The abstention is **reported, not silent**, and that distinction is the whole point. A step index is
flawlessly monotonic, perfectly regular, and identical across every stream of an episode, so a check
that graded it would *pass* — and a passing temporal result is what reaches the report and the signed
certificate, where it reads as "these sensors are synchronized". So every such run emits
`TEMPORAL.UNMEASURED_CLOCK` (info), naming the clock and the streams; it appears in the terminal
report, the JSON, the SARIF, the HTML, and the certificate's own findings summary. What still
applies is everything derived from structure and content — episode counts, shapes, duplicates,
stuck streams, empty streams, annotation integrity — plus each format's own integrity gate, enforced at
ingest rather than as a finding: a record whose step features disagree on the episode's length, or
whose TFRecord CRC-32C fails, is refused by name instead of mapped into a CDM that would read as
sound. (HDF5's gate is the same idea one level down: a chunk that fails its stored `fletcher32`
checksum, or inflates to the wrong size for its own shape, is refused rather than read past.) Frame
counts are the one thing genuinely not covered *for RLDS*: it declares no total, so
`STRUCTURAL.FRAME_COUNT_MISMATCH` has nothing to test against. An HDF5 file that writes `num_samples`
or `total` does get that check.

All three limits are recorded here rather than left implicit, on the same principle as the rest of
the catalog: a check that abstains must say so, or its silence reads as a pass.

## What a metadata-only run checks

`veridex check --metadata-only <dataset>` (LeRobot, ROS 2 rosbag2, RLDS/TFDS, Zarr, MCAP, HDF5, and MF4) answers a narrower question — *does
the manifest hold together?* — without opening a single Parquet, shard, or video file. It is the
fast CI gate for a dataset too large to read on every commit, and the shape a remote Hub check takes.

What each format offers is different, because their manifests are:

| Format | Read without touching the data |
|---|---|
| LeRobot | `meta/` — the declared episode set and per-episode lengths, every feature's dtype/shape/rate, the stored statistics, and the dataset card's licence, source datasets and annotation creators |
| ROS 2 rosbag2 | `metadata.yaml` — the topic inventory with each topic's ROS type, the declared message total, the recorder and storage |
| RLDS/TFDS | `dataset_info.json` + `features.json` — the per-split shard lengths (so the episode count), the file format and version, the citation and licence, and every per-step feature's dtype and shape |
| Zarr | `.zarray` / `.zattrs` per array and the `meta/` group — the episode boundaries and their lengths, every array's dtype and per-row shape, and the store's own metadata |
| MCAP | the summary section at the end of the file — the channel inventory with each topic's schema, the declared message totals and log-time span, the message encodings, the writing library, and (through the summary's own indexes) every Metadata record and attachment name, so the provenance matches a full read |
| HDF5 | the group tree and array headers — every episode group, each array's datatype and per-row shape, each group's declared length attribute, and every object attribute (including the robot a `robomimic` `env_args` names) |
| MF4 | the `##HD`/`##DG`/`##CG`/`##CN` block tree — every channel's name and raster, and the cycle count each group declares, without opening or decompressing a data block |

What still applies, because it is all manifest content:

| Family | Still checked |
|---|---|
| Structural | declared vs. actual episode count, duplicate episode indices, cross-episode dtype/shape consistency, stream-presence consistency, episode-index gaps |
| Statistical | every stored-statistics check over `meta/stats.json` — inverted ranges, non-finite values, a mean outside its own min/max, an impossible standard deviation (LeRobot is the only one of the five that stores statistics) |
| Provenance | the whole family: license, sensor, source format, and every completeness/consistency rule — noting that a provenance element living inside the data is *absent* here rather than assumed, so an RLDS export scores lower on provenance than a full read of it, honestly |

What abstains, and why it must:

| Check | Why it cannot run |
|---|---|
| `structural.declared-frame-count` | The declared total is a claim about frames, and no frame was read. |
| `structural.episode-boundary` (declared-length arm) | Same: "declares 120, ingested 0" is true of every sound dataset checked this way. Its duplicate-index and inverted-bounds arms still run. |
| `structural.degenerate-episode` (per-stream arms) | Every stream carries no frames *by request*, so `EMPTY_STREAM` would fire on all of them. `EMPTY_DATASET` and `EMPTY_EPISODE` still run. |
| every temporal check | There are no timestamps to grade. |
| every recomputed statistical check, `structural.stuck-stream`, `structural.duplicate-episode`, every video check | All read values, per-frame content hashes, or media headers. |

Each format also refuses what it cannot honestly approximate rather than guessing: a bare `.db3`
with no `metadata.yaml`, a rosbag2 or MCAP whose per-topic counts do not add up to its own declared
total, a TFDS export with no shard lengths, an MCAP written without a summary section, and a
Zarr store whose layout yields no episodes at all (neither `meta/episode_ends` nor a group of
arrays). Each refusal names the file or field at fault and the way to
get an answer anyway.

And one refusal worth naming: when the episode set comes from `info.json`'s `total_episodes` alone
(no `meta/episodes.jsonl`), the declared-episode-count check is **withheld** rather than run, because
the episode set *is* that number and the comparison could not fail. A check that cannot fail passing
is exactly the silence this catalog exists to prevent. With `meta/episodes.jsonl` present, the total
is an independent second assertion and the check runs.

The verdict carries `coverage: {"kind": "metadata_only", …}` — bound into its hash, printed in every
report — and `certify` refuses to issue a certificate from it. A dataset whose manifest is sound has
been told nothing about its data.

## When a check could not measure anything

Three checks in the catalog exist only to report what the rest of their family could *not* do, and
they are all built the same way — as findings, because a finding is the one disclosure that reaches
the terminal report, the JSON, the SARIF, the HTML, and the certificate's own summary:

| Check | Says |
|---|---|
| `temporal.clock-measurability` | the source records no wall clock, so the timing checks graded a step index or nothing at all — and, separately, that no two streams shared a clock with a measurable span, so the three cross-stream checks had nothing to compare |
| `statistical.value-measurability` | the adapter never read values (any MCAP or rosbag2 topic other than a `JointState` or an `Imu`, and any `bytes_list` leaf of an RLDS record), or read them but had no stored statistics to compare against (HDF5, Zarr, CAN+DBC, MF4, RLDS numeric leaves, bag `JointState` / `Imu` topics) |
| `structural.content-measurability` | frames carry no content fingerprint, so the duplicate-episode and stuck-stream checks had no bytes to compare — and, separately, that the run covers too few episodes for the seven checks that answer by comparing one episode against another |

A fourth disclosure comes from the engine rather than the catalog. When a check *crashes* instead of
producing findings, that is neither a pass nor a defect in the data — it is a hole where a
measurement should be. The verdict records it under `errored_checks`, the terminal, HTML and JSON
reports name it, SARIF reports it as **`VERIDEX.CHECK_ERRORED`**, and the certificate lists it under
`checks_errored` rather than `checks_run`. `diff --fail-on-regression` treats a check that newly
crashed as a regression, because a crash costs the score less (10 points) than the error finding it
suppressed (15) — so without that, a check panicking instead of reporting looked like a fix.

A fifth comes from the same place. When a run is judged against a policy profile (`--profile`), the
readiness verdict is reported to every output shape — and SARIF reports it as
**`VERIDEX.PROFILE_NOT_READY`**, at `error` level when the profile applies and a criterion is
unsatisfied, `note` otherwise. A profile that does not apply, or a criterion whose check did not run,
is not a pass; it is an absence of judgement, and a code-scanning gate reading the SARIF alone could
not previously tell the difference — that branch was the one output shape rendered without the
readiness block.

All three catalog checks are **informational**: a dataset is not worse for the container it was
published in. What they change is what a passing verdict is evidence *of*. Without them, a recording
whose actuator is pinned at its rail reported `data 100` with no statistical findings — and the
certificate listed all five statistical checks under `checks_run` with `categories_skipped: []`. The
original example was a CAN log, which no longer abstains: its signals are decoded into values, so
they are measured — and neither does an MF4 measurement, for the same reason. The point stands for every container whose payload Veridex fingerprints without
interpreting.

## Coverage disclosure

A run that read less than the whole dataset emits **`COVERAGE.SAMPLE`** or **`COVERAGE.METADATA_ONLY`**
(info, check id `veridex.coverage`) alongside its findings.

A run that read less than the whole dataset *without being asked to* emits
**`COVERAGE.SOURCE_UNREAD`** (**warning**, same check id): the dataset declares data the adapter did
not read. Today that is a data shard resolving outside the dataset directory, a rosbag2 shard the
bag's `metadata.yaml` lists but does not ship, a rosbag2 recording that falls short of the message
total its own manifest declares, a rosbag2 message on a topic the bag's `topics` table never
declares (there is no topic name to file it under, and inventing one would name a topic the bag does
not), an HDF5 object holding rows that sits outside the episodes — `robomimic`'s `/mask` split
group, a reward table parked at the root, an array beside the `demo_N` groups — and CAN traffic that
went into no signal stream: frames on an id the `.dbc` never defines, and log lines that are not
candump frames (CAN-FD `##`, RTR). The
verdict's `coverage` field cannot express this, because a `Coverage::Full` ingest is one that read
everything it was *willing* to read, which is not the same as everything the dataset declared. Until
this existed, a LeRobot dataset with one of its two Parquet shards symlinked out of the directory
produced the same `coverage: Full`, the same findings, the same score, and a certifiable verdict
naming the whole dataset over the half that was read; `diff` reported zero change between them.

What counts as unread is a judgement each adapter makes, and the line is whether the data is *there*:
an MF4 data block this reader does not decode into a record stream, an MF4 channel group with no time master or a record id nothing claims, a
Zarr array whose codec it cannot apply, a rosbag2 shard short of its manifest's message total, an
MCAP short of the total in its own summary — all of it is data nobody looked at, so all of it lands
here. A field the CDM has no shape for lands in `unmapped` instead and raises nothing, because it
costs the reader nothing.

It is a warning where the other two are info, and the difference is who asked. A sampled or
metadata-only run is narrow because the operator requested it, so the disclosure is a note about the
request. Data unread because a shard points outside the dataset was requested by nobody — the
dataset misrepresented itself, which is a defect in the data of the kind this tool exists to report.

This is deliberately a *finding* and not only the verdict's `coverage` field. The field itself is
rendered by the terminal report, the JSON, the HTML, and the `diff` — but **not** by SARIF, so a CI
job gating on a code-scanning upload would see a partial run as a clean scan of the whole dataset.
Findings are the one channel that reaches every renderer and the certificate's own summary — the
same reasoning behind `temporal.clock-measurability`, and behind SARIF synthesizing a result for a
check that errored rather than letting its silence pass.

`diff` reads coverage directly as well as through the finding: `render_diff` states a coverage
change before anything else, `diff --json` carries a `coverage` object, and `--fail-on-regression`
treats a change as the top regression signal. Without that, substituting a metadata-only report for
a full one silences most of the catalog, so the full run's findings read as *resolved* and the trust
score goes **up** — a regression gate passing precisely because the new run stopped looking.

`veridex.coverage` is not a registered check: coverage is a property of the ingest, which no check
can read from the CDM. It is emitted by the engine directly, so `veridex checks` still lists exactly
the checks that run, and no configuration can disable the disclosure.

## Redaction disclosure

A report is diagnostics, so it quotes the dataset: stream keys, task strings, annotator names,
licenses. `veridex check --redact` prepares a report to leave the building — the dataset identifier,
stream names, task and label text, and provenance values are replaced with stable placeholders
(`stream#1`, `text#2`), consistent within one report and meaningless outside it — and discloses that
it did with **`REPORT.REDACTED`** (info, check id `report.redaction`).

Like the coverage and scope disclosures, it is a *finding* rather than a printed banner, so it
reaches JSON, SARIF, HTML, the terminal and `diff` alike: the machine-readable report is the one most
likely to be handed to someone else, and a rendering-only banner would be invisible there.

What redaction deliberately keeps: episode indices, timestamps, frame counts, and every measured
quantity — a 210 ms drift, a 12σ outlier, a saturated fraction. Those *are* the finding; a report
without them is not redacted, it is empty. It also keeps the CDM content hash, which is what lets
whoever holds the dataset match the report to it. And it keeps the verdict itself: the status, the
score, and the exit code are the run's own, so a shared report and the private one describe the same
run.

What it does not promise: substitution is best-effort over text. An identifier shorter than three
characters is left alone (a one-character stream name collides with ordinary prose far more often
than it hides anything), and a name that is also an ordinary word may be replaced where it was not an
identifier — over-redaction, which is the safe direction. Read a redacted report as one you may
share, not as proof that nothing about the data can be inferred from it. `certify` refuses `--redact`
outright: a certificate attests a dataset by name and hash, and a redacted one would say less than it
attests. And `veridex diff` refuses to compare a redacted report against a plain one — every finding
naming a stream or a path differs textually between them, so the same findings appear as *introduced*
and *resolved* at once; `--fail-on-regression` treats the mismatch as a regression, exactly as it
treats a coverage change.

## Attestation disclosure

Most of what provenance means is not in the file: no format records who operated the robot, which
calibration was in force, or what upstream a merge drew from. `veridex attest` lets the producer sign
for those, bound to the dataset's CDM content hash, and `check --attestation` / `certify
--attestation` apply what verifies.

An attested element counts as **asserted** — the same class and the same weight as a value the source
asserts rather than records — so it raises provenance coverage, which is 30% of the trust score. That
makes disclosure part of the deal:

- **`PROVENANCE.ATTESTED`** (info, check id `veridex.attestation`) names the producer key and every
  element that came from it. A reader who does not trust that key can subtract exactly those.
- **`PROVENANCE.ATTESTATION_CONFLICT`** (warning) fires when an attested value contradicts what the
  dataset itself records as `known`. Veridex keeps the extracted value and reports the disagreement:
  either the recording is wrong or the claim is, and a signature does not get to rewrite the data's
  own account of itself.

`veridex provenance --emit ... --attestation` carries the attested elements into the emitted
Croissant and PROV documents, marked `asserted` and with the producer key named — a shared
provenance document that omitted them would describe less than the run did.

An attestation never enters the CDM, so it cannot change the content hash — a claim about the data
must not change what the data *is*. It is refused if its signature does not verify, or if it is bound
to a different dataset than the one presented.

## Scope disclosure

Coverage answers *how much of the dataset did we read*. **`SCOPE.NARROWED`** (info, check id
`veridex.scope`) answers the other half: *how much of the catalog did we run*.

It is emitted whenever fewer checks ran than are registered — `categories`, `only_checks` or
`disabled_checks` — a severity was overridden on a check that did run, or a **numeric tolerance was
loosened** off its default. That third axis hid in the gap between the first two: a loosened
threshold deselects nothing, so the check runs, measures the defect, and passes it, and the run looks
complete on every count. The direction is part of the test: a tolerance moved to be *stricter*
measures the data harder than the catalog asks and can only lower the score, so it is printed in the
report's "Tolerances (non-default)" line but is **not** a narrowing. `--profile world-model-ready`
tightens cross-sensor sync to 20 ms by construction, so a direction-blind test made every readiness
run — and every readiness certificate — report itself as narrowed. On the demo MCAP two `[tolerances]` lines took a run from exit `20` carrying a real
210 ms `TEMPORAL.CLOCK_SKEW` to exit `0` carrying neither it nor `TEMPORAL.END_OFFSET`, with no
trace in the SARIF, while `diff --fail-on-regression` exited 0 calling the 13-point climb an
improvement. The disclosure now names which thresholds were loosened and to what. Until it existed, a
`veridex.toml` carrying one line rewrote the verdict everywhere it mattered. On the demo MCAP,
`only_checks = ["structural.episode-boundary"]` turned `FAIL / 76 / 5 findings` into
`PASS / 89 / 0 findings`, with no trace of the narrowing in the terminal report, the HTML report,
the SARIF upload, or a signed certificate — and `diff --fail-on-regression` read the five vanished
findings, including a real 210 ms clock skew, as *resolved*, saw the score climb 13 points, and
exited 0.

The remedy is the one coverage already uses, for the same reason: findings are the only channel that
reaches every renderer, the `diff`, and the certificate's own summary. `effective_config` had
carried the facts all along, in the JSON envelope and the certificate — but only for a reader who
thought to look, and never for the two human-facing renderers or the regression gate.

Like `veridex.coverage`, `veridex.scope` is not a registered check, so configuration cannot switch
off the disclosure *that* configuration narrowed the run. It is measured from what actually happened
— checks executed versus registered — rather than from the config's wording, so an `only_checks` that
names the whole catalog is correctly silent, and a full run at declared severities emits nothing at
all (leaving ordinary output and content hashes unchanged).

`veridex verify` names the same limit beside the score, because a certificate issued from a narrowed
run is genuine, correctly bound, and still not a verdict on the dataset.

A narrowed run also cannot carry a **score gate**: `--min-score` is refused with exit `2` over a
narrowed run, a sampled run, or a `--metadata-only` one. The data axis starts at 100 and only
deducts, so anything that stops a check from measuring raises the score — `categories = []` runs no
checks at all and scores a perfect 100 on that axis. A gate that quietly does nothing is worse than
one that is absent.
