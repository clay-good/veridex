# Veridex check catalog

Every finding Veridex can emit, by check. Run `veridex checks` for the live list — each check lists
the finding codes it can emit (add `--json` for a machine-readable catalog) — and
`veridex check <dataset>` to run them. Each finding carries a training
**risk** and a **remedy**; this page is the quick reference for the codes you'll see.

Checks are selected, disabled, or re-severitied via [`veridex.toml`](veridex.toml.example).
Severities below are the defaults. A threshold marked *configurable* has a `veridex.toml` key; the
others are fixed defaults today (config-wiring them is a follow-up).

## Structural — is the dataset shaped like trainable data?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `structural.episode-boundary` | `STRUCTURAL.EPISODE_BOUNDARY` | error | A per-episode declared `length` (e.g. LeRobot `meta/episodes.jsonl`) that disagrees with the frames ingested, duplicate episode indices, or an episode whose `start_ts > end_ts` — all signatures of corrupt cumulative boundaries (the lerobot#4143 class), where frames load under the wrong episode. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_DATASET` | error | The dataset has no episodes at all. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_EPISODE` | error | An episode has no streams. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_STREAM` | error | A stream has no frames. |
| `structural.degenerate-episode` | `STRUCTURAL.SINGLE_FRAME_STREAM` | warning | A stream has a single frame (no temporal signal). |
| `structural.episode-continuity` | `STRUCTURAL.EPISODE_INDEX_GAP` | warning | Episode indices are non-contiguous (e.g. `0, 1, 3`) — an episode was dropped between export and ingest. Needs no manifest, unlike the declared-count check. |
| `structural.declared-episode-count` | `STRUCTURAL.EPISODE_COUNT_MISMATCH` | error | The manifest's declared episode count (e.g. LeRobot `total_episodes`) differs from the episodes ingested (a truncated export). |
| `structural.declared-frame-count` | `STRUCTURAL.FRAME_COUNT_MISMATCH` | error | The manifest's declared frame count (e.g. LeRobot `total_frames`) differs from the frames ingested (episodes present but cut short). |
| `structural.shape-consistency` | `STRUCTURAL.SHAPE_MISMATCH` | error | A stream keeps a different declared dtype/shape across episodes (un-batchable). |
| `structural.stream-presence` | `STRUCTURAL.STREAM_PRESENCE_INCONSISTENT` | warning | A stream key is present in some episodes but missing from others — a heterogeneous feature set (a sensor that dropped out, or two exports pooled together). |
| `structural.stuck-stream` | `STRUCTURAL.STUCK_STREAM` | warning | A `Video` stream repeats a byte-identical frame (same `content_hash`) for ≥5 consecutive frames while timestamps advance — a frozen/stuck camera the timestamp-based temporal checks can't see. Real camera frames are never byte-identical, so this is unambiguous. Only frames carrying a `content_hash` are compared (MCAP images are fingerprinted; LeRobot video lives outside the Parquet, so the check abstains there). Constant *scalar* streams are `STATISTICAL.DEGENERATE`'s concern, not this. |
| `structural.duplicate-episode` | `STRUCTURAL.DUPLICATE_EPISODE` | warning | Two or more episodes have identical frame **content** (same schema, timestamps, and per-frame `content_hash`) — a re-upload or a bad merge that over-weights the repeated trajectories. Sound-only: an episode is compared solely when every frame carries a `content_hash`, so it never mis-flags two different same-length episodes that merely share a time base and dataset-global stats. Fires once adapters populate per-frame content hashes; near-duplicate detection needs frame payloads and is out of MVP scope. |

## Temporal — is the time base sound?

