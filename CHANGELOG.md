# Changelog

All notable changes to Veridex are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/); versions use [SemVer](https://semver.org/).

## [Unreleased] — v0.1 MVP (in progress)

The first shippable slice of the [`bootstrap-veridex-mvp`](openspec/changes/bootstrap-veridex-mvp/)
change. Runs end-to-end: ingest → validate → score → report → sign.

### Added

- **A certificate now names its findings by code, so an abstention survives into the signed
  document.** The certificate carried findings rolled up by severity and by *family*, which cannot
  express the one thing it most has to: **which checks measured nothing**. A single-episode dataset
  whose streams hold no summarizable values signed as 46 checks run, no family skipped,
  `statistical: 1` and `structural: 1` — while all five statistical checks had nothing to measure and
  seven cross-episode checks had nothing to compare. Twelve of the forty-six presented as clean
  executed checks.

  The abstention findings exist so a pass cannot mean "nothing was asked", and
  [CONTRIBUTING.md](CONTRIBUTING.md) states that such a finding "travels into the JSON, the SARIF,
  the HTML and the certificate". The first three were true. The certificate flattened them into a
  family count, where `STATISTICAL.UNMEASURED_VALUES` is indistinguishable from a real statistical
  fault — and the offline reader is the one person who cannot re-run Veridex to find out.

  `findings_summary.by_code` is now signed alongside the coarser rollups; `verify` prints a
  `findings:` line naming each code and count, and `--json` carries `findings_by_code`. Python
  reaches it through the same shared renderer, so parity holds by construction. Codes are declared by
  checks, so the map is bounded by the catalog and never by the dataset — finding *messages* are
  deliberately not carried, being sized by the input. A certificate issued before the field existed
  carries no map, still verifies byte-identically, and the readers print no line rather than an empty
  one: absent means unknown, not none. Documented in
  [docs/trust-chain.md](docs/trust-chain.md).

- **The trust label discloses what the run could not measure.** The label already disclosed the two
  neighbouring cases — a check that *crashed* (`Checks that failed to run`) and a family that ran
  nothing (`Families not run`). The third is the one a clean result hides best: a check that ran,
  measured nothing, and therefore found nothing, which is byte-for-byte what a flawless dataset
  produces. A grade earned over checks that could not measure is a different grade, and a reader of a
  dataset card cannot re-run Veridex to discover the difference.

  Telling an abstention from a fault needs the catalog's judgement, not a name pattern, so each check
  now declares its own (`Check::abstention_codes`, empty by default, also surfaced on `CheckInfo`).
  A test holds the declaration to the catalog in **both** directions: a finding code that reads like
  an abstention and is not declared fails it, and so does a declared code outside the naming
  vocabulary — because that would mean the guard could no longer find its siblings. Proven by
  deleting one declaration and watching it name the omission. `VIDEO.MEDIA_UNREADABLE` is recorded as
  a deliberate exception with its reason: the media was reached and did not parse, which is a defect
  in the file, not a gap in what Veridex looked at.

### Fixed

- **A rig with no transform tree reported its sensors resolved.** Completing the audit the entry
  below started: `autonomy.sensor-frame-resolution` returned early when the dataset carries no
  calibration, deferring to `autonomy.calibration-completeness` — "No tree at all is one defect,
  reported once." That is right about the *report* and wrong about the criteria, which are per check.
  The certificate carried `✗ autonomy.calibration-completeness` beside
  `✓ autonomy.sensor-frame-resolution — every sensor's own frame resolves through the tree to a
  camera`, over a rig with no tree to resolve through. `ready` was already false from the first, so
  nothing was certified that should not have been, but the sentence was untrue about the run it
  described and a reader deciding what to fix was told the spatial wiring was sound.

  The check now abstains out loud (`AUTONOMY.SENSOR_FRAME_UNCHECKED`), which is a statement about the
  check rather than a second accusation about the data — and **only for a rig**. This check's faults
  are per stream and self-limiting, but the abstention fires on the *absence* of a tree, which almost
  nothing outside an autonomy rig carries; ungated it told every LeRobot arm recording and every
  ordinary MCAP file that it declares no transform tree, which is true and not a gap in anything they
  were being measured for. With this, all nine `world-model-ready`
  criteria either measure or abstain: `autonomy.sequence-complete` was the last one audited and needs
  no change, because it falls back to the sensor's own median inter-frame interval when a publisher's
  message numbering is absent, so it measures wherever there are frames to measure.

- **Two readiness criteria were met over a satellite receiver whose data was never decoded.**
  `autonomy.point-cloud-density` already gets this right: a LiDAR whose per-message point counts were
  never read makes it abstain out loud, which refuses its criterion, because "every point-cloud
  sensor actually recorded points" cannot be attested over counts nobody read. The receiver is that
  situation one sensor over, and got the opposite treatment — a `Gnss` stream carrying no decoded
  coordinates and no fix status made both `autonomy.gnss-*` checks skip it and report nothing, so
  both criteria came back **met**.

  The cause is a deferral. The source comment recorded the reasoning: *"Silent on a rig whose GNSS
  was never decoded — `STATISTICAL.UNMEASURED_VALUES` says that."* That disclosure is real, but it
  belongs to `statistical.value-measurability` — a **different check id** — and a readiness criterion
  is judged by its own check's findings. So the silence was disclosed in the report while
  `world-model-ready` counted the check as passed. "X is Y's concern" is not coverage.

  Both checks now raise their own abstention (`AUTONOMY.GNSS_UNMEASURED`,
  `AUTONOMY.GNSS_STATUS_UNREAD`), mirroring the point-cloud pattern, which makes the criteria refuse.
  Neither is an accusation: the absence of a status is not an absence of fixes, and
  `AUTONOMY.GNSS_NO_FIX` is never raised from it. The `world-model-ready` fixture gained a decoded
  receiver for the same reason it already carried point counts — a stand-in for a healthy rig has to
  carry what a healthy rig carries. Both codes are documented in [docs/checks.md](docs/checks.md),
  and the stale source comment now records why the deferral was insufficient.

- **A zero-byte recording was described as a truncated record.** A crashed or killed recorder leaves
  an empty file behind, and it is one of the most common real files a first-time user points Veridex
  at. Every format's parser described it in that format's own terms — MCAP as `ended in the middle of
  a record`, which is true (it ended before the first one) and sends a reader hunting for a
  truncation in a file with no bytes in it. The failure now carries `the file is empty (0 bytes)`,
  which is the treatment an empty *directory* already got; the file was the case that was missed.

- **`VIDEO.MEDIA_UNREADABLE` was declared an abstention and documented as not one, at the same
  time.** The previous release note recorded it as a deliberate exception — the media was reached and
  would not parse, which is a defect in the file rather than a gap in what Veridex looked at — while
  the check declared it in `abstention_codes` regardless. The trust label would then have offered
  "could not measure" for a video container that was read, at error severity. The declaration is
  removed, and the guard gained the direction that would have caught it: an entry in the test's
  exception list may not be declared as an abstention by anything. Without it the list only exempted
  a code from the *other* check, so the code and the reason beside it could disagree in silence.

- **`veridex checks` now marks which findings mean "could not measure".** The listing is the
  discovery surface — where someone learns what the tool asks of their data — and the difference it
  most has to carry is between a finding that says the data is wrong and one that says Veridex could
  not look, since a clean run and an unmeasured one produce the same silence. Abstention codes are
  suffixed `†` with a legend; `--json` carries `abstention_codes` as a field rather than a glyph.

- **The README's report sample named a section it did not show.** The prose promised findings
  "worst episodes first" while the sample omitted the per-episode rollup and the CDM hash line,
  disclosing only the elided informational findings. The note now names everything trimmed.

- **The documented `verify` transcript showed output the tool no longer prints, and nothing held
  it.** [docs/profiles.md](docs/profiles.md) shows a `verify` over a readiness certificate; adding
  the `findings:` line to the renderer left the page showing the output from before it existed. A
  sample that has drifted is worse than none — it reads as a transcript. It is now real output over
  the `av-lossy-camera` demo rig, and a test holds the *set of lines* on the page to the set
  `render_verified` produces, so a line added to the renderer or removed from it fails until the page
  is updated. Held on the labels rather than the values: the hash, timestamp and key belong to one
  run, while the lines belong to the renderer. Proven by deleting the `findings:` line from the page
  and watching it fail.

- **A rig with no satellite receiver certified as `world-model-ready` on two criteria it never
  had the data for.** A readiness criterion is judged by "the check ran and found nothing", and a
  check with nothing to examine finds nothing. `autonomy.gnss-plausibility` and
  `autonomy.gnss-fix-availability` read a GNSS stream; over a rig that carries none they produced no
  findings and the report signed `✓ every satellite fix is a possible place, and the receiver
  actually had one` and `✓ no satellite receiver reporting no fix for more than 5% of its messages`
  for a drive that never had a receiver. Two vacuously true sentences in the document's strongest
  claim.

  Reachable from an ordinary recording, not a contrived one: a LiDAR + IMU + CAN + camera rig with an
  ego trajectory — indoors or on a closed course — is a rig, perceives, and is a world-model
  candidate, and it passed both.

  `world-model-ready` now requires a receiver to apply at all, which is the same answer the profile
  already gives a rig with no ego trajectory, and for the same reason. Georeferencing is not
  incidental to it: those two criteria are bundled precisely because a drive that cannot be placed
  cannot be aligned to a map or to another drive, and a drive with no receiver cannot be placed
  either. Such a rig is now reported `N/A` — not a judgement that it is bad, but a statement that
  this profile has nothing to say about it. Documented in
  [docs/profiles.md](docs/profiles.md).

- **Two finding messages were sized by the file, not by the catalog.** A finding message is not a
  debug print — it reaches the terminal, the JSON report, the SARIF and the HTML — and nothing caps
  how many streams a file declares or how many labels it stamps on one instant. Two checks in the
  semantic family joined the whole list in:

  `SEMANTIC.AMBIGUOUS_STREAM_KEY` reports one finding per member of a colliding group, each naming
  every *other* member, which is quadratic in the bytes. 2,000 streams that normalize to the same key
  produced a 263 KB message apiece and roughly half a gigabyte of report — from a legal file.
  `SEMANTIC.ANNOTATION_CONFLICT` joined every conflicting value at a timestamp, each of them free
  text of any length.

  Both now name four and count the rest, the shape the statistical, structural, temporal and autonomy
  families already use; the full count is still stated, so nothing a reader needs to size the problem
  is lost. Tests hold each message under 400 bytes and the whole report linear in the stream count,
  and both fail when the cap is reverted. Documented per-check in
  [docs/checks.md](docs/checks.md).

- **The nutrition label promoted a filename guess into a signature.** The label's provenance line
  read `3 known · 1 attested · 2 unknown`, printing the `asserted` count under the word *attested*.
  Those are different claims. `Asserted` is usually an adapter being honest about a guess — the MCAP
  reader classes an attachment named `calibration.yaml` as `asserted` because the *filename* contains
  "calib", having read no calibration content and seen no signature. *Attestation* is a producer
  signing for a value with their own key, and the label already prints that in its own `Attested`
  row, naming the elements and the key.

  So the demo dataset's label claimed one attested element with nothing attested and no `Attested`
  row — and the label is the artifact that travels furthest, pasted into public dataset cards, where
  the caveat cannot be recovered from the terminal that produced it. The line now says `1 asserted`,
  matching `inspect`, the report, the certificate JSON and [docs/rubric-v1.md](docs/rubric-v1.md);
  only the dedicated row claims a signature. Two tests hold it: a label with an asserted element and
  no attestation must not contain the word at all, and the end-to-end attestation test now asserts
  the count and the signed row separately, so one can no longer stand in for the other.

### Added

- **Every tolerance now has to reach the two places the compiler cannot check.** Adding a
  configurable threshold touches several places, and most are compiler-enforced — including
  `profile.rs`'s `pick!` list, which builds a `Tolerances` literal and so cannot omit a field
  (verified by deleting one and watching `E0063`; the checklist had it recorded as a silent failure,
  which it is not).

  Two are genuinely silent, and neither had anything watching it. A field missing from
  `effective.rs`'s table is never printed by `--print-config`, so the one command for "what will this
  run actually use" answers incompletely. A field missing from `report.rs::tolerance_departures` is
  not disclosed as a narrowing — and that is the one that matters: loosening it would not raise
  `SCOPE.NARROWED`, so `--min-score` would gate a run that measured the data less hard than the
  catalog asks, which is precisely what the score gate exists to refuse.

  Both lists are complete today; the guard is so they stay that way. Deleting either entry for one
  tolerance fails it, each on its own.

- **A satellite receiver that said it had no fix, and a report that did not.** `NavSatFix` carries
  the receiver's own verdict in `NavSatStatus.status`, and `STATUS_NO_FIX` means the coordinates
  beside it are not a position — whatever the driver last had, or zero. Veridex already declined to
  record those as measurements, which is right. But that is what made the outage vanish: the message
  still produced a frame, on time, so the stream kept its frame count, its cadence and its span, and
  every temporal check read it as continuous. The fixes that did land were plausible, so
  `autonomy.gnss-plausibility` passed on them.

  A rig whose receiver reported no fix for **80% of a drive** produced a report byte-identical to a
  clean one — same findings, same score, same grade — and certified `world-model-ready` on GNSS. The
  frames were all there; four fifths of the trajectory was not.

  New check `autonomy.gnss-fix-availability` → `AUTONOMY.GNSS_NO_FIX`, reading a new CDM field
  (`Stream.observed_fix_availability`, `CANONICAL_VERSION` 17) that counts what the receiver
  disclaimed against what it published. Judged as a *fraction* — `gnss_unfixed_fraction`, default
  0.5, which `world-model-ready` tightens to 0.05 and names in its ninth criterion — because a brief
  outage is a fact about the road, a tunnel or an underpass, and not a fault in the data. What is a
  fault is a trajectory that is mostly absence.

  Recorded in `openspec/` as a ratified requirement rather than only as code: where a sensor states
  its own confidence in a sample, Veridex reports how much of the stream the sensor disclaimed, and
  does not let a disclaimed sample stand as a measurement.

  Demonstrated by new variant `av-no-fix`: the rig with a receiver that loses the sky a fifth of the
  way in while its driver keeps publishing the last position it had. Its report differs from the
  healthy rig's by exactly one finding — a test pins that difference rather than asserting the
  claim, and with the adapter wiring reverted it reports the outage adds nothing at all. The
  threshold is pinned on its boundary in both directions, at the default and at the one the
  certificate's criterion prints.

- **A contributing guide, for the parts of this codebase you cannot infer from reading it.** The
  repository is public and had no `CONTRIBUTING.md`. What a newcomer most needs here is not the
  build command — it is the set of places a change has to reach that the compiler stays silent
  about: registering a check and the two written-out counts that go with it, the six places a
  configurable threshold lives, and the hand-written content-hash encoder, where a new CDM field is
  simply absent from the hash until someone adds it, so two datasets differing only in that field
  collide and one's certificate attests the other.

  It also writes down the testing conventions that are otherwise folklore — prove red before green,
  mutate one claim at a time because an `assert!` stops at the first failure, pin a threshold *on*
  its boundary in both directions, and build the fixture a real recorder would write rather than
  one the reader is already known to accept.

  `crates/veridex-core/tests/contributing_guide.rs` holds the guide to two facts: every Rust
  identifier it names still exists in the workspace, and every repository path it names is still
  there. A guide that sends someone to a symbol that was renamed is worse than one that says
  nothing, because they will go looking — and the first draft did exactly that, naming a hash test
  by a name it had not had for some time. Renaming the symbol and moving a page each fail the guard
  on their own. Where a claim could be written against a number or against the mechanism that
  enforces it, the guide names the mechanism, which does not go stale.

- **The demo dataset now ships the summary a real export ships.** Every LeRobot dataset writes
  `meta/stats.json`; the demo did not. So every run of the fixture the docs tell a reader to try
  first reported `STATISTICAL.NO_STORED_STATS` — an abstention on the one comparison this format
  makes possible — and `statistical.stored-vs-observed`, the check behind the "stale stats silently
  mis-normalize your inputs" claim, had nothing to run on outside the unit tests.

  The demo now writes a real `stats.json` computed from the rows it actually wrote, so it agrees with
  the data by construction. New variant `stale-stats` ships the summary a team exported *before*
  re-recording — a stored `[min, max]` that no longer contains the values beside it, which every
  other check passes over → `STATISTICAL.STATS_STALE`.

  The `nan` variant gets stronger rather than weaker: its summary skips the NaN the way a real
  exporter's `numpy.nanmin` does, so the stored file agrees with itself and is blind, and only the
  recompute finds the bad frame. Previously it shipped no summary at all, which demonstrated less.

- **A demo for the message loss no timeline shows.** `AUTONOMY.SEQUENCE_DROPPED` had no runnable
  example. New variant `av-lossy-camera` is the rig with a camera whose transport dropped one message
  in five: the publisher numbered every one of them, and the survivors keep the times they were
  published at, which is what a dropping transport actually leaves behind. Its report differs from
  the healthy rig's by exactly one finding — a fifth of a camera goes missing while rate, gap,
  jitter, rig sync and the start/end offsets all pass, because none of them can see it. A test pins
  that difference rather than asserting the claim.

### Fixed

- **The README's headline report was a sketch the tool has never printed.** The first code block on
  the front page showed a "Veridex Trust Report" with a layout, a score line and a set of one-line
  family summaries that do not exist in any output path. It was labelled illustrative, so it was not
  a lie — but it was the one block on the page nothing could check, on a tool whose whole argument is
  that a report should say exactly what was measured, and a reader's first impression of the product
  was of a thing that does not run.

  It is now the real output of `veridex check` on the demo the Quickstart builds — the status line,
  the counts, the category rollup, and the clock-skew error with the training risk and the remedy it
  actually carries, with the four informational findings elided and said to be elided. A guard in
  `crates/veridex-cli/tests/cli.rs` holds every line of the block against a live run, and fails on
  the old sketch.

- **The docs gate could not see the modules the code lives in.** CI builds the documentation with
  `-D warnings` so that a broken intra-doc link is caught here rather than shipping to docs.rs as a
  page that never renders. It was building public items only — and most of this crate is
  crate-private: the adapters, the decoders, the check internals. rustdoc resolves links only in
  what it documents, so none of those were checked.

  Nine intra-doc links were broken, and the gate was green over every one. Two named a function
  deleted in the commit before this one, two named `tf_reachable_from` after it became
  `tf_reachable_from_any`, and the rest pointed at items that had moved module. All nine are fixed,
  and the gate now runs `--document-private-items`, which fails on all nine as written.

- **Two spelled-out counts in the docs were held to nothing.** "Seven formats support
  `--metadata-only`" and "the seven checks that answer by comparing episodes against each other" are
  numbers the registry and the catalog own, restated across the README, `docs/partial-runs.md`,
  `docs/checks.md` and a source comment in `checks/structural.rs`. The sensor-rig count in the
  status table is already held to the catalog, because it had gone stale once. These were not.

  Both are correct today; the point is that nothing would say when they stopped being. A count that
  has drifted is worse than no count, because it reads as precision: a reader told seven formats
  support a mode, who finds an eighth adapter that does, cannot tell which sentence is wrong.

  `crates/veridex-core/tests/documented_counts.rs` now holds each restatement to its source — the
  registry's `formats_supporting_metadata_only`, and the count the engine actually prints in
  `STRUCTURAL.UNCOMPARED_EPISODES` on a one-episode run, which is what a reader sees. Each of the
  three restatements was mutated on its own and fails on its own.

  The guard matches against the page with its whitespace collapsed, because these sentences wrap:
  the README states the check count with the numeral ending one line and the noun beginning the
  next, and the first, line-by-line version of this test read that as a claim with no number in it
  and passed. A restatement that merely names the checks without counting them — `docs/checks.md`
  does — is passed over rather than failed.

- **The repository went public with no way to install the tool.** The README documents fifteen
  `veridex <command>` invocations and never said how to get the `veridex` binary; the Quickstart
  runs `cargo run -p veridex-cli --`, which is a development workflow, not an installation.

  Worse, `docs/ci-recipes.md` — which opens by saying every command in it was run before it was
  written down — gave a GitHub Actions and a GitLab recipe whose first step was
  `cargo install veridex-cli`. Neither crate is on crates.io (v0.1.0 is the first release, as
  `docs/releasing.md` says plainly), so that step could not have been run and cannot succeed: a
  reader copying the recipe on a public repo fails at step one with a crate that does not exist.

  The README now has an `## Install` section, and both recipes install the way that works today.
  Both documented paths were run end to end and produce `veridex 0.1.0`:

  ```sh
  cargo install --git https://github.com/clay-good/veridex veridex-cli   # no clone needed
  cargo install --path crates/veridex-cli                                # from a clone
  ```

  A guard in `crates/veridex-cli/tests/cli.rs` now holds every `cargo install` inside a fenced block
  in the README or `docs/` to a form that works — `--git` or `--path` — so an unpublished install
  cannot again be handed to a reader as a working one. Prose may still *name* the future crates.io
  command, which is how the Install section says it does not work yet. `docs/releasing.md`, the page
  about publishing, is exempt. Delete the guard when v0.1.0 ships; it says so.

- **The demo variant lists told a reader to run commands that fail.** Every generator has one
  authoritative list — its `VARIANTS` — and five restatements of it: the module doc's bullets, the
  module doc's `Usage:` line, the `examples/` wrapper's `Usage:` line, the page that offers a reader
  a variant to append, and the line the generator prints when it writes one. Nothing watched them,
  and they had drifted apart:

  - The LeRobot docs named the default variant `broken`. The generator calls it `non-monotonic` and
    *refuses* an unknown name rather than substituting one, so the documented name printed
    `unknown variant \`broken\``. The generator said `broken` too — `describe` opens every other
    variant's line with the name that selects it, and opened that one with a name it would refuse.
    That line is printed on every successful write, which makes it the restatement a reader is most
    likely to copy.
  - The same docs told a reader to run it out of `-p veridex-core`, where these generators have not
    lived since they moved to `veridex-demo`; that command finds no such example target.
  - `near-duplicate` and `stale-stats` are real variants with no bullet and no mention in
    `docs/formats.md`, so the two faults they demonstrate looked unreproducible.
  - The MCAP wrapper's `Usage:` line was five variants short, and the README quickstart's — the
    first command anyone runs — was four short, including every autonomy-rig fault added since.

  All of them are corrected, and `crates/veridex-demo/tests/variant_docs.rs` now holds each
  restatement against `VARIANTS`: every variant is documented, every documented variant exists, both
  `Usage:` lines list the whole set in order and name the crate that actually holds the example, the
  prose offers exactly the real variants, and every printed description opens with the name that
  selects it. Written against the old sources, all five tests fail.

- **The demo rig hashed differently on macOS and Linux.** The content hash is what a certificate
  binds to, and `verify` reports a mismatch as tampering — so a hash that depends on *where* it was
  computed means a certificate issued on one machine fails against byte-identical data on another,
  and fails by accusing the data. The demo rig did exactly that.

  The cause was two floats. Its IMU body carried `phase.sin()` and `phase.cos()` written as raw
  `f64` bytes, and Rust does not promise those are bit-identical across platforms — it defers them
  to the platform's libm. One unit in the last place changed a message's bytes, so its content
  fingerprint, so the hash of the whole recording. They are now a parabolic wave built from
  multiplication, subtraction and `abs`, which IEEE-754 makes exact everywhere. The two `sin` calls
  in the MF4 demo are replaced the same way.

  Nothing in the suite could have caught this: the golden vector pins the *encoding* over a fixed
  JSON fixture, so a difference entering through the data is invisible to it, and every other hash
  test is relative — two datasets hash alike or differently — which holds under any per-machine
  offset. A new test pins the demo rig's hash end to end, generated and read, against a written-down
  value. It also asserts the same recording written twice hashes alike *while the two files differ
  byte for byte*, which is the property worth stating: the hash describes the recording, not the
  container's incidental layout.

  Found because a doc guard tightened one commit earlier compared that hash and CI disagreed with
  the developer's machine.

- **The documented certificate hash was stale, and pinning it was the wrong fix.** The autonomy
  quickstart's `certify` transcript showed a content hash that no longer matched, because the test
  diffing that page against live CLI output skipped that one field. Pinning it instead turned out to
  be wrong twice over: the value goes stale whenever the CDM gains a field *or* a dependency changes
  what the container records about itself, and it is not a claim a reader can check — it is an opaque
  digest. The page now elides it, and the guard compares the verdict and grade exactly while checking
  the hash for shape only (sixteen hex characters).

- **`docs/profiles.md` states the readiness criteria twice and nothing checked either.** The page
  lists them as a table and again inside a sample `verify` transcript; both are hand-maintained, and
  both went stale by hand this week. A new test compares them against
  `world_model_ready().criteria` — the table by set identity in both directions, so neither an added
  nor a removed criterion slips past, and the transcript by identity *and* text, since its lines
  quote each threshold verbatim. Both directions were proven to fail before they passed.

- **Two storage plugins agreed about a topic neither had read the same way.** The test that gates
  the "which plugin a team picked does not change what Veridex sees" claim replays the `.db3`
  fixture's topics into an MCAP and compares the two CDMs. Its replay wrote a header-first stub for
  `tf2_msgs/msg/TFMessage` — a message that is *not* header-first, its body being a sequence of
  `TransformStamped` — so the MCAP side decoded a capture stamp and a coordinate frame the real bag
  does not have. The comparison passed because it compared neither field.

  The replay now writes a real `TFMessage`, mirroring the fixture generator, and every other message
  carries a real `header.stamp` taken from its own log time (the `.db3` generator has always written
  one, so the replay's zeros were a divergence in themselves). The comparison now covers each
  stream's capture-stamp summary, which is what caught this.

- **A transform tree was reported as a measurement nobody read.** A `CameraInfo` channel and a
  `tf2_msgs/msg/TFMessage` were both typed `ScalarState` — the joint-position modality — so
  `STATISTICAL.UNMEASURED_VALUES` named them among the streams that might hold "a saturated
  actuator, a NaN, a stuck sensor, or a unit error". Neither has values to summarize: one announces
  a camera's calibration and the other the rig's geometry. The effect was not just a wrong word — it
  padded the finding from three streams to five, burying the ones that genuinely went unread.

  New `Modality::Calibration` says what these channels are, `inspect` now prints it, and
  `statistical.value-measurability` excludes them. The content is not dropped: both are decoded into
  the CDM's calibration and graded by the `autonomy.calibration-*` rules — including the all-zero
  `CameraInfo` that a value summary would be the wrong tool for — which an end-to-end test pins
  alongside the exclusion, so the skip cannot become a blind spot.

  `CameraInfo` was typed `ScalarState` in the first place to stop it being read as a `Video` sensor,
  which had failed a synchronized rig at `AUTONOMY.RIG_SYNC`. That fix traded one wrong
  classification for another; this is the classification both cases wanted.

### Added

- **The demo rig had a camera nothing could project into.** The flagship `av` fixture published
  images and a transform tree and no `CameraInfo` at all, so every variant of it — the healthy one
  included — reported `AUTONOMY.CALIBRATION_INCOMPLETE`, the `world-model-ready` demo could never
  show a calibrated rig, and the intrinsics decode (the matrix, the declared resolution, the
  distortion model) never ran on the fixture the docs tell a reader to try. The rig now publishes a
  real `sensor_msgs/msg/CameraInfo` beside its images, the way a driver does: 1280x720, an ordinary
  pinhole, `plumb_bob` with its five coefficients.

  New variant `av-uncalibrated-camera` is the same rig publishing what a driver emits before anyone
  calibrates it — the model named, the coefficients present, every number zero — which satisfies
  every presence check and divides by a focal length of zero → `AUTONOMY.CALIBRATION_IMPLAUSIBLE`.

  Found by running the CLI on the demo and reading what it said. The quickstart's pinned output is
  regenerated: the rig now fails on one criterion, `rig-sync`, which is the fault that demo is *for*.

- **A dropped message the timeline could not show.** `AUTONOMY.SEQUENCE_COMPLETE` estimated a rig
  sensor's lost frames from its median cadence — which needs a cadence to exist (it abstains on
  anything event-driven), needs eight frames to establish one, and cannot see the one shape that
  matters most: losses scattered one message at a time leave intervals near twice the cadence, and so
  does ordinary jitter. Meanwhile every MCAP message carries a `sequence` its publisher set, and the
  adapter recorded that field as *unmapped*. A hole in it is direct evidence that a message was
  published and never reached the file.

  `autonomy.sequence-complete` now counts where it used to estimate. New
  `AUTONOMY.SEQUENCE_DROPPED` reports the loss from the publisher's own numbering and takes the
  stream whenever that numbering is present — an inference must never be allowed to contradict a
  count — while the cadence estimate stays for every source that carries no counter. New
  `AUTONOMY.SEQUENCE_RENUMBERED` reports a counter that restarted or repeated, where a hole is the
  distance between two unrelated counts rather than a loss, and leaves completeness explicitly
  unverified instead of clean. A publisher that never used the field (every message left at 0)
  summarizes to nothing: an unused counter is not a stream whose every message went missing.

  Needed a new `Stream.observed_sequence` CDM field and so `CANONICAL_VERSION` 15 → 16, with the
  golden vector re-pinned. Read from MCAP and from rosbag2's MCAP storage plugin; a `.db3` message
  table has no such column, so the same bag recorded the other way falls back to the estimate.

  Fixing the check also fixed its fixture: the end-to-end drop test built its "dropping" sensor by
  deleting timestamps and renumbering the rest 0..n, which describes a publisher that never sent
  those messages — the one shape no dropping transport produces. It now leaves the holes.

- **A bag has two clocks, and only one of them was ever read.** Every frame timestamp Veridex takes
  from an MCAP or rosbag2 recording is the *recorder's* clock — the instant a message arrived at the
  bag. The sensor states when it actually **sampled**, in its message's own `header.stamp`, and that
  field was parsed and thrown away by the CDR decoder. So the rig-wide sync result, the clock-skew
  check and the start/end offsets were all measurements of the recording host's scheduler, and a rig
  whose sensors disagree about what time it is passed every one of them.

  New check `autonomy.sensor-clock` (the catalog's 45th, and the `world-model-ready` profile's
  8th criterion) reads both clocks and reports three faults, none of which moves a frame timestamp:
  `AUTONOMY.SENSOR_CLOCK_UNSET` (a driver that never stamped — error when no message was stamped,
  warning when it stopped partway), `AUTONOMY.SENSOR_CLOCK_REGRESSION` (a sensor clock stepped
  backwards under a recorder's that did not), and `AUTONOMY.SENSOR_CLOCK_OFFSET` (the two clocks
  disagree by more than `sensor_clock_offset_ms`, default 1 s, on *every* message — judged from the
  closest the two came all recording, so a buffering spike cannot raise it and a constant pipeline
  latency of tens of milliseconds does not either). A format that records one clock per file rather
  than one stamp per sample abstains out loud as `AUTONOMY.SENSOR_CLOCK_UNREAD`.

  Carrying the summary needed a new `Stream.observed_header_stamps` CDM field and so
  `CANONICAL_VERSION` 14 → 15, with the golden vector re-pinned. The demo rig recorder was itself
  writing `header.stamp` 0 on every message — found by the new check — and now stamps each message a
  5 ms pipeline latency ahead of the log time it is written at, from a real 2026 recording epoch
  rather than from zero. New demo variant `av-unstamped` is the same rig with a LiDAR driver that
  never stamped: full clouds, on time, in the right frame, and every other check still passing.

- **A bag's IMU was a stub, so the decoder written for it never ran on one.** The rosbag2 fixture
  generator wrote `/imu/data` through a helper documented as "any header-first message whose body
  Veridex does not decode (Image, **Imu**, …)" — true when it was written, and false since the `Imu`
  decoder landed. So the bag path's Imu decode was reachable, unexercised, and would have gone on
  being both: a fixture named `clean_rig` reported its IMU under `STATISTICAL.UNMEASURED_VALUES`,
  and the statistical family abstained on the one sensor in that recording whose values it can grade.

  The generator now writes a real `sensor_msgs/msg/Imu` body — orientation, angular velocity and
  linear acceleration, each with its covariance — and the fixtures are regenerated. The IMU carries
  measured values per dimension, `linear_acceleration.z` sits at 1 g, and a test pins both. The
  pinned golden payload hashes are over LiDAR and overflow-page messages and are unchanged. Found by
  running the CLI on the bag fixture the docs tell a reader to try.

- **A point record layout that does not fit the stride it declares.** The last unread field in the
  CDR decoder: `PointField`'s `offset` and `count` were parsed and discarded, so a cloud whose fields
  run past `point_step` or overlap each other — what a hand-rolled publisher writes when it adds a
  field and forgets to widen the stride — was read as a well-formed record. Every consumer then gets
  garbage for one field and correct values for the rest.

  The point-count decode now checks the layout alongside the length invariants it already checked:
  every field's `offset + width × count` inside the point, no two fields sharing a byte (padding
  between them is normal, overlap is not), a non-zero element count, and no datatype tag the message
  definition does not define. A cloud that fails any of them yields no count, so the stream reports
  `AUTONOMY.POINT_CLOUD_UNMEASURED` — the density question could not be asked — rather than a number
  computed over a record nothing can read.

