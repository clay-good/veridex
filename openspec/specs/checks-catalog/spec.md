# checks-catalog Specification

## Purpose

This capability enumerates the concrete **families of checks** Veridex runs over a CDM. The
`validation-engine` provides the framework; this catalog defines what is actually checked and why.
Checks are grouped into six categories. Each requirement below defines a family; individual checks
within a family carry stable IDs (e.g. `TEMPORAL.CLOCK_SKEW`) assigned in `design.md`.

The catalog's design bias: prioritize the failure modes that **silently corrupt training** and are
**invisible in single-format tools** — above all, cross-stream time-synchronization and episode
integrity, which are the wedge against the incumbent linter.

## Requirements

### Requirement: Structural integrity checks
Veridex SHALL check that a dataset's declared structure matches its actual contents: metadata and
data agree on episode boundaries and frame counts; declared streams exist and have consistent
dtypes and shapes; no orphaned or missing shards/files; and no degenerate episodes (e.g.
zero-length or single-frame) unless explicitly permitted.

#### Scenario: Corrupt episode boundaries are caught, not silently loaded
- **WHEN** episode-length metadata is corrupted so cumulative boundaries are wrong
- **THEN** a structural check fails with an `error` naming the affected episodes
- **AND** the finding explains that frames would otherwise load under the wrong episode during
  training

#### Scenario: Declared stream is missing its data
- **WHEN** metadata declares a stream that has no corresponding data or video file
- **THEN** a structural check fails and identifies the missing stream and location

### Requirement: Temporal and synchronization checks
Veridex SHALL check the time base of a dataset: per-stream timestamp monotonicity; sampling-rate
conformance to the declared rate; gaps and dropouts; and — the differentiator — **cross-stream
alignment**, verifying that streams sharing an episode are coherently aligned on a common timeline
within a stated tolerance, accounting for per-stream clocks and declared latency.

#### Scenario: Cross-stream clock skew is detected
- **WHEN** a camera stream and a proprioception stream in the same episode drift apart beyond the
  configured tolerance
- **THEN** a temporal check fails with an `error` reporting the measured skew and the frames/time
  range where it exceeds tolerance
- **AND** the finding notes the training risk (observation/action mismatch)

#### Scenario: Timestamp non-monotonicity is caught
- **WHEN** a stream's timestamps decrease or repeat within an episode
- **THEN** a temporal check fails and names the stream and offending frame indices

#### Scenario: Sampling rate deviates from declared
- **WHEN** a stream's measured rate deviates from its declared rate beyond tolerance
- **THEN** a temporal check reports the deviation for that stream

### Requirement: Statistical checks
Veridex SHALL check per-stream statistics: recorded stats (mean/std/min/max) match recomputed
values; action and state values fall within sane, declared, or learned ranges; and it SHALL flag
degenerate distributions (constant streams, saturated actuators, extreme outliers).

#### Scenario: Stored statistics disagree with the data
- **WHEN** a dataset's stored per-stream statistics do not match values recomputed from the data
- **THEN** a statistical check fails and reports the mismatch per stream

#### Scenario: Stored statistics are internally inconsistent
- **WHEN** a stream's stored statistics contradict each other (inverted min/max, negative std,
  non-finite values, or a mean outside the recorded [min, max] range)
- **THEN** a statistical check fails and reports the specific inconsistency per stream

#### Scenario: Actuator saturation is flagged
- **WHEN** an action stream is pinned at its limit for a sustained fraction of an episode
- **THEN** a statistical check emits a finding describing the saturation and its extent

### Requirement: Semantic checks
Veridex SHALL check label and annotation quality: task strings are present and meaningful (not
empty or degenerate placeholders); camera/stream keys are unambiguous; and — riding the annotation
wave rather than fighting it — where language annotations exist (e.g. LeRobot `language_events` /
`language_persistent`), it SHALL **verify** their integrity (timestamp alignment, per-frame/per-camera
uniqueness, valid structure) without producing or editing them.

#### Scenario: Empty or placeholder task labels are flagged
- **WHEN** episodes carry empty task strings or degenerate placeholders (e.g. "Hold", "Up")
- **THEN** a semantic check emits findings identifying those episodes
- **AND** the finding distinguishes "missing" from "present but low-information"

#### Scenario: Language annotations are verified, never modified
- **WHEN** a dataset carries language-event annotations
- **THEN** semantic checks verify their timestamp alignment and structural validity
- **AND** Veridex never writes, generates, or alters any annotation

### Requirement: Video and media checks
Veridex SHALL check that media streams are decodable, that frame counts match declared lengths and
the paired data streams, that resolution/codec meet declared expectations, and that video/data fps
are consistent.

#### Scenario: Undecodable or truncated video is caught
- **WHEN** a video stream cannot be decoded or its frame count differs from the declared episode
  length
- **THEN** a video check fails and reports the stream and the discrepancy

### Requirement: Provenance completeness checks
Veridex SHALL check the presence and internal consistency of provenance: whether sensor/device,
clock, calibration, operator/annotator, license, and upstream-dataset lineage are recorded, and
whether recorded provenance is internally consistent. Missing provenance SHALL be surfaced as a
finding, not treated as acceptable-by-default.

#### Scenario: Absent license and sensor provenance are surfaced
- **WHEN** a dataset records no license and no sensor/device identifiers
- **THEN** provenance-completeness checks emit findings naming each missing element
- **AND** these gaps are reflected in the certificate's "unknown" section

#### Scenario: Inconsistent lineage is flagged
- **WHEN** recorded provenance is internally contradictory (e.g. a calibration references a device
  not present in the dataset)
- **THEN** a provenance check fails and describes the contradiction

### Requirement: Duplicate and near-duplicate detection
Veridex SHALL detect exact-duplicate and near-duplicate episodes within a dataset (e.g. re-uploaded
recordings or trivially perturbed copies), since redundancy inflates dataset size without signal and
can bias training. Findings SHALL identify the duplicate groups.

#### Scenario: Re-uploaded episodes are grouped as duplicates
- **WHEN** a dataset contains episodes that are exact or near duplicates of one another
- **THEN** a check reports the duplicate groups and their episode indices
- **AND** the finding notes the redundancy/bias risk

### Requirement: Privacy and safety checks
Veridex SHALL provide checks that flag likely personally identifiable information in media streams
(e.g. faces or readable text) and other safety-relevant content, so producers can review before
publishing. These checks SHALL only flag, never redact or modify data, and SHALL be clearly
probabilistic.

#### Scenario: Likely faces in camera frames are flagged for review
- **WHEN** a video stream likely contains human faces
- **THEN** a privacy check emits a finding indicating likely PII and where
- **AND** Veridex does not alter the frames and marks the finding as a probabilistic signal

### Requirement: Every check documents its rationale and remedy
Each catalog check SHALL ship with a stable ID, the training-time risk it addresses, and a
suggested remedy, so findings teach rather than merely flag.

#### Scenario: A finding links to its rationale
- **WHEN** any check emits a finding
- **THEN** the finding references the check's documented risk and suggested remedy
- **AND** the documentation is stable across releases for a given check ID
