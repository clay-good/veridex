# Veridex check catalog

Every finding Veridex can emit, by check. Run `veridex checks` for the live list — each check lists
the finding codes it can emit (add `--json` for a machine-readable catalog) — and
`veridex check <dataset>` to run them. Each finding carries a training
**risk** and a **remedy**; this page is the quick reference for the codes you'll see.

Checks are selected, disabled, or re-severitied via [`veridex.toml`](../openspec/specs/configuration/spec.md).
Severities below are the defaults.

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

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `temporal.monotonicity` | `TEMPORAL.NON_MONOTONIC` | error | Timestamps within a stream do not strictly increase (out-of-order or duplicated frames). |
| `temporal.rate-validity` | `TEMPORAL.INVALID_RATE` | error | A stream declares a sampling rate that isn't a positive, finite number (`0`, negative, `NaN`, `inf`) — corrupt metadata the rate and gap checks would otherwise skip silently. |
| `temporal.rate-conformance` | `TEMPORAL.RATE` | warning | The observed mean rate deviates from the declared rate beyond tolerance. |
| `temporal.gap` | `TEMPORAL.GAP` | warning | An inter-frame interval is far larger than expected (dropped frames). |
| `temporal.jitter` | `TEMPORAL.JITTER` | warning | A stream's inter-frame intervals are badly irregular (coefficient of variation above tolerance) even though the mean rate can look correct — a jittery timeline that `RATE` and `GAP` both miss. |
| `temporal.clock-skew` | `TEMPORAL.CLOCK_SKEW` | error | Two streams in an episode span materially different durations — the headline cross-stream drift check. |
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
| `statistical.range-sanity` | `STATISTICAL.STD_IMPLAUSIBLE` | error | The stored `std` exceeds `(max − min) / 2`, the largest value possible for that range (Popoviciu's inequality) — the stats don't match the data. |
| `statistical.range-sanity` | `STATISTICAL.DTYPE_RANGE` | error | The stored min/max falls outside what the stream's declared integer dtype can represent (e.g. a `uint8` with max `300`) — the dtype or the stats are wrong. |
| `statistical.range-sanity` | `STATISTICAL.DEGENERATE` | warning | The stream is constant (`min == max`, `std == 0`). |
| `statistical.stored-vs-observed` | `STATISTICAL.STATS_STALE` | error | The stored stats and the values Veridex recomputed from the data disagree: a real value falls outside the stored `[min, max]`. The stored `meta/stats.json` is stale or was computed on different data, so normalization built from it clips/distorts the true inputs. Only min/max are compared (convention-free). For a multi-DoF feature it compares **per dimension** — LeRobot's `stats.json` stores per-element arrays and normalization is per dimension, so a stale stat in one joint is caught and named, where an element-0-only check would falsely report a match. Reachable where the adapter reads feature values (LeRobot). |
| `statistical.saturation` | `STATISTICAL.SATURATED` | warning | A large fraction (default ≥50%) of a stream's recomputed values sit **exactly** at one extreme — a clamped/saturated actuator or a state pinned against a rail. The controller can't tell "at the limit" from "wants to go further," so the policy imitates an observation that no longer tracks intent. Exact-equality is the signal (a noisy sensor never lands on the same float repeatedly), so it's false-positive-free; fully constant streams are `STATISTICAL.DEGENERATE`'s concern. Saturation is judged **per dimension** — a gripper pinned at element 6 of a 7-DoF `action` vector is caught, not just element 0, and the finding names the saturating dimension. Abstains below 20 samples, and where the adapter doesn't read feature values (MCAP). |
| `statistical.non-finite-observed` | `STATISTICAL.NON_FINITE_OBSERVED` | error | The recorded **data** holds a NaN or ±infinity value. Distinct from `STATISTICAL.NON_FINITE`, which inspects the source's stored `stats.json`: a clean or absent summary can still hide non-finite values in the actual feature cells, and only a recompute over real values sees them. Veridex holds these out of the recomputed summary (a NaN would poison every stat) and counts them separately, scanning **every dimension** of a multi-DoF cell (a NaN buried in one joint of a 7-DoF arm is still caught). A single NaN propagates to a NaN loss and silently kills a training run. Reachable where the adapter reads feature values (LeRobot); MCAP doesn't decode payloads and abstains. |
| `statistical.extreme-outlier` | `STATISTICAL.OUTLIER` | warning | A stream's extreme (min or max) sits many standard deviations from the mean (default ≥10σ). By Chebyshev's inequality at most `1/z²` of samples can be that far out (≤1% at 10σ), so the flagged value is provably a rare spike — a sensor glitch or unit error — not a wide-but-normal distribution. A lone extreme dominates min/max normalization and destabilizes training. Reads only summary stats (recomputed when available, else stored); for a multi-DoF feature it scans **every dimension** and names the outlying one, so a spike in a non-first joint is caught. Corrupt/degenerate stats are `statistical.range-sanity`'s concern and are skipped. |

## Semantic — are labels and keys usable?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `semantic.task-quality` | `SEMANTIC.EMPTY_TASK` | warning | An episode has a present-but-empty task string. |
| `semantic.task-quality` | `SEMANTIC.PLACEHOLDER_TASK` | info | An episode's task is a low-information placeholder (e.g. "Hold"). |
| `semantic.stream-key-clarity` | `SEMANTIC.DUPLICATE_STREAM_KEY` | error | A stream key appears more than once in one episode — a violation of the "unique within an episode" invariant that makes the name an unusable identifier. |
| `semantic.stream-key-clarity` | `SEMANTIC.AMBIGUOUS_STREAM_KEY` | warning | Two *distinct* stream keys in an episode differ only by case or whitespace. |
| `semantic.annotation-integrity` | `SEMANTIC.ANNOTATION_UNALIGNED` | error | A timestamped `language` annotation falls outside its episode's time span — it would attach to a moment the episode never recorded, so per-frame language conditioning built from it aligns to the wrong frame. |
| `semantic.annotation-integrity` | `SEMANTIC.ANNOTATION_CONFLICT` | warning | Two `language` annotations at the same timestamp carry different values — contradictory supervision for one instant. |
| `semantic.annotation-integrity` | `SEMANTIC.EMPTY_ANNOTATION` | warning | A `language` annotation is present but its value is empty/whitespace. Veridex verifies annotations, never writes or edits them. LeRobot surfaces mid-episode `task_index` changes as timestamped `language` labels; single-task episodes carry none. |

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
rig sensors (LiDAR/radar, IMU, GNSS, CAN, or ego-pose). A manipulation dataset has none, so they
never fire on it.

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `autonomy.rig-sync` | `AUTONOMY.RIG_SYNC` | error | The rig's sensors span materially different durations over an episode — the widest sensor span minus the tightest exceeds the tolerance (default 50 ms, shares `clock_skew_ms`). The N-sensor generalization of `TEMPORAL.CLOCK_SKEW`: one finding names the tightest- and widest-spanning sensors and the drift, and on a rig it *replaces* the pairwise clock-skew report to avoid O(n²) findings for one drifting sensor. |