Every check in this family except `temporal.rate-validity` and `temporal.rate-consistency` (which
grade a *declared* rate, not a timeline) reads only streams whose timestamps are **measured time**.
A source that records no clock — RLDS/TFDS has no per-step timestamp, and neither HDF5 nor Zarr has
any notion of time — carries a step index instead,
and an index satisfies all of them trivially: flawlessly monotonic, perfectly regular, identical
across every stream of an episode. Grading it would put a clean temporal result in a report and a
signed certificate on the strength of a timeline nobody measured, so those streams are skipped and
`temporal.clock-measurability` reports that they were.

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `temporal.clock-measurability` | `TEMPORAL.UNMEASURED_CLOCK` | info | A stream's timestamps are a positional step index rather than measured time, so the rate, gap, jitter, clock-skew, start/end-offset and episode-duration checks had nothing to grade. Reported once per clock for the dataset (the clock is a property of the source format, not of an episode), naming the streams. Not a defect in the data — what it changes is what a passing temporal result is *evidence of*: the absence of a measurement, not good timing. |
| `temporal.monotonicity` | `TEMPORAL.NON_MONOTONIC` | error | Timestamps within a stream do not strictly increase (out-of-order or duplicated frames). Streams sharing one timeline (a CAN or MF4 group off a single clock) are reported once, naming the others. |
| `temporal.rate-validity` | `TEMPORAL.INVALID_RATE` | error | A stream declares a sampling rate that isn't a positive, finite number (`0`, negative, `NaN`, `inf`) — corrupt metadata the rate and gap checks would otherwise skip silently. |
| `temporal.rate-conformance` | `TEMPORAL.RATE` | warning | The observed mean rate deviates from the declared rate beyond tolerance. |
| `temporal.gap` | `TEMPORAL.GAP` | warning | An inter-frame interval is far larger than expected (dropped frames). |
| `temporal.jitter` | `TEMPORAL.JITTER` | warning | A stream's inter-frame intervals are badly irregular (coefficient of variation above tolerance) even though the mean rate can look correct — a jittery timeline that `RATE` and `GAP` both miss. |
| `temporal.clock-skew` | `TEMPORAL.CLOCK_SKEW` | error | Two streams in an episode span materially different durations — the headline cross-stream drift check. The tolerance (`clock_skew_ms`, default 50 ms) is widened by the larger of the two streams' own sampling periods: a stream observing a window at period `T` spans a whole number of `T`s, so two synchronized sensors at different rates differ by up to one period with no drift at all. |
| `temporal.start-offset` | `TEMPORAL.START_OFFSET` | warning | Two streams sharing a `clock_id` start at materially different absolute times (a sensor that came online late) — a misalignment `CLOCK_SKEW`'s duration comparison can miss. |
| `temporal.end-offset` | `TEMPORAL.END_OFFSET` | warning | Two streams sharing a `clock_id` end at materially different absolute times (a sensor that dropped out early, or a truncated tail) — the mirror of `START_OFFSET`; because `end = start + duration`, a tail misalignment can slip past both `START_OFFSET` and `CLOCK_SKEW`. |
| `temporal.rate-consistency` | `TEMPORAL.RATE_INCONSISTENT` | warning | A stream declares one sampling rate in some episodes and a materially different one in others — differently-configured sources pooled under one key, or wrong rate metadata. Every per-episode check passes, but a global fixed-rate assumption is wrong for part of the data. The temporal sibling of `STRUCTURAL.SHAPE_MISMATCH`. |
| `temporal.episode-duration` | `TEMPORAL.EPISODE_DURATION_OUTLIER` | warning | An episode's total duration is a large multiple (default 10×, configurable) away from the dataset's *median* episode duration — a truncated capture cut short, or a recorder left running, not natural task variation. The median baseline is robust to the outliers it hunts; the check abstains below 4 measurable-duration episodes (no stable "typical"). |