- **`STATISTICAL.UNMEASURED_VALUES` told an RLDS user to re-check their data in RLDS.** Running the
  CLI over the RLDS demo, its `language_instruction` and `observation/image` streams were named as
  carrying no statistics, beside a remedy reading "check them in a format whose values Veridex reads
  (LeRobot, **RLDS/TFDS**, HDF5, …)" — the format already in use. The finding was collapsing two
  silences that lead a reader to opposite actions.

  A stream this source did not hand over values for is *unmeasured*: read it from one that does. A
  text feature has no minimum, maximum, mean or standard deviation in this format or any other —
  that is **unmeasurable**, and no re-run changes it. Now `STATISTICAL.UNMEASURABLE_VALUES` (info),
  judged from the dtype the source declares and never guessed from a name, with a remedy that says
  there is nothing to do.

  **Imagery is deliberately not in it**, and finding out why took journeying two more formats: an
  HDF5 or Zarr image feature is a plain `uint8` array Veridex reads and summarizes per dimension,
  while an RLDS `bytes_list` leaf or a bag's camera topic is an encoded frame it fingerprints. So
  whether a picture's values are measurable is a property of how the source stores them, not of the
  modality — calling it unmeasurable would have told a Zarr user that a stream Veridex had already
  measured could never be measured. It stays with `UNMEASURED_VALUES`, whose remedy no longer
  enumerates formats (a list that goes stale, and that listed RLDS to an RLDS user) but names the
  property that decides it: read the values from a source that stores them rather than an encoded
  payload.

- **The sweep finished clean on the last two families, and the rule it taught is written down.**
  `semantic`'s three boundary comparisons — the episode-span construction and both ends of the
  annotation-alignment window — were all killed, so that family was already pinned; `video`'s one
  threshold is the shared `rate_deviation`, a float ratio nothing lands on exactly. Nothing to fix,
  which is worth recording as much as a fix is.

  What the sweep taught is now a convention in [`openspec/project.md`](openspec/project.md): a
  threshold a document promises is pinned **at** its boundary, in both directions, because a test
  that exercises it merely *near* the limit passes just as well with the comparison moved by one.
  That covers the certificate's criterion wording, inclusive physical bounds, and format conventions
  — and says explicitly when not to pin, so the next threshold added does not have to rediscover it.

- **Four more structural boundaries pinned, and one survivor found harmless.** Extending the sweep
  to the ten comparisons that back documented structural behavior, eight survived. Triaged rather
  than pinned wholesale — a surviving mutation is not automatically a gap:

  - **`STRUCTURAL.EPISODE_BOUNDARY`** reports an *inverted* boundary, `start > end`. A single-frame
    episode has `start == end`, which is short, not backwards — the mutation would have called every
    one-frame episode a corrupt boundary.
  - **`STRUCTURAL.STEP_COUNT_MISMATCH`** allows one row of difference, because a dataset storing one
    more observation than actions is following the terminal-observation convention. The mutation
    would have flagged that convention on every dataset that uses it.
  - **`STRUCTURAL.FROZEN_EPISODE`**'s minority rule — "a stream frozen in half its episodes or more
    is the dataset's shape, not a fault in it" — and its three-episode floor. Both halves of that
    sentence were unpinned.

  And `structural.stream-presence`'s `present_in.len() >= total` survived because a later
  `missing.is_empty()` guard makes it inert: the mutation changes which branch runs and nothing a
  reader sees. Left alone rather than pinned with a test that asserts nothing.

- **Both saturation knobs were unpinned too, completing the threshold sweep.** Aimed at the four
  remaining comparisons that read a knob from `veridex.toml`, the sweep killed two —
  `statistical.outlier_z` and `structural.near_duplicate_fraction` were already pinned — and left the
  two saturation gates alive. A stream sitting exactly on the `saturation_fraction` a user
  configured, or carrying exactly the `saturation_min_samples` they set, could have been silently
  dropped from the check they turned it on for. Both land exactly: one is a count of whole samples,
  the other a ratio of two of them. Now pinned on both sides, each proven against its mutation.

  That closes the sweep across the catalog's configurable thresholds: every knob a user can set in
  `veridex.toml` now has a test that fails if its comparison moves by one, except the jitter CV and
  the rate deviation, which are ratios of measured floats that no real recording lands on exactly.

- **Every user-configurable temporal threshold was unpinned at its boundary — including the headline
  one.** The same sweep, aimed at the five comparisons that read a knob from `veridex.toml`, found
  all five mutations alive: `clock_skew_ns`, `gap_factor`, `jitter_cv`, `rate_deviation` and
  `episode_duration_factor` could each have been silently tightened to fail data that sits exactly on
  the limit a user configured.

  Three are reachable exactly and are now pinned on both sides: a clock drift of exactly the
  allowance (the tolerance widened by the larger sampling period), an interval of exactly
  `expected × gap_factor`, and an episode exactly `factor` times shorter or longer than the median.
  Each test was proven against the mutation that survived without it. The other two — the jitter CV
  and the rate deviation — are ratios of measured floats whose exact equality no realistic recording
  produces, so the mutation there changes nothing a dataset can reach.

- **A mutation sweep over the autonomy checks: four documented thresholds were unpinned.** Flipping
  every comparison in `checks/autonomy.rs` to its neighbour — `>` to `>=`, `<` to `<=`, `==` to `!=`
  — and running the suite against each, 26 of 56 mutations survived. Most are inert (a guard, a
  plural, a modality test whose branch does not change), but four sit on thresholds the
  `world-model-ready` certificate names to a reader, and each could have been silently tightened to
  fail a rig that meets the criterion it attests:

  - **"within a 20 ms cross-sensor span drift"** — a spread of exactly the allowance is *within* it.
  - **"no rig sensor dropping more than 5% of its frames"** — exactly 5% is not more than 5%, and the
    fraction is a ratio of whole frames, so it lands there exactly.
  - **"no step above 100 m/s implied speed"** — 10 m in 100 ms is exactly 100 m/s, exact in binary
    floating point too.
  - **latitude ±90°, longitude ±180°** — the poles and the antimeridian are places a receiver
    reports, and the bound is inclusive.

  Each is now pinned on both sides, at the boundary rather than near it, and each test was proven
  against the mutation that survived without it.

- **The README's sensor-rig row was four rules behind the catalog.** It enumerated finding codes —
  `RIG_SYNC`, `SEQUENCE_COMPLETE`, `EGO_POSE_CONTINUITY`, `CALIBRATION_INCOMPLETE`, the two
  `SENSOR_FRAME_*` — and an enumeration goes stale the moment the list grows. It named neither the
  GNSS checks nor anything added since: the point-cloud density family, the ego body frame, and the
  three new ways calibration can be present and arithmetically unusable. The existing doc guard
  catches a page naming a *dead* code and says nothing about a page omitting a live one.

  The row is now a summary that says it is one, organized by what the family catches (time, space,
  calibration, data) and pointing at the catalog for the list — so there is no enumeration left to
  rot. The one number it commits to, how many checks the family has, is asserted against the live
  catalog.

- **A metadata-only MF4 run said what it skipped, not how much.** `declare_channel_group` took the
  `##CG` cycle count and threw it away (`let _ = cycle_count;`) under a doc comment claiming the run
  reports "how many samples each declares". So `inspect --metadata-only` showed three streams and
  zero frames, correctly noting that no data block was opened — and nothing said whether the
  measurement behind them was four samples or four million.

  The omission note is now quantified: "…for the 400 record(s) the ##CG headers declare — one sample
  of every channel in the group each, so a full read yields that many frames per stream". Pinned
  against a full read of the same file, because the number is only worth printing if it is the
  number.

- **Two CAN buses in one log were decoded as one.** `parse_candump_line` read the interface column
  into a `_`-prefixed binding and dropped it. A vehicle has several CAN buses and `candump -l can0
  can1` writes them all to one file — and a CAN id is per-bus: `0x100` is one message on the
  powertrain bus and something else entirely on the chassis bus. Decoded by id alone, both buses'
  frames landed in the same signal stream, whose values and statistics blend two unrelated physical
  quantities into a summary of nothing, indistinguishable in the report from a clean single-bus read.

  Streams are now named `<interface>:<Message>.<Signal>` when a log carries more than one interface,
  so the buses stay apart; a single-bus log — nearly every log — keeps the names it has always had,
  so no existing CDM hash moves. Which bus the `.dbc` describes is not something the log or the
  database says, so that is disclosed rather than guessed: every frame from a bus the database does
  not describe was decoded from the wrong definitions.

  The disclosure is a **fidelity** note, not an unread source. Filing it under `unread_sources` —
  which means "there is data here and I did not look at it" — raised `COVERAGE.SOURCE_UNREAD`, whose
  text tells a reader every result "speaks for the part that was" read, beside a remedy about
  re-exporting missing files. Every frame on both buses was read; what the CDM cannot represent is
  which bus the database is for. Caught by running the CLI on a two-bus log rather than by reading
  the code.

- **The README's picture of the engine named three of seven check families.** The flowchart read
  "structural · temporal · provenance checks" and the sequence diagram said the same, so the front
  page understated the catalog by four families — including `autonomy`, which is most of what a rig
  log is checked for. Prose that summarizes a list drifts the moment the list grows, and nothing was
  watching: the existing doc guards cover `docs/checks.md` and not the page most readers see first.
  Both diagrams now name every family, and a test asserts the flowchart node against the live
  catalog's categories.

- **An ego trajectory recorded for a frame the transform tree never names.** `decode_odometry_pose`
  read `child_frame_id` into a `_`-prefixed binding and dropped it — the last discarded field in the
  CDR decoder. That frame is the vehicle body the trajectory is *of*, and it is a different question
  from the one the per-stream frame rules ask, and the one they deliberately do not: an ego-pose
  stream's own `frame_id` is the *reference* frame (`odom`, `map`), joined to the body dynamically
  rather than by the static tree, which is why `is_spatial_sensor` excludes it. The body frame is
  the static question. Every sensor's extrinsics hang off it, so a rig publishing odometry for
  `base_footprint` while its tree roots at `base_link` cannot place a single observation along the
  drive — and the tree is well-formed, every sensor resolves through it, and the trajectory itself
  is continuous, so nothing reported it.

  Now carried as `Episode::ego_frame` and reported as `AUTONOMY.EGO_FRAME_UNKNOWN` (error), once per
  episode. Silent where the source names no body frame: a trajectory that does not say what it is of
  is not a trajectory of the wrong thing. `CANONICAL_VERSION` goes to **14** — two logs differing
  only in which frame the odometry claims are a passing dataset and a failing one — with the golden
  vector re-pinned and the fixture reaching the new arm with a real value.

