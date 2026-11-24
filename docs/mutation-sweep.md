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
buffer's end), and **101 candidate rules**, of which eight were real.

### The eight that were real

| Site | The rule | Pinned by |
| --- | --- | --- |
| `sqlite.rs` `payload_len <= max_local` | A row whose payload is exactly `U - 35` bytes is kept whole on its page | `rosbag2_adapter::a_row_exactly_on_the_local_payload_limit_is_read_from_its_page` |
| `adapter/mod.rs` `size > limit` | `--max-source-bytes N` admits a file of exactly `N` | `source_size::a_file_exactly_at_the_ceiling_is_read_and_one_byte_over_is_refused` |
| `adapter/mod.rs` `*fraction > 1.0` | `--sample-fraction 1.0` is the whole dataset, not one past the range | `sampling::a_fraction_of_exactly_one_is_the_whole_dataset_and_still_says_sample` |
| `kitti.rs` `*w >= 0.0` | `S_0N: 0.0 0.0` states no image size; `Some(0)` would put every principal point outside its own image | `kitti_adapter::a_calibration_that_states_a_zero_image_size_states_no_size` |
| `cdr.rs` `width > 0` | An unconfigured `CameraInfo` of all zeros declares no size, for the same reason | `cdr::tests::an_unconfigured_camera_info_declares_no_image_size` |
| `cdr.rs` `nanosec >= 1_000_000_000` (the **second** one, in `decode_header_seq`) | A ROS 1 sequence is only believed behind a normalized stamp | `cdr::tests::a_ros1_sequence_behind_an_unnormalized_stamp_is_not_believed` |
| `lerobot.rs` `fps <= 0.0` | A declared rate of zero is carried as stated and never used as a divisor | `lerobot_adapter::a_declared_fps_of_zero_cannot_stand_in_for_a_missing_timestamp_column` |
| `mdf4.rs` `bit_offset < 8` | `cn_bit_offset` is the offset *within* a byte, so 0-7; an 8 read literally shifts the field a byte past where the signal sits | `mdf4::tests::a_bit_offset_past_the_byte_is_not_a_channel_this_reader_decodes` |

### Worked arguments for the equivalents

Three representative ones, because the reasoning is the reusable part:

- `rlds.rs` `if shift >= 64` — the varint loop advances `shift` by 7, so it takes the values
  0, 7, … 56, 63, 70 and **never 64**. Flipping to `>` moves the refusal from 64 to 70 across a gap
  no input can land in.
- `sqlite.rs` `reserved >= page_size` — the clause beside it, `page_size - reserved < MIN_PAGE_SIZE`,
  already refuses `reserved == page_size`. Either spelling also keeps the subtraction from
  underflowing, which is the other reason the first clause is there.
- `mdf4.rs` `bits == 0 || bits >= 64` in `sign_extend` — flipped, a 64-bit value takes the shift
  path, where `shift = 64 - bits` is zero and `((value << 0) as i64) >> 0` is the same `value as
  i64` the guard clause returns. The clause is defensive at that end, not load-bearing.
- `zarr/store.rs` `grid[d] * chunks[d] < shape[d]` — flipped, the chunk-grid walk visits one
  position past the array's extent when a dimension is an exact multiple of its chunk size. The
  copy is harmless: `copy_chunk_into_row` clips each extent to `dims[d] - origin[d]`, which is zero
  there, and returns without writing. The `<` is still right — it is the difference between
  loading a chunk that is part of the array and one that is not.

## The same sweep over new code

The rule this project follows for its own changes: every comparison added by a change is swept
before it ships. The November 2026 run over the adapters' and checks' new code (KITTI's image and
OXTS reading, the two new calibration rules, the registry's bounds, the label's cell renderer) found
four comparisons — the others are `==` and `is_empty`, which this sweep does not mutate — and two of
them were rules worth pinning:

| Site | The rule | Pinned by |
| --- | --- | --- |
| `kitti.rs` `*w > 0.0` | `S_0N: 0.0 0.0` states no size | `kitti_adapter::a_calibration_that_states_a_zero_image_size_states_no_size` |
| `label.rs` `out.chars().count() < flattened.chars().count()` | An ellipsis claims text was cut, so a value of exactly 120 characters must not carry one | `certificate::a_label_marks_a_cut_value_and_only_a_cut_one` |

The other two are cost ceilings (`registry.rs`'s 1 MiB record ceiling and the image-header prefix),
where the exact boundary is not a promise and what is tested is that exceeding it is refused.

## Two things the harness gets wrong if you let it

**Three answers, and the third is the common one.** Of the survivors looked at by hand here, the
provably-equivalent class was the largest, and in every case the argument came from somewhere other
than the line itself: an invariant on the loop variable, a clause beside it, a clip downstream. That
is why the triage cannot be done from the survivor list alone.

**A line of text is not a site.** Survivors are reported by source line, and a repeated line is two
different sites: `cdr.rs` has two copies of `nanosec >= 1_000_000_000`, one covered since the
beginning and one that nothing touched. Triage from the mutant's byte offset, not from its line.

**A targeted run under-kills.** Each mutant is judged by the test targets that cover its file, which
is what makes a 338-mutant sweep finish in hours instead of days — but a target list that omits a
test is indistinguishable from a boundary nothing holds. `--test cli` does not compile the binary's
own unit tests, so an earlier CLI sweep reported every one of them as a survivor; the fix was to run
`-p veridex-cli` whole. Before believing a survivor, re-apply its mutant by hand and run the suite.
