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
| `structural.episode-boundary` | `STRUCTURAL.EPISODE_BOUNDARY` | error | Duplicate episode indices, or an episode whose `start_ts > end_ts` (corrupt cumulative boundaries, the lerobot#4143 class). |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_DATASET` | error | The dataset has no episodes at all. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_EPISODE` | error | An episode has no streams. |
| `structural.degenerate-episode` | `STRUCTURAL.EMPTY_STREAM` | error | A stream has no frames. |
| `structural.degenerate-episode` | `STRUCTURAL.SINGLE_FRAME_STREAM` | warning | A stream has a single frame (no temporal signal). |
| `structural.episode-continuity` | `STRUCTURAL.EPISODE_INDEX_GAP` | warning | Episode indices are non-contiguous (e.g. `0, 1, 3`) — an episode was dropped between export and ingest. Needs no manifest, unlike the declared-count check. |
| `structural.declared-episode-count` | `STRUCTURAL.EPISODE_COUNT_MISMATCH` | error | The manifest's declared episode count (e.g. LeRobot `total_episodes`) differs from the episodes ingested (a truncated export). |
| `structural.declared-frame-count` | `STRUCTURAL.FRAME_COUNT_MISMATCH` | error | The manifest's declared frame count (e.g. LeRobot `total_frames`) differs from the frames ingested (episodes present but cut short). |
| `structural.shape-consistency` | `STRUCTURAL.SHAPE_MISMATCH` | error | A stream keeps a different declared dtype/shape across episodes (un-batchable). |

## Temporal — is the time base sound?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `temporal.monotonicity` | `TEMPORAL.NON_MONOTONIC` | error | Timestamps within a stream do not strictly increase (out-of-order or duplicated frames). |
| `temporal.rate-conformance` | `TEMPORAL.RATE` | warning | The observed mean rate deviates from the declared rate beyond tolerance. |
| `temporal.gap` | `TEMPORAL.GAP` | warning | An inter-frame interval is far larger than expected (dropped frames). |
| `temporal.clock-skew` | `TEMPORAL.CLOCK_SKEW` | error | Two streams in an episode span materially different durations — the headline cross-stream drift check. |
| `temporal.start-offset` | `TEMPORAL.START_OFFSET` | warning | Two streams sharing a `clock_id` start at materially different absolute times (a sensor that came online late) — a misalignment `CLOCK_SKEW`'s duration comparison can miss. |

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

## Semantic — are labels and keys usable?

| Check id | Finding code | Severity | Fires when |
|---|---|---|---|
| `semantic.task-quality` | `SEMANTIC.EMPTY_TASK` | warning | An episode has a present-but-empty task string. |
| `semantic.task-quality` | `SEMANTIC.PLACEHOLDER_TASK` | info | An episode's task is a low-information placeholder (e.g. "Hold"). |
| `semantic.stream-key-clarity` | `SEMANTIC.AMBIGUOUS_STREAM_KEY` | warning | Two stream keys in an episode differ only by case or whitespace. |

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