- **`autonomy.point-cloud-density` was silent where it never measured.** The check concludes from
  `Stream::observed_point_counts`, and a stream that carries none — a format that states no
  per-message point count — hit the `let Some(...) else { continue }` and produced nothing. A
  point-cloud sensor nobody asked the question about was then indistinguishable in the report from
  one that was asked and came back clean, which is the whole value of the result. Now
  `AUTONOMY.POINT_CLOUD_UNMEASURED` (info), reported once for the dataset and naming the streams.

  **Withheld under `--metadata-only`**, through `run_in`. A run that did not open the message bodies
  read no counts for any stream of any format, so the finding would blame the data for a silence the
  *request* caused, beside a remedy pointing the reader at a format that reads more.
  `COVERAGE.METADATA_ONLY` already states the run's own shape. The text names the **property** the
  count is read from rather than listing the formats that supply it, because such a list goes stale
  the moment an adapter reads more.

  The abstention reaches the **readiness criterion**, and that is the point of it. `world-model-ready`
  attests that "every point-cloud sensor actually recorded points", which over counts nobody read is
  not a claim anyone can make — and without the finding the criterion counted zero findings, reported
  green, and signed a rig whose LiDAR was never measured as ready to build a world model from. It now
  refuses. The `healthy_rig` profile fixture carries the counts a healthy rig read in full carries,
  rather than passing the criterion by never having been measured.

- **The two rosbag2 storage plugins were only proven to agree about names and timestamps.**
  `which_storage_plugin_recorded_a_bag_does_not_change_what_veridex_sees` compared stream names,
  modalities and frame times — the shape of the recording. But most of what Veridex sees on a rig is
  decoded out of the message *bodies*: the point layout and point counts, each sensor's coordinate
  frame, the transform tree, the intrinsics, the ego trajectory. Those go through two separate
  readers, one per plugin, and a field wired into one and missed in the other passed the comparison
  unchanged. Worse, the fixture wrote a **one-byte payload**, so neither path decoded anything and
  the agreement was between two readers that had both read nothing.

  The fixture now writes real CDR bodies — every ROS message header-first so each yields a
  `frame_id`, and a whole `PointCloud2` so the layout and per-message point count decode too — and
  the comparison is a **content hash** of the whole CDM. Exactly three things are normalized away,
  because exactly three are properties of the container rather than of what was recorded: the
  dataset id (a bag is a directory, a bare recording is a file), `metadata.yaml`'s keys, and the
  names of the log clock and the recorder. Disabling the point-count wiring in either reader now
  fails the test.

  Also asserted directly on the bag path: `the_av_message_headers_populate_the_rig_cdm` now pins the
  LiDAR's decoded point counts, which had been wired into two rosbag2 call sites with no test on
  either.

- **The demo rig's LiDAR now writes real point clouds, and a variant where it writes empty ones.**
  `make_demo_mcap`'s `PointCloud2` was the last stub body in the flagship rig — a header and a
  varying `u64` — so the demo carried no declared point layout and no point counts, and
  `autonomy.point-cloud-density` abstained on the one sensor a world model is mostly built from. It
  now writes a whole cloud, tail included: fields, `is_bigendian`, `point_step`, `row_step` and the
  `data` blob, because the count decode believes a body only when those invariants hold. The healthy
  rig's findings are unchanged, which is the point.

  New variant **`av-dead-lidar`**: the same rig with a LiDAR whose driver lost its sensor. Every
  cloud is well-formed, on time, in the right coordinate frame, declaring the same four fields — and
  holds zero points. It adds exactly one finding, `AUTONOMY.POINT_CLOUD_EMPTY`; the structural and
  temporal families pass on it unchanged, because the empty messages keep the stream's schema, rate
  and continuity intact. Walked through in the [autonomy quickstart](docs/autonomy-quickstart.md),
  whose printed output is pinned against a live run so the page cannot rot.

- **A LiDAR that recorded no points.** New check `autonomy.point-cloud-density` (#44), emitting
  `AUTONOMY.POINT_CLOUD_EMPTY` (error) when every `PointCloud2` message on a stream carried zero
  points, and `AUTONOMY.POINT_CLOUD_DROPPED` (warning) when only some did.

  This is the autonomy fault every other check passes. A driver that lost its sensor keeps
  publishing perfectly-formed clouds at the configured rate: the right schema, the right `frame_id`,
  monotonic timestamps, no jitter — and `width` of zero. The structural family sees frames, the
  temporal family sees a clean 10 Hz, `autonomy.sensor-frame-resolution` places the sensor in the
  tree, and the rig certified as world-model-ready on a LiDAR that recorded nothing. Proven
  end-to-end through the real MCAP adapter and the real engine, because the whole claim is that
  everything *else* passes on it — a unit test on a hand-built CDM cannot show that.

  A count is believed only when the body proves it is a `PointCloud2`. The count is the first two
  `uint32`s after the header, so a decode that read only those would believe whatever bytes happen to
  sit there — and a channel's declared schema is not proof of its bodies. A recorder that stubs the
  payload, a mislabelled topic, a truncated write: each presents as a `PointCloud2` channel, and a
  fabricated count reaches the report as a finding about honest data, which is worse than the silence
  it replaces. So the decode walks the field list to the `point_step`/`row_step`/`data` values behind
  it and returns a count only when the message's own invariants hold — at least one declared field, a
  non-zero point stride, a `row_step` covering a row of `width` points, `data` of exactly `row_step ×
  height` bytes, and those bytes actually present. An empty cloud satisfies every one of them, which
  is the case the check exists for; a buffer of zeroes does not, which is what the field and stride
  rules are for. The field count is capped at 64 before the walk, because it is a `uint32` out of the
  file and the walk is a per-message cost.

  The count comes from each message's own `height × width`, which a `PointCloud2` states in its
  header ahead of the bulk blob, so no point payload is decoded; it is read per message rather than
  once per stream, because the layout is a property of the stream and the first message settles it
  while whether a sweep held points is a property of each message. Summarized as a running
  `PointCounts` (message count, min, max, empty) rather than a list, since the number of messages on
  a topic is chosen by the file. Silent where no counts were read at all — every non-ROS format, and
  a `--metadata-only` run — because a stream whose density was never measured is not a stream
  measured and found empty. Added as a seventh `world-model-ready` criterion, without which a rig
  failing it would still certify as ready.

  `CANONICAL_VERSION` goes to **13**: a LiDAR that published ten thousand empty sweeps and one that
  published ten thousand full ones are otherwise identical in the hash, and the clean one's
  certificate would verify the broken one. Re-pinning the golden vector also turned up that the
  fixture had **no point-cloud stream at all**, so the `point_fields` arm was itself only ever
  reached as the absent marker; the fixture now carries a real cloud stream and both arms are
  asserted.

- **A distortion coefficient list that does not fit its own model.** The same `CameraInfo` decoder
  also read `distortion_model` into a `_`-prefixed binding and dropped it, which left the
  coefficients recorded verbatim with nothing saying how many of them there should be. `plumb_bob`
  takes 5, `rational_polynomial` 8, the fisheye models 4 — so five coefficients still declared under
  `rational_polynomial`, which is what a calibration copied between two models leaves behind, is
  finite, positively-focal, inside its own image, and cannot be undistorted, because three of the
  eight terms were never written. Now carried on `CameraIntrinsics` and reported as
  `AUTONOMY.CALIBRATION_IMPLAUSIBLE`.

  The coefficients themselves are still never interpreted — their meaning is model-specific and
  reading it would be a guess. Only the count, which is not. And two silences are deliberate: the
  model namespace is **open**, so a name Veridex has not heard of abstains rather than reading "I
  have not heard of this" as "these disagree" (the rule `canonical_codec` already follows for
  codecs), and an **empty** `d` is what `CameraInfo` specifies for a camera with no distortion, so a
  rectified stream that names a model and writes no coefficients is not accused.

  `CANONICAL_VERSION` goes to **12**, for the same reason it went to 11: the same five coefficients
  are a complete calibration under one model and a truncated one under another, so the hash binds
  the model. Golden vector re-pinned here, with the fixture reaching the new arm with a real value.

- **A `CameraInfo`'s image dimensions were read and thrown away.** `decode_camera_info` parsed
  `height` and `width` — they sit in the message immediately before the intrinsic matrix — into two
  `_`-prefixed bindings and dropped them, and `AUTONOMY.CALIBRATION_IMPLAUSIBLE`'s own documentation
  then said a principal point could not be judged because it "would need the image dimensions the
  CDM does not carry." The source said them; nothing carried them.

  They are now on `CameraIntrinsics` (`width`, `height`, `None` when the source is silent — a driver
  publishing `0` is stating nothing, not a one-pixel camera), which makes `cx`/`cy` checkable as the
  pixel coordinates they are. A principal point at or past the image edge is reported: intrinsics
  calibrated at 1920×1080 and applied to a stream recorded at 640×480 put the optical centre off the
  right-hand edge, and every undistortion silently rectifies about a point outside the sensor. So
  does a matrix copied out by hand with `cx`/`cy` transposed into `fx`/`fy`. Judged only where the
  source declares the dimension — a format that carries no image size (MF4, HDF5) is not measured
  against an assumed one — and the boundary is the last valid pixel, pinned in both directions so an
  off-centre principal point stays legitimate and a one-pixel overshoot is still caught.

  `CANONICAL_VERSION` goes to **11**: the dimensions change what a check concludes about the same
  `cx`/`cy`, so the content hash has to bind them, and the golden vector is re-pinned in this commit
  with a fixture that reaches the new arm with a real value rather than the absent marker.

- **A rotation quaternion that is not a unit quaternion is not a rotation.**
  `AUTONOMY.CALIBRATION_IMPLAUSIBLE` judged a transform's rotation by two tests — is it finite, and
  is it the all-zero uninitialized value — which leaves everything between them passing. A
  quaternion is a rotation only when its norm is 1, and the standard quaternion-to-matrix conversion
  does not renormalize: norm `n` composes a uniform scale of `n²` into the transform. So a 90° yaw
  written as `[0.707, 0, 0, 0]` — the `w` component dropped, which is what a producer that
  serializes only the vector part leaves behind — is finite, is nowhere near zero, satisfies every
  presence check, and places every LiDAR point at **half** its real distance from the rig. The fused
  scene is quietly wrong rather than visibly broken, and no downstream tool reports it.

  Now flagged when the norm is more than **1%** from 1, with the finding naming both the norm and
  the scale it implies so a reader can recognize which producer bug it is and how far the placement
  is off. The tolerance sits in the wide gap between the honest cases and the defects, not next to
  either: honest producers miss unit by parts in ten thousand (a quaternion serialized at three
  decimal places is within 2e-4; an unnormalized least-squares fit within a few parts in a
  thousand), while the real defect classes miss by tens of percent. The boundary is pinned in both
  directions and on both sides of 1 — a fixed-point quaternion read without its scale factor
  overshoots exactly as a truncated one undershoots — so the rule cannot be one-tailed and the
  threshold cannot be moved without a test failing.

  The unusable-calibration report is now **bounded** to eight named elements plus a sentence
  counting the rest. These defects are the systematic kind — a producer that drops the `w` component
  drops it on every edge — and nothing caps how many transforms a file may declare, so one finding
  per bad element was a report size the *input file* chose, in the terminal, the JSON, the SARIF and
  the signed certificate alike. The overflow is counted rather than dropped: a bound that stops a
  check mid-judgement has to reach the verdict, or a reader cannot tell a capped report from a
  complete one.

- **A rig with fewer `CameraInfo`s than cameras** now reports `AUTONOMY.CALIBRATION_INCOMPLETE`.
  The rule asked only whether the intrinsics list was *empty*, so a six-camera surround rig that
  published a single `CameraInfo` — one driver configured, five not — satisfied it, and the
  `world-model-ready` calibration criterion reported green over five cameras nothing can project
  into. The shortfall is **counted, never name-matched**: a `CameraInfo` names its own topic
  (`/camera_front/camera_info`), never the image stream it calibrates, so pairing them would mean
  guessing at the ROS namespace convention and accusing whichever camera the guess missed.
  Arithmetic cannot make that mistake. Cameras are counted by distinct **coordinate frame** rather
  than by topic, so a bag republishing one camera as `image_raw` plus `compressed` stays one camera;
  when any camera declares no frame the count would be a guess and the rule abstains, that stream
  being already reported by `AUTONOMY.SENSOR_FRAME_UNDECLARED`.

- **The duplicate-episode signature is a digest, not the 65 KB of text it summarized.**
  `DuplicateEpisode::signature` rendered an episode's whole content into a `String` — two hex
  characters per byte of every frame's content hash, formatted one byte at a time — and both callers
  used it only for equality, as a map key and pairwise. Every signature is held at once, so a
  2,000-episode dataset built and retained about **130 MB** of strings to answer a question that
  needs 32 bytes each; a 20,000-episode one would hold over a gigabyte, on a frame count the input
  file chooses. Now a SHA-256 over the same content: `structural.duplicate-episode` goes from
  **3.1 s to 0.26 s** and `near-duplicate-episode`, which shares the signature, from **5.3 s to
  0.86 s** (release, 2,000 episodes × 800 frames).

  Every field is fed with an explicit tag and length, because a digest is only as trustworthy as the
  boundaries between the fields in it — run one field into the next and two genuinely different
  episodes hash alike, which reports them as duplicates of each other on data that is fine. A unit
  test walks pairs that differ only across a boundary (`ab`+`c` against `a`+`bc`, `k`=`xy` against
  `kx`=`y`, shape `[1,2]` against `[12]`, an absent dtype against an empty one) and fails if any two
  collide; removing the length prefixes makes it fail.

- **`autonomy.calibration-completeness` re-read its transform tree once per episode.** The same
  shape, one check over: the tree is dataset-level and so is everything derived from it, but
  `break_is_localizable` rebuilt the set of every frame the tree names and `tf_component_count`
  rebuilt its whole adjacency and walked it, inside the per-episode loop — for an answer identical on
  every episode. Both counts come from the input file, so a 2,000-episode drive log with a
  20,000-frame tree paid their product: **203 seconds, now 0.66**. The component count is computed
  at most once, and only on the branch that needs it.

- **`autonomy.sensor-frame-resolution` was quadratic three ways over counts a bag chooses.** Nothing
  caps how many channels a recording may declare, so the number of cameras and the number of spatial
  sensors on one episode are both file-controlled, and a log of 5,000 image topics beside 5,000 LiDAR
  topics is legal. The check tested each sensor's frame against a `Vec` of camera frames (their
  product), built the camera-reachable set by walking the whole transform tree once per camera, and —
  the dominant cost — called `tf_reachable_from` once per spatial sensor, rebuilding the tree's
  entire adjacency map and traversing its component each time, only to ask whether the result was
  empty. That last question is membership in the set of frames the tree names, and needs no walk at
  all. A set for the camera frames, one multi-source walk instead of one per camera, and a
  precomputed frame set take that rig from **147 seconds to 0.11**. The camera list in
  `AUTONOMY.SENSOR_FRAME_UNRELATED` is now sorted, deduplicated and bounded like the other frame
  lists, so one finding cannot carry thousands of names into the signed certificate.

- **`veridex diff` no longer takes minutes on a large report.** Its three partitions — introduced,
  resolved, unchanged — each scanned the other report's whole finding list per finding, so the work
  grew as the *product* of two counts that come from the files the caller hands in, with each
  comparison a deep equality over a multi-field JSON object carrying long message, risk and remedy
  strings. Two 20,000-finding reports (an ordinary size for a large dataset reported per episode) is
  4e8 of those: a diff that never returns rather than one that says what changed. Now set
  membership keyed on the finding's own JSON text, which partitions **identically** rather than
  approximately, because `serde_json::Map` is a `BTreeMap` here and that text is canonical. The same
  case now finishes in about two seconds; a test pins both the timing and the exact partition, and a
  second test pins the canonicalization the key depends on.

- **A broken rig calibration is now one finding, not one per episode.** Both rules that judge the
  calibration read `dataset.calibration` — a dataset-level document — yet were emitted inside
  `autonomy.calibration-completeness`'s per-episode loop, so a 200-episode drive log reported
  `AUTONOMY.CALIBRATION_IMPLAUSIBLE` two hundred times, buried the one actionable line, and inflated
  every rollup that counts findings by episode. `autonomy.sensor-frame-resolution` already claims
  each stream once for exactly this reason. Both codes now report once, at **dataset** scope; the
  presence rules beside them still report per episode, because "does this episode carry spatial
  sensors" genuinely is a per-episode question.

- **An attested element is no longer also reported partial.** Signing for an element is a claim
  about the whole dataset — that is what makes the trust score count it as covered — so it settles
  the scope question the way a dataset-scoped record does. Without this, a dataset whose lineage was
  recorded for one episode *and* attested by its producer had the same report say both, printing a
  remedy the reader had already followed: the exact contradiction `PROVENANCE.MISSING_*` was fixed
  for. What the attestation is stays disclosed by `PROVENANCE.ATTESTED`.

- **`PROVENANCE.PARTIAL` proven end-to-end through the RLDS adapter**, on the shape it was written
  for: a TFDS export where only some shards carry `episode_metadata/file_path`. The test asserts
  that `upstream` is reported partial for 2 of 4 episodes, that `PROVENANCE.MISSING_UPSTREAM` stays
  silent because the element is genuinely present, and that `ProvenanceCoverage` still counts it as
  covered — which is exactly why the finding has to exist. Also corrected the abstention summary
  table in `docs/checks.md`, which described `structural.content-measurability` as reporting only
  unfingerprinted frames and not the too-few-episodes half it has emitted since
  `STRUCTURAL.UNCOMPARED_EPISODES` was added.

- **The emitted Croissant and PROV documents now say what a provenance element describes.** Both
  flattened scope away, so an `upstream` recorded for one episode of four was published — to other
  tools, not just shown to a person — as the dataset's lineage. Lineage that looks complete is worse
  than lineage that is absent, which is the reasoning `prov:wasDerivedFrom` already applied to
  naming one upstream of three. Each Croissant provenance element now carries a `scope`
  (`dataset`, `episode/3`, `episode/3/stream/…`), and the PROV entity carries
  `veridex:partialProvenance` naming each key and how many episodes it covers. The derivations and
  attributions themselves are unchanged — episode 0 really does derive from that file — so every
  existing consumer reads what it always read; what is new is the qualifier beside it. PROV has no
  vocabulary for "this holds for part of the entity" short of splitting the entity per episode,
  which would change the graph shape, so the coverage is stated under the `veridex:` namespace.

- **`veridex inspect` now qualifies a partially-recorded provenance element.** It printed the value
  and class of every expected key with no indication of *scope*, so an episode-scoped lineage
  recorded for one episode of four read exactly like a dataset-scoped one recorded for all of them.
  The line now ends `(recorded for 1 of 4 episode(s))` where that is true, and says nothing extra
  where a dataset-scoped record supplies the key or every episode carries one. The same defect
  `inspect` was fixed for when it showed one sensor out of thirty-two, arriving through scope rather
  than through count.

- **`PROVENANCE.PARTIAL`** — an expected provenance element recorded for *some* episodes and no
  others. It is not missing, so none of the `MISSING_…` codes fires; and the provenance coverage
  percentage counts the strongest class found in any record, so lineage recorded on one episode of a
  thousand reads as lineage for the dataset — in the report and in the signed certificate. An Open
  X-Embodiment conversion where only part of the shards carried `episode_metadata/file_path` is
  exactly that: `upstream: known`, 999 episodes with no origin, and nothing saying which is which.

  The denominator is the episodes **in this run**, so a sampled run never reports a partiality its
  own request created — that narrowing is disclosed by the run's coverage note. A dataset-scoped
  record covers every episode and is silent here, and a placeholder value (`unknown`, `n/a`) does not
  count as an episode that carries the element, so an absent element stays `MISSING_…` rather than
  becoming partial. This reports the fact; it does not change the coverage arithmetic, which is
  rubric-defined.

- **The cycle rule is linear, and both ambiguity messages are bounded.** Asking whether every edge
  of a loop is valid at once compared each pair; intervals on a line that overlap pairwise share a
  common point, so it is exactly `max(start) <= min(end)` — one pass. The loop's own length is a
  number the file chooses (a chain of 100k mount frames closing on itself is a legal input), and so
  is the count of parents naming one frame, so neither is rendered in full any more: both messages
  name up to eight and then say what they elided, with the count always exact. Without that, one
  finding carried a megabyte of frame names into the terminal, the JSON, the SARIF and the signed
  certificate.

- **The multiple-parent rule is a sweep, not a pairing.** Nothing caps how many transforms a log may
  carry, and the adapters key them by `(parent, child)`, so every distinct parent a file names for
  one frame survives ingest. Comparing every pair is quadratic in that file-chosen number — 100k
  parents is 5e9 comparisons, a hang rather than a finding, reached by exactly the malformed rigs
  the check exists for. An `O(k log k)` endpoint sweep answers the same question exactly: no
  sampling, no cap, nothing skipped. Two validity windows that **touch** at one instant overlap (the
  frame really does have two parents there) while two that **abut** do not (that is a
  recalibration); both boundaries are pinned by tests.

- **`make_demo_mcap -- <out> av-ambiguous-tf`** — a runnable rig for the ambiguity above, and the
  proof it is invisible without it. The `av` five-sensor rig with a second broadcaster claiming
  `lidar_top` from a `lidar_mount` that is itself parented to `base_link`: the frame graph stays one
  connected component and the LiDAR still reaches the camera, so `AUTONOMY.CALIBRATION_INCOMPLETE`
  and every `AUTONOMY.SENSOR_FRAME_*` code stay silent. An integration test asserts the healthy and
  ambiguous rigs differ by exactly `AUTONOMY.CALIBRATION_AMBIGUOUS` and nothing else, and the
  [autonomy quickstart](docs/autonomy-quickstart.md) shows the run.

