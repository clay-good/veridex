# Changelog

All notable changes to Veridex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/); versions use [SemVer](https://semver.org/).

## [Unreleased] — v0.1 MVP (in progress)

The first shippable slice of the [`bootstrap-veridex-mvp`](openspec/changes/bootstrap-veridex-mvp/)
change. Runs end-to-end: ingest → validate → score → report → sign.

### Fixed

- **Eleven documentation errors that would have shipped to docs.rs.** Four public items linked to
  private ones (`StuckStream::STUCK_RUN`, `Jitter::MIN_INTERVALS`, `EpisodeDuration::MIN_EPISODES`
  and the MCAP adapter's pointer at the CDR decoder) — each a threshold the prose names, so the
  constants are public now and the one internal module is no longer linked. Six links carried a
  redundant explicit target, and one (`score`) was ambiguous between a function and a module. On a
  published crate every one of these is a page that renders wrong or not at all, and nothing on
  crates.io says so.

  CI now builds the docs for both published crates with `-D warnings`, and runs
  `cargo publish --dry-run` on `veridex-core` — the manifest defect fixed in the previous commit was
  invisible to every other gate.

- **`diff` read a redacted report as a dataset that changed everywhere.** Redaction substitutes every
  identifier a finding quotes, so a redacted report and its unredacted twin describe the same run in
  different words — and `diff` compared them as findings. On this repo's demo dataset that is four
  findings "introduced" and three "resolved" between two runs of the same bytes, and
  `--fail-on-regression` exits 20 on a dataset nobody touched. The other direction is worse: with the
  redacted report as the *old* one, a genuine regression hides inside the substitution noise.

  `diff` now detects the mismatch (the `REPORT.REDACTED` disclosure is in exactly one of the two),
  leads the report with it the way it leads with a coverage change, carries it in the JSON diff, and
  treats it as a regression. Two reports redacted the same way compare normally — the placeholders
  are stable for a given dataset, which is what makes that possible.

*The near-duplicate check, audited against the case it exists for, an hour after it shipped.*

- **A recording uploaded forty times went unreported, and then unmentioned.** The check skips a
  frame hash held by many episodes as boilerplate — right for a home position, and set at 32 it
  defeated the check's own headline case: a recording ingested forty times shares every frame with
  thirty-nine others, so *every* hash was over the ceiling and the whole group vanished. The ceiling
  is 512 now, far above any plausible duplication group; what bounds a pathological dataset is the
  200,000-pair ceiling, which abstains **loudly**.

  Worse than the miss was the silence. Past the ceiling an episode is not compared at all, and
  nothing said so — a skipped episode and a clean one produced identical output. Episodes whose every
  frame was ruled boilerplate are now counted and reported through
  `STRUCTURAL.NEAR_DUPLICATE_UNCHECKED`, which already existed for the pair ceiling and now names
  which of the two limits it hit.

*A self-audit of the six features above, done immediately after writing them. Each defect was
reproduced before it was fixed.*

- **`--redact` leaked exactly the strings a dataset is most identifying by.** The substitution was
  built from stream names, task text, labels, provenance values and the dataset id — and two
  findings the catalog really emits quote something else entirely. The video family names the media
  file it could not read or pair (`videos/acme_warehouse_aisle_7/episode_000000.mp4`), and
  `COVERAGE.SOURCE_UNREAD`'s whole content is a list of source *paths*, which are not in the CDM at
  all: an unread source is precisely the file that did not become data. A coordinate-frame name
  (`acme_wrist_cam_link`) and dataset metadata values (the robot model, the site) went through
  untouched too. All four classes are now enumerated, and a second, pattern-based pass replaces any
  surviving token carrying a path separator — because the enumerated set can only cover what the CDM
  holds, and a check can quote a directory it merely looked in.

- **`--print-config` accepted a dataset path and five run flags, and ignored all of them.** It reads
  no dataset, so `check --print-config --sample-episodes 3 --metadata-only my-dataset/` printed the
  configuration and silently discarded everything else asked for — the accepted-and-ignored failure
  this CLI's whole allow-list exists to prevent, introduced in the same release that added the flag.

- **The near-duplicate check looked each episode's signature up by scanning the episode list.** The
  suppression list is built per candidate episode and each lookup walked every episode, so the cost
  was quadratic in the episode count on top of a per-episode signature that is itself linear in
  frames. Built in one pass now.

- **CI had been red on `main` since 2026-08-16 — every run since, across 72 commits — and the README
  badge said so.** Two independent causes, neither visible to a local run:

  - *Lint drift.* CI lints on the current stable toolchain, which gained `clippy::manual_checked_ops`
    (2026-08-16, and the lint job has failed from that run onward) and later
    `clippy::question_mark` coverage for two shapes written before those lints existed: a
    hand-written zero guard around a division (`adapter/mdf4.rs`) and a `match` on an `Option` that
    returns `None` (`adapter/candbc.rs`). Both are now written the way the newer lint asks, which
    reads better anyway — a `checked_div` and a `?`. Nothing about the behavior changes; the adapter
    tests that cover both paths still pass unchanged.
  - *Three parity tests left behind by the fixes they were meant to guard* (from 2026-08-22).
    `certify` was changed to exit with the verdict it signed rather than 0 over a failing dataset,
    and two tests still ran it under `check=True`, so the deliberate exit 20 read as a crash.
    `veridex inspect --json` was changed to carry the run's coverage and unread sources beside the
    CDM, which moved the CDM under a `dataset` key, and the metadata-only test still indexed the old
    top level. The tests now assert the behavior each fix introduced — the exit code `certify`
    reports, and the coverage `inspect` now carries — rather than being loosened to accept both
    shapes.

### Added

- **`veridex label` — the certificate in the form a person meets it.** A certificate is a document a
  machine verifies. The trust-certificate spec also asks for a *nutrition label*: the same facts,
  compact, for a dataset card — which is how a certificate actually travels. Nothing rendered one.

  `veridex label --certificate c.json --key issuer.pub` prints Markdown to paste into a Hugging Face
  dataset card, a README, or a PR: grade and score with both sub-scores, findings by severity and by
  family, provenance known/attested/unknown, the bound content hash, what version and rubric produced
  it, who issued it and when — plus the readiness verdict, the checks that failed to run, and the
  families that did not run, when there are any. It renders from the signed certificate alone, so it
  cannot describe a verdict other than the one that was signed.

  Three refusals make it something you can trust when you find it in someone else's README. A
  certificate that does not verify gets no label at all — a paste-ready grade with no provenance
  behind it is exactly the artifact a forger wants. A trust decision about the issuer is required
  rather than defaulted, as with `verify`. And when the answer is `--allow-any-issuer`, the label
  *itself* says the issuer is unverified, because whoever reads it will never see the terminal it was
  produced in. Every label ends with the sentence the spec insists on: a certificate is a statement
  of fact about a dataset, not an endorsement of it.

  Python gets `veridex.label(...)`, with a parity test asserting both front-ends render one text.

- **The named policy profiles the configuration spec asks for — two of the three, and a reasoned
  refusal for the third.** `--profile` existed for exactly one bundle, `world-model-ready`, which is
  a *readiness* profile: it names criteria and produces the per-criterion verdict a certificate
  signs. The spec also asks for `strict`, `standard` and `lenient`, which are a different thing —
  bundles of thresholds, with no readiness claim at all.

  `strict` measures the same catalog harder: 20 ms of cross-stream drift instead of 50, 5% rate
  deviation instead of 10%, a 2x gap instead of 3x, jitter at 0.3 instead of 0.5, outliers at 6σ
  instead of 10σ, and 1% of a rig sensor's frames droppable instead of 5%. Every one is *tighter*
  than the default, which is what makes it gateable: measuring harder than the catalog asks can only
  lower a score, so it emits no `SCOPE.NARROWED` and `--min-score` still applies. `standard` is the
  defaults under a name, so a pipeline records which policy it ran under and a later change to
  `strict` is visible rather than undocumented.

  Profiles now declare which kind they are, because the two were being conflated: a threshold profile
  has no criteria, and rendering the readiness block for one printed — and would have *signed* —
  `NOT READY` about criteria it never had.

  There is no `lenient`, and asking for one says why instead of "unknown profile": a profile may only
  tighten, because a loosened threshold does not deselect a check — the check runs, measures the
  defect, and passes it, which is exactly what `SCOPE.NARROWED` exists to surface per threshold and
  by how much. A run carrying that disclosure cannot be gated with `--min-score` or certified as a
  clean whole-catalog result; bundling loosened thresholds under a reassuring name would launder it.
  `relaxed` and `permissive` get the same answer.

- **A corruption sweep over every binary fixture, in both ingest paths.** Veridex reads files it did
  not write, and the failure that matters is not a wrong verdict — a corrupt file has no right
  verdict — but a **panic**, which is not a finding, not an exit code, and not something a CI gate
  can read. Four of them have been found and fixed here already (an MCAP length prefix inside a
  chunk, two HDF5 sizes that overflowed the arithmetic reading them, a vacuous `all()` over an empty
  collection), each one a real file away from a real crash.

  Every committed HDF5 fixture, the MCAP fixture, and every member file of a Zarr *store* is now
  truncated at four points, flipped at six deterministic offsets, size-maxed at four, and — the part
  that turned out to matter — damaged at its header and its trailer, where format detection, the
  magic number, the superblock and MCAP's footer live. Random offsets in a 100 KB file almost never
  land there.

  Each mutation is ingested twice: once with detection, once with the format **forced**. That
  distinction is the difference between a sweep that can fail and one that cannot. With a destroyed
  header, detection declines the file and no parser ever runs — correct behavior, and it means the
  parsing code is never reached. Forcing the format is what a user does when detection is ambiguous,
  and it is where all four historical crashes lived: a panic planted in the HDF5 superblock scan is
  invisible to the detected path and caught immediately by the forced one.

  It found nothing today. That is the expected outcome for a regression guard, and the reason to have
  it is that the next one is caught by CI rather than by whoever is holding the file.

- **The crates can actually be published.** None of the three artifacts has shipped yet, and two of
  them could not have: `veridex-cli` and `veridex-py` depended on `veridex-core` by *path only*, and
  `cargo publish` refuses a dependency without a version requirement — so a release attempt would
  have failed at the second step, after the first was already irreversible on crates.io. Both now
  carry `version = "0.1.0"` beside the path.

  The manifests also carried nothing crates.io shows a visitor: no keywords, no categories, no
  README, no homepage — and a `repository` pointing at `github.com/veridex/veridex`, which is not
  this repository. Each crate now has a README of its own (the workspace README lives above the
  package directory and cannot be packaged), and `veridex-core` excludes 2.3 MiB of binary test
  fixtures that only this repo's tests read, taking the package from 346 files to 86.

  The core README's usage example is compiled as a doctest on the crate's own docs, so the first
  code a visitor reads cannot drift from the API. [docs/releasing.md](docs/releasing.md) records the
  publish order, the checklist, and the one confusing part: `cargo publish --dry-run -p veridex-cli`
  *cannot* succeed until `veridex-core` is on crates.io, because Cargo resolves the version
  requirement it just substituted for the path.

- **Rollups: by category, by episode, by stream — and in the machine-readable report at all.** The
  reporting spec asks for findings summarized at dataset, episode and stream scope, by severity *and
  category*. What existed was a worst-episodes ranking in the terminal and HTML reports. Two slices
  were missing everywhere, and every slice was missing from `--json`, whose only consumer is a CI job
  — the one least able to re-derive them.

  `By category` now leads the report, the worst-episodes ranking is joined by a worst-**streams**
  ranking, and `--json` carries all three under `rollups`. The stream rollup is keyed by stream
  *name* across episodes on purpose: the question it answers is "which sensor is the problem", and a
  camera that drifts in forty episodes is one answer, not forty — so it also reports how many
  episodes contributed. Every renderer derives them from one function over the verdict, so the
  terminal, the HTML artifact and the JSON cannot summarize the same run differently — and a
  `--redact`ed verdict rolls up to placeholder stream names without redaction knowing rollups exist.

- **`veridex check --redact` — a report that can leave the building.** The reporting spec asks for a
  shareable mode, and there was none. A report is diagnostics, so it quotes the dataset: stream keys,
  task strings, annotator addresses, licenses. That is exactly what a team cannot hand to a customer,
  a vendor, or a public issue tracker — while the part they want to hand over, the findings and the
  score, carries no such problem.

  Redaction is a rendering-time substitution, not a different run. The dataset identifier, stream
  names, task and label text, and provenance values are replaced with stable placeholders
  (`stream#1`, `text#2`), consistent within one report so a reader can still tell two findings
  concern the same stream, and meaningless outside it. Substitution is longest-identifier-first, so a
  stream named `arm` cannot leave `arm/gripper` disclosed in pieces.

  What it keeps is the harder half to get right. Episode indices, timestamps, frame counts, and every
  measured quantity stay — a report that dropped the 210 ms drift would not be redacted, it would be
  empty. The verdict, the score, the exit code, and the CDM content hash are the run's own, so a
  shared report and the private one describe the same run and the hash still matches the report to
  the dataset.

  The disclosure (`REPORT.REDACTED`, info) rides as a *finding* rather than a printed banner, which
  is what makes it reach JSON, SARIF, HTML, the terminal and `diff` alike — the machine-readable
  report is the one most likely to be handed to someone else. It states the limits rather than
  implying safety: substitution is best-effort over text, an identifier under three characters is
  left alone, and a name that is also an ordinary word may be replaced where it was not an
  identifier. `certify` refuses `--redact` outright — a certificate attests a dataset by name and
  hash, and a redacted one would say less than it attests.

  Python takes `redact=True` on `check`, `check_sarif`, and `check_html`, with a parity test
  asserting the two front-ends emit the same shared document. Six unit tests, three CLI tests and one
  parity test, proven red against redacting silently, redacting the location but not the message, and
  substituting shortest-first.

- **`STRUCTURAL.NEAR_DUPLICATE_EPISODE` — the partial copy the exact check cannot see.** The catalog
  owed the duplicate requirement its other half: an episode re-uploaded with its tail trimmed, a
  merge that pulled one recording in twice, an episode wholly contained in a longer one. Every frame
  of the overlap is byte-identical and the episodes are not, so `STRUCTURAL.DUPLICATE_EPISODE` was
  silent while the redundancy trained twice.

  The evidence is set overlap over per-frame `content_hash`es — no payload is decoded — reported when
  the *weakest* shared stream still clears `near_duplicate_fraction` (default 0.80, over
  `min(|a|, |b|)`, so containment counts as full overlap). A similarity check's whole difficulty is
  not firing on honest data, so three guards decide what counts as evidence: a stream qualifies only
  if every frame is hashed, it runs at least 8 frames, and at least 80% of those frames are distinct
  from one another (an arm at rest or a quantized channel repeats a handful of values across every
  episode in a dataset — overlap there is a fact about the sensor); every stream both episodes carry
  must agree, so one coincidentally-similar channel cannot carry a claim the camera contradicts; and
  a hash held by more than 32 episodes is boilerplate, skipped, which also keeps the pair counting
  linear in frames rather than quadratic in episodes.

  Pairs the exact check reports are suppressed using *its own* signature function, so the
  suppression can never be wider than what that check actually says — the direction that loses a
  defect. Same frames with a different time base, which the exact check does not report, is reported
  here. A group of near-identical episodes is one finding, not one per pair, because the score
  deducts per finding and it is one root cause. Past 200,000 candidate pairs the check abstains with
  `STRUCTURAL.NEAR_DUPLICATE_UNCHECKED` rather than silently, and the unfingerprinted-content
  disclosure now names this check alongside the two it already named.

  Still out of scope, and now said in one place instead of implied: a **re-encoded or perturbed**
  copy shares no bytes at all, so only payload similarity could find it. Proven end-to-end through
  the real LeRobot adapter (`make_demo_lerobot -- <dir> near-duplicate`), with the false-positive
  case — two honest takes of one task — asserted clean.

- **The environment layer the configuration spec's precedence names.** Configuration was defaults →
  file → flags; the spec's order is defaults → file → **environment** → flags, and the middle layer
  did not exist. It is the layer a container or a CI job can set without writing a file, which is
  most of them.

  Every `veridex.toml` key now has exactly one `VERIDEX_` twin — `VERIDEX_FAIL_ON`,
  `VERIDEX_MIN_SCORE`, `VERIDEX_CATEGORIES`, `VERIDEX_ONLY_CHECKS`, `VERIDEX_DISABLED_CHECKS`,
  `VERIDEX_SEVERITY_OVERRIDES`, and one `VERIDEX_TOLERANCE_<KEY>` per tolerance — plus
  `VERIDEX_CONFIG` (which file to read) and `VERIDEX_PROFILE` (which profile to judge against). A
  *partial* mapping was the thing to avoid: a variable that looks like it configures something and
  does not is the same defect as a flag accepted and ignored.

  Values from the environment meet exactly the bar file values meet, through the same validator: an
  out-of-range tolerance, an unknown category or severity, a malformed `id=severity` pair, a
  `min_score` above 100. Two refusals are specific to this layer. A `VERIDEX_TOLERANCE_*` name
  matching no key is refused with the list of keys it could have meant — a mistyped one (say,
  `VERIDEX_TOLERANCE_CLOCK_SKEW`, missing the `_MS`) moves nothing, so the run would silently keep
  the default threshold the operator meant to change. And an *empty* value is refused rather than
  obeyed: in a shell script that is almost always an unset variable expanding to nothing, and
  `VERIDEX_CATEGORIES=""` read as an instruction selects no categories at all — which runs no checks
  and scores a perfect data score. A `VERIDEX_*` name that is not a config key (the test harness's
  own `VERIDEX_BIN`, a user's tooling) is left alone.

  `--print-config` gained the layer: a value the environment set prints as `(environment)`, not as
  the file it was merged onto. The Python bindings deliberately do **not** read the process
  environment — an imported library that reconfigured itself from `VERIDEX_*` would change what
  `veridex.check(...)` means without the caller writing anything.

- **`veridex check --print-config` — the effective configuration, and where every value came from.**
  The configuration spec requires a way to print the merged configuration, and a verdict recorded
  only the *resolved* numbers. Those cannot answer the question people actually ask: why is this
  threshold 20 ms when my `veridex.toml` says 50? Each setting now prints with the layer that set it
  — built-in default, config file, policy profile, or command-line flag — and with what that layer
  overrode — a `clock_skew_ms` of 20 prints as `(profile)`, naming the 50 in the file it tightened,
  and a `min_score` of 90 prints as `(flag)`, naming the 70 in the file it beat.

  It reads no dataset (the configuration does not depend on one), and it validates the config
  exactly as a run would — an unknown check id, an out-of-range tolerance, an unknown key, an
  unknown profile are all errors here too — which makes it the cheapest way to check a
  `veridex.toml` before pointing it at data. Every key it prints is the key `veridex.toml` uses, so
  a printed value can be pasted straight back. `--json` emits the same document as
  `veridex.config/1`, and Python gets `veridex.effective_config(config=..., profile=...,
  min_score=..., fail_on=...)` rendering through the same core helper, with a parity test asserting
  the two resolve a config identically. That binding accepts a config carrying `min_score` /
  `fail_on`, which `veridex.check` refuses: reporting what a config *says* is a different job from
  running under it, and refusing there would make the one call that exists to explain a CI config
  unable to read one.

- **`veridex watch` — validation while the data is still being recorded.** The last command in the
  CLI spec's minimum surface that had no implementation. Each tick fingerprints the dataset on disk
  (names, kinds, sizes, mtimes; nothing is opened, and a symlink out of the dataset is never
  followed, so activity in a file no adapter would read cannot wake it) and re-validates only when
  something moved — re-ingesting a growing multi-gigabyte log every two seconds is not a change
  detector, it is a load generator. The first pass prints the whole report; every pass after it
  prints only the delta — findings introduced, findings resolved, and how the trust score moved —
  through the same `diff` renderer `veridex diff` uses.

  Three things a recording dataset does that a finished one does not, and what each meant for the
  design:

  - *It is unreadable part of the time.* A half-written shard or a manifest mid-rewrite is an
    ordinary moment in a recording, so an ingest error is printed and the watch continues. Exiting
    there would end a watch seconds after a real recording started.
  - *It never ends on its own.* The loop runs until interrupted; `--iterations <n>` bounds it to `n`
    polling ticks, which is what makes it a CI step and what makes it testable. The exit code is the
    last **completed** validation's, under the same `--fail-on` threshold as `check` — and a watch
    that never completed one is exit 2, not a pass, because exiting 0 there would tell a CI job the
    dataset passed when nothing was ever read.
  - *Its output is read as it arrives.* `--json` emits one document per validation, one per line
    (`veridex.watch/1`, carrying the report and the diff against the previous one), because a stream
    has no closing bracket; stdout is flushed each tick, since it is block-buffered into a pipe.

  What it is not, stated because the spec's word is "incrementally": each pass re-runs the whole
  catalog over the whole dataset. What is incremental is the *trigger* (only a change re-validates)
  and the *output* (only the delta is printed) — not the validation. On a dataset large enough for a
  full pass to hurt, raise `--interval`.

  `--min-score` and the sampling flags are refused rather than accepted-and-ignored: a score gate is
  a claim about a whole dataset, and a sample of the episodes recorded *first* is the opposite of
  what a watch is looking at. Six CLI tests and six unit tests, each proven to fail against the
  mutation it guards (ignore the fingerprint, abort on a read error, never diff, pass when nothing
  validated, follow the symlink, drop size/mtime from the digest). The `--help` coverage test now
  reads its command list out of the source's `COMMANDS` table instead of a hand-kept copy, which had
  silently not covered the new command.

- **Metadata-only ingestion** (`veridex check --metadata-only`, `veridex.check(..., metadata_only=True)`),
  the last unbuilt option in the ingestion spec, for LeRobot. The CDM is built from `meta/` alone: the
  declared episode set and per-episode lengths, every feature's dtype/shape/rate, `meta/stats.json`,
  and the dataset card's license. No Parquet or video file is opened — with the `data/` directory
  deleted the result is byte-identical, which is the test.

  The hard part is not reading the manifest, it is not lying about what that means. Every episode
  carries zero frames *by request*, and three structural findings are true of every sound dataset read
  this way: "declares 120 frames but 0 were ingested", "stream has no frames", "manifest declares 2400
  frames but 0 were ingested". Emitted, they would fail every dataset checked this mode. So the engine
  now hands each check a `CheckContext` saying whether frames were read, and those arms abstain — while
  the arms that read the manifest (duplicate episode indices, inverted bounds, empty dataset/episode)
  keep running.

  What stays live is more than it sounds: the whole stored-statistics family (an inverted range, a
  non-finite value, a mean outside its own bounds — caught without reading the data it summarizes), the
  whole provenance family, and cross-episode shape and stream-presence consistency. One check is
  deliberately *withheld*: when the episode set is derived from `info.json`'s `total_episodes` alone,
  comparing that number against a set built from it could not fail, so it is reported as omitted rather
  than passed. With `meta/episodes.jsonl` present the total is an independent second assertion, and the
  check runs — a manifest whose two episode counts disagree is caught here.

  Coverage rides in the verdict as `metadata_only`, bound into its hash, printed in the terminal, JSON,
  and HTML reports, and `certify` refuses to issue from it. An adapter has to claim
  `Adapter::supports_metadata_only()` to be handed the option at all, so the six formats that keep their
  structure inside the container are refused by name rather than silently reading everything; and
  `--metadata-only` combined with a sample is refused, because one verdict cannot carry two different
  partial coverages without losing one.

- **Zarr hardening, from a two-agent audit.** Fifteen confirmed defects, including two that returned
  wrong data with no error at all. What changed:

  - **Blosc codec ids were read off the wrong table.** The header's top flag bits are the
    *compformat* (0 blosclz, 1 lz4/lz4hc, 2 snappy, 3 zlib, 4 zstd), not blosc's public compcode. So a
    `zstd` chunk went to the zlib inflater and a `zlib` chunk was refused *as snappy* — every store
    using either was unreadable, and `zstd` is what the Diffusion Policy tooling reaches for.
  - **The byte shuffle was undone once per chunk instead of once per block.** Equivalent only when
    there is a single block; with more, every value came back scrambled — no error, just wrong
    numbers, fingerprinted and summarized as if they were the data. The reason the tests missed both:
    every blosc chunk in the fixtures was small enough that blosc stored it verbatim, so the entire
    codec body was unexercised. The fixtures now include arrays that genuinely compress, several
    forced to many blocks.
  - **Every episode of a group-per-episode store held every other episode's arrays.** Each episode
    now owns only its own group, and streams are named below it, so the same sensor is the same stream
    name in every episode — which is what makes the cross-episode checks comparable at all. The same
    layout also handed every episode the *first* timeline found anywhere in the store; an episode that
    recorded no time was stamped with another episode's nanoseconds. Timelines are now read per
    episode.
  - **A panic, two hangs, and an OOM, all reachable from a store's own bytes.** `"<f4294967295"` in a
    `.zarray` overflowed the arithmetic derived from it and aborted the process; a chunk path naming a
    FIFO blocked in `open` forever, and one symlinked to `/dev/zero` grew past 6 GB; a `.zarray`
    declaring gigabyte chunks with no chunk files on disk allocated them anyway, because the fill path
    was the one allocation never charged against the budget. Element widths are bounded, only regular
    files are read as chunks, and the fill is charged like everything else.
  - **Symlinks are no longer followed.** A store linking to a directory outside itself had that
    directory's bytes read, hashed, and signed into a certificate as part of the dataset; a link to its
    own parent made the directory walk exponential. The LeRobot and CAN adapters already refused this;
    now Zarr does too, and says so.
  - Smaller, same spirit: a `<U5` element is 20 bytes, not 5; a zero-length dimension after the first
    is refused rather than yielding rows that hash to nothing; an unparseable `.zattrs` is reported
    rather than read as "no attributes"; a `float16` array reports its values as *not examined* instead
    of "read and clean" (the same fix landed in the HDF5 adapter); one unreadable array no longer
    refuses the whole store, but when nothing survives the refusal carries the reason; a store that is
    a single array reads as one episode; and an empty boundary pair stays an empty episode so
    `STRUCTURAL.EMPTY_EPISODE` can name it.

  A 3,000-case byte-mutation sweep over the fixtures produced no panics and no hangs, worst case 13 ms.

- **Zarr adapter** — the replay-buffer layout Diffusion Policy, UMI, and the tooling around them ship
  in, read into the CDM as the fifth first-class format behind `veridex check`, and the last format on
  the roadmap.

  A replay buffer is one flat array per key with every episode concatenated end to end, and the
  boundaries kept beside it in `meta/episode_ends`. Those boundaries *are* the episode structure, so
  Veridex slices every `data/` array at them — and treats them as data to be checked rather than
  trusted: a boundary that runs backwards, or past the end of the arrays it indexes, is refused, and
  rows past the last boundary belong to no episode and are disclosed instead of being attached to the
  last one. An off-by-one in a replay buffer is exactly the corruption this tool exists to catch. A
  store that is not a replay buffer still reads, under the same group rules as HDF5.

  Zarr's chunks are plain files, so there is no index to walk — but there is a codec to get right, and
  a compressed array read through the wrong one does not fail: it yields plausible numbers. This
  reader implements `zlib`, `gzip`, `zstd`, `lz4`, and `blosc` — the container `numcodecs` reaches for
  by default — including its per-block split streams and byte shuffle. Everything else (`blosclz`,
  `snappy`, the bit shuffle, Fortran order, a filter chain, a v3 store) is refused by name with what
  to re-save it as. Values are summarized as they are read, so the statistical checks are live, and
  the timeline is the step index unless a `timestamp` array declares its `units`.

  An array's `fill_value` is honored, which matters more than it sounds: a chunk that was never
  written is not in the store at all, and Zarr's default fill for a float array is `"NaN"`. Reading
  those rows as zeros would turn missing data into plausible data — and would hide exactly the NaNs
  `STATISTICAL.NON_FINITE_OBSERVED` exists to catch.

  Tested against real `zarr` + `numcodecs` stores committed as fixtures, with per-row SHA-256 values
  taken from Python — including one array per codec over identical values, which must all decode to
  the same bytes, and a half-written array whose gaps must hash to what Python reads back. The chunk-to-row assembly is now shared with the HDF5 adapter rather than written
  twice: the same logical array must not hash differently depending on the container it arrived in.

- **The refusal messages are now asserted, and one of them was lying.** An HDF5 attribute this reader
  *failed to read* (a variable-length string whose global-heap object is corrupt) was reported as "an
  array or a compound value, which the CDM cannot hold" — telling the user to change their data when
  the tool was what fell short. Each case now carries its own reason. Alongside it: the messages for a
  bad superblock version and for corrupt `TREE`, `HEAP`, and `SNOD` blocks are pinned by tests that
  reach them by patching a real file, because a message no test ever sees is a message that is wrong.
  A sampled HDF5 run is also now covered for what the rest of the formats already were: its coverage
  moves the verdict hash, and it cannot be certified.

- **HDF5 values are summarized, which makes the statistical checks live.** The adapter already read
  every row to fingerprint it; it now also recomputes, per dimension, what the values *are* —
  min/max/mean/std (Welford, single-pass), how often a dimension sits exactly at an extreme, and how
  many values are non-finite. That turns five previously inert checks into live ones for HDF5,
  including two errors: a NaN in the recorded data (`STATISTICAL.NON_FINITE_OBSERVED`), a clamped
  actuator (`STATISTICAL.SATURATED`), and a lone spike (`STATISTICAL.OUTLIER`), each judged **per
  dimension** and named — a gripper pinned at element 6 of a 7-DoF action, or a NaN buried in joint 2,
  is invisible to an element-0-only read.

  The accumulators now live in one place (`adapter/stats.rs`) rather than inside the LeRobot adapter,
  because two adapters recomputing statistics differently would mean the same logical dataset scores
  differently in two formats — which is the cross-format neutrality claim itself.

  Two deliberate limits, both disclosed in the ingest report. Per-dimension statistics stop at 256
  values per frame: that reasoning is about robot signals, and "the statistics of pixel (211, 47, 2)"
  answers nothing — wider arrays are still scanned for non-finite values, and the report names them.
  And an integer array is reported as read-and-clean rather than unexamined, because it cannot hold a
  NaN in the first place.

- **HDF5 adapter hardening, from a multi-agent audit.** Four agents went at the new adapter with
  distinct lenses (format conformance, hostile input, CDM invariants, test-coverage mutation
  survival). What they found, and what changed:

  - **A soft link made the reader call the file truncated.** In an old-style group, a symbolic
    link's symbol-table entry carries an *undefined* object-header address, and the reader followed
    it — reading at `0xFFFF…` and blaming the file. Soft and external links are now recognized by
    their cache type, skipped, and named in the report.
  - **One flipped byte cost forty seconds and still passed.** A chunked row is *assembled* from
    chunks and fill value, not read, so nothing about the file's own size bounds it: adding `0x5A`
    to the third byte of a dataspace dimension in a 23 KB fixture turned a 144-byte row into a
    141 MB one, and the result was a *successful* dataset whose frames were mostly fill value
    fingerprinted as content. Synthesized rows are now charged against the expansion budget before
    the buffer exists — the same sweep now runs in 0.05 s and the crafted file is refused by name.
  - **Attributes in dense storage came back as none.** An object with many attributes moves them
    into a fractal heap. The reader does not read those, and an object whose attributes all live
    there looked like an object with no license, no task, and no declared count. It now says so.
  - **A partial timeline left an episode's bounds in the wrong unit.** Where a timestamp array
    covers only some of an episode's arrays, the episode kept nanosecond bounds while its other
    streams were on a step index — and `Episode::duration_ns` would have subtracted those into a
    duration for the outlier check to compare against real ones. The bounds are now unset and the
    mismatch disclosed.
  - **Declared counts are withheld where there is no single actual count.** When an episode's arrays
    disagree on their row count, the frame-count checks compare a declared total against the longest
    array and fail a sound file. Both `num_samples` and `total` are now dropped in that case, with a
    note saying why.
  - Arrays sitting beside the episode groups are disclosed rather than passed over; episodes are
    emitted in index order; and an unsupported filter now names itself (`szip`, `lzf`, `blosc`,
    `zstd`, …) instead of printing a bare id.
  - The CLI's `--sample-episodes` help listed only LeRobot and RLDS as samplable formats; HDF5
    supports it too.

  Tests went from 16 to 34, on 13 real `h5py` fixtures (up from 5), including: chunk shapes smaller
  than the dataset on *every* axis with a ragged edge on each — the case that catches a wrong stride
  or odometer carry in the chunk-to-row copy, and the single largest untested path the audit found;
  every unit spelling and the rejection of one that means nothing; the fractal-heap refusal; a
  decompression bomb refused on what it declares; a single-byte corruption sweep asserting the answer
  is always a dataset or a named error; and an end-to-end assertion that `TEMPORAL.UNMEASURED_CLOCK`
  reaches the verdict rather than only the ingest report.

- **HDF5 adapter** — the format `robomimic`, MimicGen, RoboTurk, and most hand-rolled lab
  collectors write, read into the CDM as the fourth first-class format behind `veridex check`. The
  container is parsed directly, with **no libhdf5 dependency**: superblocks v0–v3, object headers v1
  and v2 (`OHDR`) with their continuation chunks, old-style groups (a v1 B-tree over symbol-table
  nodes plus a local heap) and new-style compact link messages, the compact, contiguous, and chunked
  (v1 B-tree indexed) storage layouts, variable-length strings through the global heap, and the
  `deflate`, `shuffle`, and `fletcher32` filters.

  The mapping is the file's own structure, never a guess: a **group of arrays is an episode**
  (`/data/demo_0`, or the root itself for a one-trajectory file), every array below it is a
  **stream** carrying the dtype and shape the file declares, and an array's first dimension is its
  frame count. Two things are read as a modality, both facts rather than substrings: an array named
  exactly `action`/`actions`, and a `uint8` array whose per-frame shape is `[H, W, C]` with 1, 3, or
  4 channels — an image *by its structure*. Attributes become metadata and provenance, with
  `num_samples` and `total` carried as the source's own frame assertions so
  `STRUCTURAL.FRAME_COUNT_MISMATCH` has something to test. Sampling works (HDF5 has an episode
  axis), and each episode records the group it came from, so an index derived from a name is never
  mistaken for one the file stated.

  Two honesty rules this format forces. First, **HDF5 has no notion of time.** Frames carry a step
  index on the clock `hdf5-step-index`, and the temporal checks abstain and report that they did
  (`TEMPORAL.UNMEASURED_CLOCK`) rather than passing on an index. A file that records timestamps gets
  measured time only when it also declares their units in a `units` attribute — whether a bare
  `time` column is seconds or nanoseconds is not stated, and guessing it would fabricate every rate,
  duration, and skew verdict derived from it. Second, a structure the reader does not implement
  (dense fractal-heap links, the HDF5 1.10 chunk indexes, an unknown filter) is **named and
  refused**, never skipped past: a group read as empty would turn a large dataset into a clean
  verdict over nothing. Integrity is checked as the file is read — a chunk that fails its stored
  `fletcher32` checksum, or inflates to the wrong size for its own shape, is a parse error.

  Tested against **real `h5py` output** committed under `crates/veridex-core/tests/fixtures/hdf5/`,
  with per-row SHA-256 values taken from `h5py` itself: a reader tested only against its own writer
  proves the two agree, not that either matches the format.

- **RLDS / TFDS adapter** — the layout Open X-Embodiment and most TFDS-published robot datasets
  ship in, read into the CDM as the third first-class format behind `veridex check`. TFRecord
  framing and `tf.train.Example` are parsed directly (no TensorFlow, no new dependency), and the
  masked CRC-32C over both the length prefix and the payload is **verified on every record** — a
  corrupt or truncated shard is refused by name rather than parsed past. `features.json` drives the
  mapping: each leaf under the `steps` sequence becomes a stream carrying its declared dtype and
  per-step shape, `language_instruction` becomes `episode.task` (with a mid-episode change surfaced
  as a timestamped `language` label), `episode_metadata/file_path` becomes `provenance.upstream`,
  and the split `shardLengths` become the declared episode count the truncation check tests against.
  Step values are fingerprinted into `frame.value_ref.content_hash`, never decoded.

  Two decisions worth stating, because both are places a quieter adapter would have lied. First,
  **an episode's step count is never written down in RLDS** — it is derived, per feature, by
  dividing the serialized list length by the element size `features.json` declares. Veridex does
  that division for every step feature and requires the answers to agree; a record whose features
  disagree (19 camera images against 20 actions), or whose tensor is not a whole multiple of its own
  element size, contradicts the schema it was serialized against and is refused, rather than mapped
  into a short episode that would read as sound. Second, **RLDS records no wall clock.** Frames are
  stamped with their step index on a clock named `rlds-step-index`, no rate is invented, and the
  ingest report states the omission — so the rate, gap, jitter and skew checks abstain instead of
  grading a dataset against a period Veridex made up, and the missing clock surfaces as
  `PROVENANCE.MISSING_CLOCK` rather than as silence. Sampling works (RLDS has an episode axis):
  the draw resolves from `shardLengths` before any shard is read, and an unselected record is
  framed, its length prefix verified, and then seeked past without its payload being read.

  Shards are read a record at a time rather than loaded whole, so peak memory is the largest
  single episode, not the largest shard — real Open X-Embodiment shards run 60 MB to 850 MB and a
  dataset ships hundreds of them, so `--sample-episodes 1` used to mean an 850 MB allocation for a
  few kilobytes of interest. The honest cost of skipping by seek: a sampled run does not verify the
  payload checksum of the records it passed over. `examples/make_demo_rlds` generates `clean`,
  `truncated`, `desynced`, and `corrupt` variants for trying it end-to-end.
- **A clock the source never recorded is no longer graded as if it were.** The CDM now records, per
  stream, whether its timestamps are *measured time* or a positional step index (`clock_kind`, bound
  into the content hash at `CANONICAL_VERSION` 7). Every temporal check that compares timestamps
  reads only measured streams; the two that grade a *declared* rate still apply everywhere.

  This closes a false assurance the RLDS adapter exposed. A step index is flawlessly monotonic,
  perfectly regular, and identical across every stream of an episode, so each temporal check ran,
  compared, and **passed** on it — `TEMPORAL.GAP` computed a 1 ns baseline and cleared it,
  `TEMPORAL.JITTER` reported a coefficient of variation of exactly zero, `CLOCK_SKEW` reported zero
  drift. The result was a verdict with no temporal findings and a signed certificate recording ten
  temporal checks executed with `categories_skipped: []`, which reads as "these sensors are
  synchronized" on a dataset where nothing was ever measured. `EPISODE_DURATION_OUTLIER` went
  further and emitted the arithmetic out loud: *"episode 4 lasts 0.0 ms — 26.3x longer than the
  dataset median of 0.0 ms"*, a step count compared as a duration and printed in milliseconds.

  A new check, `temporal.clock-measurability`, emits `TEMPORAL.UNMEASURED_CLOCK` (info) naming the
  clock and the streams it blinded. It is a *finding* deliberately: findings reach the terminal
  report, the JSON, the SARIF, the HTML, and the certificate's own findings summary, whereas the
  ingest report's coverage note reaches only `veridex inspect`. A passing temporal result on such a
  dataset now says what it is — the absence of a measurement, not evidence of good timing.
- **Canonical Dataset Model (CDM)** — the cross-format neutrality substrate
  (dataset / episode / stream / frame / provenance / label), with deterministic canonicalization
  streamed into SHA-256 and property-tested determinism.
- **Adapters** — LeRobot v3 (Parquet) and MCAP, each populating the CDM with a fidelity report of
  mapped / unmapped / omitted fields. A cross-format gate test proves the same logical dataset
  yields equivalent CDMs in both formats. The LeRobot adapter resolves task strings
  (`task_index` + `meta/tasks.jsonl` → `episode.task`), so the semantic task-quality check runs on
  real datasets; the omission is reported honestly when no `meta/tasks.jsonl` is present. It reads
  the SPDX license from the dataset card's (`README.md`) YAML frontmatter — where LeRobot datasets
  actually record it — so a licensed dataset no longer trips `PROVENANCE.MISSING_LICENSE`. It also
  fingerprints each feature cell's raw value bytes into `frame.value_ref.content_hash` (a SHA-256,
  never a decode of the values), so — like MCAP below — the CDM hash is content-sensitive (a tampered
  export no longer verifies against the original's certificate) and exact-duplicate episode detection
  works end-to-end; cells whose type isn't a hashable numeric feature (e.g. images stored outside the
  Parquet) are left unhashed, honestly. The MCAP
  adapter extracts the file header's writing `library` (as a `recorder` provenance element) and
  `profile`, every producer-written **Metadata** record (preserved in dataset metadata, with
  well-known keys — license/sensor/calibration/operator/upstream — mapped to typed provenance), and
  **Attachment** summaries (a calibration-looking attachment supplies the `calibration` element), so
  provenance reflects who produced the recording and how. Each message's raw bytes are fingerprinted
  into `frame.value_ref.content_hash` (a SHA-256 of the bytes, not a decode), so the CDM content hash
  — and thus certificate binding — is sensitive to actual frame content: a tampered recording with
  identical topics and timestamps no longer hashes the same, and content-level checks (duplicate
  episodes) have something exact to compare.
- **Validation engine** — check registry with duplicate-id rejection, category/id selection,
  severity overrides, deterministic stably-ordered verdicts with a result content hash, fault
  isolation for panicking checks, and reproducibility metadata.
- **Checks catalog** — 39 checks across seven families (the seventh, **autonomy**, is described in its
  own entries below), each finding carrying a training **risk** and a **remedy** and located to the exact
  episode / stream / frame:
  - **Structural** — episode-boundary integrity (the lerobot#4143 class: a per-episode declared
    `length` from `meta/episodes.jsonl` that disagrees with the frames ingested, duplicate episode
    indices, or inverted `start_ts`/`end_ts`), degenerate
    episodes/streams (including a zero-episode dataset), episode-index continuity, declared-vs-actual
    episode/frame counts (truncated exports), cross-episode dtype/shape and stream-presence
    consistency, exact-duplicate episodes (`STRUCTURAL.DUPLICATE_EPISODE`, content-hash-gated so it
    never mis-flags same-length episodes), and a frozen-camera check (`STRUCTURAL.STUCK_STREAM`).
  - **Temporal** — monotonicity, declared-rate validity (`TEMPORAL.INVALID_RATE`), rate conformance,
    gaps, jitter, the headline cross-stream `TEMPORAL.CLOCK_SKEW`, shared-clock start/end offsets,
    cross-episode rate consistency (`TEMPORAL.RATE_INCONSISTENT`), and episode-duration outliers.
  - **Statistical** — stored-stats range and sanity (inverted range, non-finite, negative or
    Popoviciu-implausible std, mean-out-of-range, integer-dtype range, degeneracy). Where the adapter
    reads feature values (LeRobot), Veridex recomputes statistics from the actual cells and adds four
    data-facing checks: `STATISTICAL.STATS_STALE` flags a stored `meta/stats.json` whose range doesn't
    bound the data (stale stats poison normalization); `STATISTICAL.SATURATED` flags a clamped actuator
    whose values sit **exactly** pinned at one rail (exact-equality is the signal, so a noisy sensor is
    never mis-flagged); `STATISTICAL.OUTLIER` flags an extreme many σ from the mean, provably a rare
    spike by Chebyshev's inequality (≤1% of samples at 10σ); and `STATISTICAL.NON_FINITE_OBSERVED`
    flags a NaN or ±infinity in the cells that a clean or absent `stats.json` hides — a single one
    propagates to a NaN loss and silently kills a training run. All four scan **every dimension** of a
    multi-DoF feature and name the offending joint, so a stale stat, saturated gripper, spike, or NaN
    buried in element 6 of a 7-DoF `action` is caught, not just element 0.
  - **Semantic** — task-string quality and stream-key clarity (an exact-duplicate key is an error, a
    case/whitespace collision a warning); and language-annotation integrity
    (`SEMANTIC.ANNOTATION_UNALIGNED` / `SEMANTIC.ANNOTATION_CONFLICT` / `SEMANTIC.EMPTY_ANNOTATION`):
    timestamped language
    annotations are verified — in span, unique per instant, non-empty — never written or modified. The
    LeRobot adapter surfaces mid-episode `task_index` changes as timestamped `language` labels
    (single-task episodes carry none), so the check runs on real multi-task datasets.
  - **Provenance-completeness** — presence, internal consistency, and placeholder detection (a
    `license` of `"unknown"` is present in form but empty in substance, so it isn't counted as real).

  The full catalog — every check, its finding codes, default severity, and exactly when it fires —
  lives in [docs/checks.md](docs/checks.md), guarded against drift in both directions by tests.
- **Trust certificate** — a deterministic v1 score and A–F grade (provenance weighted as a separate
  30% axis), a content-bound certificate document, and Ed25519 signing with offline verification
  that rejects tampering, transplantation, untrusted issuers, and unsupported signature algorithms.
- **Provenance emit** — MLCommons Croissant (JSON-LD) and minimal W3C PROV, preserving
  known / asserted / unknown classes without fabrication. The PROV graph attributes the dataset to
  every known agent (recorder as a `prov:SoftwareAgent`, annotator as a `prov:Person`, sensor as a
  `prov:Agent`) and derives it from a known upstream, with each agent resolvable as a graph node.
- **Reporting** — human-readable terminal output with worst-episodes-first rollups (and a note of
  any non-default tolerance the run applied, so a "no findings" result is read against the right
  thresholds), a versioned
  JSON envelope (`veridex.report/1`), SARIF 2.1.0 (`veridex check --sarif`) for CI code-scanning
  (rules carry a description and a link to the check catalog), a
  self-contained HTML report (`veridex check --html`), and verdict diffing (`veridex diff`) that
  reports findings introduced / resolved / unchanged and the trust-score movement between two
  reports — with `--fail-on-regression` to fail CI when the new report introduces findings or a
  lower score.
- **CLI** — `veridex check | inspect | checks | certify | verify | provenance | keygen | diff`
  (`inspect` summarizes the CDM structure — including each episode's wall-clock span, so a
  duration outlier is visible at a glance — and the provenance coverage — known/asserted/unknown per
  expected element, with placeholders shown as missing; `checks` lists the built-in catalog — id,
  category, default severity, scope, and the finding
  codes each check can emit — as text or
  `--json`; tests guard those codes against the doc catalog in both directions, so a code can't
  ship undocumented and a stale doc row can't outlive its code),
  with format
  autodetection (`--format` override, ambiguity is refused), a configurable failure threshold
  (`--fail-on`), a trust-score gate for CI (`--min-score 0-100`, fails below the threshold), and
  documented exit codes (0 pass · 10 warnings · 20 fail · 2 tool-error). An end-to-end integration
  test drives the real binary over the whole trust flow — `check` (terminal + JSON) then
  `keygen → certify → verify` against a committed dataset fixture, including rejection of an
  untrusted issuer key.
- **Python bindings** (`import veridex`) exposing `check` / `content_hash` / `inspect` / `catalog` / `diff` / `keygen` / `certify` / `verify` /
  `provenance` / `version`, calling the same core pipeline as the CLI, with passing CLI ⇄ Python
  parity tests over `check`, `inspect`, `catalog`, `provenance`, `diff`, and `certify`/`verify` —
  each shares a single core render helper (`render_catalog_json`, `render_provenance`,
  `render_diff_json`) with the CLI, and because Ed25519 signing is deterministic the certificate a
  given key issues is byte-identical across CLI and Python.
- **Configuration** — a `veridex.toml` (auto-discovered, or `--config`) that selects categories,
  disables checks, overrides per-check severities, and sets the failure threshold and minimum trust
  score (`min_score`, overridable by `--min-score`); the effective config is recorded in every
  verdict. Unknown TOML keys are rejected, and a check id that names no real check (a typo in
  `disabled_checks`, `only_checks`, or a `severity_overrides` key) is a hard error rather than a
  silent no-op. A `[tolerances]` table tunes the temporal and statistical checks' numeric thresholds
  (`clock_skew_ms`, `start_offset_ms`, `end_offset_ms`, `rate_deviation`, `gap_factor`, `jitter_cv`,
  `episode_duration_factor`, `saturation_fraction`, `saturation_min_samples`, `outlier_z`,
  `sequence_drop_fraction`, `ego_max_speed_mps` — see
  [docs/veridex.toml.example](docs/veridex.toml.example)); each is optional, validated
  (finite, non-negative; positive `gap_factor`), and falls back to the check's default. The
  tolerances the run used are recorded in the verdict's effective config, so a result is fully
  reproducible from what it reports.
- **Runnable demos** — `examples/make_demo_mcap` (synthetic cross-stream clock skew) and
  `examples/make_demo_lerobot` (a LeRobot v3 dataset with an out-of-order timestamp, a
  `truncated` variant whose manifest over-declares its frame count → `STRUCTURAL.FRAME_COUNT_MISMATCH`,
  a `boundary` variant whose `meta/episodes.jsonl` declares the wrong length for one episode → the
  lerobot#4143 `STRUCTURAL.EPISODE_BOUNDARY`, and a `jitter` variant whose one episode has an
  irregular inter-frame spacing → `TEMPORAL.JITTER`; plus `short-episode`, `duplicate`, `saturated`,
  `spike`, `nan`, and `multi-joint` variants — the full list is in the README), each with a `clean`
  variant, so `veridex check` has something to find end-to-end in **both** formats.
- **CI** — GitHub Actions running fmt, clippy (`-D warnings`), and the full test suite, plus a
  Python job that builds the extension with maturin and runs the CLI ⇄ Python parity test on every
  push. The `veridex` binary has its own integration tests (`crates/veridex-cli/tests/cli.rs`)
  asserting command dispatch, argument validation, and the CI exit-code contract (0 · 10 · 20 · 2).
- **Autonomy sensor-rig CDM extensions (A0)** — the first slice of `autonomy-sensor-data`: the CDM now
  represents a multi-sensor rig as *extensions* of the existing model, not a fork (design A1). New
  modalities (`point-cloud`, `imu`, `gnss`, `can-signal`, `ego-pose`); a declared per-point field
  layout on a stream (`Stream.point_fields`); a rig `Calibration` on the dataset — the coordinate-frame
  transform (TF) tree plus per-camera intrinsics, each with a `valid_from`/`valid_to` validity range so
  a recalibration mid-log is representable; and a per-episode ego-vehicle trajectory (`Episode.ego_poses`).
  All are optional and absent for manipulation datasets, whose CDM and verdicts are unchanged. Every
  content-bearing field is bound into the content hash — the TF tree, intrinsics, and ego trajectory
  canonicalized order-independently, the point-field layout order-significant — with
  `CANONICAL_VERSION` bumped 2 → 3. The spatial/sequence checks that read these are still to come (A2).
- **MCAP autonomy message classification (A1, first slice)** — the MCAP adapter now recognizes the
  common ROS/ROS 2 autonomy message types by schema name and maps them to the new rig modalities
  (`PointCloud2`/`LaserScan` → point-cloud, `Imu` → imu, `NavSatFix` → gnss, `Odometry` → ego-pose,
  CAN frames → can-signal), instead of lumping them into `scalar-state`. So an AV rig log's streams
  are typed correctly at ingest. The message **bodies** are now CDR-decoded too: a hand-rolled,
  bounds-checked ROS 2 CDR reader (`adapter/cdr.rs` — no new dependency, `#![forbid(unsafe_code)]`,
  declines malformed/big-endian bodies without panicking) reads each AV message's structural *header*
  (never the bulk point/pixel payload) to populate the rig CDM: `PointCloud2` → `Stream.point_fields`,
  `CameraInfo` → camera intrinsics, `TFMessage` → the transform tree, `Odometry` → the ego trajectory.
  Proven end-to-end through the adapter and by per-decoder unit tests. A new `make_demo_mcap -- <out> av` variant
  writes a five-sensor rig (camera, LiDAR, IMU, GNSS, ego-odometry) with a single-sensor sync drift
  injected on the IMU; `veridex inspect` shows the typed rig and `veridex check` flags the drift.
- **`AUTONOMY.RIG_SYNC` — rig-wide time sync (A2)** — the first autonomy check and a new `autonomy`
  check family. It generalizes the pairwise `TEMPORAL.CLOCK_SKEW` to N sensors: on an episode that is
  a sensor rig (≥3 AV-native rig sensors), it reports the rig-wide sync spread — the widest sensor
  span minus the tightest — as a **single** error naming the tightest- and widest-spanning sensors,
  instead of O(n²) pairwise findings. On a rig it *supersedes* `CLOCK_SKEW` (which now skips rig
  episodes), so a drifting sensor no longer floods the report; a manipulation dataset has no rig
  sensors, so it never enters rig mode and `CLOCK_SKEW` behaves exactly as before. It shares the
  `clock_skew_ms` tolerance (same semantics, one knob). On the `av` demo this turns four pairwise
  `CLOCK_SKEW` errors into one clear `AUTONOMY.RIG_SYNC` finding.
- **`AUTONOMY.SEQUENCE_COMPLETE` — rig sequence completeness (A2)** — flags a rig sensor that quietly
  drops an aggregate fraction of its frames (default > 5%): its observed frame count against the count
  its own median inter-frame cadence implies over its active span. It catches many small drops that
  `TEMPORAL.GAP` (a single oversized interval) and `TEMPORAL.RATE` (which needs a declared rate MCAP
  rigs lack) both miss. Rig-only, median-baseline (robust to the drops it hunts, no declared rate or
  shared clock needed), and skips streams with too few frames for a stable estimate. Proven end-to-end
  through the MCAP adapter (`a_frame_dropping_sensor_is_flagged_incomplete_end_to_end`).
- **`AUTONOMY.EGO_POSE_CONTINUITY` — ego trajectory continuity (A2)** — flags an episode whose ego
  trajectory (`Episode.ego_poses`, decoded from Odometry) contains a step whose implied speed
  (distance / elapsed time) exceeds the plausible maximum (default 100 m/s ≈ 360 km/h): a GPS glitch,
  localization reset, or stitched log that teleports the ego frame, so every later sensor observation
  registers against a wrong world pose. Reports the worst jump and how many occurred. Runs end-to-end
  on the CDR-decoded ego trajectory (`a_teleporting_ego_trajectory_is_flagged_end_to_end`).
- **`AUTONOMY.CALIBRATION_INCOMPLETE` — rig calibration completeness (A2)** — the principle-respecting
  form of the LiDAR-camera reprojection check. Veridex never decodes the bulk point/pixel payload, so
  it cannot reproject actual points; instead it verifies the calibration needed to *is present and
  coherent*. On a rig with spatial sensors it flags: no transform (TF) tree at all; a TF tree split
  into disconnected components (sensors that can't be related, found by connected-components over the
  frame graph); or cameras with no intrinsics. Runs on the CDR-decoded TF tree + intrinsics, proven
  end-to-end (`a_rig_without_a_transform_tree_is_flagged_incomplete_end_to_end`).
- **`world-model-ready` profile + readiness certificate (A4)** — a named policy profile
  (`crate::profile`, applied with `veridex certify --profile world-model-ready`) that tightens
  cross-sensor sync to 20 ms and bundles the four autonomy criteria a world-model set needs. The
  certificate gains a signed `readiness` block reporting per-criterion pass/fail and the threshold
  each attests, plus `applicable` (is the dataset a sensor rig) and an overall `ready` flag. Honest by
  construction: a non-rig is `N/A`, never a vacuous pass, and the report claims nothing beyond the
  criteria listed. The block is signed like every other certificate field (verifies offline). See
  [docs/profiles.md](docs/profiles.md).
- **Readiness certificates are readable offline, from both surfaces (A5)** — `veridex verify` now
  reports what a certificate *attests*, not merely that its signature checks out: the CDM hash it is
  bound to, the trust score and provenance coverage, and — for a certificate issued with
  `--profile` — the profile verdict and every readiness criterion. `--json` emits the same facts as a
  machine-readable summary, with the signed `readiness` block verbatim. Everything printed comes out
  of the signed document, so a doctored readiness block fails verification instead of being read back
  (covered by a test that flips `ready` to true and asserts the certificate no longer verifies).
  Python reaches parity: `veridex.certify(..., profile="world-model-ready")` issues the identical
  profiled certificate (byte-for-byte with the CLI, checked in the parity suite) and
  `veridex.verify(...)` returns the identical summary. Certify and verify share one core renderer, so
  the two surfaces can't drift.
- **Autonomy provenance lineage (A3)** — the MCAP adapter now extracts the sensor-rig lineage a
  producer records in Metadata: firmware, calibration session, platform/vehicle and drive/run IDs,
  capture region, HD-map version, and — acute for public-road capture — redaction and consent status.
  Each is classified `known` (read from the source bytes) and surfaced in both provenance emits: the
  Croissant `veridex:provenance` list and the PROV entity as `veridex:` properties. Extracted without
  changing the coverage denominator, so a manipulation dataset's coverage score is unchanged. The `av`
  demo carries the lineage end-to-end.
- **Scenario-dimension coverage (A3/A6)** — a **descriptive** report of the conditions a dataset was
  recorded under. `crate::scenario` recognizes scenario tags (weather, time-of-day, environment,
  lighting, season, traffic) from episode labels and reports each dimension's value distribution across
  episodes, marking a **sparse** cell (a value in under 10% of covered episodes). It is descriptive by
  design (A6): never a finding, never a score change, never a required balance — the target
  distribution is the training team's call. The MCAP adapter extracts recognized scenario metadata
  keys into episode labels, and `veridex inspect` shows a "scenario coverage" section.
- **Scenario / map / simulation references (A1)** — Veridex now records *what a log was recorded or
  replayed against*: the OpenSCENARIO scenario, the OpenDRIVE road network / HD map, the OSI version,
  and the simulator or replay tool. `crate::simref` recognizes the well-known metadata spellings and
  the MCAP adapter maps them to `scenario_ref` / `map_ref` / `osi_version` / `simulator` provenance,
  each `known`. Versions are extracted, never guessed: when the reference names a sidecar that really
  sits next to the log, the ASAM revision declared in that file's own header (`revMajor`/`revMinor`,
  the same shape in `.xosc` and `.xodr`) is read from its bytes; otherwise the version is whatever
  dotted version the recorded value itself carries, and a bare file name (`town10.xodr`) yields no
  version rather than a wrong one. A reference pointing outside the dataset (absolute, or with `..`)
  is recorded but never followed. An explicitly recorded `map_version` always wins over an OpenDRIVE
  header revision. References travel with both provenance emits and show in `veridex inspect` as a
  "scenario & map references" section; the `av` demo carries them. Reading the reference is the scope
  — Veridex does not parse scenario semantics, road geometry, or ground truth.

- **ASAM MDF 4.x (MF4) adapter** — the dominant automotive measurement format, read into the CDM
  (`adapter/mdf4.rs`, no new dependency). Walks the block graph (`##HD` → `##DG` → `##CG` → `##CN`),
  takes each channel group's **time master** as the timeline, and emits one stream per measured
  channel with a frame per record, applying identity and linear (`##CC` type 1) conversions to get
  physical values. Integer and float channels decode in both byte orders; values are fingerprinted
  into the CDM content hash, so an altered measurement no longer hashes the same. The writing program
  from the identification block becomes `recorder` provenance, and a non-4.x file is rejected as an
  unsupported version rather than mis-parsed. Everything outside that core — compressed (`##DZ`) or
  listed (`##DL`) data, unsorted data groups, bit-packed or non-numeric channels, other conversion
  types, an over-declared cycle count — is reported as an `unmapped` field and contributes no frames,
  so a reader always knows what the verdict covered. Every block read is bounds-checked and every
  chain walk is loop-guarded: a truncated or byte-corrupted file yields an error or an empty result,
  never a panic (tested against file prefixes and corrupted bytes). Autodetected by the registry from
  the file's own identification block, not its extension. Fixtures are assembled byte by byte, so the
  adapter is tested against the on-disk layout rather than a writer sharing its assumptions.
- **CAN + DBC adapter** — a new AV-native ingestion path (`adapter/candbc.rs`). It ingests a directory
  holding a `.dbc` signal database and one or more candump ASCII logs (`.log`/`.asc`), parses the DBC
  (`BO_` messages, `SG_` signals), and decodes each CAN frame's signals in **both DBC byte orders** —
  little-endian (Intel, `@1`) and big-endian (Motorola, `@0`, walking the sawtooth bit numbering from
  the signal's most-significant bit) — applying the factor/offset and sign-extension, into one
  `CanSignal` stream per `Message.Signal`. A signal whose bits fall outside the frame is declined
  rather than truncated. DBC-coverage gaps (CAN ids seen in the log with no DBC definition) are
  surfaced as `unmapped` fields. Decoded values are fingerprinted into the CDM content hash.
  Autodetected by the registry (a directory with a `.dbc`). Dependency-free text parsing; unit,
  integration, and CLI end-to-end tests — including a Motorola signal laid over a byte-swapped copy
  of its Intel twin, which must decode to identical samples. Recomputed signal stats (to feed the
  statistical checks) remain a follow-up.
- **Sampled ingestion** — `check` / `inspect` can validate a subset of a dataset's episodes:
  `--sample-episodes <n>` takes the first *n* by index, and `--sample-fraction <f> [--sample-seed
  <s>]` draws a deterministic fraction (episodes ordered by `SHA-256(seed, index)`, so the same seed
  always draws the same episodes and a positive fraction never draws none). The same requests are
  available from Python as `sample_episodes=` / `sample_fraction=` / `sample_seed=` on
  `veridex.check()` and `veridex.inspect()`.

  Sampling is resolved from the declared episode set (`meta/episodes.jsonl`, else `info.json`'s
  `total_episodes`) *before* any Parquet is read, so an unselected episode is never fingerprinted,
  never accumulated into the recomputed statistics, and never charged to the frame budget — a
  sample of a dataset that exceeds the budget succeeds where the full ingest is refused. Only
  LeRobot has an episode axis; MCAP, CAN+DBC, and MF4 ingest a recording as a single episode and
  **refuse** a sampling request rather than returning everything labelled as a sample.

  A sampled run cannot be mistaken for a full one. The verdict carries a `coverage` field, digested
  into `result_content_hash`; the terminal, JSON, and HTML reports all state the sample and the
  episode count; `veridex inspect` says so next to the hash it produced; and `certify` **refuses**
  to issue a certificate from a partial run, because a certificate is a claim about a dataset and
  the episodes it never read are exactly where the problem would be. `verify` and `provenance`
  reject the sampling flags outright. Under a sample the adapter also drops the dataset-level
  declared totals from the CDM, so a deliberate subset is never reported as a truncated export —
  while the per-episode declared lengths (the lerobot#4143 check) still apply to the episodes that
  *were* read.

- **`AUTONOMY.SENSOR_FRAME_UNKNOWN` / `AUTONOMY.SENSOR_FRAME_UNRELATED`** (`autonomy.sensor-frame-resolution`)
  — the LiDAR-camera miscalibration class a well-formed calibration hides. `CALIBRATION_INCOMPLETE`
  asks whether a rig has a transform tree; this asks the question that decides whether a fusion
  pipeline works: for *this* sensor, does a chain of transforms exist from the frame it stamps its
  data with to the camera it is fused against? Two ways that fails, neither visible from the tree's
  own shape — the sensor's frame is not in the tree at all (a perfectly connected tree recorded for
  `lidar_top` while the driver publishes `lidar_top_v2`, so every geometric operation silently has no
  transform), or the frame is in the tree but in a subtree nothing joins to the camera's. Veridex
  never decodes point coordinates or pixels, so it does not compute a reprojection *error*; it
  verifies the reprojection is defined at all. Abstains when the sensor declares no frame, and leaves
  "no tree at all" to `CALIBRATION_INCOMPLETE` — which in turn now leaves the disconnected-tree report
  to this check whenever the sensors name their frames, so one defect is charged once, at the finest
  granularity available.

  Fed by a new CDM field, `Stream.frame_id`, which the MCAP adapter decodes from the `header.frame_id`
  of any header-first ROS message (first one wins). It is bound into the content hash
  (**`CANONICAL_VERSION` 4 → 5**), because a check fails a stream on it: a correctly wired rig and a
  stranded one must not hash alike, or the certificate for one would verify against the other.
  Proven end-to-end through the real adapter, and by a new demo variant —
  `make_demo_mcap -- <out> av-miscalibrated` writes the five-sensor rig with the LiDAR parented to a
  `lidar_mount` frame nothing joins to `base_link`.

- **Video and media checks** (`video.media-readable`, `video.media-conformance`) — the last check
  category in the catalog with no implementation. A video dataset is two artifacts nothing
  reconciles: a manifest and a data table on one side, an `.mp4` on the other, paired by frame index
  and never compared. Veridex now reads each video file's ISO base media (MP4) **container headers**
  — never a decoded pixel — and carries both the manifest's declared encoding and the container's
  own into a new CDM field, `Stream.media`. That makes four failures visible:
  `VIDEO.MEDIA_MISSING` (an episode's video was never uploaded), `VIDEO.MEDIA_UNREADABLE` (the file
  is not a parseable container, with the reason naming the structure that was wrong),
  `VIDEO.FRAME_COUNT_MISMATCH` (the container's sample count differs from the episode's rows, so
  every pair past the shorter one teaches an action against an image from a different moment), and
  the export-drift trio `VIDEO.RESOLUTION_MISMATCH` / `CODEC_MISMATCH` / `FPS_MISMATCH`.

  Charged at the granularity of the defect: a frame-count mismatch is per episode, while a
  resolution, codec, or rate that disagrees with the manifest is one export defect and is reported
  once per stream, naming the first episode and how many share it. Codecs compare across the names
  for one encoder (`h264`/`avc1`, `hevc`/`hvc1`, `av1`/`av01`, `vp9`/`vp09`), so the manifest and the
  fourcc spelling the same thing differently is not a finding.

  `Stream.media` binds into the content hash (**`CANONICAL_VERSION` 5 → 6**): a re-encode changes
  nothing else in the CDM, so without it a certificate issued for a sound export would verify
  against a broken one. The container walk is bounded like every other untrusted read — box sizes
  are validated against the bytes that actually remain rather than believed, nesting is capped, and
  only the `moov` box is read into memory, under a ceiling.

  The LeRobot adapter resolves `videos/**/<feature>/episode_<n>.mp4`. A layout that concatenates
  many episodes into one file is reported as an **unmapped field** and the checks abstain —
  attributing a shared file's frames to one episode would invent the very number the checks compare.
  Four demo variants prove the whole path end-to-end: `make_demo_lerobot -- <out> video` (clean),
  `video-desync`, `video-missing`, and `video-reencoded`.

### Fixed

*The entries below through "A narrowed check set is not a clean run" close a **six-agent audit** of
the HDF5 and RLDS adapters, the certificate and canonical encoder, scoring/diff/profiles, the two
output layers, and the Python/CLI front-ends. Three of the six agents independently converged on the
same hole from three directions, which is why it earned a root fix rather than three patches. The
certificate's field-coverage enumeration came back clean: all 22 certificate and verdict fields, and
their nested sub-fields, are covered by the signature — every single-field mutation produced a
signature mismatch, and the `skip_serializing_if` optional fields create no deletion hole. So did
determinism: 200 pseudo-random permutations of every order-insensitive collection produced a
byte-identical CDM hash, result hash, and trust score, and every encoder sort key was confirmed
total. The v1 rubric in `docs/rubric-v1.md` matches `certificate/score.rs` numerically, term for
term. The HDF5 chunk decode path was likewise cleared against `h5py` output — multi-level chunk
B-trees, per-chunk filter masks, big-endian shuffle+fletcher32 at non-8-byte strides, rank-4 chunking
and extendible datasets all round-trip exactly. Those results are worth recording as plainly as the
defects.*

- **A narrowed check set is not a clean run.** A `veridex.toml` carrying one line — `only_checks`,
  `categories`, `disabled_checks`, or a `severity_overrides` entry — silently rewrote the verdict
  everywhere a human or a machine would read it. On `demo.mcap`,
  `only_checks = ["structural.episode-boundary"]` turned `FAIL / 76 / 5 findings` into
  `PASS / 89 / 0 findings`, with 1 of 38 checks run and no trace of that in the terminal report, the
  HTML report built to travel, the SARIF a CI code-scanning job reads, or a signed certificate.
  `diff --fail-on-regression` read the five vanished findings — including a real 210 ms clock
  skew — as *resolved*, saw the trust score climb 13 points, and exited 0.

  `effective_config` had carried the facts all along, in the JSON envelope and in the certificate,
  but only for a reader who thought to look. This is the same failure shape `CoverageNote` exists to
  prevent, one axis over: coverage answers "how much of the dataset did we read", and nothing
  answered "how much of the catalog did we run". So it takes the same remedy — a finding, because
  findings are the only channel that reaches every renderer, the diff, and the certificate's own
  summary. The engine now emits `SCOPE.NARROWED` under `veridex.scope`, which like `veridex.coverage`
  is deliberately not a catalog check, so configuration cannot switch off the disclosure that
  configuration narrowed the run. It is measured from what happened (checks executed vs. registered)
  rather than the config's wording, so a full run emits nothing and ordinary hashes are unchanged.
  `veridex verify` names the same limit beside the score.

- **Unknown fields were rejected only on the outermost certificate structs.** `SignedCertificate`,
  `Certificate`, `Issuance`, `FindingsSummary`, `CriterionResult` and `ReadinessReport` carried
  `deny_unknown_fields`; none of the types nested inside them did. Attacker-authored text added
  inside `trust_score`, `checks_run[i]`, `by_severity`, `provenance_coverage` or `effective_config`
  was dropped by serde, re-serialized to the originally signed bytes, and verified — so
  `veridex verify` printed "✓ certificate verified" over a document carrying, say, an
  `"auditor_note": "independently audited, safe for training"` that any consumer reading the JSON
  would see as authenticated. That is precisely what the comment at `signing.rs:115` says must be
  impossible. The existing test only injected at the top level, which is how the gap survived.

- **`verify` with no dataset path reported success identically to a bound verification.** Without a
  path the transplant check never runs, yet the output was byte-identical and the `bound to:` line
  read as a confirmation when it only echoed the certificate's own claim — so a certificate issued
  for one dataset, presented beside another, was accepted by every invocation that omitted the path,
  and `"verified": true` had no counterpart to `issuer_verified`. Now a `⚠ dataset NOT checked`
  banner and a `dataset_checked` field, matching how a missing trusted issuer is already handled.

- **HDF5 fabricated unwritten data as zeros.** A chunked dataset that is only partly written has no
  index entry for the unwritten chunks; HDF5 defines those regions as the declared fill value, and
  that is what `h5py` returns. The reader never parsed the Fill Value message (`0x0005`) at all and
  zero-initialized the row. The written chunks decoded correctly, so only the invented part was
  wrong — silently, with `coverage: Full` and an empty `unmapped_fields`. The fabricated values were
  hashed into `frame.value_ref.content_hash`, so a certificate bound a dataset to bytes `h5py` never
  read there, and were fed to the statistics as if measured. Worst instance: with `fillvalue=nan` and
  a partial write, `h5py` sees NaNs and Veridex reported `observed_non_finite = Some(0)` — which
  means "every value was read and every one was finite", so `STATISTICAL.NON_FINITE_OBSERVED`
  returned a confident clean answer over data it never looked at. The same root cause had the
  opposite symptom one branch over: a row covered by *no* chunk was refused outright as "the
  dataset's chunk index is incomplete", blaming a file that was complete and correct — and that shape
  is the most common thing a robot logger produces, pre-allocating N steps and writing fewer.

- **RLDS read a shard differently than TensorFlow reads it, five ways.** A map entry carrying its
  `value` submessage twice had the first silently discarded; protobuf merges repeated embedded
  messages, so TensorFlow concatenates the two lists and reads three steps where Veridex read two —
  the same bytes yielding a different episode length depending on who reads them, with Veridex
  signing its own answer. `--sample-episodes 10` over a 3-episode dataset whose manifest declares no
  shard lengths recorded a *declared* total of 10 and then raised
  `STRUCTURAL.EPISODE_COUNT_MISMATCH` (Error) against a sound dataset, for the size of the user's own
  flag. The ingest report claimed "masked CRC-32C → verified on every record" under a sample, where
  only the length prefix of a skipped record is checked. A `shape` present but not an object was read
  as a scalar, inflating a 2-step episode to 14 frames. An unparseable `shardLengths` entry was
  reported as "declares none".

- **Terminal output executed dataset-supplied ANSI escapes.** Every string a finding carries can come
  from the dataset — a stream name copied verbatim out of `info.json`, a directory name — and
  `render_terminal` wrote them straight to the TTY. A stream named
  `"\x1b[2J\x1b[1;1HVeridex report\n  Status:   PASS\x07"` clears the screen and repaints a forged
  PASS banner over the failing verdict about to be printed, and that name is embedded in ordinary
  `TEMPORAL.CLOCK_SKEW` messages. Control characters now render as visible `\xNN` escapes. The HTML
  and SARIF renderers were already safe.

- **PROV graphs dissolved in silence.** Every `@id` in `to_prov` interpolated free text — a dataset
  id from a directory name, an annotator lifted from source metadata — with no encoding. A space is
  enough: `veridex:dataset/my robot data` is not a well-formed IRI, and a JSON-LD processor drops the
  node and every triple about it rather than erroring. Measured with rdflib: a control dataset parsed
  to 7 triples, an annotator of `Jane Doe & Co` to 3 (the agent node and its attribution edge gone),
  a dataset id of `my robot data <2026>` to 0. The document still looked like valid JSON and
  `veridex provenance --emit prov` still reported success.

- **Surplus positional arguments were dropped without a word.** The parser refused unknown flags,
  unsupported flags and flags missing their value, then took `positionals.first()` and discarded the
  rest. `veridex check datasets/*.mcap` checked the first file, exited 0 on it, and never opened the
  others — a CI job reads that as "all my datasets passed".

- **`--profile` silently reverted thresholds the operator had tightened.** A profile is built as
  `Tolerances { clock_skew_ns: 20ms, ..default() }`, so every field it does not name holds a
  *default*, not an absence of opinion. Assigning the whole struct made `--profile` *loosen* the run:
  a config setting `ego_max_speed_mps = 1.0`, `outlier_z = 2.0` and `gap_factor = 1.5` had all three
  reset to 100.0 / 10.0 / 3.0 — and the "Tolerances (non-default)" line then said nothing, because
  the reverted values were once again exactly the defaults.

- **Python accepted a CI gate and threw it away.** `min_score` and `fail_on` live on `CheckConfig`
  and are read by the CLI directly; `to_run_config()` does not carry them, so
  `veridex.check(path, config=open("veridex.toml").read())` — the migration path the README
  prescribes — parsed the gate, validated it (a `min_score = 200` was even rejected), and discarded
  it. A config whose entire purpose was to fail CI returned a clean result. Now refused with an error
  naming the fields in the returned report that carry what it would have decided.

- **A profile's verdict never reached a machine consumer.** `--json`, `--sarif` and `--html` applied
  the profile's tolerances and reported none of its criterion verdicts, so the consumer most likely
  to gate on readiness could not see it. Also: a certificate's `rubric_version` was signed but never
  validated and never rendered, so a score produced under a different rubric printed with no version
  beside it — and scores are only comparable within one.

- **Two disclosure lines misstated their own numbers.** The tolerance line integer-divided
  nanoseconds, printing a deliberately tightened `clock_skew_ms = 0.5` as `clock-skew 0ms` and
  `rate_deviation = 0.004` as `rate 0%`; and 50.9 ms printed as `50ms`, which is exactly the default,
  so a *loosened* threshold read as untouched and the warning argued against itself.

*The entries below through "Two refusals named the nearest thing to the mistake" close an earlier
five-agent audit of the adapters, the canonical encoder and certificate, the check families, and
both front-ends. The encoder came back clean: all 62 CDM leaf fields are bound into the content
hash, with one documented exception (a `MediaStatus` reason string, derived from OS error text and
excluded for cross-platform hash stability). Everything else is below.*

- **A dataset's identity depended on how its path was typed.** `Path::file_name` returns `None` for
  a path ending in `.`, so `veridex check .` run from inside a dataset fell through to the adapter's
  fallback string — `"lerobot"`, `"zarr"` — while the same directory named absolutely took its real
  name. The id is bound into the CDM content hash, so identical bytes hashed two different ways and
  `veridex verify .` rejected a **genuine** certificate with a content-hash mismatch: the exact
  determinism the README promises, and the whole offline-verification story. The source path is now
  resolved before the name is taken, at all three sites. RLDS (id from `dataset_info.json`) and HDF5
  (a file always has a name) were never affected.

- **A LeRobot manifest could make Veridex read outside the dataset.** A feature key in
  `meta/info.json` is a JSON object key an attacker chooses, and it was joined onto the dataset
  directory to locate that feature's video. `Path::join` neither rejects `..` nor resists an absolute
  argument — an absolute one discards the base entirely — so a published dataset declaring a feature
  named `../../../../etc/shadow` had Veridex open that file and copy its real container headers into
  the CDM. `Media` is bound into the content hash and the signed certificate, and `MediaStatus`
  separates missing from unreadable from read, so the verdict was an existence-and-content oracle
  over the filesystem of everyone who checked the dataset. Containment is enforced at the single
  probe choke point — lexically, then again after resolution, since the component filter cannot see
  through a symlink — and a path that escapes is named rather than quietly called absent.

- **A normal Hugging Face download ingested as an empty dataset.** Both tree walks refused every
  symlink, to stop a link pointing at an ancestor recursing forever. But `snapshot_download`
  materializes every file in a repo as a symlink into the blob cache, so that is the ordinary
  on-disk shape of a downloaded LeRobot dataset: zero episodes at `Coverage::Full`, and every video
  it did have reported `VIDEO.MEDIA_MISSING`. Symlinked *files* are now followed; symlinked
  directories are still refused, which is where the recursion was.

- **A `timestamp` column of the wrong type was replaced with a clock Veridex invented.** Per row, a
  column Veridex cannot read is indistinguishable from a null cell — and a null cell legitimately
  falls back to `frame_index / fps`. Applied to a whole int64 column (nanoseconds, which several
  exporters write), that fallback discarded the recorded clock and substituted a mathematically
  perfect 1/fps ladder, still labelled `ClockKind::Measured`. Every temporal check then ran against
  a synthetic timeline and passed unconditionally: a five-second mid-episode gap certified clean. The
  column's type is checked once, up front, so the fallback stays what it was for. Alongside it, a
  negative `episode_index` (`-1` is a sentinel some exporters write) wrapped through `as u64` into
  18446744073709551615 and put those frames in a phantom episode no declared length is compared
  against.

- **The LeRobot fidelity report claimed files it had not read.**
  `meta/stats.json -> stream.stats` was pushed unconditionally although `load_stats` returns empty
  on a missing *or corrupt* file — and a corrupt one silently disables every stored-vs-observed
  comparison. An unparseable line in `meta/episodes.jsonl` was skipped without a word, which under
  `--metadata-only`, where that file *is* the episode set, reads as a smaller and perfectly clean
  dataset. Stats now report absent and unparseable as distinct omissions, and a malformed manifest
  line is refused the way a duplicate index already was.

- **Multiplexed CAN signals were decoded from every frame.** A multiplexed message reuses the same
  payload bytes for different signals, and one signal — the multiplexor, marked `M` — says which set
  the current frame carries. The indicator was never parsed: it stayed glued onto the signal's name
  (`"ValueB m1"`) and every multiplexed signal was decoded from every frame of its id. A frame whose
  selector said `m0` still produced a `ValueB` sample, reading `m0`'s bytes through `m1`'s layout and
  scaling — a plausible number that was never on the bus, given a CDM stream of its own,
  fingerprinted into the content hash, and graded by every check. Multiplexing is common in
  production DBCs. With no decodable multiplexor, nothing is known about which set is present, so
  none is decoded: an absent sample is a gap the temporal checks can see, where a fabricated one is
  not.

- **candump timestamps carried about a microsecond of invented jitter.** Epoch-scale seconds times
  1e9 needs 61 bits of mantissa against `f64`'s 53, so two lines exactly 1 µs apart came out 1024 ns
  apart — and `clock_kind: Measured` hands that to the temporal checks as though the bus had produced
  it. Composed from the integer seconds and fractional digits separately. Immaterial at a 10 ms
  raster, material at 1 kHz.

- **A file that parsed to nothing ingested as a clean, complete dataset.** A candump log every line
  of which failed — a CAN-FD capture (`##`), RTR frames, a text file that is not a candump at all —
  produced a successful, zero-finding, `Coverage::Full` dataset with an empty `unmapped`: a signable
  clean bill of health over a file that yielded nothing. Lines are counted now rather than skipped,
  and a partial failure is reported as the coverage gap it is. Two MDF4 walks did the same against
  this module's own doc promise: a `##DG` whose `cg_first` link points at a malformed block lost its
  entire data group, and a channel whose `##TX` name is missing vanished though it was real and
  decodable.

- **A check that crashed rendered as a check that passed.** `catch_unwind` isolates a panicking
  check so one crash cannot take the run down; what it must not do is make the crash disappear.
  `status_from` read only the severity counts, so a run in which *every* check panicked came back
  `Pass`, `veridex check` exited 0, and CI went green over a dataset on which nothing was measured.
  The certificate was worse, its reader being offline and unable to re-run anything: `checks_run` was
  `executed_checks` verbatim, and that field records *invocation*, not success, so the crashed check
  appeared among the checks that ran, beside an all-zero severity summary, and `categories_skipped`
  did not mention the category whose only check had died. The document now carries `checks_errored`
  (omitted when empty, so ordinary certificates are byte-unchanged), and the status is
  `PassWithWarnings` — a crash is not evidence the data is bad, it is evidence the verdict is
  incomplete.

- **`verify` never read the certificate's schema.** Every other version mismatch in that function
  fails closed; this one accepted a document declaring a future schema whose fields happened to parse
  under today's struct. A signature makes a certificate unforgeable, not intelligible.

- **Stored per-dimension statistics were never sanity-checked.** A LeRobot `meta/stats.json` for a
  7-DoF `action` stores min/max/mean/std as arrays, and the adapter carries the whole thing.
  `statistical.range-sanity` read element 0 and nothing else, so an inverted range, a NaN, a negative
  standard deviation or a dead joint on any axis above the first passed clean — and
  `value-measurability` counts `dim_stats` as "stats present", so nothing abstained either and the
  certificate listed the check as executed with no categories skipped. On a 7-DoF arm that is six
  unexamined joints. Each dimension is evaluated now, and the finding names which one.

- **A shape baseline could be frozen in a state it could never leave.**
  `structural.shape-consistency` captured one baseline from the first episode declaring either a
  dtype or a shape, and never enriched it — and the comparison requires both sides declared. So a
  stream whose first episode stated a dtype but no shape had `shape: None` fixed as its baseline
  permanently, and shape drift for it could never be reported however many later episodes conflicted.
  HDF5 and Zarr both write no shape for a 1-D dataset, making this the ordinary case: an `/action`
  that is `(N,)` in one episode file and `(N,7)` in another is precisely the un-collatable drift the
  check exists for.

- **A slow sensor with exactly two samples was graded as skewed.** The span comparison widens its
  tolerance by each stream's sampling quantum, because a stream observing a window at period `T`
  understates it by up to one full period with a perfect clock. That quantum was 0 below two
  intervals — exactly where a slow sensor lands in a short episode. A 1 Hz LiDAR beside a 100 Hz IMU,
  perfectly synchronized, drew a headline `TEMPORAL.CLOCK_SKEW` **error** for a 990 ms "drift" that is
  one LiDAR period, and flipped to clean the moment it caught a third sample. The quantum now falls
  back to the *declared* rate, which is a statement rather than a guess, bounded by the single
  observed interval so a sensor that fired twice and stopped cannot widen its way out of a real
  defect.

- **`--json --sarif` silently emitted SARIF.** The renderer dispatch is an if/else chain, so the
  losing flag was dropped without a word: a CI job doing `check --json --sarif > report.json` got the
  wrong document, which `veridex diff` then refused as not a Veridex report. Silently ignoring a flag
  is what `reject_flags_except` exists to prevent. Relatedly, `given_flags()`'s doc claimed "a test
  asserts this covers the parser's whole flag set" and no such test existed — the array is a fixed
  `[(&str, bool); N]`, which forces nothing, so a flag added to the parser without an entry would be
  accepted by every command. The two lists live in one file and are now compared as the textual fact
  they are.

- **A closed stdout panicked.** Rust's runtime ignores `SIGPIPE`, so a write to a closed pipe becomes
  `EPIPE`, which `println!` turns into a panic: `veridex checks | head -5`, or quitting `less` partway
  through a report, aborted with a backtrace and exit 101 — neither in the documented 0/10/20/2
  contract, and both ordinary usage.

- **`certify` wrote into the dataset.** The default certificate name is relative, so it landed in the
  working directory — which *is* the dataset after `cd my-dataset && veridex certify .`, the most
  natural way to do it. "It never mutates your dataset" is a README promise the adoption guide
  repeats. Nothing was corrupted and the CDM hash is unaffected, but a promise that holds except when
  inconvenient is not one a policy can rest on. Refused, with the one-flag fix in the message.

- **Python `diff` accepted what the CLI refuses, and the JSON diff dropped coverage.** The
  `is_report_shaped` guard was CLI-only, so a truncated artifact diffed as "every finding resolved,
  no regression" — silence from a file that was never a report, read as a clean bill of health. And
  `render_diff_json` carried no coverage at all, though `render_diff` leads with `Coverage: CHANGED`
  and `--fail-on-regression` gates on it: substituting a metadata-only report for a full one silences
  most of the catalog, so the machine consumer — the only consumer that document has — saw findings
  resolved and the score go up because the new run stopped looking.

- **Whole check families abstained without saying so.** The project's own principle is that a check
  which cannot measure must report that, or its silence reads as a pass. Two families did not.
  `statistical.value-measurability`: MCAP, CAN+DBC, MF4 and RLDS fingerprint payload bytes without
  interpreting them, so every statistical check hit its `let Some(..) else { continue }` — a CAN log
  with a wheel speed pinned at 655.35 km/h for 70% of its frames scored `data 100`, and the
  certificate listed all five statistical checks under `checks_run` with `categories_skipped: []`.
  HDF5 and Zarr are the narrower case: they recompute but publish no stored statistics, so the two
  stored-vs-observed checks can never fire there. Both are now reported, as separate codes, because
  they are different statements. `structural.content-measurability`: two checks compare frame bytes
  and abstain on any frame lacking a `content_hash`. A LeRobot video feature's pixels live in `.mp4`
  files outside the Parquet, and `duplicate-episode` aborted the whole episode signature if *any*
  frame lacked a hash — so the ordinary layout of a real LeRobot dataset made two byte-identical
  episodes undetectable, and `stuck-stream` (which only inspects `Video` streams) never ran at all.
  Both findings are informational: a dataset is not worse for the container it was published in.
  What changes is what a passing verdict is evidence of.

- **`diff` was coverage-blind.** Substituting a metadata-only report for a full one silences most of
  the catalog, so the full run's findings read as *resolved*, the trust score went up, and
  `--fail-on-regression` passed — precisely because the new run stopped looking. A coverage change is
  now a regression on its own, stated before anything else in the rendered diff.

- **Readiness was evaluated over partial runs.** Every `world-model-ready` criterion reported
  `passed: true, ran: true, findings: 0` over a metadata-only dataset with no frames at all, because
  `ran` records that a check was invoked, not that it had anything to inspect. A non-full run is no
  longer applicable. Alongside it, Python `certify` never called `Certificate::certifiable` though
  its doc comment claims both front-ends do — absent rather than satisfied, and one parameter away
  from a certificate over a run that never read the data.

- **`--min-score` could be satisfied by reading nothing.** Under `--metadata-only` the trust score's
  data axis is computed from checks that overwhelmingly had nothing to measure, so it lands near 100
  whatever the data holds — making `--min-score 90 --metadata-only` a one-flag way to pass a CI gate
  on a dataset whose values are garbage. The certify refusal does not contain this: the score also
  reaches `--json`, `--html`, and `diff`, none of which refuse it. The gate is now refused on a
  metadata-only run. A *sample* is different in kind — it scores real data, just less of it — and
  keeps working.

- **`check --profile` judged nothing.** `--help` calls a profile what the run is "judged against",
  and `check` only borrowed its tolerances: it printed no criterion verdicts at all, so the one thing
  the flag names was the one thing it did not report. It now renders the same per-criterion block
  `certify` does, from the same helper — unsigned, being the only difference.

- **A shared-file video layout was invisible to the entire video family.** A LeRobot v3 layout that
  concatenates episodes into shared video files gives Veridex nothing it can pair with a specific
  episode's rows, which was handled by attaching no `media` at all — but `media: None` is exactly
  what a non-video feature carries, so the video checks iterated straight past those streams and
  emitted nothing, for files that might hold no container at all. `MediaStatus::Unattributable` now
  records the abstention where a check can see it, reported once per stream as
  `VIDEO.MEDIA_UNATTRIBUTED` (info). Nothing is observed and nothing is accused; what is stated is
  that a desynced or re-encoded video here is not absent from the report, it was never looked for.
  The new status takes a fresh encoder tag rather than renumbering, so every dataset that hashed
  before it existed still hashes identically.

- **A single measured episode was reported as a systematic export defect.** The frame-count roll-up
  charges a video-length defect once when every episode is off by the same signed amount — an encoder
  dropping a leading frame — instead of naming each episode. It decided a stream had more than one
  episode by counting episodes that carried a `media` field at all, including those whose file was
  missing, whose container would not parse, or whose container declared no sample count. None of
  those yielded a length, so a stream with one short video beside one missing file announced "every
  episode's video is 1 frame(s) shorter than its rows" from one measurement, and discarded the
  per-episode report naming the file and both counts. It now counts the episodes that produced a
  length and requires two.

- **Jitter was charged as dropped frames.** `AUTONOMY.SEQUENCE_COMPLETE` counts a frame as dropped
  when an inter-frame gap sits near a multiple of the stream's median cadence, but the ±0.25-period
  window and the CV-0.5 abstention gate were not consistent with the 5% drop threshold they guard: on
  a 401-frame stream with gaussian jitter and nothing dropped, a CV of 0.44 measured "~6% of its
  frames". At that noise level a single interval reaches twice the median by chance often enough that
  the estimate is not merely noisy, it is unfounded. The window narrows to ±0.15 and the gate to CV
  0.40 — no false positive over 40 honest jittery streams (CV 0.1–0.45), while a real 10% drop rate is
  still caught and a 20% one is now covered by its own test.

- **Two refusals named the nearest thing to the mistake rather than the mistake.** `--max-frames -5`
  reported "--max-frames requires a value", because a leading `-` was read as the flag's value being
  swallowed by the next flag, so a negative number never reached the parser that knows what the flag
  accepts. And `verify --key issuer` (the secret file, not `issuer.pub`) reported "untrusted issuer" —
  a secret key is also 64 hex characters, so it parses as a public key, and `keygen` writes the two
  paths one letter apart. Verification now derives the public key the given secret would produce and
  says so when it matches the signer; a genuinely different issuer is still an untrusted issuer.

- **A `videos/` tree that never arrived passed clean and silent.** A LeRobot manifest declaring
  `dtype: "video"` says the stream's pixels live in video files. When none were found, the checks
  abstained — so an un-pulled LFS pointer or an interrupted `snapshot_download`, the single most
  common real video breakage, scored identically to the intact dataset and remained certifiable.
  Worse, it was a discontinuity: *one* absent episode was an error, *all* absent was silence. Now
  reported as `VIDEO.MEDIA_ABSENT`, charged once for the stream — one tree that never arrived is one
  gap, not one per episode. Only `dtype: "video"` carries the expectation, so a feature stored
  inline or as individual images is not asked for a video it never had.

- **The codec table was a closed allowlist over an open namespace.** A manifest records the
  *encoder* (`libopenh264`, `h264_videotoolbox`, `vp8`, `mpeg4`) while the container records the
  *format* (`avc1`, `vp08`, `mp4v`); an unrecognized spelling was compared literally and could never
  match, so `VIDEO.CODEC_MISMATCH` fired on sound datasets. The comparison now requires **both**
  names to resolve through the alias table and abstains otherwise — "I have not heard of this" is
  not "these differ" — and the table covers the common hardware and library encoders.

- **The resolution fallback assumed channel-last and ignored the manifest's own axis names.** With
  `video.width`/`video.height` absent, the declared resolution was read from `shape[0]`/`shape[1]`,
  so a channel-first `[3, 480, 640]` feature declared a height of **3** and reported a resolution
  mismatch against a perfectly good video. It now reads the axis order from the feature's `names`,
  and without those falls back only when the shape is unambiguously channel-last — stating nothing
  rather than fabricating a resolution.

- **A part-converted video layout was called incomplete while the same run said it should abstain.**
  A feature with both per-episode files and a v3 aggregate was recorded as unresolvable *and* still
  charged `VIDEO.MEDIA_MISSING` for the episodes inside the aggregate — a finding its own coverage
  note contradicted. An unresolvable feature now suppresses the missing-file report entirely.

- **A missing video was reported at a path the dataset does not use.** The expected path was
  fabricated as flat `videos/<feature>/episode_<n:06>.mp4`, wrong for the real chunk layout and
  hardcoding both the padding and the extension. It is now copied from the sibling episode's own
  file, so the finding names a path the user can act on.

- **Two episodes at two different wrong resolutions were reported as one.** The per-stream dedup
  froze the first occurrence's detail, so the second episode was reported as holding a resolution it
  does not hold — hiding the more serious condition, that the episodes disagree with each other. The
  dedup key now includes the observed value.

- **The MP4 probe misread four container shapes that real encoders write.** A **fragmented** file
  (`ffmpeg -movflags frag_keyframe+empty_moov`, DASH/CMAF, most hardware recorders) keeps an empty
  sample table in `moov` and its samples in `moof` fragments; its `stsz` count of zero was read as a
  real frame count, failing every episode of a valid dataset with a hard error. An all-ones `mdhd`
  duration — which ISO/IEC 14496-12 reserves for *unknown* — was taken at face value and fabricated
  a rate of ~0.002 fps. The compact sample table (`stz2`) was not read at all, silently disabling
  the frame-count check. And a `trak` without an `mdia` abandoned the whole track scan, reporting
  "no video track" about files that have one. All four are fixed and covered by fixtures.

- **A box declaring "to the end of the file" was reported as an absent `moov`.** Such a box makes
  anything written after it unreachable; the error now names it instead of blaming the header.

- **The content hash bound a free-form error string.** `MediaStatus::Unreadable`'s reason was
  hashed, and it is derived in part from the operating system's own error text — platform- and
  locale-dependent — and reworded whenever a message is improved. That broke "same bytes, same hash
  on any platform" and would have turned every wording fix into a silent hash change. The status
  *variant* still binds (that is what a check fails on); the prose does not, and the probe's reasons
  are now built from [`std::io::ErrorKind`] rather than OS strings so reports are stable too.

- **A certificate that stopped verifying after a canonical-encoding change read as tampering.**
  `CANONICAL_VERSION` has now been bumped three times; each bump rehashes byte-identical data, so
  `verify` reported a content-hash mismatch on an untouched dataset with no hint as to why. The
  error now names the version difference between the issuing and the verifying Veridex and says to
  re-issue before reading it as tampering.

- **A systematic video-length defect flooded the report.** `VIDEO.FRAME_COUNT_MISMATCH` was charged
  per episode unconditionally, so an export where every video is one frame short — an encoder
  dropping a leading frame, a converter counting from one — produced one finding per episode for a
  single defect, against a catalog whose rule is to charge one defect once. When every episode of a
  stream is off by the *same* amount it is now charged once, at the same severity; episodes off by
  differing amounts stay per-episode, because each of those is separately wrong.

- **A media uri could hash differently on Windows than on Linux.** `Stream.media.uri` was built with
  `Path::display`, which emits the platform separator, and the uri binds into the content hash. It
  is now joined with `/` explicitly.

- **A duplicate stream key inflated an export defect's episode count.** The per-stream rollup counted
  occurrences rather than distinct episodes, so a stream name appearing twice in one episode — a
  condition Veridex reports rather than assumes away — reported the defect as spanning more episodes
  than it did.

- **A broken transform tree could be reported by neither calibration check.** `CalibrationCompleteness`
  deferred its disconnected-tree finding whenever *any* rig sensor declared a `frame_id`, but
  `SensorFrameResolution` speaks about a stream only if *that stream* declares one, and its
  connectivity half additionally needs a camera whose frame the tree knows. The suppression
  precondition was far weaker than the successor's speaking precondition, so four real rig shapes went
  silent: the stranded sensor is the one with no frame (a mixed log where one driver omits
  `header.frame_id` lands here); no camera declares a frame; the camera names a frame the tree does not
  know; and a LiDAR-only rig with no camera at all. Suppression is now conditional on the successor
  actually being able to name the stranded sensors, so the worst case is a defect reported twice rather
  than not at all.
- **One mis-stamped sensor produced one finding per episode.** The calibration is dataset-level and
  stream names repeat in every episode, so a 50-episode drive log yielded 50 identical error-severity
  copies of a single defect, and 60 decoded CAN signals off one stranded bus yielded 60. Each
  `(stream, code)` is now claimed once, the same way the dataset-level statistical checks dedupe.
- **A bus signal was asked to reach the camera.** `SensorFrameResolution` scanned every rig modality,
  so a `CanSignal` (a scalar, never projected into an image) or an `EgoPose` stream (whose frame is
  joined to the body dynamically, not by the static TF tree) could be flagged for having no transform
  chain to a camera. It now scans the sensors a reprojection is actually defined for: point-cloud,
  camera, IMU, GNSS.
- **A lying `total_episodes` could exhaust memory before a byte of data was read.** Under a sampling
  request with no `meta/episodes.jsonl`, the episode set was built from `info.json`'s declared total —
  an attacker-controlled `u64` in a few-hundred-byte manifest, materialized before either ingest budget
  exists, so neither `--max-frames` nor `--max-decompression-ratio` bounded it. `u64::MAX` panicked on
  capacity overflow; `100000000` measured 16.6 s and 1.3 GB and then returned *Ok*. `--sample-episodes`
  now materializes only the indices it can select, and the random draw refuses a declared total above
  1,000,000 rather than trusting it.
- **A frame name could expand past the decompression budget.** The CDR reader's slice is bounded by
  the message body, but invalid UTF-8 expands 3x on the way out (each bad byte becomes a 3-byte
  U+FFFD) and the decoded name is *retained* in the CDM, while the budget charges the raw body: 63
  channels each carrying 1 MiB of `0xFF` measured 198 MB retained from a 19.8 KB file. Names are now
  capped at 4 KiB — three orders of magnitude above any real ROS frame id.
- **A sampled run dropped a truncation it could still see.** Dropping the dataset-level declared totals
  under a sample also dropped the comparison that *is* in the sample's scope: the sample selected N
  episodes and only 2 materialized. A dataset declaring 10 episodes while holding 2 passed clean under
  `--sample-episodes 10`. The selected-episode count is now recorded, so the structural check still
  catches an episode set the manifest declares and the data lacks.
- **Python discarded `sample_seed` when given with `sample_episodes`.** The CLI rejects that pair (a
  seed only means something for the random draw); the binding's own doc comment claimed it did too, and
  only the no-sample branch enforced it.
- **A non-finite LeRobot timestamp was cast rather than rejected.** `(ts * 1e9).round() as i64` turned
  a `NaN` cell into `0` — a fabricated start-of-recording that reads as an ordinary timestamp — and an
  infinity into `i64::MAX`. Non-finite cells now contribute no frame, matching `mdf4::seconds_to_ns`,
  which had this guard first.
- **Every command silently tolerated the flags it does not act on.** The shared parser accepts one flag
  set for all eight commands, and the per-command rejection list was a hand-maintained deny-list, so
  `check --out r.json` looked like it wrote a file, `diff --min-score 90` looked like a gate, and
  `checks` ignored all twenty other flags — each silent by construction, which is the exact failure the
  list existed to prevent. It is now an allow-list per command: a flag missing from one is rejected,
  which is loud and trivially fixed. `certify` keeps its own message for the sampling flags, because
  "does not support --sample-episodes" is true but does not say why.
- **The coverage banner carried floating-point noise.** `--sample-fraction 0.29` rendered as
  `28.999999999999996% of episodes`, in every report and in the verdict hash.
- **A rig that could not be spatially fused certified as `world-model-ready`.** Adding
  `autonomy.sensor-frame-resolution` moved the disconnected-transform-tree report off
  `autonomy.calibration-completeness` — which the profile judges — and onto the new check, which was
  not in `WORLD_MODEL_READY_CRITERIA`. The defect landed in a check the profile did not watch, so all
  four criteria reported `passed: true` while the verdict said `fail`: a signed certificate carrying
  `status: "fail"` beside `readiness.ready: true`, and `ready` is the field a consumer gates on. For a
  disconnected tree this was a straight regression — before, the tree tripped a criterion and `ready`
  was correctly false. The check is now the profile's fifth criterion, and three regression tests pin
  the invariant behind it: **a failing verdict never carries `ready: true`**. Every autonomy check that
  can fail a rig belongs in the criteria list, which is now stated where the list is defined.
- **The findings sort key was not total.** It ordered on five of `Finding`'s eight fields, omitting
  `category`, `risk`, and `remedy`, so two findings differing only in those fell through to `Vec`
  order — which is execution order. `result_content_hash` is computed over that sequence, so the same
  two findings emitted in either order would have had to hash alike. Not reachable from today's checks
  (each emits deterministically), but it was the one ordering in the codebase that could tie on
  non-identical content. All eight fields are now in the key.

- **One shared timeline produced a finding per stream.** Several streams in an episode routinely share
  a timeline — an MF4 channel group samples every channel on one raster, a CAN message decodes into
  many signals off the same frames — so `TEMPORAL.GAP` and `TEMPORAL.JITTER` re-reported one root
  cause once per stream. A normal 8-channel event-driven log produced 32 warnings for 4 real facts,
  deducting enough to floor the data score at 0. The timeline checks now report once and name how
  many streams share it.
- **`AUTONOMY.SEQUENCE_COMPLETE` called a complete event-driven log 88% dropped.** Its baseline is the
  frame count a stream's own median cadence implies over its span — meaningless for a change-triggered
  signal that arrives in bursts with long idles, which never aimed at a cadence. It now abstains when
  the intervals are far from uniform (that shape is `TEMPORAL.JITTER`'s to report); a genuinely
  dropping steady stream stays well inside the bound.
- **A few hundred KB of crafted input could exhaust memory.** Every adapter materializes
  *streams × samples* frames and both factors come from the file — a CAN log's signals-per-id against
  its frame count, an MF4 group's channels against its records, a LeRobot `info.json`'s declared
  features (which need no matching Parquet column) against its rows. Measured: 344 KB of crafted CAN
  produced 6.4M frames and 900 MB, doubling with each doubling of input, so a ~10 MB file projects to
  tens of GB and an OOM-killed CI gate. Ingestion now charges a **frame budget** (default 20M, well
  above real datasets — a one-hour ten-sensor 100 Hz rig is 3.6M) *before* allocating, and refuses
  with a clear error naming the limit rather than being killed. `--max-frames <n>` raises it;
  `--max-frames 0` removes it.
- **Python had no SARIF or HTML binding**, so the two CI-facing render formats were CLI-only despite
  the stated parity. `veridex.check_sarif` and `veridex.check_html` now expose them through the same
  shared render helpers, so their output is byte-identical to `--sarif` and `--html`.
- **Commands accepted gate flags they could not honor.** `inspect --min-score 90` looked like a gate
  and was none, and `--fail-on` was equally inert on `inspect`, `provenance`, and `verify`. Each now
  refuses the flag by name rather than ignoring it.
- **The `av` demo's ego trajectory never decoded, so the flagship readiness demo said N/A.** Its
  Odometry topic carried an 8-byte dummy payload like every other sensor, so `Episode.ego_poses` came
  back empty — and the `world-model-ready` profile, which applies only to a rig carrying a perception
  sensor *and* an ego trajectory, correctly abstained. The generator now writes a real CDR Odometry
  body (a ~10 m/s drive down +x), so the demo exercises ego-pose decoding and prints the NOT READY
  report the quickstart documents. A test pins profile applicability in both directions.
- **`veridex diff` skipped flag validation, so a typo turned the CI gate off.** It scanned argv for
  the flags it recognized and dropped everything else, so `--fail-on-regresion` (one letter short)
  silently disabled the regression gate and exited 0 — the exact failure the shared parser exists to
  prevent. `diff` now goes through it, and unknown options are a tool error like everywhere else.
- **`veridex diff` read a wrong-shaped file as "no findings".** An empty `{}`, a truncated artifact,
  or a SARIF file passed by mistake produced "all resolved, no regression" and passed the gate.
  Both inputs must now carry a findings array, and a diff between reports bound to different dataset
  content says so.
- **`check --profile` was parsed and thrown away.** The run silently used the default, looser
  thresholds while the user believed the profile's applied, and an unknown profile name passed
  without a word. `check` now resolves the profile, applies its tolerances, and rejects an unknown
  name — matching `certify`.
- **`certify --config` was accepted and ignored**, including its validation. A signed certificate
  could disagree with the `check` just run on the same data in the same directory (`check` also
  auto-discovers `veridex.toml`; `certify` did not), and a config naming a nonexistent check was
  silently accepted here while `check` rejected it. `certify` now loads, validates, and applies the
  same configuration, with a profile's tolerances taking precedence.
- **A crashed check rendered as a clean pass in HTML and SARIF.** Only the terminal report listed
  `errored_checks`, so a CI job gating on SARIF or a human reading the shareable HTML artifact saw
  green while a check never ran. HTML gains an "Errored checks" section, SARIF a
  `VERIDEX.CHECK_ERRORED` result per errored check. The HTML report now also discloses non-default
  tolerances, as the terminal one already did.
- **`verify --json` printed plain text on failure**, leaving a machine consumer nothing to parse.
- **`veridex --help` omitted four real flags**, including `--allow-any-issuer`, the documented way to
  skip issuer trust.
- **Python could not see a config, so it disagreed with the CLI.** `veridex.check` now takes
  `config=` (the contents of a `veridex.toml`), validated the same way; Python still never
  auto-discovers a config file, since an import should not pick up behavior from the working
  directory.
- **The LeRobot/Parquet path had no expansion bound at all.** Every row of a Parquet file was decoded
  into memory before the frame budget was charged, and the decompression budget was never consulted:
  a 50 KB zstd file measured **1.26 GB** resident and a 149 KB file **3.76 GB**, in both cases raising
  the budget error only after the memory was spent. Both budgets are now charged per record batch as
  it decodes, and the per-row cost is the larger of what `info.json` declares and what the Parquet
  actually holds — a manifest declaring zero features no longer rides a 50,000-column file for free.
- **A crafted MF4 block length could panic or be silently accepted.** The `at + length` containment
  check in the block-header reader used unchecked arithmetic on a file-declared `u64`: a header
  claiming `u64::MAX - 8` bytes panicked in debug (the mode the test suite runs in) and, in release,
  wrapped into a header that passed validation — so a corrupt file was accepted as a clean, signable,
  zero-episode dataset instead of being refused.
- **Duplicate MF4 channel names were disambiguated quadratically.** Each collision restarted its
  suffix counter at zero and re-probed from scratch, so *N* identically-named channels cost O(N²):
  16,000 of them in a 1.3 MB file measured 18 seconds, and a 100 MB file extrapolated to hours of CPU
  inside a CI gate. Each collision is now one probe.
- **A certificate could verify against a dataset it was not issued for.** `declared_frame_count` was
  deliberately left out of the content hash as an assertion *about* content rather than content — but
  `structural.episode-boundary` reads it and fails on it, so two datasets differing only there (one
  passing, one failing) hashed identically and the clean one's certificate verified against the
  corrupt one. It is now encoded; `CANONICAL_VERSION` is **4**.
- **The hash depended on input order for exactly the datasets Veridex exists to catch.** Episodes were
  ordered by `index` alone and streams by `name` alone — neither a total order, and duplicates of both
  are faults the catalog reports. A stable sort left ties in `Vec` order, so two datasets holding the
  same duplicate-index episodes in different orders produced different content hashes and different
  `result_content_hash`es. Both now break ties on the item's own canonical encoding (computed only for
  items that actually tie, so an ordinary dataset pays nothing). `canonicalize_order` also now sorts
  episode labels and the calibration transform/intrinsics sets, which the encoder already treated as
  sets — closing the gap before a reader resolves "the transform valid at time t" by first match.
- **A signed certificate had no canonical byte form.** Hex decoding and the algorithm check were
  case-insensitive, so uppercasing `signature`, `public_key`, or `algorithm` produced a different file
  that still verified. Verification now requires the canonical spelling, so a consumer that pins or
  de-duplicates certificates by file digest cannot be handed two files that both verify.
- **Every honest multi-rate rig was reported as clock-skewed.** `TEMPORAL.CLOCK_SKEW` and
  `AUTONOMY.RIG_SYNC` compare stream *spans*, but a stream observing a window at period `T` spans a
  whole number of `T`s — so two perfectly synchronized sensors at different rates differ by up to one
  period with no drift at all. The 50 ms tolerance was therefore smaller than the intrinsic bias of
  any sensor slower than 20 Hz: a zero-drift rig of 10 Hz LiDAR + 100 Hz IMU + 5 Hz GNSS was measured
  reporting a 70 ms "drift" (500 ms with a 1 Hz GNSS), and a 30 fps camera beside a 10 Hz state stream
  scored F. Both checks now widen the tolerance by the larger of the two streams' own sampling
  periods. A real 500 ms drift on a 10 Hz sensor is still flagged.
- **`AUTONOMY.SEQUENCE_COMPLETE` still called complete event-driven data dropped.** Dividing the span
  by the median cadence charges idle stretches as missing frames, and the interval-uniformity guard
  did not bound that (a stream of 40 x 80 ms and 10 x 200 ms intervals — every event present — sat
  under the guard and was reported ~23% dropped). It now counts the frames that gaps at *multiples* of
  the cadence actually swallowed, so an idle burst costs nothing and a steady sensor's real drops are
  still found.
- **One root cause could be deducted many times.** `TEMPORAL.NON_MONOTONIC` had no shared-timeline
  guard, so a single stuck timestamp on an 8-channel CAN group cost eight Errors and floored the data
  score; it now reports once per timeline and names the rest, as `TEMPORAL.GAP` and `TEMPORAL.JITTER`
  already did. `SEMANTIC.AMBIGUOUS_STREAM_KEY` and `SEMANTIC.DUPLICATE_STREAM_KEY` were emitted per
  episode, so one naming mistake across 50 episodes cost 100 warnings; a naming mistake is a property
  of the schema, so each collision is now reported once, naming the first episode it appears in.
- **A constant stream's float-noise `std` was either missed or called impossible.** `DEGENERATE`
  required `std == 0.0` exactly, and the Popoviciu tolerance scaled with the *range* rather than the
  magnitude of the values. A constant channel at 0.7 with a `std` of 1e-12 escaped entirely; at 1e-8 —
  what naive `E[x²] − E[x]²` cancellation produces at that magnitude — it became a
  `STATISTICAL.STD_IMPLAUSIBLE` **error**, as did a near-constant channel at 300.0 with an f32-computed
  `std`. Both now use one magnitude-scaled rounding tolerance. A genuinely impossible `std` is still an
  error.
- **`STATISTICAL.SATURATED` claimed a stream before testing it**, the same defect just fixed in
  `range-sanity`: a clean episode-0 copy of a stream masked a saturated one in a later episode, and
  the finding depended on episode order.
- **Three thresholds were unreachable from config.** `STATISTICAL.OUTLIER`'s sigma and the two
  autonomy tolerances (`AUTONOMY.SEQUENCE_COMPLETE`'s tolerated drop fraction,
  `AUTONOMY.EGO_POSE_CONTINUITY`'s maximum plausible speed) were hardcoded to their defaults while
  every other family's were tunable — so a rig with a legitimately faster platform, or a
  deliberately sparse sensor, had no way to say so. They are now `outlier_z`,
  `sequence_drop_fraction`, and `ego_max_speed_mps` under `[tolerances]`: validated on parse
  (a sigma at or below 1.0, a drop fraction outside `[0, 1)`, or a non-positive speed is rejected,
  not silently accepted), snapshotted into the signed effective config, and listed in the report's
  non-default-tolerances note.
- **A clean episode could mask a later episode's corrupt statistics.** `statistical.range-sanity`
  reports each stream once (stored stats are dataset-level), but it claimed the stream name *before*
  evaluating it — so only the first episode carrying a stream was ever examined. Exact today, wrong
  the moment an adapter attaches per-episode stats. It now claims the stream when it produces a
  finding, like its sibling checks: a clean episode 0 followed by a corrupt episode 1 is reported,
  and attributed to the episode it was found in. Findings still never scale with episode count.
- **The frame budget bounded frames, not the bytes they arrive in.** An MCAP chunk header declares how
  much it unpacks into, and nothing checked that figure: a few hundred bytes claiming 8 GiB of chunk
  contents sent the reader into an unbounded read loop, and a chunk full of oversized messages costs
  one frame each — cheap by the frame budget, ruinous in memory. Ingestion now also charges a
  **decompression budget**, sized at 100x the file's own size (with a 64 MiB floor) so it scales with
  genuinely large logs while refusing bomb-scale ratios. It is charged off the chunk headers *before*
  the file reaches the reader, and again against the message bytes that actually arrive, so a header
  that understates its expansion buys nothing. `--max-decompression-ratio <n>` raises it; `0` removes
  it.
- **A scenario/map version could be read from the wrong place and recorded as extracted.** The ASAM
  `revMajor`/`revMinor` scan searched the whole file for each attribute independently, so a templated
  `.xodr` whose comment or `description` mentioned `revMajor="0"` had that read as its declared
  version — class `known`, i.e. presented as read from the file's bytes. Both attributes are now read
  from the same header element, comments are skipped, and the element is walked as `name="value"`
  pairs, so a mention inside another attribute's value or a longer name ending in `revMajor` no
  longer matches. Empty values no longer yield the version `"."`, and a bare `name=` at a truncated
  buffer's end no longer abandons the scan.
- **Two datasets could share a content hash and disagree on the verdict.** The canonical encoder
  treats several collections as *sets* — the ego trajectory, dataset metadata, provenance records and
  their elements — but `canonicalize_order` sorted only episodes and streams, and some checks read
  those collections as sequences or by first match. Verified: the same six ego poses in two Vec orders
  hashed identically while one reported five 200 m/s teleports and the other passed; duplicate
  metadata keys and provenance records behaved the same way. Since a certificate binds the content
  hash, it could attest a hash that also matches a dataset that fails. `canonicalize_order` now sorts
  every collection the encoder canonicalizes, with the encoder's own sort keys so the two cannot
  drift, and a property test permutes all of them at once and asserts both the content hash and the
  verdict are unchanged.
- **Provenance emit could contradict itself across permutations.** Elements were sorted by `key`
  alone (ties left in Vec order) and mapped fields like `license` took the first match, so two
  datasets with an identical content hash emitted different attribution. Both now use the encoder's
  full content key, and `inspect`/`provenance` canonicalize before rendering on both surfaces.
- **A decoded value's fingerprint could differ between x86 and ARM.** The CAN+DBC and MF4 adapters
  hashed `f64::to_bits` of an *arithmetic result*, and a DBC or `##CC` coefficient of `inf` makes
  `0.0 * inf` a NaN whose default sign is platform-specific (`-0.0` was likewise distinguishable from
  `+0.0`). Both now route through the encoder's canonical float bits, so the same bytes hash the same
  everywhere — which is what the determinism contract promises.
- **A 33 KB MF4 file could allocate 1.35 GB.** The block-graph walk kept a visited set per parent
  chain, but MF4 links may legally point at shared blocks — so *n* data groups each re-walking the
  same *n* channel groups each re-walking the same *n* channels was O(n³) streams. One visited set
  now spans the whole walk, making it linear in file size.
- **MF4 could produce plausible-but-wrong data instead of reporting it.** An unapplied `##CC`
  conversion was reported for a signal but ignored on the **time master**, silently shifting every
  timestamp in the group (it now stops the group); channels declaring per-sample invalidation bits
  were decoded as if every sample were valid (they are now skipped and reported); a second channel
  group inside a sorted data group was decoded against the same records from offset 0; and a
  three-way name collision emitted two streams with the same name.
- **MF4 rasters were compared as if they shared a clock.** Every stream got one `mf4-master` clock id,
  so a 1 Hz group and a 100 Hz group over the same measurement tripped start/end-offset checks. Each
  channel group is now its own timeline.
- **A bus-only measurement was treated as a sensor rig.** Rig detection counted AV-native streams, and
  a CAN or MF4 log is dozens of `CanSignal` streams off one bus — so ordinary raster differences read
  as rig-wide clock drift (an *error*), and the pairwise `TEMPORAL.CLOCK_SKEW` was suppressed on those
  datasets. A rig now also requires two distinct AV-native modalities, which every real rig has.
- **`veridex verify` implied trust it had not checked.** With no `--key`, verification confirmed only
  that a certificate was internally consistent and bound to the presented dataset — so a certificate
  forged about *real* data and signed with an attacker's own key verified cleanly, exit 0, reporting
  whatever score it claimed. `verify` now requires a trust decision: name the issuer with `--key`, or
  pass `--allow-any-issuer` for the self-consistency check alone, which prints a warning and reports
  `issuer_verified: false` in `--json`. Python's `veridex.verify` mirrors this (`allow_any_issuer=`).
- **Certificates tolerated fields the signature never covered.** The signature is computed over the
  parsed structure, so an injected `trust_score_override` (or anything else) survived verification
  and would be read as authentic by any consumer parsing the JSON directly. Every certificate type
  now rejects unknown fields.
- **A symlink could lead the reader outside the dataset.** `simref`'s sidecar lookup rejected `..`
  and absolute paths but still followed symlinks, and the CAN+DBC adapter's input discovery did not
  check at all. Both now refuse: the sidecar path is canonicalized and re-checked for containment
  under the dataset root, and a symlinked CAN log is skipped.
- **A corrupt element count could reserve gigabytes.** The ROS CDR decoder bounded a declared element
  count against the message's *byte* length, but each element is far larger than a byte — a 100 MB
  TFMessage claiming 100M transforms reserved ~13 GB before the first read failed. Counts are now
  bounded by the smallest each element can encode.
- **`keygen --force` left a pre-existing key file world-readable.** The `0600` mode applies only at
  creation, so overwriting an existing path wrote a fresh secret seed into it without tightening
  permissions. It now sets the mode explicitly after the write.
- **A readiness criterion could pass without its check ever running.** `ReadinessReport::evaluate`
  derived `passed` from "this check produced no findings" — but a check disabled in `veridex.toml`,
  filtered out by `categories`/`only_checks`, or one that failed internally also produces none. A
  dataset that genuinely failed `autonomy.rig-sync` could be certified `READY` by disabling that
  check. Each criterion now records whether its check actually **ran** (executed and did not error);
  silence from a check that never ran blocks `ready` and prints as `? … [check did not run]`. The
  field is omitted when the check ran, so certificates issued before it existed still verify
  byte-identically.
- **`world-model-ready` applied to datasets its criteria couldn't judge.** Applicability was "is this
  a sensor rig", and a rig is ≥3 AV-native sensors — which a bus-only CAN or MF4 log satisfies. With
  no perception sensor and no ego trajectory, calibration completeness and ego-pose continuity abstain,
  so such a log was certified ready on two criteria that examined nothing. A profile now carries an
  explicit `applies_to` predicate, and `world-model-ready` demands a rig **with** a perception sensor
  and an ego trajectory; anything else is `N/A`.

- The `veridex-data` wheel could not build: `pyproject.toml` was missing a `version` (now taken
  dynamically from the crate) and referenced a nonexistent package `README.md` (now added). The
  wheel builds and the parity test passes under pyo3 0.29.
- A mistyped or missing dataset path was misreported as `unsupported format: no adapter recognized
  the source`. Ingestion now checks a local path exists first and returns a clear
  `no such file or directory` (`IngestError::SourceNotFound`), distinct from an unrecognized format.
- `veridex verify --key <path>` with a missing/invalid key file was silently reinterpreting the path
  string as the key, then reporting `untrusted issuer` (a verification *failure*, exit 20) instead of
  a tool error. The `--key` value is now resolved unambiguously — a 64-char hex key inline, otherwise
  a file path — and an unreadable or non-hex key file is a clear exit-2 error, not a false mismatch.
- `veridex keygen` silently overwrote an existing key file — an unrecoverable loss of a signing key.
  It now refuses to clobber an existing secret or public key unless `--force` is passed.
- `veridex check --fail-on <typo>` silently fell back to the default threshold, quietly disabling the
  strictness a CI user asked for. An unrecognized `--fail-on` value is now an exit-2 error.
- The temporal checks (rate, gaps, clock-skew) computed timestamp intervals with plain `i64`
  subtraction, which overflowed on corrupt timestamps spanning the full `i64` range — a panic in
  debug builds (isolated to an errored check) or a wrapped value in release. They now use saturating
  subtraction, so pathological timestamps are reported rather than crashing the check.
- The content hash silently omitted four `Stream` stats fields — the stored per-dimension stats
  (`dim_stats`) and the recomputed `observed_*` fields — because the hand-written canonical encoder
  had drifted from the struct. Two datasets differing only in a corrupted per-joint stat vector
  hashed identically. The encoder now binds every content-bearing stream field, a regression test
  guards each one, and `CANONICAL_VERSION` bumps 1 → 2.
- Provenance canonicalization sorted records by scope alone and elements by key alone — neither a
  total order — so a scope with more than one record, or two elements sharing a key, could hash
  differently under a mere reordering. Both now sort on full content (permutation-independent).
- The LeRobot adapter read Arrow bookkeeping cells (`timestamp`, `episode_index`, `task_index`)
  without consulting the null bitmap, so a null cell read as a fabricated `0` — inventing a
  mid-stream `ts = 0`, misattributing frames to episode 0, or mislabeling a task. Null cells now
  abstain, so a null timestamp correctly falls back to `frame_index / fps`.
- `STATISTICAL.MEAN_OUT_OF_RANGE` compared the stored mean against min/max with no float tolerance,
  so a source's independently-rounded mean landing one ULP past a bound on a near-constant stream
  raised a hard error on honest data. It now allows the same small tolerance as the Popoviciu std
  check.
- The LeRobot per-dimension statistics silently misaligned when a multi-DoF cell had a **null leaf**:
  a dropped joint contributed nothing, sliding every later dimension down one and polluting their
  min/max/mean/std (false `STATS_STALE`/`SATURATED`, misattributed dimensions). A null leaf now holds
  its dimension slot (absent, not shifted), matching the content-hash path; a regression test covers it.
- The verdict and human/JSON/SARIF reports were **input-order-dependent** while the content hash was
  order-independent, so two datasets that hashed identically but were built with their episodes/streams
  in a different order could produce different `result_content_hash` and report bytes. The pipeline now
  canonicalizes episode order (by index) and stream order (by name) before validating, so the verdict
  matches the hash's order-independence.
- A non-finite tolerance (`NaN`/`inf`) constructed via the library/Python API serialized to JSON
  `null` — a signed certificate embedding it could never be re-verified — and silently disabled the
  checks that guard on it. Tolerances are now sanitized to their finite defaults before the run and in
  the recorded config.
- `veridex check --min-scor 90` (any mistyped or unknown flag) was silently ignored, quietly dropping
  the CI gate the user asked for; a value-flag could also swallow the next flag as its value
  (`--key --format`). Unknown options and missing flag values are now exit-2 errors.
- The LeRobot adapter never reconciled the Parquet data columns against the `meta/info.json` feature
  declarations, so an undeclared data column was silently dropped and a declared-but-absent feature
  became a phantom stream with no content — neither disclosed. The fidelity report now lists an
  undeclared column as `unmapped` and a declared-but-absent feature as `omitted`.
- The LeRobot adapter never validated `codebase_version`, so a v2.x export (which still has
  `meta/info.json`) was misparsed as v3. A recognized-but-unsupported version is now rejected cleanly
  with `IngestError::UnsupportedVersion`.
- Recomputed per-dimension variance used the one-pass `E[x²]−E[x]²` formula, which loses precision
  (and can clamp a real variance to 0 → spurious `DEGENERATE`) for signals riding a large DC offset.
  It now uses Welford's numerically stable online algorithm. Integer index columns stored as an
  unsigned or narrower Arrow type are now accepted instead of falsely rejecting the dataset, and the
  Parquet directory walk no longer follows symlinks (a self-referential link could recurse unbounded).
- Robustness: MCAP `log_time` above `i64::MAX` now saturates instead of wrapping negative and
  corrupting frame ordering; `STREAM_ABSENT` no longer lists a duplicate episode index twice; and the
  saturation check skips a zero-sample summary rather than emitting a `NaN%` finding, while the score's
  penalty arithmetic saturates so a pathological finding count cannot overflow.
- `SEMANTIC.ANNOTATION_UNALIGNED` treated a declared episode window as authoritative even when it was
  *narrower* than the recorded frames, so a `language` annotation on a genuinely recorded frame outside
  that window raised a false Error (flipping the episode to FAIL). The alignment span is now the union
  of the declared bounds and the actual frame extent; a genuinely out-of-range annotation still fires.

### Security

- **A dataset manifest could name any file on the host.** A LeRobot feature key — an untrusted JSON
  object key — was joined onto the dataset directory to locate that feature's video, and neither `..`
  nor an absolute path was rejected. Veridex opened the named file and copied its container headers
  into the CDM, which is bound into the content hash and the signed certificate; `MediaStatus`
  separates missing from unreadable from read, so a published dataset turned every verdict issued
  over it into an existence-and-content oracle over the checker's filesystem. Containment is now
  enforced at the probe, lexically and again after symlink resolution. See the Fixed section for
  detail.

- **Two untrusted-input overflows aborted the process.** A DBC declaring a 63-bit signed signal hit
  `1i64 << 63` == `i64::MIN` and panicked in any debug or CI build (the existing width coverage
  tested 64 and 65 and stepped over 63). And an MCAP record declaring `u64::MAX` bytes overflowed
  the framing walk — where testing it end to end found the deeper problem: the vendored `mcap`
  reader overflows on the same input, so a **17-byte** hostile file killed the run before any
  Veridex guard was reached. A record claiming more bytes than the whole file is now refused by
  name; a merely truncated one is still left to the reader, which describes it better.

- Certificate verification now uses Ed25519 `verify_strict`, rejecting non-canonical signatures and
  small-order keys so a given certificate has exactly one valid signature (no malleability).

- `veridex keygen` wrote the secret signing key world/group-readable (default umask), so another local
  user on a shared host or CI runner could read it and forge certificates. On Unix the secret key is
  now created `0600` (owner-only); the public `.pub` file is unchanged.

- Upgraded `pyo3` 0.22 → 0.29, clearing three advisories (RUSTSEC out-of-bounds read in
  `PyList`/`PyTuple` `nth`/`nth_back`, the missing `Sync` bound on `PyCFunction::new_closure`, and
  the `PyString::from_object` buffer-overflow risk). The bindings' API surface was already on the
  `Bound` API, so the bump is source-compatible.

### Decided

- **The certificate's crypto substrate: a mirrored module, not a shared crate — and not COSE or
  JWS.** This was the last open question from M0, and until now the docs answered it wrongly. Six
  places across the specs, `project.md`, and the README asserted that Veridex signs with COSE/JWS
  "reusing Invariant's substrate," one of them normatively (`SHALL`). Neither half was true:
  Veridex has never depended on a COSE or JOSE crate, and it shares no code with Invariant — which
  *does* depend on `coset`. A reader following the README would have reached for a JOSE library and
  found nothing to point it at.

  What actually ships, now recorded as design D6a and described consistently everywhere: a detached
  **Ed25519** signature over `b"veridex.certificate.sig.v1\0"` concatenated with the certificate's
  JSON. The algorithm is fixed rather than read from the document, so `verify` rejects any other
  `algorithm` value instead of dispatching on it — the `alg`-confusion class does not exist here.

  The decision is recorded with the limitation it carries rather than as a clean win. The
  `.veridex.json` file is written pretty-printed while the signature covers the *compact*
  serialization of the nested certificate, so a third-party verifier cannot sign over a substring of
  the file — it must re-serialize the parsed object with serde's field order and Rust's float
  formatting. Exact and tested inside Veridex; fiddly outside it. Adopting RFC 8785 canonical JSON,
  or signing the file bytes as written, is the natural v2 and would be a new `algorithm` value plus
  a schema bump rather than a rewrite.

### Not yet included

Streaming / larger-than-memory reads and remote Hub ingestion (`Source::Remote` is *refused* with a
clear error rather than silently ignored, returning `IngestError::NotImplemented`); and publishing to
PyPI / crates.io. Metadata-only ingestion is no longer in this list — it shipped, and has its own
entry above.
