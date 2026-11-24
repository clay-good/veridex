# The mutation sweep

Veridex's thresholds are its product. A check that fires at the wrong number is worse than no
check, so the house instrument for finding an unpinned threshold is a **comparison-flip sweep**:
every `>` becomes `>=`, every `<` becomes `<=`, and back, one at a time, and the test suite is run
against each mutant. A mutant the suite does not notice is a boundary nothing holds in place.

Run it with the script in the session scratch (`sweep_targeted.py`), one worker per shard, each in
its own worktree copy:

```sh
python3 sweep_targeted.py <worktree> <mutants.json> <results.json>
```

## What a survivor means

A survivor is a question, not a verdict. Three answers are legitimate, and telling them apart is the
whole work:

| Class | What it is | What to do |
| --- | --- | --- |
| **A real rule** | The comparison states something a reader or a flag promises, and moving it by one changes an answer someone depends on | Pin it with a test that fails under the mutant, and red-prove that |
| **A cost ceiling** | `MAX_*` and the budgets: a bound far from any honest input, chosen to stop a crafted one | Leave the exact value unpinned. What is worth testing is that **tripping it is reported**, which the budget tests already do |
| **Provably equivalent** | Another invariant makes the flipped case unreachable, or a later clip makes it harmless | Record the argument at the site; do not write a test that cannot fail |

## The sweep of 2026-11-24 — every adapter and decoder

338 comparisons across `crates/veridex-core/src/adapter/`, judged by each file's own format tests
plus the crate's unit tests: **110 killed, 220 survived, 7 did not compile, 1 timed out** (a flipped
loop bound that spins; the harness bounds each mutant at 600 s and records the timeout rather than
counting it a survivor).

The survivors by file — `cdr.rs` 27, `candbc/blf.rs` 23, `mdf4.rs` 22, `hdf5/format.rs` 19,
`zarr/mod.rs` 19, `hdf5/mod.rs` 18, `kitti.rs` 13, `mcap.rs` 13, `zarr/store.rs` 12, `sqlite.rs` 10,
`lerobot.rs` and `rlds.rs` 9 each, the rest fewer — are a map of where the sweep's reach ends, not a
list of defects. Sorted into the three classes above: **67 cost ceilings**, **52 buffer bounds**
(an offset or length comparison whose flipped case needs an input crafted to land exactly on a
buffer's end), and **101 candidate rules**, of which seven were real.

### The seven that were real

| Site | The rule | Pinned by |
| --- | --- | --- |
| `sqlite.rs` `payload_len <= max_local` | A row whose payload is exactly `U - 35` bytes is kept whole on its page | `rosbag2_adapter::a_row_exactly_on_the_local_payload_limit_is_read_from_its_page` |
| `adapter/mod.rs` `size > limit` | `--max-source-bytes N` admits a file of exactly `N` | `source_size::a_file_exactly_at_the_ceiling_is_read_and_one_byte_over_is_refused` |
| `adapter/mod.rs` `*fraction > 1.0` | `--sample-fraction 1.0` is the whole dataset, not one past the range | `sampling::a_fraction_of_exactly_one_is_the_whole_dataset_and_still_says_sample` |
| `kitti.rs` `*w >= 0.0` | `S_0N: 0.0 0.0` states no image size; `Some(0)` would put every principal point outside its own image | `kitti_adapter::a_calibration_that_states_a_zero_image_size_states_no_size` |
| `cdr.rs` `width > 0` | An unconfigured `CameraInfo` of all zeros declares no size, for the same reason | `cdr::tests::an_unconfigured_camera_info_declares_no_image_size` |
| `cdr.rs` `nanosec >= 1_000_000_000` (the **second** one, in `decode_header_seq`) | A ROS 1 sequence is only believed behind a normalized stamp | `cdr::tests::a_ros1_sequence_behind_an_unnormalized_stamp_is_not_believed` |
| `lerobot.rs` `fps <= 0.0` | A declared rate of zero is carried as stated and never used as a divisor | `lerobot_adapter::a_declared_fps_of_zero_cannot_stand_in_for_a_missing_timestamp_column` |

### Worked arguments for the equivalents

Three representative ones, because the reasoning is the reusable part:

- `rlds.rs` `if shift >= 64` — the varint loop advances `shift` by 7, so it takes the values
  0, 7, … 56, 63, 70 and **never 64**. Flipping to `>` moves the refusal from 64 to 70 across a gap
  no input can land in.
- `sqlite.rs` `reserved >= page_size` — the clause beside it, `page_size - reserved < MIN_PAGE_SIZE`,
  already refuses `reserved == page_size`. Either spelling also keeps the subtraction from
  underflowing, which is the other reason the first clause is there.
- `zarr/store.rs` `grid[d] * chunks[d] < shape[d]` — flipped, the chunk-grid walk visits one
  position past the array's extent when a dimension is an exact multiple of its chunk size. The
  copy is harmless: `copy_chunk_into_row` clips each extent to `dims[d] - origin[d]`, which is zero
  there, and returns without writing. The `<` is still right — it is the difference between
  loading a chunk that is part of the array and one that is not.

## Two things the harness gets wrong if you let it

**A line of text is not a site.** Survivors are reported by source line, and a repeated line is two
different sites: `cdr.rs` has two copies of `nanosec >= 1_000_000_000`, one covered since the
beginning and one that nothing touched. Triage from the mutant's byte offset, not from its line.

**A targeted run under-kills.** Each mutant is judged by the test targets that cover its file, which
is what makes a 338-mutant sweep finish in hours instead of days — but a target list that omits a
test is indistinguishable from a boundary nothing holds. `--test cli` does not compile the binary's
own unit tests, so an earlier CLI sweep reported every one of them as a survivor; the fix was to run
`-p veridex-cli` whole. Before believing a survivor, re-apply its mutant by hand and run the suite.