- **`AUTONOMY.CALIBRATION_AMBIGUOUS`** — a rig transform tree that is present, connected, valid edge
  by edge, and **not a tree**. Every calibration check Veridex had walked the coordinate-frame graph
  *undirected*: `AUTONOMY.CALIBRATION_INCOMPLETE` counts connected components, and
  `autonomy.sensor-frame-resolution` asks whether a sensor's frame reaches a camera. Both answer
  whether two sensors *can* be related, and neither says anything about whether the answer is
  **unique**.

  Two shapes slip through. A frame given **two different parents** over overlapping validity — two
  nodes both broadcasting a transform for `lidar_top`, one from `base_link` and one from a mount
  frame, which is tf2's `TF_MULTIPLE_PARENT` warning — leaves the graph connected and every edge
  individually valid, so a consumer places the LiDAR by whichever chain it happens to resolve and
  two tools fusing the same log disagree with nothing flagged. And a **cycle** (`base_link` →
  `lidar` → `radar` → `base_link`) gives every frame exactly one parent while leaving the rig with
  no root frame at all.

  A new code on the existing `autonomy.calibration-completeness` check, so the `world-model-ready`
  calibration criterion judges it already. Error severity: which of two chains places the sensor is
  not a judgment call, it is a question the log does not answer. Deliberately narrow — a re-parenting
  across **disjoint** validity windows is a recalibration and is not reported, and a disagreement in
  the *numbers* between two chains is a calibration-quality judgment this does not make. Proven
  end-to-end through the MCAP adapter, which keys transforms by `(parent, child)` so both
  conflicting edges survive ingest: the doubled rig differs from the correctly-wired one by exactly
  this one finding.

- **`AUTONOMY.GNSS_IMPLAUSIBLE` / `AUTONOMY.GNSS_UNSET`** — the second half of the "GNSS geospatial
  sanity" follow-up, now that the coordinates are decoded. A satellite fix is the one rig measurement
  whose validity has an absolute physical answer: a latitude outside ±90° or a longitude outside
  ±180° is **not a place**, and when one appears the receiver, the unit conversion or the field order
  is wrong — silently, because the numbers still look like coordinates. Error severity.

  The second code is a fix at exactly `(0, 0)` for every frame: a receiver that never acquired one.
  Null Island is a real point in the Gulf of Guinea, so it is judged by exact equality across the
  whole stream — a vehicle that genuinely drove there would not hold six decimal places of zero for
  an entire recording. The same reasoning `STATISTICAL.SATURATED` rests on, and what keeps it free
  of false positives. Warning severity, because the data is real; what is wrong is where it says it
  is.

  Check #43, and a sixth `world-model-ready` criterion — a drive whose fix is impossible or never
  acquired cannot be aligned to a map or to another drive, which is what a world model built from
  more than one of them requires. Silent where the GNSS was never decoded: a check that cannot see
  the values must not report them plausible, and `STATISTICAL.UNMEASURED_VALUES` says so instead.

- **A rig's GNSS was the one sensor nothing measured.** `sensor_msgs/msg/NavSatFix` was the only AV
  message body the CDR decoder did not read, so a GNSS stream was fingerprinted rather than measured
  and every statistical check abstained on it — a receiver frozen at one fix, publishing NaNs, or
  railed at a coordinate limit reported nothing at all, while the identical faults on the IMU beside
  it were caught. It is the follow-up the autonomy spec named as "GNSS geospatial sanity", and the
  half of it that needed the values decoded first.

  Latitude, longitude and altitude are now decoded and measured per dimension, in MCAP and in both
  rosbag2 storage plugins. A message declaring `STATUS_NO_FIX` contributes nothing: those fields hold
  whatever the driver left behind — zeros, or the last position it had signal for — and recording
  them would report a vehicle parked at Null Island as a fact about the drive.

  The demo rig now writes real `NavSatFix` **and** `Imu` bodies rather than placeholder payloads. Both
  readers decode a whole message, so the flagship demo was under-exercising them, and the IMU the
  demo exists to show drifting was the one stream nothing could grade. The new decoder joins the
  message-body panic sweep.

  One test had to be un-brittled to allow it: the corrupt-chunk regression patched **byte 2044** of
  the demo log by hand, and changing the message bodies moved the chunk out from under it. It walks
  the record framing to the compressed payload now, so the next change to the demo does not silently
  turn it into a test that damages nothing.

- **Two spec entries rewritten, one of them into readability.** The MF4 task in
  `add-autonomy-support` had been appended to five times this week and had become a wall of prose
  with an orphaned sentence in the middle of it; it is now a short list of what the adapter does,
  what it declines and why, and the follow-ups, pointing at the module doc rather than restating it.
  Both it and the LeRobot/MCAP provenance task in `bootstrap-veridex-mvp` are marked complete —
  neither is a partial implementation any more, and the provenance one now points at the per-format
  table instead of describing two adapters out of eight.

- **The autonomy quickstart's rig table understated three adapters.** It said MF4 applies "linear
  conversions" (it applies every numeric one now), said nothing about bit-packed channels or the
  `##SI` sources that became `provenance.sensor`, said nothing about the DBC transmitter that became
  the same, and described producer metadata without saying a rosbag2 `custom_data` map now reaches
  it through the same shared key table. A table of "what Veridex reads from a rig log" is worth
  keeping current.

- **Pointing at the folder that holds your dataset got you a list of eight format names.** It is the
  most common first-use mistake — `veridex check recordings/` rather than
  `veridex check recordings/drive.mcap` — and the answer was `unsupported format: no adapter
  recognized the source (supported: lerobot, mcap, rosbag2, …)`. Correct about the directory, and
  silent about the four recordings sitting one level inside it.

  A failed detection on a directory now says what is in it:
  `but 2 entries inside it are readable — point Veridex at one of them: drive-a.mcap (mcap),
  drive-b.mf4 (mf4)`, and `the directory is empty` when that is the answer. A directory of things
  nothing can read gets no hint, because an empty list is noise, and an unreadable *file* is
  unchanged — there is nothing inside it to point at.

  The scan is one level deep and bounded (200 entries looked at, 5 named). Detection opens files,
  and walking a tree to be helpful is how a tool that refused to read anything ends up reading
  everything.

- **Nothing held the config reference to the config parser.** `docs/veridex.toml.example` is where a
  reader learns a key exists — the README points at it for the `VERIDEX_*` environment twins too —
  and a tolerance added to the parser but not to the example is a knob nobody can find. The two
  happened to agree today; nothing kept them that way.

  A test now walks `config.rs` and asserts every settable key — all thirteen tolerances, every
  top-level setting, and every environment variable the merge layer honors — is named in the
  example. Renaming one in the example makes it fail by name.

- **Every SARIF rule sent a reader to the top of a four-hundred-line page.** `helpUri` is the one
  link a code-scanning system shows someone who has never used Veridex, and all of them pointed at
  `docs/checks.md` with no fragment — so the reader landed on the whole catalog and had to find
  their own finding in it. Each rule now lands on its family's section.

  An anchor is fragile against a reworded heading in a way a bare page link is not, so a test asserts
  every anchor the reporter produces is a heading the page actually has, for every code in the
  catalog. (Rewording one heading makes the test name the code, the anchor, and the headings that do
  exist.)

- **A setting written in the config file was reported as a default.** The effective configuration
  answers "was this run configured, and by whom", and it is signed into every certificate. Three
  settings have no "unset" value to test for — `fail_on` defaults to `error`, `disabled_checks` and
  `severity_overrides` to empty — so it asked "does this differ from the default?", which is a
  different question. A file that wrote `fail_on = "error"` had its own setting attributed to
  `(default)`, and an auditor reading the certificate could not tell a producer who chose the
  default from one who never opened the file. Two keys in the same file disagreed: an explicit
  tolerance of `50` read as `(config file)` while an explicit `fail_on` read as `(default)`.

  The parsed config now records which top-level keys the document actually carried, read from the
  document rather than inferred from the values, and every setting is attributed from that.

  While there: an explicitly empty `only_checks = []` or `categories = []` selects *nothing* — a
  very different run from "all", and one the scope disclosure already reports — and it rendered as a
  blank cell, which says neither. It reads `none` now.

- **Pinned the redaction property the two front-ends now depend on.** The report's new `dataset.id`
  is redacted by the CLI with the redactor that already processed the whole verdict, and by the
  Python binding with a fresh one. That is only safe because the id resolves through the redactor's
  *deterministic* replacement table rather than its path-placeholder map, which is numbered in the
  order paths are met — otherwise the same dataset would redact to `path#1` from one front-end and
  `path#7` from the other, and the CI parity job would only catch it for an id that happens to
  contain a slash. The property is now asserted directly, across five id shapes, and stated on
  `Redactor` where the next person to add a field will read it.

- **Only one page was guarded against naming a finding code that no longer exists.** A test already
  held `docs/checks.md` and the catalog to each other in both directions — but the catalog page is
  not the only one that quotes codes. The README's headline list, the eight format walkthroughs, the
  autonomy quickstart and the profile reference all name them, all as the thing a reader should
  expect to see, and a renamed code would have left every one of those pointing at something that
  can never fire while only the catalog page said so.

  The guard now sweeps the README and every page under `docs/`, and looks for codes unquoted as well
  as in backticks, because those pages paste real terminal output where a code appears bare. Over a
  hundred mentions are checked. (Renaming one in the README makes the test name the page and the
  token.)

- **The autonomy quickstart printed output the tool no longer produces.** Its `certify` block showed
  `grade C (70)` and a criterion line from an older build; the real run says `fail, grade C (73)` and
  names the arithmetic check the criterion gained. A page whose whole value is "run this and see the
  same thing" is worth exactly as much as its pasted output is current.

  Fixed, and pinned: a test runs the very commands the page prints against the demo rig and asserts
  the page still contains the status line, the measured `RIG_SYNC` numbers, every readiness criterion
  verdict, and the certified line. Pasted output rots in silence; this makes it fail loudly instead.
  (Reverting the fix makes the test name the exact line that drifted.)

- **One report said an element was both attested and missing.** Applying a producer attestation
  raised provenance coverage from 33% to 50% — the attested `clock` counted, and
  `PROVENANCE.ATTESTED` named it and the key that signed it — while `PROVENANCE.MISSING_CLOCK` fired
  in the same run, with the remedy "attest this element", which the reader had just done. The score
  was computed from the attestation and the checks were not: they ran against the raw dataset and
  never saw it.

  The verified attestation's keys now reach the checks through `CheckContext`, the same channel that
  already tells them what the ingest was allowed to read, and `provenance.completeness` stays silent
  on an element a signed producer supplied. Every element nobody attested is reported exactly as
  before, so the silence is scoped to what was actually claimed.

- **`veridex diff` could not tell you that you compared two different datasets.** The guard exists
  and is documented — "a diff assumes the two reports describe the same dataset; nothing about the
  comparison holds otherwise" — and it was dead. It reads the dataset id out of a report, and the
  reports `check --json` writes never carried one, so it saw `None` every time. Diffing last week's
  report of dataset A against this week's report of dataset B produced a clean-looking
  "3 introduced, score -5" and said nothing.

  A check report now carries `dataset: { id }` — through the same redactor as everything else, so a
  `--redact` report carries the placeholder rather than the name a shared report must not leak. The
  field is optional and a report without one is treated as no evidence of a mismatch, so an older
  report still diffs.

  One interaction fell out and is fixed with it: a redacted report's id is a placeholder by
  construction, so comparing it against a real one is guaranteed to differ. The redaction mismatch
  is the true cause and is already reported on its own, so the dataset guard stands down when the
  two reports disagree about redaction — otherwise the gate would send someone to audit a baseline
  artifact that is perfectly fine.

- **`veridex verify --help` printed every flag for eight other commands.** Asking a subcommand how
  to use it returned the whole tool's help — the previous fix for `--help` being rejected outright —
  so a reader had to scan a wall of options for a hand-written `(verify)` suffix to find their own.
  And that suffix was *prose*: nothing could read it, so the help could claim a flag the parser
  rejected and nothing would catch it. `keygen --help` listed `--max-frames`; `verify --help`
  listed the sampling flags; both are refused by name if you use them.

  `veridex <command> --help` now prints that command alone: what it does, its real argument shape
  (`veridex diff [options] <report-a.json> <report-b.json>`, `veridex keygen [options]
  <output-path>` — not a generic `<dataset>` for everything), and only the options it honors. The
  applicability is read from `COMMAND_FLAGS`, the same table `reject_flags_except` enforces, which
  also removed a per-call-site allow-list array from every command. Two unit tests hold both
  directions: nothing listed is rejected, and nothing accepted is undocumented.

- **`veridex inspect` showed one sensor out of thirty-two.** The provenance summary picked a single
  element per key, so a bus log recording one ECU per transmitting node, a rig with several `##SI`
  acquisition devices, and a dataset merged from three parents each showed exactly one — with a
  coverage line reading `1/6` either way, which is correct and gives no hint that the line above it
  is a thirty-second of the answer. (The JSON and the Croissant/PROV documents carried them all;
  only the human summary collapsed.)

  Every value for a key is listed now, deduplicated, four at a time with the remainder counted, and
  tagged with the strongest class present — the same one the coverage above counts.

- **A gateway log's ECU list was the file deciding how many provenance elements to build.** The
  CAN+DBC extraction added one `sensor` element per transmitting node with no bound at all, and the
  MF4 one trimmed at eight without saying so. A `.dbc` may define any number of messages each with
  its own transmitter, and a `##SI` chain is as long as the file makes it, so both are now bounded —
  and both **disclose the trim**. A cap that fires in silence is a report that names 32 of 40 ECUs
  and looks complete over less than it covers; the full list stays readable in dataset metadata
  either way.

- **Three ECUs became one agent called "A, B, C".** The provenance extraction added this week joined
  several sources into a single element — a rig's `##SI` devices, a bus's transmitting nodes, a
  card's `source_datasets` — and the lineage emitters then read each as one thing. The PROV graph
  gained an agent with a label no device has; the Croissant credited one creator for a dataset two
  teams annotated. The CDM has always been able to hold several: provenance is a list, and
  `prov:wasDerivedFrom` already relied on it for upstreams and nothing else did.

  Each adapter now emits one element per source, and both emitters name every one — several
  `prov:Agent` nodes with the entity attributed to all of them, and a `creator` list rather than a
  single Person. The readable one-line form is still in dataset metadata, where a joined string is
  what a reader wants.

- **The Croissant would have asserted a person named "crowdsourced".** A Hugging Face card's
  `annotations_creators` says how a dataset's annotations were *produced* — `crowdsourced`,
  `machine-generated`, `expert-generated` — which is the honest answer to what `provenance.annotator`
  asks. But the emitters mapped any `annotator` value straight onto schema.org's `creator` as a
  `Person`, and onto a `prov:Person` node in the PROV graph. Reading LeRobot cards would have put
  `{"@type": "Person", "name": "crowdsourced"}` into a standards document: something no source said,
  no validator would catch, and precisely what the Croissant output promises not to do.

  A category is now carried as `veridex:annotationCreators` — saying what it is — instead of being
  asserted as an agent, in both the Croissant and the PROV entity, and the PROV graph gains no
  person node and no attribution to one. A value that *names* someone still attributes exactly as
  before, and a card listing both (`crowdsourced, Acme Labs`) attributes to the name rather than
  losing it. The defect was reachable from any adapter, not just LeRobot: an MCAP `Metadata` record
  with `annotator: machine` would have done the same.