## Statistical — do the stored per-stream statistics hold together?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `statistical.range-sanity` | `STATISTICAL.NON_FINITE` | error | A stored min/max/mean/std is NaN or infinite. |
| `statistical.range-sanity` | `STATISTICAL.RANGE_INVERTED` | error | Stored `min > max`. |
| `statistical.range-sanity` | `STATISTICAL.NEGATIVE_STD` | error | Stored standard deviation is negative. |
| `statistical.range-sanity` | `STATISTICAL.MEAN_OUT_OF_RANGE` | error | The stored mean lies outside `[min, max]`. |
| `statistical.range-sanity` | `STATISTICAL.STD_IMPLAUSIBLE` | error | The stored `std` exceeds `(max − min) / 2`, the largest value possible for that range (Popoviciu's inequality) — the stats don't match the data. The slack allowed scales with the magnitude of the values, so honest float cancellation on a near-constant channel at 300.0 isn't called impossible. |
| `statistical.range-sanity` | `STATISTICAL.DTYPE_RANGE` | error | The stored min/max falls outside what the stream's declared integer dtype can represent (e.g. a `uint8` with max `300`) — the dtype or the stats are wrong. |
| `statistical.range-sanity` | `STATISTICAL.DEGENERATE` | warning | The stream is constant (`min == max`, and `std` zero within a rounding tolerance that scales with the magnitude of the values — an exporter computing `std` in floating point reports a constant channel's spread as noise, not exactly zero). |
| `statistical.stored-vs-observed` | `STATISTICAL.STATS_STALE` | error | The stored stats and the values Veridex recomputed from the data disagree: a real value falls outside the stored `[min, max]`. The stored `meta/stats.json` is stale or was computed on different data, so normalization built from it clips/distorts the true inputs. Only min/max are compared (convention-free). For a multi-DoF feature it compares **per dimension** — LeRobot's `stats.json` stores per-element arrays and normalization is per dimension, so a stale stat in one joint is caught and named, where an element-0-only check would falsely report a match. Reachable where the adapter reads feature values *and* the source stores its own (LeRobot); HDF5 stores no statistics, so there is nothing to compare its recompute against. |
| `statistical.saturation` | `STATISTICAL.SATURATED` | warning | A large fraction (default ≥50%) of a stream's recomputed values sit **exactly** at one extreme — a clamped/saturated actuator or a state pinned against a rail. The controller can't tell "at the limit" from "wants to go further," so the policy imitates an observation that no longer tracks intent. Exact-equality is the signal (a noisy sensor never lands on the same float repeatedly), so it's false-positive-free; fully constant streams are `STATISTICAL.DEGENERATE`'s concern. Saturation is judged **per dimension** — a gripper pinned at element 6 of a 7-DoF `action` vector is caught, not just element 0, and the finding names the saturating dimension. Abstains below 20 samples, and where the adapter doesn't read feature values (MCAP). Reachable for LeRobot, HDF5, and Zarr. |
| `statistical.non-finite-observed` | `STATISTICAL.NON_FINITE_OBSERVED` | error | The recorded **data** holds a NaN or ±infinity value. Distinct from `STATISTICAL.NON_FINITE`, which inspects the source's stored `stats.json`: a clean or absent summary can still hide non-finite values in the actual feature cells, and only a recompute over real values sees them. Veridex holds these out of the recomputed summary (a NaN would poison every stat) and counts them separately, scanning **every dimension** of a multi-DoF cell (a NaN buried in one joint of a 7-DoF arm is still caught). A single NaN propagates to a NaN loss and silently kills a training run. Reachable where the adapter reads feature values (LeRobot, HDF5, Zarr); MCAP doesn't decode payloads and abstains. An integer array cannot hold a NaN, so reading it is enough to report it clean — a different answer from never having read it. |
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
| `video.media-readable` | `VIDEO.MEDIA_ABSENT` | error | The manifest declares a stream's pixels live in video files (`dtype: "video"`) and **no** media file for it was found at all — the signature of an un-pulled LFS pointer or an interrupted download. Charged once for the stream: one tree that never arrived is one gap, not one per episode. Nothing in the manifest or the data table records this, so without the check the dataset reads as complete until a loader tries to open a video. |
| `video.media-readable` | `VIDEO.MEDIA_MISSING` | error | An episode's media file is absent, though the dataset stores that stream's video one file per episode. The episode's rows claim imagery the dataset does not hold. |
| `video.media-readable` | `VIDEO.MEDIA_UNREADABLE` | error | The file exists but is not a readable container — the finding names the structure that was wrong (no `moov` box, a truncated header, a box declaring more bytes than remain). A container that will not parse will not decode, and training fails at that episode hours into a run. |
| `video.media-conformance` | `VIDEO.FRAME_COUNT_MISMATCH` | error | The container's sample count differs from the frames the paired data stream carries. Every video/data pair past the shorter of the two is wrong, so the policy learns actions against images from a different moment. Reported per episode — one bad video is one bad episode — **except** when every episode of a stream is off by the same amount, which is one systematic export defect (an encoder dropping a leading frame, a converter counting from one) and is charged once. Rolling it up changes how often it is reported, never its severity. |
| `video.media-conformance` | `VIDEO.RESOLUTION_MISMATCH` | warning | The container's encoded resolution differs from the manifest's. The declared resolution comes from `info.video.width`/`height`, falling back to the feature's `shape` — read through the manifest's own axis `names`, or, absent those, only when the shape is unambiguously channel-last. Charged **once per stream per distinct value**, naming the first episode and how many share it: a re-export is one defect however many episodes it touched, but two episodes at two different wrong resolutions are two. |
| `video.media-conformance` | `VIDEO.CODEC_MISMATCH` | warning | The container's codec differs from the declared one. Both names must resolve through the alias table (`h264`/`libx264`/`h264_videotoolbox`/`avc1`, `hevc`/`hvc1`, `av1`/`av01`, `vp9`/`vp09`, `vp8`, `mpeg4`, `mjpeg`, `prores`) — encoder names are an open namespace, so a spelling Veridex does not recognize means *cannot tell*, and the check abstains rather than calling it a difference. Charged once per stream, per distinct value. |
| `video.media-conformance` | `VIDEO.FPS_MISMATCH` | warning | The container's frame rate (its sample count over its media duration) differs from the declared rate beyond `rate_deviation` — the same relative tolerance `temporal.rate-conformance` uses. Video time drifts against the action timeline, worsening through the episode. Charged once per stream. |

## Provenance — do we know where the data came from?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `provenance.completeness` | `PROVENANCE.INCONSISTENT` | warning | A provenance element's class and value disagree (known/asserted without a value, or unknown with one). |
| `provenance.completeness` | `PROVENANCE.PLACEHOLDER_VALUE` | info | A known/asserted element's value is a low-information placeholder (`unknown`, `n/a`, `none`, …) — present in form but empty in substance, so it doesn't count as real provenance. |
| `provenance.completeness` | `PROVENANCE.MISSING_LICENSE` | warning | No license is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_SENSOR` | info | No sensor/device is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_CLOCK` | info | No clock source is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_CALIBRATION` | info | No calibration is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_ANNOTATOR` | info | No annotator/operator is known. |
| `provenance.completeness` | `PROVENANCE.MISSING_UPSTREAM` | info | No upstream lineage is known. |

Provenance findings do not lower the data-quality sub-score; provenance coverage is a separate 30%
axis of the [trust score](rubric-v1.md).

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
| `autonomy.rig-sync` | `AUTONOMY.RIG_SYNC` | error | The rig's sensors span materially different durations over an episode — the widest sensor span minus the tightest exceeds the tolerance (default 50 ms, shares `clock_skew_ms`), widened by the slower sensor's own sampling period — a rig is multi-rate by construction, and each span quantizes to its sensor's period, so a synchronized 10 Hz LiDAR and 100 Hz IMU differ by up to 100 ms with no drift. The N-sensor generalization of `TEMPORAL.CLOCK_SKEW`: one finding names the tightest- and widest-spanning sensors and the drift, and on a rig it *replaces* the pairwise clock-skew report to avoid O(n²) findings for one drifting sensor. |
| `autonomy.sequence-complete` | `AUTONOMY.SEQUENCE_COMPLETE` | warning | A rig sensor dropped an aggregate fraction of its frames (> 5%, configurable via `sequence_drop_fraction`): its inter-frame gaps sitting at *multiples* of its own median cadence account for the frames those gaps swallowed. Counting multiples rather than dividing the span by the cadence is what keeps an idling event-driven signal from being called incomplete. Catches many small drops that `TEMPORAL.GAP` (single large gap) and `TEMPORAL.RATE` (needs a declared rate MCAP lacks) miss. Robust median baseline; skips streams with too few frames for a stable estimate, and abstains on an event-driven signal whose intervals are far from uniform (no cadence to fall short of — that shape is `TEMPORAL.JITTER`'s). |
| `autonomy.ego-pose-continuity` | `AUTONOMY.EGO_POSE_CONTINUITY` | error | The ego trajectory (`Episode.ego_poses`, decoded from Odometry) has a step whose implied speed (distance / elapsed time) exceeds the plausible maximum (100 m/s ≈ 360 km/h, configurable via `ego_max_speed_mps`) — a GPS glitch, localization reset, or stitched log that teleports the ego frame, so every later observation registers against a wrong pose. Reports the worst jump and how many occurred. |
| `autonomy.calibration-completeness` | `AUTONOMY.CALIBRATION_INCOMPLETE` | warning | A rig with spatial sensors (point-cloud or camera) is missing the calibration needed to fuse them: no transform (TF) tree at all, a TF tree split into disconnected components (sensors that can't be related), or cameras with no intrinsics (CameraInfo). The principle-respecting form of the LiDAR-camera reprojection check — Veridex never decodes the bulk point/pixel payload, so it verifies the calibration is *present and coherent* rather than reprojecting actual points. The disconnected-tree case is left to `autonomy.sensor-frame-resolution` only when that check can actually name the stranded sensors — every spatial sensor declares a frame and a camera names one the tree knows. Otherwise this reports it, so a broken tree is never silent. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.SENSOR_FRAME_UNKNOWN` | error | A spatial sensor (point-cloud, camera, IMU, GNSS) stamps its data with a coordinate frame (`header.frame_id`) that the transform tree never mentions, so that sensor has no extrinsics. Reported once per stream however many episodes it spans — the calibration is dataset-level, so it is one defect. Invisible to any check on the tree itself: a rig can carry a perfectly connected TF tree recorded for `lidar_top` while the LiDAR publishes `lidar_top_v2`, and every geometric operation involving it silently has no transform. |
| `autonomy.sensor-frame-resolution` | `AUTONOMY.SENSOR_FRAME_UNRELATED` | error | A rig sensor's frame is in the transform tree, but no chain of transforms connects it to any camera frame — the extrinsics exist for part of the rig and the link to the image frame is missing, so the sensor's observations cannot be projected into the image. This is the LiDAR-camera miscalibration class, verified as "is the reprojection *defined*"; Veridex never decodes point coordinates or pixels, so it does not compute a reprojection error. Abstains when no camera names a frame the tree knows (nothing to measure against) — in which case `autonomy.calibration-completeness` reports the break instead. Bus signals and ego-pose streams are out of scope: a CAN scalar is never projected into an image, and an ego frame is joined to the body dynamically rather than by static TF. |

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

**Near-duplicate episodes.** Exact duplicates are detected by content
(`STRUCTURAL.DUPLICATE_EPISODE`, from per-frame content hashes). *Near*-duplicates — a re-upload with
a trivial perturbation — need a similarity measure over frame payloads, which again means decoding
them. A dataset can therefore hold two nearly-identical episodes and pass.

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

`veridex check --metadata-only <dataset>` (LeRobot only, today) answers a narrower question — *does
the manifest hold together?* — without opening a single Parquet or video file. It is the fast CI
gate for a dataset too large to read on every commit, and the shape a remote Hub check will take.

What still applies, because it is all manifest content:

| Family | Still checked |
|---|---|
| Structural | declared vs. actual episode count, duplicate episode indices, cross-episode dtype/shape consistency, stream-presence consistency, episode-index gaps |
| Statistical | every stored-statistics check over `meta/stats.json` — inverted ranges, non-finite values, a mean outside its own min/max, an impossible standard deviation |
| Provenance | the whole family: license, sensor, source format, and every completeness/consistency rule |

What abstains, and why it must:

| Check | Why it cannot run |
|---|---|
| `structural.declared-frame-count` | The declared total is a claim about frames, and no frame was read. |
| `structural.episode-boundary` (declared-length arm) | Same: "declares 120, ingested 0" is true of every sound dataset checked this way. Its duplicate-index and inverted-bounds arms still run. |
| `structural.degenerate-episode` (per-stream arms) | Every stream carries no frames *by request*, so `EMPTY_STREAM` would fire on all of them. `EMPTY_DATASET` and `EMPTY_EPISODE` still run. |
| every temporal check | There are no timestamps to grade. |
| every recomputed statistical check, `structural.stuck-stream`, `structural.duplicate-episode`, every video check | All read values, per-frame content hashes, or media headers. |

And one refusal worth naming: when the episode set comes from `info.json`'s `total_episodes` alone
(no `meta/episodes.jsonl`), the declared-episode-count check is **withheld** rather than run, because
the episode set *is* that number and the comparison could not fail. A check that cannot fail passing
is exactly the silence this catalog exists to prevent. With `meta/episodes.jsonl` present, the total
is an independent second assertion and the check runs.

The verdict carries `coverage: {"kind": "metadata_only", …}` — bound into its hash, printed in every
report — and `certify` refuses to issue a certificate from it. A dataset whose manifest is sound has
been told nothing about its data.