- **The README's provenance claim was two weeks behind the code.** Its "What's built" row named
  only the scenario/map/sim reference extraction and the Croissant + PROV emission, and the headline
  bullet described provenance as something surfaced and scored without saying where any of it comes
  from — while five adapters gained extraction and every free-form key now routes through one shared
  table. Both now say what a reader can expect from their own format, and link to the
  [per-format table](docs/checks.md#what-each-format-can-supply).

- **`ros2 bag record --custom-data` wrote provenance into every bag and Veridex read none of it.**
  `custom_data` is the supported way for a ROS 2 producer to record what a bag is *of*, and it lands
  in `metadata.yaml` as a free-form key/value map. The adapter parsed the scalars around it and
  skipped the block entirely, so a team that recorded their sensor and their license into every bag
  still scored both unknown.

  It is now read, with well-known keys resolving through the same table an MCAP `Metadata` record
  uses — one key, one meaning, whichever container it was written in — and every entry carried as
  `rosbag2:<key>` metadata whether or not it is one. A key the table does not know is preserved, not
  promoted.

  The manifest is untrusted input and the map has no declared bound, so both the entry count and each
  value's length are capped; without them one file decides how much of the CDM, the content hash and
  every report it occupies. And the block ends at the next line that is not one of its entries — a
  reader that kept consuming would eat `message_count`, the manifest's own assertion about the
  recording that the coverage reconciliation is built on.

- **The well-known key table recognised five of the six elements it is scored against.** `license`,
  `sensor`, `calibration`, `annotator` and `upstream` all had rows; `clock` had none. A producer who
  wrote `time_source: GPS` or `clock: PTP grandmaster` into an MCAP `Metadata` record, an HDF5
  attribute or a Zarr `.zattrs` entry had it read as free-form metadata — so the one question a
  *timing* verifier most wants answered was the one key it could not recognise. `clock`,
  `clock_source`, `time_source`, `timebase`, `time_base`, `time_sync` and `sync_source` now map to
  it. A timestamp field or a frame rate still does not: naming a clock is not the same as carrying
  one.

  `docs/checks.md` gains a **what each format can supply** table — where every provenance element
  comes from, per format, and what deliberately is not counted (the Hub's `original` and
  `no-annotation`, the DBC's `Vector__XXX`, a node that never transmitted). Five adapters gained
  extraction this week and nothing said, in one place, what a reader could now expect from their
  own format.

- **A key called `sensor` meant one thing in an MCAP and nothing in an HDF5.** Veridex has a curated
  table of well-known metadata keys — `device`, `lidar_model`, `source_dataset`, `calibration_version`
  and two dozen more — that map a producer's free-form key onto a typed provenance element. It lived
  inside the MCAP adapter, so an HDF5 or Zarr store naming its device or its upstream dataset had
  them read as free-form metadata while the identical key inside an MCAP `Metadata` record became
  provenance.

  The table now lives in `adapter/mod.rs` and all three use it. One key, one meaning, whichever
  container it was written in.

  Each store reader keeps two rows of its own, checked first and deliberately: `author` in a store's
  root attribute is who wrote the *file*, which is not the question `provenance.annotator` asks (who
  labelled the data), so it stays its own element rather than inheriting the recording-metadata
  reading; and `date` stays `created`, which the shared table has no row for.

- **A CAN+DBC dataset carried no provenance record at all.** Not the ECU that produced the traffic,
  not even the `source_format` element every other adapter emits — the field was literally empty, so
  a bus log scored 0/6 while its own signal database named the node behind every message.

  Each `BO_` line names the ECU that puts that message on the bus, and the transmitters of the
  messages the log *actually carried* are now `provenance.sensor`, `known`, each node named once
  however many of its messages appear. Two things are deliberately not claimed: a node the database
  declares but whose traffic never reaches the wire (that would attribute the data to an ECU that
  produced none of it), and `Vector__XXX`, the DBC's way of writing "no node specified" — a value
  present in form and empty in substance, which is what the placeholder rule exists to keep out of a
  coverage score.

- **A `robomimic` file named its robot in a root attribute and provenance scored it unknown.**
  `robomimic`, MimicGen and the robosuite tooling all record the embodiment in one place: the
  `env_args` attribute, whose `env_kwargs.robots` names the robot the trajectories were recorded on.
  Veridex carried the whole blob into metadata and read nothing out of it, so a lab's entire HDF5
  corpus came back with the same `provenance.sensor` as a dataset that named nothing — and the
  README calls HDF5 "what most lab collectors write".

  The robot is now extracted as `provenance.sensor`, `known`, naming every robot the blob lists (a
  bimanual setup has two). A blob that is absent, is not JSON, or names no robot has nothing
  extracted and nothing claimed — the mapped-field list stays silent too, because it is a statement
  that the run read something. The short `env_name`-only form that `robomimic_small.h5` carries is
  exactly that case, and a new fixture (`robomimic_env_args.h5`, real `h5py` output like the rest)
  carries the full form.

- **A LeRobot dataset named its source in its own card and still scored it unknown.** The adapter
  read one field out of the Hugging Face dataset card — the license — while two other standard Hub
  fields answer provenance questions outright: `source_datasets` says which dataset this one was
  derived from, and `annotations_creators` says who produced its annotations. A re-upload of
  somebody else's data looked exactly as unattributed as one that genuinely had no source.

  Both are now extracted as `known` — read out of the card, not claimed by whoever handed the data
  over — mapping to `provenance.upstream` and `provenance.annotator`, in scalar or list form, with
  every value the card names rather than the first.

  The Hub's two "none" values are deliberately **not** extracted. `original` means derived from
  nothing and `no-annotation` means nobody annotated it; both answer the question the same way a
  *missing* element already does, so counting them as coverage would raise the score of a dataset
  that named no source and no annotator — which is the one distinction the provenance axis exists to
  make. And a card that carries only a license has the run report only that: a mapped field is a
  statement that something was read.

- **The demo generators could only be built by spawning `cargo`.** They lived as `examples/` under
  `veridex-core`, so a test that wanted one ran `cargo run --example` — and when several test
  binaries do that at once they contend on the build lock and the shared target directory, and an
  invocation fails. It fails inside an *unrelated* test, so it reads as a real regression. That is
  what stopped the corruption sweep from covering LeRobot, and it is why the CLI suite had been
  quietly one more concurrent generator away from going red.

  The four generators now live in a new **`veridex-demo`** crate (`publish = false`) as ordinary
  functions — `veridex_demo::{lerobot, rlds, mcap, mf4}::write(dir, variant)` — with the
  `examples/` binaries kept as thin wrappers, because the docs point at them by name. Nothing in the
  crate depends on `veridex-core`: a generator that shared the reader's idea of a format could not
  catch the reader being wrong about it, so each still writes the on-disk layout from the format's
  own specification.

  Every test that used to shell out now calls the function. **LeRobot joins the corruption sweep**,
  which was the point. The commands in the README and `docs/formats.md` change from
  `cargo run -p veridex-core --example …` to `cargo run -p veridex-demo --example …`; nothing else
  about them moves. A mistyped variant is still refused by name rather than silently producing a
  different fixture — that check now lives in one place instead of four.

- **The "every adapter" corruption sweep reached four of the eight.** `corrupted_inputs.rs` opens by
  calling itself every adapter over damaged versions of its own fixtures, and it damaged HDF5, MCAP,
  Zarr and rosbag2 — the four formats with a committed binary fixture. MF4, CAN+DBC, LeRobot and
  RLDS/TFDS had none, so they were never swept, which left the two readers rewritten this week (an
  MF4 block graph of file-stated offsets and lengths, a DBC signal database that declares arbitrary
  bit positions and widths) as the ones nothing had tried to crash.

  CAN+DBC is now swept there — its dataset is two text files, so it is written on the spot rather
  than committed — and MF4 has the broader sweep of its own described below. Each pristine source is
  asserted to ingest to real frames before it is mutated: a sweep over a fixture the adapter
  silently declines survives every mutation and proves nothing. Nothing panicked.

  LeRobot stays uncovered, and the file now says so instead of claiming otherwise: a test cannot
  build a demo *example* without spawning `cargo`, which contends with the rest of the suite and
  made it flaky, so closing that gap means lifting the generator out of `examples/` into something a
  test can call — a change to make deliberately.

- **A corruption sweep that only asserts "nothing panicked" cannot tell you what it reached.** Every
  sweep in `corrupted_inputs.rs` counted mutations and checked for unwinds, and a run in which every
  single mutation was refused at a magic number or a checksum looks identical to one that reached
  the parser behind it. That is how the RLDS blind spot below stayed invisible.

  Each sweep now records what each mutation *did* — the process died, the adapter refused the
  source, or it read it anyway — and asserts that both non-fatal outcomes occur, per format. An
  all-refused run is exercising one gate; an all-accepted run is not damaging anything the source
  validates. All four sweeps pass today, so the assertion is a tripwire: a future change that starts
  refusing everything early will say so instead of quietly proving less.

- **A checksum was shielding the RLDS parser from every corruption test.** A TFRecord checksums both
  its length prefix and its payload, and both of the adapter's damage tests flipped a byte and
  watched the CRC catch it. That is the right behavior for accidental corruption — and it means the
  protobuf reader, the feature decoder and the shape arithmetic *behind* the checksum had never been
  handed a damaged record at all. A checksum is not a defence against a hostile file: an attacker
  mutates the payload and recomputes it.

  The new sweep does exactly that. It mutates the record body, then re-frames it through the same
  independent format writer the fixtures use, so the length prefix and both CRCs are valid and the
  record arrives looking intact. Every byte position, three mutations each, plus an eight-byte
  `0xFF` run (the shape behind every allocation abort this repo has fixed) and a payload cut short
  but framed as whole. The two JSON manifests, which no checksum protects at all, get the same
  treatment. The sweep asserts that mutations land on *both* sides — some accepted, some refused —
  because an all-refused run would only be exercising the gate it never got past. Nothing panicked.

- **The MF4 corruption sweep only ever ran over one file shape.** It mutated an uncompressed `##DT`
  fixture, so it never reached the decompressor, the data-list walker, the record demultiplexer, the
  bit-field slicer or the conversion tables — every one of which reads lengths, counts, record ids
  and table sizes straight out of an untrusted file, and all of which are new. The sweep now runs
  over nine shapes (deflated, transposed, header-listed, unsorted, source-bearing, and one per
  conversion family), corrupting every byte position at three mutations each, truncating at every
  offset, and repeating the corruption pass under `--metadata-only`, which walks the block graph on
  its own path. Nothing panicked, hung, or allocated without bound.

- **The autonomy quickstart described a constant sensor latency as the wrong finding.** It said a rig
  with known trigger offsets "will report that drift as sync spread". It will not: a whole-stream
  shift leaves every span the same length, so `AUTONOMY.RIG_SYNC` — which compares durations — is
  silent, correctly. What such a rig reports is `TEMPORAL.START_OFFSET` and its mirror
  `TEMPORAL.END_OFFSET`, which are true statements about it. The boundary between the two checks is
  now pinned by a test, and the documented limit says what it actually costs.

- **An MF4 reported raw detector counts as though they were the measurement.** A `##CC` conversion
  is the rule that turns raw bits into the physical quantity they stand for, and only the linear one
  was applied. A sensor's calibration curve is not always a straight line: the rational type and the
  three look-up tables are how a real one is stored, and each was skipped with the raw value
  recorded in its place — then summarized, graded by the whole statistical family, and signed into a
  certificate as the channel's values.

  Every numeric conversion MDF defines is now applied: rational
  (`(p1x² + p2x + p3) / (p4x² + p5x + p6)`), value-to-value with interpolation and without (nearest
  key), and value-range-to-value with its default. A table whose keys are out of order — or hold a
  NaN — is declined rather than read at the wrong entry, and `cc_val_count` is bounded by the
  block's own length, so a forged count cannot read past it or allocate beyond it.

  What is still unevaluated is now filed by what it costs the reader. The **algebraic-formula** type
  produces a number that is in the file as a rule, so leaving it unevaluated means every value of
  that stream is a raw count standing in for a physical quantity — that is `COVERAGE.SOURCE_UNREAD`
  in the verdict, not a note only `inspect` prints. The four **text-valued** types produce a string
  a numeric stream has no shape for, and recording the raw code is the honest answer — those stay
  unmapped, costing nothing. Both now name the rule (`conversion type 3 (algebraic formula)`) rather
  than a bare number.

- **An MF4 full of bus traffic produced almost no streams.** The adapter decoded only whole-byte
  channels on a byte boundary, and that is not how an automotive measurement stores signals: a
  12-bit pedal position starting three bits into a byte, a 4-bit gear packed above it in the same
  word, a 24-bit sensor count — all ordinary, all refused. The refusal was disclosed, which is why
  it was survivable, but the measurement was in the file and nothing read it.

  Little-endian integer channels are now decoded at **any bit offset and any width up to 64 bits**,
  sign-extended from the *field's* own width — a 10-bit signed steering angle reads as `-512` and
  not as a large positive spike. A field spans at most nine bytes (seven bits of offset plus
  sixty-four of value), and one that runs past the end of its record yields no stream rather than
  being assembled out of the bytes that follow it, which would read the next record's data as this
  one's for every sample.

  Bit-packed **big-endian** fields remain declined, by name: MDF's bit numbering for a straddling
  Motorola field is not the DBC sawtooth, and a wrong reading there is a plausible number rather
  than a failure. Big-endian integers and IEEE floats in whole bytes on a byte boundary are decoded
  as before.

- **An MF4 scored 0/6 on provenance while naming its hardware in every channel group.** An `##SI`
  acquisition-source block states which ECU, bus, I/O device or tool produced a channel group's
  samples, and a channel may name a finer one of its own. That is exactly the question
  `provenance.sensor` asks, and the answer was in the file the whole time — Veridex read the writing
  program out of the identification block and nothing else.

  Both `cg_si_acq_source` and `cn_si_source` are now read into `provenance.sensor`, each source
  named once however many channels point at it, and qualified by its bus or path — two ECUs called
  `Gateway` on different buses are two sources. `known`, never asserted: it came out of the
  measurement, not from whoever handed it over. Past eight sources the remainder is counted rather
  than listed, so a gateway log naming hundreds of ECUs still reads as a sentence.

  A file that names no source claims none, and its report does not list the `##SI` mapping either —
  a mapped field is a statement that the run read something, and claiming one it never saw is the
  same defect in miniature. An `##SI` with a missing or empty name is not a source: it would put an
  empty string into `provenance.sensor`, which reads as extracted knowledge and is not any.

- **An unsorted MF4 data group ingested to zero frames.** A bus logger does not write one raster at a
  time — it writes records as the samples arrive, several channel groups interleaved in one data
  block, each record prefixed with the `cg_record_id` of the group it belongs to. Veridex declined
  the whole group, so such a file produced no frames at all and every check passed on nothing.

  The stream is now demultiplexed into one contiguous stream per channel group, each sliced at that
  group's own record length — the differing strides are the point, since a splitter that assumed one
  length would misalign everything after the first record of the other group.

  Two consequences fell out of it. **Each channel group now gets its own clock id**
  (`mf4-master#<data-group>.<channel-group>`, previously named after the data group alone). Every
  `##CG` carries its own time master, so two channel groups are two independent timelines; sharing an
  id would have made the cross-stream temporal checks compare one raster's span and rate against
  another's and report the difference as a defect. And a **variable-length signal-data group** is now
  declined by name: its records are length-prefixed rather than fixed-stride, so slicing them at
  `cg_data_bytes` read every one at the wrong offset — a full set of confidently wrong values.

  A record's length is known only from its id, so an id no channel group claims leaves every later
  record at an unknown offset. That refuses the whole group rather than returning what came before
  it: a partial decode would silently truncate the measurement while the run still read as complete.

- **`docs/formats.md` claimed to demonstrate eight formats and showed six.** CAN+DBC and ASAM
  MDF/MF4 had no section on the page README sends readers to for exactly those two. Both are there
  now, each with a runnable demo and its real output: the CAN one is two heredocs (a four-line
  candump log and the `.dbc` that gives its bytes meaning), and the MF4 one is a new
  `make_demo_mf4` example.

  That example writes a measurement the way a logger writes one — a ~4 s, 100 Hz vehicle raster
  whose records are deflated into `##DZ` chunks, transposed column-major first, chained through a
  `##DL` behind an `##HL` — with `clean`, `gap`, `saturated` and `uncompressed` variants. The
  `uncompressed` variant exists to be diffed against `clean`: generated under the same file name in
  different directories, the compressed and uncompressed measurements produce an identical CDM
  hash, which a new CLI test pins end-to-end through the binary. How the records were stored is not
  what they mean.

- **A real MF4 measurement ingested to zero frames.** The MF4 adapter read a data group's records
  only from an uncompressed `##DT` block. Loggers do not write those. They deflate the records into
  a `##DZ`, split them across a `##DL` data list as the drive runs, or both behind an `##HL` header
  list — so on the files the format is actually used for, every channel came back with no frames,
  and every temporal, statistical and structural check ran on nothing and passed. The verdict
  disclosed it (`COVERAGE.SOURCE_UNREAD`), which is the reason it was survivable, but the disclosure
  was all a fleet log got.

  All four shapes now resolve into the one record stream they describe, and decode to a measurement
  byte-identical to the uncompressed original — the same streams, timestamps, value fingerprints and
  recomputed statistics, asserted against the `##DT` fixture rather than against a hand-written
  expectation. That includes `dz_zip_type` 1, where the writer lays the bytes out column-major before
  deflating: read without reversing that, a transposed block does not fail, it yields a full set of
  confidently wrong values.

  Every length in a compressed block is a claim by the file, so none is trusted. The declared
  expansion is charged to the shared decompression budget *before* a decompressor is pointed at the
  stream (a 60-byte block claiming 8 GiB is refused, not allocated), each read is hard-capped at that
  declared length, and a stream that produces fewer bytes than it promised is reported rather than
  decoded — a short buffer would silently drop the tail of the measurement. A data list whose
  elements do not all resolve refuses the whole group for the same reason: half a list is not a
  shorter measurement, it is a misaligned one, since every record after the missing chunk would be
  read at the wrong offset.

  What is still declined, and still disclosed as unread: a `##DZ` holding something other than a `DT`
  record stream (`SD`/`RD` signal and reduction data, the column-oriented `DV`/`DI`/`RV`/`RI` blocks
  of MDF 4.2), an undefined zip type, and — unchanged — unsorted data groups. `--metadata-only` still
  describes a measurement from its header tree without opening a data block, which is now the
  cheapest way to inventory a large one rather than the only way to see a compressed one at all.

- **Upgrading Veridex made every `--fail-on-regression` gate blame the data.** A release that adds a
  check, adds a finding code, or rewords a message puts findings under `introduced` on a dataset that
  did not change by a byte — which is exactly what the three checks above do. The first gate run
  after an upgrade reported "3 finding(s) introduced" and sent someone to audit data that was fine.

  A diff across two Veridex versions is a comparison of catalogs, not of data. It still fails the
  gate — silently passing a comparison that cannot be made is the worse error, and re-baselining
  after an upgrade is a deliberate act — but it now fails **by name**: `Veridex: CHANGED — 0.1.0 ->
  0.2.0` leads the terminal diff, the JSON document carries a `veridex_version` block beside the
  `dataset`, `coverage` and `redaction` ones, and the gate's message says to re-baseline rather than
  quoting a finding count. The same reasoning as the dataset, coverage and redaction mismatches
  already there: a statement about the two documents, not about the data.

- **A one-episode recording never said that seven checks had nothing to compare.** An MCAP file and
  a bare rosbag2 recording are one episode *by construction*, and seven checks in the catalog answer
  their question by comparing one episode against another — duplicate and near-duplicate detection,
  cross-episode stream presence and shape consistency, episode-index continuity, the frozen-episode
  check, and the episode-duration outlier. Over a single recording all seven produce nothing, and
  nothing said so: the demo MCAP scored `data 100`, grade B, and the certificate listed every one of
  them as executed with no categories skipped.

  `STRUCTURAL.UNCOMPARED_EPISODES` (info) names them and the number of episodes each needs. It is the
  third axis of the reasoning behind `TEMPORAL.UNMEASURED_CLOCK` and `STATISTICAL.UNMEASURED_VALUES`:
  not "no clock", not "no values", but "nothing to compare against". It speaks of the **run**, not
  the dataset — `--sample-episodes 1` over a five-hundred-episode dataset leaves one episode in the
  CDM, and "this dataset holds 1 episode" would be false about the dataset while true about the run.
  That is the same mistake `STATISTICAL.UNMEASURED_VALUES` made under `--metadata-only`, caught this
  time before it shipped.

- **Five findings reached users with runs of spaces inside their sentences.** `rustfmt` joins a
  string literal wrapped with a `\` line continuation without removing the indentation that followed
  it, so a message written across four source lines rendered with thirty spaces in the middle. One of
  them was the ego-pose non-finite finding, which travels into the terminal report, the JSON, the
  SARIF and the signed certificate. It compiles, and every test asserting `contains(...)` still
  passes. A test now walks the crate's own source and fails on a run of three or more spaces between
  two words — the column alignment in the renderers pads after punctuation and is untouched.

- **A camera with no focal length certified as calibrated.** `autonomy.calibration-completeness`
  tested that intrinsics were *present*. An uncalibrated ROS camera driver publishes a `CameraInfo`
  of all zeros, which is present — so a rig carrying it scored a clean pass and the
  `world-model-ready` calibration criterion reported green, over a camera that can project nothing.
  Every fusion built on it is undefined, and Veridex would have signed it as ready.

  `AUTONOMY.CALIBRATION_IMPLAUSIBLE` (error) now covers calibration that is present and cannot be
  used: a focal length that is not positive and finite, a principal point that is not a finite
  non-negative pixel coordinate, a non-finite distortion coefficient, a transform holding a
  non-finite value or an all-zero rotation quaternion — the uninitialized value, not a pose. Only
  **impossibilities** are judged, never implausibility: a long lens, an off-centre principal point, a
  strong distortion coefficient and an unnormalized-but-real quaternion are all legitimate, and
  telling sensible from silly would need image dimensions the CDM does not carry. Because the code
  belongs to a check the readiness profile already judges, the defect reaches `ready` with no new
  criterion to forget. The criterion's own printed guarantee — the sentence a signed certificate
  carries — is updated with it: "connected transform (TF) tree and camera intrinsics present, **and
  arithmetically usable**".

- **`structural.frozen-episode` — the recording where the robot never moved.** The commonest failure
  in a teleoperated dataset, and it fell exactly between two checks that each defer to the other.
  `structural.stuck-stream` looks only at `Video`, because a frozen *scalar* stream is the
  statistical family's business; and `STATISTICAL.DEGENERATE` reads summary statistics, which for a
  LeRobot dataset are computed **dataset-wide** — one dead episode among fifty does not move them.
  Fifty good episodes plus one where nothing moved scored exactly the same as fifty-one good ones,
  and the policy learned that holding still is sometimes correct.

  The evidence is frame content, not values — every frame of the stream carrying the same
  `content_hash` — so it reaches every format that fingerprints frames rather than only the ones
  whose numbers Veridex reads. Three guards keep it off honest data: only streams carrying more than
  one scalar per frame (a single column is as likely to be a `reward` or `done` that is legitimately
  constant through a demonstration that failed), only when the frozen episodes are a strict minority
  of the dataset, and only on evidence — eight frames, three episodes, and every frame fingerprinted.

- **`structural.step-alignment` — an episode whose arrays disagree about its own length.** A step
  index *is* a row index: when a source stamps frames with one (HDF5 and Zarr), `action[i]` and
  `observation.state[i]` are the same moment by construction, and the only thing that can break the
  pairing is the arrays holding different numbers of rows. Nothing in the catalog looked. The whole
  temporal family abstains on a step index — deliberately and correctly, since an index is flawlessly
  monotonic and perfectly regular — and `structural.declared-frame-count` needs a count these formats
  rarely declare. An episode holding 100 actions beside 50 observations therefore came back clean,
  with every pair past row 50 trained from the wrong observation. On measured time the same defect
  has always surfaced as `TEMPORAL.CLOCK_SKEW`; on a step index it surfaced as nothing.

  A difference of **one** row is tolerated, and only one: several collectors store the terminal
  observation a trajectory ends in, which is a deliberate convention and not a defect — a check that
  failed sound robomimic data would be the fastest way to conclude the tool is wrong. Two rows is no
  convention. Proven end-to-end against an `h5py`-written fixture carrying both cases at once.

- **A `JointState` topic that reorders its joints is refused, not mismeasured.** The message
  guarantees only that `position[i]` belongs to `name[i]` *within that message* — nothing says two
  messages order their joints alike, and a publisher aggregating several sources is exactly where
  they might not. Accumulating positionally across a reordering folds two joints into one dimension:
  a statistic for a joint that does not exist, reported under whichever joint's name came first.
  That is a confident wrong answer, which is worse than no answer. The joint set is now fixed by the
  first message that names one joint per position, and a message contradicting it drops the whole
  stream's values and discloses the topic as unread coverage, where a reader meets it as
  `COVERAGE.SOURCE_UNREAD`. Refusing is also what keeps the bound: the alternative — an index of
  every joint name ever published on the topic — is an allocation the file gets to size.

- **A finding about one joint now calls it by name.** Every statistical finding on a multi-DoF
  stream names the dimension it is about, and it named it by index: `observation.state
  (dimension 5)` is a number to go count columns against. The sources were already saying which
  joint that is — a LeRobot feature's `names`, the `name[]` array a `JointState` publishes, an IMU's
  fixed axes — and Veridex was dropping it on the floor. Findings now read `dimension 5 \`gripper\``,
  and fall back to the index where the source names nothing.

  `--redact` scrubs the names with everything else it scrubs: a joint called
  `acme_wrist_gripper_v2` must not leave the building just because a finding says which one
  saturated, and the disclosure the redacted report carries now says so.

  `Stream.dim_names` carries it, bound into the content hash like every other content field
  (`CANONICAL_VERSION` 9 → 10, golden vector re-pinned): two datasets whose reports name different
  joints must not hash alike. A `names` list that is not one name per scalar is declined rather than
  guessed at — an image feature's `["height", "width", "channel"]` labels the axes of a tensor, and
  reading those as element names would report a saturated pixel channel as the joint `width`.

- **An IMU recorded to a bag is graded on its values too.** The same gap as the arm below, on the
  sensor that appears on nearly every rig: a `sensor_msgs/msg/Imu` is thirty-seven doubles with no
  bulk blob among them, so it is entirely its own measurement — and an accelerometer railed at its
  ±16 g limit for three quarters of a recording, reporting its own ceiling rather than the world,
  produced no finding at all. Its orientation, angular velocity and linear acceleration are now
  summarized per axis.

  A field whose `covariance[0]` is `-1` is one the driver declares it does **not** provide, and ROS
  leaves it zero-filled. Those slots are held out of the statistics rather than read as zeros:
  summarizing them would report a bare gyro as an IMU whose orientation is frozen at the origin — a
  defect it does not have, sitting on top of the ones it might.

- **A robot arm recorded to an MCAP or a ROS 2 bag is now graded on its values.** A bag's message
  payloads are opaque bytes to Veridex — it fingerprints them and never decodes them — which is the
  honest position for imagery and point clouds. But it also meant a `sensor_msgs/msg/JointState`
  went unread, and that message carries nothing *but* the measurement: a handful of joint angles.
  The consequence was the exact failure this tool exists to prevent. An arm recording whose elbow sat
  hard against its stop for three quarters of the run — a saturated actuator, which teaches a policy
  to command a limit it can never leave — came back `data 100` with no statistical finding at all,
  over a certificate listing all five statistical checks as run with nothing skipped.

  `JointState.position` is now decoded and summarized per joint, exactly as LeRobot's and HDF5's
  values are and through the same accumulators, so `STATISTICAL.SATURATED`, `NON_FINITE_OBSERVED`,
  `OUTLIER` and `DEGENERATE` all reach a bag. Both storage plugins do it, and a test asserts the two
  agree: which one a team picked must not change the verdict. Every other topic stays opaque and
  still says so through `STATISTICAL.UNMEASURED_VALUES`, which now names only the streams that were
  genuinely not measured rather than the whole recording. A `JointState` publishing effort without
  positions is declined rather than recorded as a measurement of nothing, and both of its sequence
  counts are bounded by what the message body could actually hold.

- **rosbag2's MCAP storage plugin — the ROS 2 default since Jazzy — is read as a bag.** `ros2 bag
  record` wrote `sqlite3` shards through Iron and writes `.mcap` ones now, and Veridex claimed only
  the first: pointing it at a Jazzy bag directory answered "unsupported format: no adapter recognized
  the source". The way through was to point it at one `bag_0.mcap`, which reads that shard as a bare
  recording — losing the manifest (the recorder's distribution, the message total to reconcile
  against, the order the shards were written in) and every other shard of a split recording.

  A bag directory of `.mcap` shards is now read as the bag it is, through the same path the `.db3`
  one takes: shards in manifest order, one episode, the same reconciliation against the bag's own
  `message_count`, the same disclosure of a shard the manifest lists but the directory does not hold.
  An MCAP channel carries what the `topics` table carries — topic, schema, encoding, the publisher's
  QoS — so the modality, the latched flag and every decoded AV header come out identical; a test
  pins that the same recording through either plugin yields the same CDM. The report names the
  container it actually read, so an MCAP-backed bag's mapped fields speak of channels and log times
  rather than SQLite tables it does not have.

  Two refusals guard the edges. A directory of `.mcap` files with **no** `metadata.yaml` is not
  claimed — it could as easily be three unrelated recordings in one folder, and reading those as one
  bag would concatenate three timelines into one episode and report the seams as defects. And a
  manifest that disagrees with the shards beside it (declaring `sqlite3` over `.mcap` files, or the
  reverse) is refused by name: reading it either way would speak for messages nothing opened.

- **A ceiling on one file read whole into memory (`--max-source-bytes`, 4 GiB by default).** MCAP,
  ASAM MF4 and a rosbag2 `.db3` are random-access containers — the summary sits at the end of the
  file, the block graph is a web of offsets, SQLite's b-tree walk seeks — so each is read whole, by
  design. That makes the allocation the file's size, and a recording far past what the machine holds
  did not fail with a verdict: it failed with the process, because a failed allocation aborts and the
  OOM killer does not wait for that. No report, no exit code to act on, no clue that size was the
  problem.

  The size is now refused on `stat`, before the read, with an error naming the file's size, the
  ceiling, and what to do about it. The way out is per format, because it differs: an MCAP and a
  rosbag2 bag answer `--metadata-only` without holding the file (three seeks into the summary
  section; the bag's `metadata.yaml`), and an MF4 does not — its block graph is offsets into the
  file, so a header-only run holds it too, and its refusal says so rather than promising an escape
  the format does not have. `--max-source-bytes 0` removes the ceiling.

### Added

- **LeRobot v2.0 and v2.1 datasets are read.** They were refused as an unsupported version, which
  ruled out most of the LeRobot datasets published to date — v3.0 is recent, and the Hub is full of
  v2.1. The refusal turned out to be nearly the whole of the gap: v2 writes one Parquet and one MP4
  per episode where v3 packs many into each, but the episode a row belongs to is the `episode_index`
  column either way, `meta/info.json` / `episodes.jsonl` / `tasks.jsonl` are the same files, and the
  adapter already discovered data by walking `data/` and resolved per-episode videos by name.

  One real difference needed closing: v2.1 keeps its statistics **per episode** in
  `meta/episodes_stats.jsonl` instead of one dataset-wide `meta/stats.json`. Read as "no stats file",
  a dataset that ships statistics is reported as shipping none — and every stored-vs-observed
  comparison silently skipped, on the majority of published LeRobot data. Those are read now and
  attached to each episode's own streams, and the ingest report names where they came from — in a
  full read, in a `--metadata-only` one, and over the Hub, where `meta/episodes_stats.jsonl` joins
  the fixed manifest list a remote run is allowed to fetch.

- **The statistical checks now grade an RLDS/TFDS dataset too.** The values in a TFRecord are already
  decoded — parsing the `tf.train.Example` into typed lists is what produces the per-step
  fingerprints — and the adapter threw the numbers away after hashing them, leaving the whole
  statistical family abstaining on the largest public robot corpus there is. A `float_list` or
  `int64_list` leaf is measured now, per dimension, so a spike in joint 6 of a 7-DoF action is caught
  rather than hidden behind joint 0; a `bytes_list` leaf (an image, an instruction string) is still
  fingerprinted rather than interpreted, and says so.

### Fixed

- **The ROS message-body decoders are swept for panics like every other reader.** They parse the one
  class of bytes a *publisher* chooses — the counts and lengths inside a message steer this reader's
  arithmetic and its allocations — and the sweep over damaged *files* reaches them only through a
  container that usually fails first, so it never got that far. Every decoder now runs over every
  truncation and 512 byte flips of a valid body of each message type, each decoder over every body
  (a channel's declared schema is content too, so a `CameraInfo` decoder can be handed a
  `PointCloud2`). No panic was found; the guard is that the next edit cannot introduce one quietly.

- **A `--metadata-only` run accused every stream of carrying no values.**
  `STATISTICAL.UNMEASURED_VALUES` reads the *format*: a stream with no statistics is one whose values
  the adapter does not interpret. Under `--metadata-only` that is true of every stream in every
  format, by request — so the finding stopped describing the dataset and started describing the flag.
  It named a bag's `/imu/data` as a stream carrying no statistics, over a recording a full read
  measures per axis, beside a remedy telling the reader to go re-check the data in some other format.
  The actual fix was to drop the flag, which `COVERAGE.METADATA_ONLY` already says. It is now
  withheld under a narrow run, the same way `TEMPORAL.UNCOMPARED_STREAMS` is, and a full read still
  names every payload it fingerprinted without interpreting.

- **A metadata-only run reported a calibrated rig as missing its calibration provenance.** The
  element is decoded from ROS message bodies, which such a run does not open, so it read the absence
  it had created itself — the defect `autonomy.calibration-completeness` was fixed for a fortnight
  ago, arriving through provenance instead. Recording in-band calibration as provenance is what made
  it visible: the full run started reporting it and the narrow one did not. `MISSING_CALIBRATION` and
  `MISSING_UPSTREAM` (RLDS records lineage inside the TFRecord) are now silent where no payload was
  read; every other expected element comes from a manifest, a header or a dataset card, which such a
  run does read, so its absence still means the same thing in either mode.

- **A boolean channel was reported as a saturated actuator.** `STATISTICAL.SATURATED` asks what
  fraction of a stream's values sit exactly at one extreme, which for a two-state channel is all of
  them: RLDS carries `is_first` and `is_last` on every step of every episode — 1 once, 0 for the rest
  — and LeRobot writes `next.done` the same way. Measuring RLDS values surfaced it immediately, as
  two warnings on every well-formed dataset in the corpus. Boolean-dtype streams are skipped; what
  would be a defect on such a channel is being constant, and `STATISTICAL.DEGENERATE` reports that.

- **A LeRobot manifest's per-episode task is read.** `meta/episodes.jsonl` states each episode's
  task — it is how a v2.1 dataset records what its demonstrations are of — and the adapter read only
  the `length` from those lines. Every episode of a task-labelled dataset therefore reached the CDM
  unannotated: the semantic annotation checks had nothing to grade, and the report of a labelled
  dataset said it carried no tasks. What the *data* says still outranks it (a per-row `task_index`
  through `meta/tasks.jsonl` is the finer-grained record, and is where a mid-episode task change
  lives); the manifest line is the fallback, and a `--metadata-only` run — which opens no Parquet at
  all — now has a task where before it always had none.

- **`STATISTICAL.OUT_OF_DECLARED_RANGE`: the values, against the range their own source declares.**
  A DBC states each signal's physical span (`[0|16383.75]`), which is a fact about the data separate
  from any summary of it — what the bus designer specified, before a frame was read. Comparing the
  two answers what neither a checksum nor a statistic can: whether this log was decoded against the
  database that describes it.

  That is the failure it exists for. A CAN log read with the wrong DBC does not error — the bytes are
  the right length, every signal produces a number, the timeline is intact — and the only tell is
  that the numbers stop fitting the declared spans: a wheel speed of 40,000 kph, a temperature of
  −3,000 °C, wrong in every stream at once. A warning rather than an error, because the narrower
  reading is real too: a sensor operating out of spec, and the finding names how far past it went.

  The CDM carries `Stream.declared_range` for this (`CANONICAL_VERSION` 8 → 9, since the same
  samples are in-spec under one declaration and out of it under another), the CAN+DBC adapter fills
  it from each `SG_` line, and a `[0|0]` — what a DBC writes for "unspecified" — stays an absence
  rather than becoming a bound that reports every non-zero sample. MF4's `##CN` value range is a
  candidate for the same field once it is certain whether it bounds raw or physical values;
  declaring the wrong one would invent findings.

- **The statistical checks now grade a CAN log.** A CAN signal is the one payload in this crate that
  is *decoded* rather than fingerprinted — a wheel speed is a number, not an opaque blob — but the
  adapter threw those numbers away after hashing them, so the whole statistical family abstained.
  That gap was the example the abstention finding was written around: a log with a wheel speed pinned
  at its rail for 70% of the recording scored `data 100` with no statistical findings, over a
  certificate listing all five statistical checks as run with nothing skipped.

  Per-signal statistics are recomputed from the decoded values now, through the same single-pass
  accumulator LeRobot and HDF5 use — the same accumulator, so one signal in two formats cannot reach
  two verdicts — and that log is `STATISTICAL.SATURATED` naming the signal and the fraction.
  `STATISTICAL.UNMEASURED_VALUES` no longer fires for CAN+DBC; `NO_STORED_STATS` does, because a DBC
  declares a signal's range but stores no summary statistics to compare against.

  **MF4 is the same gap and gets the same fix.** An MF4 channel is decoded too — the `##CC`
  conversion is applied and the result is a number — so a fleet measurement whose steering angle sits
  at its end-stop for the whole drive is `STATISTICAL.SATURATED` now instead of scoring `data 100`.

### Fixed

- **A rig that carries its own calibration was scored as having none.** A ROS 2 recording with a
  complete static transform tree and `CameraInfo` intrinsics — decoded into the CDM, bound into its
  content hash, and graded by `AUTONOMY.CALIBRATION_INCOMPLETE` and the frame-resolution checks —
  still reported `PROVENANCE.MISSING_CALIBRATION`, whose stated risk is that missing calibration
  "blocks spatial and multi-camera reasoning". That tree is precisely what removes the risk. The MCAP
  and rosbag2 adapters now record the element (`Known`, "recorded in-band: N transform(s), M camera
  intrinsic(s)") when the recording carries one, so a rig bag's provenance coverage reflects what it
  actually holds. An explicit metadata key still outranks it, a calibration-*named* attachment stays
  `Asserted`, and a recording with no calibration in it gets no element: provenance Veridex made up
  would be worse than provenance it does not have.

- **A stored standard deviation of zero over a non-zero range was reported by nobody.** It describes
  no possible set of values — values that are not all identical have some spread — and it is what a
  source writes when its statistics were carried over from another stream or never computed at all.
  `statistical.extreme-outlier` divides by that std, making every z-score infinite, and steps aside
  for "corrupt stats, `range-sanity`'s finding". `range-sanity` checked the *upper* bound of the same
  inequality and not the lower one, so neither reported it and a dataset carrying statistics that
  contradict themselves passed clean.

  `STATISTICAL.STD_IMPLAUSIBLE` now covers both directions of the contradiction. Only exactly zero is
  impossible — a distribution sitting almost entirely at its mean has a small std over a wide range,
  which is ordinary data — and a genuinely constant stream keeps `STATISTICAL.DEGENERATE`, since a
  stuck sensor is a different defect from an impossible statistic.

- **A diff never checked that its two reports were about the same dataset.** `veridex diff` compares
  two saved reports and, with `--fail-on-regression`, gates CI on the result. It assumed the two were
  about the same dataset and enforced nothing: a job whose baseline artifact path is wrong, or one
  pointed at another project's report, got a confident "2 resolved, score +37" and exited 0 — a pass
  that means nothing, and the one failure mode a regression gate has no other way to notice.

  The dataset's **id** is now compared, reported first in the terminal render and in
  `diff --json`, and treated as a regression. The guard that existed compared the CDM **content
  hash**, which is exactly backwards: that hash differs between every pair of reports worth diffing —
  a dataset that gained an episode since yesterday is the ordinary case — so it printed "these
  reports describe different dataset content" on the intended workflow and said nothing about the
  actual mistake. The hash is still read, for the one thing it does say: when both reports were
  computed over identical content, the render says so, because then whatever moved moved in Veridex
  or its configuration rather than in the data.

### Changed

- **The corrupted-input sweep now reaches rosbag2 bags.** The sweep damages every committed fixture
  and asserts that ingestion returns `Ok` or `Err` rather than unwinding — a panic is not a finding,
  not an exit code, and not something a CI gate can read. It covered HDF5 files, one MCAP, and Zarr
  stores; a bag is only recognized as a *directory*, so neither the SQLite reader, the zstd shard
  path, nor the new MCAP-storage reader was reachable from it. All three are swept now, manifest
  included — `metadata.yaml` is content like everything else, and is exactly where a hostile bag
  would put a length or a path it wants followed. No panic was found; the sweep is the regression
  guard.

### Fixed

- **CAN traffic the DBC does not define was a note, not a coverage hole.** The CAN+DBC adapter
  already found both gaps — frames on an id the `.dbc` never defines, and log lines that are not
  candump frames (CAN-FD `##`, RTR) — and filed both as `unmapped`, which reaches `veridex inspect`
  and nothing else. Those frames were on the bus and went into no stream, so a partial DBC over a
  busy vehicle log produced `coverage: Full`, no warning, and a certifiable verdict speaking for the
  whole recording while measuring whichever fraction of it the DBC happened to cover. Both are now
  `unread_sources` and raise `COVERAGE.SOURCE_UNREAD` (warning) in the verdict.

  The finding names every unread source, and a bus can carry hundreds of undefined ids, so the eight
  busiest are named individually and the rest are counted — with their frame total, because a
  bounded disclosure must not become a shortened one.

- **An HDF5 file is more than its episodes, and the report said nothing about the rest.** The
  adapter reads episodes from `/data` and never looks anywhere else in the object tree. Everything
  the root holds beside it went unread *and* undisclosed: `robomimic` ships a `/mask` group naming
  which demonstrations belong to the train and validation splits, and hand-rolled collectors park
  reward tables and raw logs at the root the same way. A file whose second half was never opened
  produced `coverage: Full`, a full-marks structural pass, and a certifiable verdict speaking for
  the whole file.

  The root is now walked, and each object beside `/data` is classified on the line the rest of the
  adapter uses: anything holding **rows** is data that is there and unread, so it becomes an
  `unread_sources` entry and raises `COVERAGE.SOURCE_UNREAD` (warning) in the verdict; anything
  holding none — an empty group, a scalar array, a zero-row array, a committed datatype — is an
  unmapped note, because there is nothing there to have read. An array sitting beside the `demo_N`
  groups *inside* `/data` was already named, but as unmapped; it holds rows, so it is now unread
  too. The walk reads object headers only, so a `--metadata-only` run discloses exactly what a full
  read discloses, and a subtree deeper than the walk's bound answers "unread" rather than "empty".

  Pinned by `root_siblings.h5` (real `h5py` output) and three tests: the ingest-report split, the
  finding reaching the verdict and naming every source, and full/metadata-only parity — each
  verified red against the old behavior.

- **A near-duplicate abstention deleted the near-duplicates it had already found.**
  `structural.near-duplicate-episode` returns an info finding when it could not examine every
  episode — one whose every frame hash is shared past the boilerplate ceiling, or a pair count past
  what it tracks at once. That return happened *before* the pairs it had already counted were
  flagged, so a single boilerplate-only episode among six hundred was enough to throw away a genuine
  re-upload: the reader saw one info line about coverage and no warning at all. The copy was not
  absent from the report, it was deleted from it.

  An abstention says what was *not* looked at; it must not replace what was. Both findings are now
  emitted. Pinned by a test that puts a real near-duplicate pair alongside enough boilerplate to
  trigger the abstention, and verified red against the old behavior.

- **Four adapters filed data they never read as a field they could not map.** The distinction is the
  one this tool exists for: "unmapped" means the CDM has no shape for a field, which costs the
  reader nothing; **unread** means the data is there and nobody looked at it, so every result is
  over less of the source than it appears to be — and only unread raises `COVERAGE.SOURCE_UNREAD`
  in the verdict.

  In **MF4**, that covered a compressed (`##DZ`) data block, an unsorted data group, a group with no
  time master or an undecodable one, a second channel group dropped from a sorted group, a channel
  declaring per-sample invalidation, an undecodable channel, and a group declaring more cycles than
  its data block holds. A fleet log whose every data block is compressed — which is how loggers
  write them — came back with no frames, `Coverage::Full`, and a verdict that said nothing about it.

  In **Zarr**, it covered an array this reader cannot open: one unsupported codec among readable
  arrays dropped that stream and raised nothing, so a store missing its camera looked complete.

  In **RLDS/TFDS**, it covered a `steps/*` key the records carry and `features.json` never declared
  — a per-step value no stream represents. The episode-level half of the same case stays unmapped
  deliberately: undeclared `episode_metadata/*` is ordinary in the Open X-Embodiment corpus and
  costs the reader no coverage, so calling it unread would fire on sound datasets.

  In **LeRobot**, it covered a Parquet column the manifest never declared — whose values are in the
  data and which no stream represents. The rosbag2 adapter already treated the same situation (a
  message on an undeclared topic) as unread coverage; the two disagreed about what an undeclared
  column means, and now do not.

  Both now reach the verdict. Each fix was verified red first: without it, the end-to-end test finds
  no `COVERAGE.SOURCE_UNREAD` among the findings.

- **The `--metadata-only` refusal sent the reader to a second refusal.** It suggested sampling the
  dataset instead — but the two formats that reach it, CAN+DBC and MF4, ingest a recording as one
  episode and refuse a sample for that same reason. It now says what is true of each: a format with
  an episode axis is pointed at sampling, one without is told to check it in full. The wording is
  fixed too: "keeps its structure inside the container" stopped being the distinguishing property
  when MCAP and HDF5 gained the mode — what these two do is interleave structure with data, so
  there is nothing that describes the recording without being it. The README said the same stale
  thing and now says the accurate one.

- **A metadata-only run accused a well-calibrated rig of having no extrinsics.** Found by diffing a
  full report against a metadata-only one on the same bag, an hour after adding that path.
  `autonomy.calibration-completeness` concludes from the *absence* of a transform tree and camera
  intrinsics — and on a rig log both are decoded from message **bodies**, which a metadata-only run
  never opens. So it read the absence it had created itself and reported a fully calibrated bag as
  having "no transform (TF) tree", twice, at warning severity.

  A check that fires on what a run declined to look at is measuring the request, not the data. It
  now abstains when frames were not read — the same `CheckContext` the frame-based structural checks
  already use. Unlike those, this one is not visible from the CDM alone: a metadata-only rig and a
  genuinely uncalibrated one carry an identical `None` calibration, which is exactly why the
  ingest's own answer has to be the one consulted.

  `TEMPORAL.UNCOMPARED_STREAMS` is withheld there for a different reason — it is true, and
  `COVERAGE.METADATA_ONLY` already states it in full, so saying it again is noise rather than
  honesty. The bag now comes back `data 100` with the coverage disclosure and nothing else.

- **The demo rig log did not publish `/tf_static` the way a real one does.** `make_demo_mcap --
  <out> av` claims to model an autonomy rig log, and a real one carries each publisher's QoS on the
  channel — including the transient-local (latched) profile every ROS 2 stack offers `/tf_static`.
  The demo wrote no QoS, so its one-message transform tree was read as a sensor that fired once and
  stopped, and the run carried a `STRUCTURAL.SINGLE_FRAME_STREAM` warning and a
  `TEMPORAL.END_OFFSET` about `/tf_static` alongside the rig fault it exists to show.

  The demo now writes the profile. Its three remaining findings are all about the one thing that is
  actually wrong — the IMU stops 300 ms early, reported by `RIG_SYNC` and by `END_OFFSET`, plus the
  missing camera intrinsics — and the trust score moves from 70 to 73 (`data 73` → `77`). The
  autonomy quickstart's transcript is updated to match, and was re-run to produce it.

- **A certificate issued under an older CDM encoding was reported as tampering.** A content hash
  only means something within one canonical encoding, so when the encoding changes, byte-identical
  data hashes differently and `verify` saw only "the hashes differ" — which is the wording of
  tampering, aimed at someone who altered nothing.

  There *was* a guard: a note appended when the certificate's *release* version differed from the
  verifying build's. That is a proxy, and it fails exactly where it is needed — the encoding can
  change between two builds carrying the same version string, which is what happened in this release
  when `CANONICAL_VERSION` went 7 → 8 under `veridex 0.1.0`. Every certificate issued before that
  commit now fails against untouched data with the note suppressed.

  A certificate now records the encoding version its hash was computed under, and `verify` answers
  from that rather than inferring: a separate failure that names both versions, says plainly that it
  says nothing about whether the data changed, and points at `veridex certify` to re-issue. It is a
  distinct case rather than a trailing note, because a message beginning "content-hash mismatch" is
  read as an accusation whatever follows it.

  Nothing is weakened. The declared encoding is inside the signed payload and the signature is
  checked first, so editing it to claim an older encoding is a signature failure, not a softer
  message — there is a test for that. A genuine transplant under the same encoding is still reported
  as one, with nothing to caveat. And a certificate issued before this field existed still verifies:
  the field is omitted rather than defaulted, so its bytes and the signature over them are exactly
  what they were — also tested, because getting it wrong would invalidate every certificate already
  issued, which is the one thing a portable trust document must never do.

- **`file-10.parquet` was read before `file-1.parquet`, in three adapters.** Found by asking whether
  the rosbag2 shard-ordering bug fixed a commit earlier was a one-off. It was not: every adapter that
  reads a dataset spread over several files read them in **lexicographic** name order, and frames
  land in their stream in the order the files are read. `bag_10` before `bag_2`, `file-10.parquet`
  before `file-1.parquet`, `-10-of-12` before `-2-of-12`.

  The conventional exporters zero-pad, which is exactly why this stayed hidden — the bug is in what
  Veridex *accepts*, not in what LeRobot or TFDS write, and a re-export, a conversion script, or a
  hand-assembled subset does not have to pad. A twelve-shard LeRobot dataset named without padding
  came back with its frames out of order at frame 19 and `TEMPORAL.NON_MONOTONIC` errors on sound
  data. The same shape in RLDS is quieter and worse: episodes are numbered by a counter running over
  the shards in read order, so the same recording got different episode indices — and a different
  content hash — depending on how its shards happened to be named.

  Reordering the frames is never the answer, because frame order is data-defined and preserved on
  purpose: reordering would hide the out-of-order timestamps this tool exists to find. So the files
  are read in the order their *numbers* give. `adapter::natural_key` — extracted from the rosbag2
  fix, where it was written for this — now orders shards in the LeRobot, RLDS and rosbag2 adapters,
  and the media walk beside them. Zarr and HDF5 were checked and are not affected: Zarr computes a
  chunk's path from its coordinates rather than listing them, and HDF5 already parses the trailing
  number out of a group name (`demo_10` → 10) instead of trusting name order.

- **A latched ROS topic cost a flawless bag eight points.** Every ROS 2 stack publishes `/tf_static`
  *latched*: once at startup, retained for late subscribers. Graded as a sampled stream it drew
  `STRUCTURAL.SINGLE_FRAME_STREAM` ("carries no temporal signal") and `TEMPORAL.END_OFFSET` ("ends
  1990 ms before `/imu/data`") — both true as stated, neither describing a fault. The score deducts
  per finding, so a clean bag reported `data 92`, and a recording with three latched topics
  proportionally worse. The headline number was wrong on well-formed data, in the direction that
  makes people stop trusting it.

  A stream now carries what the source *declares* about delivery (`Stream::latched`), read from the
  topic's recorded QoS durability — rosbag2's `topics.offered_qos_profiles` column, and the same
  profile on an MCAP channel, because rosbag2 writes bags through both plugins and which one a team
  picked must not change the verdict. A latched stream is exempt from the four checks that ask
  whether streams cover the same window: `STRUCTURAL.SINGLE_FRAME_STREAM`, `TEMPORAL.START_OFFSET`,
  `TEMPORAL.END_OFFSET` and `TEMPORAL.CLOCK_SKEW`. The clean bag now reports `data 100`.

  Nothing is inferred from the frames, and that restraint is the point: a latched topic and a sensor
  that fired once and died are identical in the data and opposite in meaning, so only a recorded
  declaration exempts anything. The QoS reader accepts four unambiguous spellings (the `rmw` enum
  numbers and the policy names, across the three ways rosbag2 has written that column), treats
  `system_default` and `unknown` as saying nothing, and returns nothing when two publishers disagree
  — a wrong `latched` silences a sensor that genuinely died, which is worse than the warning it
  would have saved. A source that records no QoS keeps every check it had.

  **`CANONICAL_VERSION` is now 8.** Checks reach different verdicts on this field, so the hash binds
  it — without that, a rig whose transform tree is latched and one whose LiDAR died after a single
  sweep would hash alike, and the clean one's certificate would verify the broken one. Certificates
  issued under v7 do not verify under v8, by design: the version is mixed into the domain separator
  so hashes from different encodings never collide. The golden vector is re-pinned in this commit,
  and its fixture now carries a latched stream so the vector reaches the new encoder arm with a
  value rather than the absent marker.

- **A bag that was still recording could not be watched.** `veridex watch` exists to re-validate a
  dataset *while it is being recorded* — where catching a clock skew is worth the most, because the
  robot is still driving. rosbag2 writes `metadata.yaml` when the recorder closes, so a bag in
  progress is a directory holding a growing `.db3` and nothing else, and requiring the manifest to
  recognize a bag directory refused exactly that case as an unrecognized format. The flagship pairing
  did not work.

  A directory holding a `.db3` is now a bag, manifest or not — no other adapter here claims one, so
  there is nothing to be ambiguous with. What the manifest would have supplied is reported as
  omitted rather than assumed: no declared message total to reconcile the recording against, no
  recording distribution, no shard order beyond the shards' own numbering. A `metadata.yaml` with no
  shard beside it is still not a bag; it belongs to some other tool.

  While proving that, a second gap: **a SQLite write-ahead log beside a shard is data this run did
  not read.** Under `journal_mode=WAL` — rosbag2's resilient storage preset — recently committed
  messages live in `<shard>.db3-wal` until a checkpoint folds them back. Veridex walks the shard's
  own pages and does not replay a write-ahead log, so those messages exist, are committed, and were
  not seen. That is now a `COVERAGE.SOURCE_UNREAD` disclosure naming the log and saying to check the
  bag again once the recorder has closed it. The fixture is a real uncheckpointed WAL holding 50
  committed messages, and the test asserts both halves: 401 frames read, not 451, and the log named
  as unread.

- **A split recording failed because `bag_10` sorted before `bag_2`.** `ros2 bag record
  --max-bag-size` rolls a long bag into `bag_0.db3` … `bag_11.db3`, and the shards were read in
  lexicographic name order. Frames are appended to their stream in the order their shards are read,
  and the CDM preserves that order deliberately — reordering them would hide exactly the
  out-of-order timestamps this tool exists to find — so a sound twelve-shard recording came back
  `FAIL`, trust 43, with two `TEMPORAL.NON_MONOTONIC` errors and two `TEMPORAL.GAP` warnings. Split
  recordings are the ordinary shape of any long bag, so this was most of the ones worth checking.

  Shards are now ordered by their *number* (digit runs compared as numbers), then reordered by the
  order `metadata.yaml` lists them — the bag's own record of how it wrote them. Taking an ordering
  from the manifest follows no path anywhere; only the files already found by listing the bag
  directory are ever opened, which is the same rule that governs which shards are read at all. The
  same fixture now reports `data 100` with no errors.

  Caught by running a twelve-shard fixture through the adapter shipped an hour earlier. Both
  existing multi-shard behaviors were single-shard in every test until now, which is why the
  ordering never came up.

- **A synchronized rig failed because `/rosout` was called a sensor.** `AUTONOMY.RIG_SYNC` compares
  how long each stream spans and reports the spread as cross-sensor drift. It was comparing *every*
  stream in a rig episode — and a real ROS recording carries far more than its rig. `ros2 bag record
  -a` captures `/rosout`, `/parameter_events` and `/diagnostics` beside the sensors; a transform tree
  is published once at startup; a `CameraInfo` channel is latched or runs at 1 Hz. None of them
  samples the world, none keeps a sensor's cadence, and all of them are routinely short of the
  recording's window.

  So a perfectly synchronized five-sensor rig came back:

  ```
  [error] AUTONOMY.RIG_SYNC  episode 0
      episode 0: rig sensors are out of sync — `/rosout` spans 150.0 ms but `/imu/data`
      spans 1990.0 ms, a 1840.0 ms drift across 7 sensors
  ```

  Error severity, `FAIL`, on sound data, with a remedy — *re-synchronize the rig against a common
  time base* — that sends the reader after something that is not wrong. This is the worse direction
  for a false positive to run: a tool that fails good data teaches people to stop reading its output.

  The check now compares what its own message and risk statement are about: streams that sample the
  physical world (LiDAR/radar, camera, IMU, GNSS, CAN, ego-pose, audio, tactile). A new
  `Modality::is_sensor` draws that line, distinct from the existing `is_rig_sensor`, which answers a
  different question — "does this stream's presence mean we are looking at a rig" — and deliberately
  excludes cameras because manipulation datasets have them. A camera is not evidence of a rig; in a
  rig it is certainly a sensor, and one that dropped out early is exactly what this check is for.

  A `CameraInfo` topic is now typed `ScalarState` rather than `Video`, in both the MCAP and rosbag2
  paths. It carries a camera's calibration, not its imagery, on whatever cadence the driver chose —
  and Veridex already decodes its content into `Dataset::calibration`, which is where intrinsics
  belong. Typing it as imagery made a latched calibration topic a sensor whose span was compared
  against a LiDAR's.

  Nothing is silenced. A genuinely short sensor still fails, and now names the sensor rather than the
  log topic that happened to be shortest. The non-sensor streams' timing is still reported by
  `TEMPORAL.START_OFFSET` and `TEMPORAL.END_OFFSET`, which say what is true about them without
  claiming they are sensors. What remains, and is documented rather than hidden: a transform tree
  published once still draws `STRUCTURAL.SINGLE_FRAME_STREAM` and `TEMPORAL.END_OFFSET`, because
  Veridex cannot tell a latched topic from a sensor that fired once and died — and treating the
  second as the first is the error worth avoiding.

- **A redacted report published what a producer attested.** Attested values are deliberately *not* in
  the CDM, so a redactor built from the dataset — which is how it is built — cannot know them, and
  the conflict finding quotes them verbatim: `license: recorded \`value#3\` → attested
  \`acme-internal-secret-terms\``. The recorded side was redacted and the attested side was not. A
  producer who attests an operator's address or an internal licence term and shares a redacted report
  published exactly the string redaction exists to remove.

  The redactor now takes the attested values too (`Redactor::and_attested`), in both front ends. The
  conflict is still reported; only the strings are replaced.

- **A merged dataset's lineage named one parent out of three.** The CDM has always been able to hold
  several `upstream` elements — provenance is a list — and the PROV emit read the first and dropped
  the rest, silently. A lineage document that names one parent of a merge is worse than one that
  names none, because it looks complete. Every recorded upstream is now a `prov:wasDerivedFrom`
  edge with its own entity node; a single upstream still emits the singular form every existing
  consumer reads.

  `attest` can sign for them too: a key may repeat where more than one value is a fact rather than a
  contradiction, which today means `upstream` alone. Two different licenses in one signed document
  is refused at signing time — that is a claim needing resolution before it is signed, not after.

- **A certificate that contradicted itself about the provenance it attested.** Found by auditing the
  attestation feature an hour after writing it, against the defect class this repo has already been
  burned by twice: a signed fact no reader is shown.

  `certify --attestation` computed the trust score with the attested element counted and built the
  certificate's `provenance_coverage` block *without* it — one signed document carrying `provenance
  50%` beside `known 2 · asserted 0 · unknown 4`, which is 33%. A verifier comparing the two had no
  way to tell which was right.

  And the disclosure did not reach the reader who most needs it. `verify` printed nothing about the
  attestation, `verify --json` carried no field for it, and the label counted `0 attested` — so an
  offline reader, who cannot re-run Veridex, saw a trust score raised by a stranger's signature with
  nothing naming the key. All three now show it, and the Python `certify` gained the
  `attestation` argument the CLI had, so the two front-ends still issue the identical document.

- **The Croissant emit declared conformance that no Croissant reader could see.** A JSON-LD document
  means whatever its `@context` says it means, and two terms in ours expanded to the wrong IRI. Under
  `@vocab: https://schema.org/`, a bare `conformsTo` expands to `https://schema.org/conformsTo` —
  while Croissant's reference implementation reads `http://purl.org/dc/terms/conformsTo`, so the
  document's claim to be Croissant 1.0 was invisible to every tool that would act on it, and the
  document was read under the legacy v0 rules instead. `sha256` was mapped to `cr:sha256` against a
  reader that looks for `https://schema.org/sha256`, so the one field that pins *which* data the
  metadata describes was silent too.

  Both were syntactically valid JSON-LD and semantically empty — and the existing tests asserted the
  *spelling* of every term while asserting nothing about what those terms expand to, which is why
  they passed. The context now follows the canonical Croissant 1.0 one (`dct` prefix, `sc` prefix,
  `conformsTo: dct:conformsTo`, `@type: sc:Dataset`), and the test expands each term through the
  emitted context and checks the IRI a reader would resolve.

  Verified against the Croissant 1.0 spec and the `mlcroissant` reference implementation's own
  constants. What stays absent is deliberate: `datePublished`, `url` and `version` have no honest
  value here, so Veridex omits them rather than inventing them — a validator warns about exactly
  those three, which is the correct thing for it to say.

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

### Changed

- **Two north-star requirements now record that they were decided against, not deferred.** The
  configuration spec asked for a `lenient` profile; Veridex refuses one by name, because a profile
  that loosens a threshold raises the score without changing the data. The checks-catalog spec asked
  for PII detection; that means decoding pixels, which is the commitment Veridex is built against.
  Both decisions were already implemented, documented and tested — only the specs still pointed at
  them as work to do, which is how a north star quietly becomes wrong.

- **A certificate is stamped with an RFC 3339 UTC instant, not seconds since the epoch.**
  `issued at: 1787940281` told a reader of `veridex verify` nothing about whether a certificate was
  from last week or last year; it now reads `issued at: 2026-08-28T18:07:38Z`. The field's own
  documentation always said RFC 3339 — the CLI now agrees with it, and `attest` stamps the same way.
  A certificate issued the old way still verifies: the timestamp is signed as text and nothing in
  the trust chain parses it. The conversion is written out rather than pulled in — a date crate in
  every downstream build for one line of output — and pinned against the epoch, both sides of a leap
  day, 1900-style and 2000-style century rules, and the 32-bit second boundary.

- **A stale claim in the README's status section.** It still said Veridex offers "no near-duplicate
  detection beyond exact content matches" — untrue since `STRUCTURAL.NEAR_DUPLICATE_EPISODE` shipped
  this release. The honest statement is narrower and now written: exact duplicates *and* partial
  copies are caught, because those share frames byte-for-byte; a re-encoded or perturbed copy shares
  no bytes and is out of reach without decoding. The same section gained the threshold profiles,
  producer attestation, the report rollups, and `--redact`, so what it lists is what ships.

- **The security and rubric documents describe attestation.** `SECURITY.md` had a line anticipating
  it — "asserted provenance reflects what a producer signed" — written before the feature existed. It
  now states the mechanism: a separate producer key, a distinct signing domain so an attestation can
  never verify as a certificate, binding to the CDM content hash, and the fact that applying one
  raises provenance coverage *only*, with the key disclosed in the verdict and recorded in the
  certificate. A security document that omits a signing path is incomplete in the way that matters.

  `docs/rubric-v1.md`, the authority on the score, now says that the provenance axis is the one that
  can move on a *signature* rather than on the data — and what makes that safe: the disclosure names
  the key, so a reader who does not trust it can subtract exactly those elements.

- **The README stopped being a manual.** It had grown to 35 KB, 22 KB of which sat under a heading
  called "Quickstart" — the per-format tour, the trust chain, sampling, budgets, redaction, watching,
  attestation, labels. A quickstart that takes twenty minutes to read is not one, and the first
  thing a visitor meets is the thing most worth getting right.

  Nothing was cut; three sections moved out whole, with their prose intact:
  [docs/formats.md](docs/formats.md) (what each of the seven adapters reads and refuses to guess),
  [docs/trust-chain.md](docs/trust-chain.md) (attest → certify → verify → label, and what each
  signature does and does not prove), and [docs/partial-runs.md](docs/partial-runs.md) (budgets,
  sampling, manifest-only checks, and why a partial run cannot gate or certify). A "Going further"
  table points at all of them and at the docs that already existed.

  Every command in the moved pages was run before the move: the five in `formats.md` and the whole
  five-step chain in `trust-chain.md`.

- **The default terminal report is readable again.** A sound dataset's report is almost entirely
  `info` findings — what could not be measured, which provenance elements are absent, what a partial
  run did not cover — and each carried a risk and a remedy paragraph. On a clean LeRobot dataset that
  is seven findings and forty lines of guidance, with the two lines that actually say whether the
  data is usable at the top, above the fold and easy to miss.

  `error` and `warning` findings keep their risk and remedy, because those say what is *wrong*. An
  `info` finding now prints its code, location and message and stops there — every finding is still
  listed, nothing is summarized away — and the report states how many had their guidance omitted and
  that `--full` prints it. Every machine-readable output (`--json`, `--sarif`, `--html`) is
  unchanged, and `render_terminal` still renders everything: the compact form is a new
  `FindingDetail` the CLI selects, not a new default for the library.

### Added

- **A remote check reads an RLDS/TFDS export too, not only LeRobot.** Which layout a repository
  holds is settled by asking for each one's first required file — `meta/info.json`, then
  `dataset_info.json` — one request per layout, every path still fixed in Veridex's source. A TFDS
  export is usually published one version directory deep, so the directory can be named in the
  reference: `veridex check hf://org/name/my_dataset/1.0.0 --metadata-only`. It is named by the
  caller and never discovered, because a path the server chose would undo the point of a fixed file
  list, and two directories in one repository are two datasets — the id carries the path so their
  hashes cannot collide.

  A repository holding neither layout is refused naming both and the file each was looked for by. A
  repository holding a layout's first file but not its second — a TFDS directory with no
  `features.json` — is refused naming that file, rather than reported as "not a dataset this can
  read".

- **Whether a format has an episode axis to sample is now declared, not enforced four times.** Each
  single-episode adapter called `reject_sampling` from inside its own `ingest`, which is the shape
  that lets the fifth one forget. Adapters now declare `supports_sampling`, the registry refuses the
  request before the adapter is handed the source — on both the detected and the `--format` path —
  and `--help` builds its list from the same answer. That is exactly how `--metadata-only` already
  worked; the two are now consistent. A test pins which formats declare which, so gaining or losing
  an episode axis is a deliberate change.

- **One test now holds every format to the invariant the mode rests on**: a metadata-only run must
  describe the *same* dataset a full read does — the same episodes, streams, datatypes, shapes,
  modalities and clocks — minus the frames. A run that named different streams would not be a
  narrower answer to the same question but a different answer, which the coverage note would then
  make look like the first. The loop is driven by the registry, so a seventh adapter claiming the
  flag fails until it has a dataset to check against. Five formats pass it today; RLDS is exempted
  by fixture rather than by invariant (a full read needs a real TFRecord shard, and the writer for
  one lives in `rlds_adapter.rs`, where the same check runs against it). Verified to bite by
  renaming one adapter's streams and watching it name the format.

- **The `--metadata-only` format list is asked of the registry, not written down.** `--help` builds
  its list from the adapters that actually claim the capability, so it cannot drift; and a new test
  fails when a supporting format is missing from `docs/partial-runs.md`, `docs/checks.md`, or the
  README — the same guard `docs/checks.md` already has against an undocumented check. Verified by
  temporarily renaming one format and watching the test name the document that omits it.

- **A summary-only MCAP read now finds the provenance a full read finds.** It reported
  `provenance 0%` on a file that states its provenance perfectly well, which is a claim about the
  read rather than about the file — and provenance is 30% of the trust score. The summary carries a
  Metadata index and an Attachment index, so both are reachable without opening a chunk: the licence,
  sensor, clock and annotator a producer wrote into Metadata records, the scenario/map references and
  the scenario dimensions, and the calibration attachment's name (still `Asserted`, since it is a
  name heuristic). On the demo rig the provenance block is now byte-identical to the full read's.

  Every offset followed is the file's own number, so each is bounds-checked against the file's real
  length and the set is capped at 256 records and 4 MiB; an index entry pointing outside the file is
  skipped rather than followed, leaving the rest of the inventory legible. The one thing still not
  resolved is a scenario sidecar's version, which a full read takes from the referenced file's own
  ASAM header — opening that is reading a second recording — so only a version the recorded value
  itself carries is used, and the difference is disclosed.

- **The corruption sweep now runs every damaged file twice — full and `--metadata-only`.** A
  metadata-only run follows offsets and lengths the file states about *itself*: an MCAP's summary
  pointer, an HDF5 object header, a Zarr `.zarray`. It reaches that arithmetic without the frame
  reading that would otherwise trip over the same corruption first, so it is a different code path
  and a hostile file must not be able to panic it either. Verified reachable rather than assumed: a
  temporary panic inside the summary reader fires 85 times across the sweep.

- **A full MCAP read is now reconciled against the total the file declares about itself.** An MCAP
  closes with a Statistics record counting the messages it holds. A file truncated after that record
  was written, or one whose chunks this reader could not walk to the end of, yields fewer — and was
  read as a complete recording, which is the "silence reads as a pass" failure the tool exists to
  prevent. The shortfall is now disclosed as an **unread source**, the same way a rosbag2 short of
  its `metadata.yaml` total is, and travels into the verdict with it.

  The other direction is not a coverage hole — every message present was read, and it is the summary
  that is wrong — so it is recorded as an unmapped field instead. And a file with no summary section
  (a streaming writer legitimately omits one) disables the reconciliation rather than failing the
  read, and says so: a check that silently did not run reads exactly like one that passed.

- **`--metadata-only` for MF4**, read from the block header tree — `##HD` → `##DG` → `##CG` →
  `##CN` — which states every channel's name and raster separately from the `##DT`/`##DZ` blocks
  holding the samples. It is also the only way to describe a **compressed** measurement at all: a
  full read declines a `##DZ` block, so such a file yields no frames and a coverage warning, while
  its header tree still names every signal. That is how fleet loggers write them, and a test pins
  exactly that: a compressed fixture that a full read reports as empty, and a header-only run that
  names its `speed` channel.

  One honest difference from the other formats, recorded rather than smoothed over: a metadata-only
  MF4 run can name *more* streams than a full read finds, because a full read drops a channel whose
  data type this reader cannot decode while the header tree still declares it. It is therefore
  exempted from the cross-format "same dataset, minus the frames" invariant, by name and with the
  reason, rather than by quietly not being in the list.

  CAN+DBC is now the only format that refuses the flag — a stream of frames with nothing in front of
  it.

- **`--metadata-only` for HDF5**, where the structure is in the container but not in the *data*: the
  group tree, each array's datatype and per-row shape, and every object attribute are headers, and
  reading them touches no chunk. On a robomimic-shaped file of hundreds of gigabytes that is the
  difference between a manifest check and a full read.

  It yields the same episodes, streams, datatypes and shapes a full run reports, plus each group's
  declared length attribute where the file writes one. The proof is precise rather than approximate:
  the test finds a byte whose corruption a *full* read catches as a chunk checksum failure — so that
  byte is chunk data, not a header — and checks that the header-only read of the same file is
  untouched by it. As with Zarr, whether the file records measured time is a header fact, so a file
  with a units-declaring timestamp array reports `hdf5-time` here rather than a step index.

  With this, the refusal for a format that cannot honor the flag applies to the two that genuinely
  interleave structure with data — CAN+DBC and MDF4 — and the test that pins that refusal now uses
  a CAN log rather than an HDF5 file.

- **`--metadata-only` for MCAP**, read from the summary section the format writes at the end of
  every finalized file: a Channel and a Schema record per topic, and a Statistics record carrying
  the message total, the per-channel totals and the recording's log-time span. Three seeks in front
  of a recording that is routinely tens of gigabytes, with no chunk opened and nothing decompressed
  — a test overwrites every byte between the header and the summary and the run is unchanged, while
  a full read of the same file is not.

  It yields the topic inventory with each topic's modality, the message encodings the file declares,
  the declared counts, and the library that wrote it. Every offset it follows comes out of the file,
  so every one is checked against the file's real length first, and the summary section's own size
  is capped.

  Three refusals rather than approximations: `summary_start = 0` — a streaming writer that never
  finalized — is refused by name, because the topics then exist only in the records; a footer
  pointing outside the file is refused rather than followed; and a summary whose per-channel counts
  do not add up to its own total is refused, the same invariant the rosbag2 manifest path enforces,
  because presenting three channels out of twelve as the recording's contents is invisible to the
  caller.

- **`--metadata-only` for Zarr**, where the structure is outside the data by construction: `.zarray`
  gives every array's dtype, per-row shape and row count, `.zattrs` the store's metadata and
  provenance, and the `meta/` group the episode boundaries — a few kilobytes in front of a replay
  buffer that may be hundreds of gigabytes of chunks. A test corrupts every `data/` chunk in the
  store and the run is unchanged, which is the claim rather than a comment about it.

  Each episode carries the length its boundaries declare and one empty stream per array, with the
  same dtype, shape and modality a full run reports. Two decisions worth naming. The `meta/` group
  *is* read — the boundaries are the store's manifest, and without them there is no episode set to
  check. And the clock is the store's own: whether a timeline array exists and states its units is
  knowable from `.zattrs` alone, so a timed store reports `zarr-time` here, not a step index.
  Reporting the step index would be this run's abstention dressed up as a fact about the source,
  and the content hash would then bind it as one.

- **`--metadata-only` for RLDS/TFDS**, the format Open X-Embodiment ships in — where the mode earns
  the most: those directories run to hundreds of gigabytes and their manifest is two files of a few
  kilobytes. `dataset_info.json` gives the per-split shard lengths (so the declared episode count),
  the file format, version, citation and licence; `features.json` gives every per-step feature with
  its dtype and shape. No shard is opened — the test proves it by deleting the shard and checking
  the dataset anyway.

  What it cannot see, said rather than inferred: no steps, values, or content hashes, no TFRecord
  CRC verification, no language instruction or episode task, and no `episode_metadata/file_path` —
  which is where an RLDS episode's upstream lineage lives, so a metadata-only run scores *lower* on
  provenance than a full one rather than claiming lineage it never read.

  Three things are refused instead of approximated. A manifest with no shard lengths has no episode
  set, and an empty dataset would score a clean 100 over a catalog that measured nothing — refused
  by name, naming the splits at fault. Per-episode step counts are left absent, because RLDS
  declares episodes per shard and never steps per episode; deriving one would hand
  `STRUCTURAL.EPISODE_LENGTH_MISMATCH` a number Veridex made up. And the declared-episode-count
  check is withheld, since the episode set came from that very total — the same reasoning the
  LeRobot path already uses for `total_episodes`.

  One bound comes with the mode: a run that reads no frame never fires the frame budget, and one
  `Stream` is built per (episode × feature) from two numbers in a few hundred bytes of JSON.
  `"shardLengths": ["100000000"]` is a hundred-byte file asking for hundreds of millions of streams,
  so the product is charged against the frame budget before the first one is built.

- **A remote check now says which commit it read.** `hf://org/name` reads the `main` branch, and a
  branch moves. Until now the run recorded only the revision it *asked for* — `main` — which names
  no particular bytes: the report could not be re-run, and its content hash could not be traced back
  to anything. Veridex now keeps the commit the Hub served the manifest from (the `X-Repo-Commit`
  response header) as the dataset's `hub_commit` metadata, so it binds into the content hash and
  `veridex inspect` prints it beside the hash it produced:

  ```
  source:   hf://lerobot/pickplace@main (commit 0c1e9f…)
  ```

  `veridex check` prints the same line as a footer under its report. It is deliberately not a
  verdict field: a verdict identifies its dataset by content hash alone and says nothing about where
  the bytes came from, and the commit is already inside the CDM that hash covers.

  Re-running against `hf://org/name@<commit>` pins the read to those bytes. Two reads of one
  repository at two commits are now two datasets to the hash, which is the point — a hash that could
  not tell them apart would let yesterday's result stand for today's data.

  Two bounds come with it. A manifest is five requests, so a branch can move part-way through one: a
  read whose responses name two different commits is **refused by name**, and the refusal gives the
  commit to pin to, rather than stitching half of each into a dataset that existed at no commit. And
  the header is a stranger's string that reaches the content hash, so anything that is not 40 hex
  digits is treated as if it were absent — where the Hub names no commit, nothing is recorded and
  the gap is disclosed among the run's omitted fields, because an invented commit would be worse
  than none.

- **Remote (Hub) ingestion — check a dataset you have not downloaded.** `veridex check
  hf://org/name --metadata-only` reads a LeRobot dataset's manifest straight from the Hugging Face
  Hub: `meta/` and the dataset card, a few hundred kilobytes beside a repository that is routinely
  hundreds of gigabytes. It answers "is this the dataset I think it is, does it declare what I need,
  and is its manifest self-consistent" in a second. `https://huggingface.co/datasets/org/name` works
  too, and `@revision` reads a branch, tag or commit.

  Everything about the design is a bound:

  - **The file list is fixed in Veridex's source**, not discovered from anything the server returns,
    so a hostile repository cannot enlarge what is requested. A test asserts the requests never
    leave that list.
  - **Requests and every redirect they follow are host-checked** against the Hub's own hosts, over
    HTTPS only — a 302 is the server choosing where Veridex connects next, and an allowlist covering
    only the first hop is not an allowlist. Plaintext is refused even to the right host.
  - **No credential of any kind is sent.** No token is read from the environment or the filesystem,
    so a private or gated dataset answers 401 and is reported as private. Quietly forwarding a
    credential a user happens to have to a host they did not name in the command is not a
    validator's decision to make.
  - **Responses are capped** per file and in total, and the read stops at the cap rather than
    trusting `Content-Length`.
  - **A remote run is a metadata-only run**, with every refusal that comes with one: no score gate,
    no certificate. A remote source *without* `--metadata-only` is refused by name — Veridex
    validates, it does not download.

  The manifest is staged in a temporary directory and read by the ordinary local adapter, so a
  remote check and a local check of the same manifest are the same code reading the same bytes. The
  dataset is identified as `org/name` — the repository, not the staging directory, which is
  differently named on every run and must not reach the CDM; a test pins that two reads of one
  dataset hash identically, and that two owners publishing the same dataset name do not.

  Verified against the real Hub, not only a fake one: `veridex check
  hf://lerobot/svla_so101_pickplace --metadata-only` returns `PASS` / trust 79 (C) / `data 100 ·
  provenance 33%` over 50 declared episodes, in about a second. Running it is what found the one
  defect a fake Hub could not — the Hub answers a manifest read with a *relative* redirect
  (`/api/resolve-cache/…`), which the host allowlist refused because it was not an absolute URL. An
  invented server answers the way its author expects.

  The socket lives behind a `remote` cargo feature, on for the `veridex` binary and the Python
  package, off for anyone embedding `veridex-core` — validating a local dataset should not require
  compiling a TLS stack. Everything above the socket is behind a `FetchFile` trait and tested
  against a fake Hub, so none of it needs a network to run in CI, and CI now builds and tests the
  feature-off configuration too. Nothing else in Veridex touches a network: a certificate still
  verifies offline, which is the property the whole trust chain rests on.

- **`--metadata-only` reads a rosbag2 bag from its manifest.** A bag can be a terabyte, and
  `metadata.yaml` already lists every topic, its ROS type, and how many messages it holds — so
  `check` and `inspect` can now answer "is this the recording I think it is, and does it carry the
  topics I need" in seconds without opening a shard. The recording distribution, the storage plugin
  and any compression come along too. rosbag2 joins LeRobot as the second format supporting the
  option; the others keep their structure inside the container and are still refused by name.

  What it cannot see, said rather than left inferred: no timestamps, no message bytes, no content
  hashes, and no rig calibration or ego trajectory — those are decoded from message *bodies*. Every
  stream carries zero frames by request, the coverage is `metadata_only`, the frame-dependent checks
  abstain rather than firing, and `certify` refuses the run outright.

  Two cases are refused rather than approximated. A bare `.db3` has no manifest at all, so there is
  nothing to read but the shard the caller asked not to open. And a manifest whose per-topic counts
  do not add up to its own declared total means Veridex did not understand the whole inventory —
  presenting three topics out of twelve as the bag's contents is invisible to the caller, which is
  the failure this tool exists to prevent, so the run is refused naming both numbers. That guard is
  what makes it safe to parse four levels into a YAML file with a hand-written reader at all.

- **`TEMPORAL.UNCOMPARED_STREAMS` — the three alignment checks now say when they had nothing to
  compare.** `CLOCK_SKEW`, `START_OFFSET` and `END_OFFSET` are the checks that answer whether a
  dataset's sensors are aligned, and all three need at least two streams sharing a clock. Given
  fewer they reported nothing — which is the same silence a perfectly synchronized dataset produces,
  and it reached the certificate's list of executed checks looking exactly like that.

  Found while testing the latched-topic exemption above, which sharpened the case: a ROS bag holding
  nothing but latched topics — a transform tree and a robot description, each published once, no
  sensor data at all — came back `data 100` with not one temporal finding. The shape is not specific
  to ROS: a single-stream dataset has it, and so does one whose streams each sit on their own clock.

  Reported once for the dataset, naming how many episodes of how many, and informational, like the
  other measurability disclosures: the dataset is not worse for having one stream, but what a
  passing temporal result is *evidence of* is different. An episode with no measured time at all is
  left alone — `TEMPORAL.UNMEASURED_CLOCK` already covers it in full, and saying it twice would put
  a second line on every RLDS dataset. That suppression is deliberately narrower than the other
  finding's precondition, so an episode mixing measured and step-index streams gets both; there is a
  test for the boundary, because a suppression wider than the thing it defers to is how a defect
  ends up reported by neither check.

- **rosbag2 is held to the cross-format neutrality gate.** The claim behind Veridex is that which
  format a team stores their data in does not change what the tool sees, and until now that was
  proven for one pair (LeRobot ⇄ MCAP). rosbag2 and MCAP are the two storage plugins of *one*
  recorder, so a divergence between them would show up as a dataset that changes shape when a team
  switches storage — the one thing a cross-format verifier must not do. A new gate replays the bag
  fixture's own topics, ROS types and message times into an MCAP and requires the two CDMs to carry
  the same episodes, streams, modalities and timestamps. The two paths reach that shape by different
  routes: one reads a SQLite `topics` table, the other MCAP channel and schema records.

  A rosbag2 end-to-end test now runs through the real binary as well: check, certify, verify offline,
  and a certificate that correctly refuses to verify against a different bag. The parts that make a
  certificate portable — autodetection, the dataset id taken from the path, the content hash — live
  between the adapter and the CLI, so they are worth exercising there. It also pins the split
  recording at CLI level, where the shard-ordering regression would show as exit 20.

- **Compressed rosbag2 bags (`.db3.zstd`) are read directly.** `ros2 bag record --compression-mode
  file --compression-format zstd` compresses each finished shard and deletes the original, which is
  how any recording large enough to care about is stored — so the bags most worth checking were
  exactly the ones Veridex turned away with "unsupported format". They are now ingested like any
  other shard.

  The shard is unpacked under the same decompression budget that bounds every other container in the
  crate, and bounded *during* the read rather than charged after it: the cap handed to the decoder is
  what the budget has left. The difference is not theoretical — a fixture that unpacks to 96 MiB from
  3 KB reports `requested: 67108865` (one byte past the budget) rather than `100663296`, which is
  what an implementation that decompressed first and charged afterwards would report, having already
  spent the memory it is being refused for.

  A compressed bag and the same bag uncompressed produce identical streams, frames, timestamps and
  content hashes; only the dataset's name differs. That includes the name: `shard_0.db3.zstd`'s file
  stem is `shard_0.db3`, and taking it would have identified the same recording differently depending
  on how it was stored — and the id is bound into the content hash, so a certificate issued over the
  uncompressed bag would not have verified against the compressed one.

  Per-*message* compression (`--compression-mode message`) is refused by name. Those bags' tables are
  plain, so Veridex would read them — and every frame's fingerprint would be of a zstd frame rather
  than the message, and no AV message header would decode, so a full sensor rig would come back with
  no point fields, no calibration and no ego trajectory. A wrong answer is worse than a refusal, and
  the refusal names `ros2 bag convert` as the way out.

- **ROS 2 rosbag2 (`.db3`) is the eighth format.** rosbag2 is what a ROS 2 robot records by default
  and where most existing robot logs are sitting, and until now Veridex could only read the *other*
  storage plugin the same recorder writes. `veridex check` now takes a bag directory (`metadata.yaml`
  beside one or more `.db3`) or a bare `.db3`, maps `topics` to streams and `messages` to frames on
  the bag's single log clock, and runs the identical catalog over it. The AV message headers go
  through the same CDR decoders MCAP uses, so a `PointCloud2` still supplies the per-point field
  layout, `CameraInfo` and `TFMessage` the intrinsics and transform tree, and `Odometry` the ego
  trajectory — and the bulk payload is still fingerprinted, never decoded.

  Three things it refuses to guess, each of which would otherwise turn a damaged bag into a clean
  verdict:

  - **A message on a topic the `topics` table never declares** is neither filed under an invented
    stream nor dropped in silence. It is disclosed as unread coverage
    (`COVERAGE.SOURCE_UNREAD`), because a bag with rows nothing can attribute must not produce the
    verdict an intact one does.
  - **The manifest's `message_count` is reconciled against the recording.** A recorder killed
    mid-flush leaves a `.db3` short of the total `metadata.yaml` closed with; the shortfall is
    reported as unread rather than read as a whole bag. That total is deliberately *not* mapped to
    the CDM's declared frame count — it counts every topic's messages, while that field is what each
    of an episode's streams should hold, and comparing the two would fail a sound bag.
  - **`relative_file_paths` is content, so it is never followed out of the bag.** The shards read are
    the `.db3` files in the bag directory; a manifest entry with a directory component, or one naming
    a shard that is not there, is recorded as unread.

  Columns are bound by name from each table's own `CREATE TABLE`, because rosbag2 has added columns
  across bag versions and reading position 3 because that is where `serialization_format` sat in
  version 4 would report a type-description hash as a serialization format in a version 9 bag.

  SQLite is read by Veridex's own hand-written, bounds-checked table-b-tree reader
  (`adapter/sqlite.rs`, no new dependency), for the same reason the HDF5 and Zarr readers are
  hand-written: a `.db3` is an untrusted file, and a general-purpose engine will follow a page chain a
  corrupt header points into with allocations no ingest budget can charge. It refuses a page outside
  the file, refuses a b-tree or overflow chain that revisits a page, and caps the payload one row may
  assemble before the bytes are copied.

  The fixtures are real Python-`sqlite3` output, regenerated by
  `crates/veridex-core/tests/fixtures/rosbag2/generate_fixtures.py` — a reader tested only against a
  writer from the same repository proves the two agree with each other, not that either agrees with
  the format. Golden payload hashes from that writer pin the overflow-chain reassembly, where an
  off-by-one in the local/overflow split still yields bytes that look like a message.

  Not covered, and reported as such rather than assumed: the compressed bag storage (`.db3.zstd`),
  per-topic QoS profiles, and metadata-only ingestion from `metadata.yaml` alone.

- **Attested provenance reaches the Croissant and PROV emits.** A producer who signs for what the
  format does not record then runs `veridex provenance --emit croissant` and gets a document
  describing less than the run did — the attested facts simply absent. `--attestation` now applies
  there too: the elements appear marked `asserted` (never `known`, and never overwriting what the
  dataset records), the Croissant names the signing key under `veridex:attestedBy`, and PROV says it
  with the vocabulary it has — the dataset entity is `prov:wasAttributedTo` a producer agent
  identified by that key, and an attested annotator becomes an agent exactly as a recorded one does.

  The bound content hash is unchanged either way, which is the invariant that makes this safe: an
  attestation adds to what the document *says*, never to what the data *is*. With no attestation the
  documents are byte-identical to before.

- **`veridex attest` — producer attestation, which the provenance checks were already telling users
  to use.** Six checks carried the remedy "attest this element with `veridex certify` inputs", and
  no such input existed. Most of what provenance means is not in the file — no format records who
  operated the robot, which calibration was in force, or what upstream a merge drew from — and
  Veridex will not infer any of it, so until now the only way to raise provenance coverage was to
  change the recording format.

  An attestation is the producer saying it, in a document that can be checked: signed with the
  **producer's** key (not the certificate issuer's, so a verifier can decide whether it trusts that
  key), and **bound to the dataset's CDM content hash**, so it cannot be moved to other data.
  `check --attestation` and `certify --attestation` apply one that verifies; one that does not, or
  one about a different dataset, applies to nothing and says which.

  Three properties hold it honest. Attested elements are carried **beside** the CDM, never folded
  into it — the content hash describes the data, and a claim about the data must not change what the
  data *is*. The run **discloses** it (`PROVENANCE.ATTESTED`, info, naming the producer key and every
  element it supplied), because provenance coverage is 30% of the trust score and a reader who does
  not trust that key has to be able to subtract exactly those. And an attested value that contradicts
  what the dataset records is **reported, not preferred**
  (`PROVENANCE.ATTESTATION_CONFLICT`, warning): either the recording is wrong or the claim is, and a
  signature does not get to rewrite the data's own account of itself.

  A certificate records the producer key, the keys it supplied, and the attestation's timestamp,
  signed like every other field — a rewritten attestation record fails verification. Attestations use
  their own signing domain, so one can never be presented as a certificate or the reverse, and their
  own error type, because "the certificate was altered" is the wrong sentence to print while refusing
  an attestation.

  Python gets `veridex.attest(...)` and `veridex.check(..., attestation=...)`, with parity tests
  asserting one document and identical coverage. Ten tests, proven red against dropping the
  content-hash binding, against attested keys not counting, and against an attested value silently
  overriding a recorded one.

- **`veridex check --out <file>`, and CI recipes that were run before they were written down.**
  `check` could only write its report to stdout, so every CI snippet needed a shell redirect — which
  is not equivalent everywhere: PowerShell's `>` writes UTF-16 with a BOM, and that is not the JSON
  or SARIF any consumer parses. `--out` now works for `--json`, `--sarif` and `--html`, printing
  `wrote <path>` on stderr so it composes with a caller reading stdout, and failing loudly rather
  than losing a report when the path cannot be written.

  [docs/ci-recipes.md](docs/ci-recipes.md) collects what was scattered across the README: the exit
  codes and what `2` does *not* mean, a GitHub Actions gate, uploading SARIF to the Security tab,
  gating on a **regression** instead of on pre-existing findings (the practical way to adopt Veridex
  on a dataset that already has some), the GitLab equivalent, pinning policy in `veridex.toml` or the
  environment, and the three things not to do. Every command in it was run against the demo dataset
  first.

- **Guards for two output properties that were true only by luck.** The machine-readable outputs were
  audited against what actually consumes them — the SARIF 2.1.0 schema (validated externally, and it
  passes) and a browser.

  `every_sarif_result_resolves_to_a_declared_rule` asserts what GitHub code scanning depends on: each
  result's `ruleId` is declared by the driver, each `level` is one of SARIF's four, each result has a
  message and a location. This tree emits rule ids that belong to no registered check —
  `REPORT.REDACTED`, `SCOPE.NARROWED`, `VERIDEX.CHECK_ERRORED` — which is exactly where a dangling
  rule would appear, and the test includes one of them deliberately.

  `a_hostile_name_cannot_script_the_shared_html_report` covers the output built to be *shared*: a
  stream named `<script>…` in an HTML report opened in a browser is stored XSS delivered by the tool
  that was supposed to be checking the data. Every string in that report comes from a dataset Veridex
  did not write. The escaping was already correct in every path, including the rollups added this
  release, and nothing was holding it there.

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
