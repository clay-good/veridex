# validation-engine Specification

## Purpose

The validation engine is the framework that runs checks over a CDM instance and produces a
deterministic **verdict**. It owns the check registry, severities, categories, selection/config,
and the shape of results. It knows nothing about specific checks (those live in `checks-catalog`)
and nothing about source formats (those live in `dataset-ingestion`). Its job is to run a declared
set of checks reproducibly and aggregate their findings.

## Requirements

### Requirement: Check registry and plugin surface
The engine SHALL maintain a registry of checks, each identified by a stable, unique check ID, and
SHALL allow checks to be added without modifying the engine. Each check SHALL declare its ID,
human-readable title, category, default severity, and the CDM scope it applies to (dataset,
episode, or stream).

#### Scenario: A newly registered check runs without engine changes
- **WHEN** a check is registered with a unique ID and required metadata
- **THEN** the engine includes it in runs by its category and ID
- **AND** no change to engine code is required to execute it

#### Scenario: Duplicate check IDs are rejected
- **WHEN** two checks register the same check ID
- **THEN** the engine refuses to start and names the conflicting ID
- **AND** no run proceeds with an ambiguous registry

### Requirement: Severities and categories
Every finding SHALL carry a severity of `error`, `warning`, or `info`, and every check SHALL
belong to exactly one category (at minimum: structural, temporal, statistical, semantic, video,
provenance). Severities SHALL determine verdict status and process exit codes.

#### Scenario: Severity drives verdict status
- **WHEN** a run produces at least one `error` finding
- **THEN** the verdict status is `fail`
- **AND** when the worst finding is a `warning`, the status is `pass-with-warnings`, and with only
  `info` or no findings, the status is `pass`

### Requirement: Selective and configurable runs
The engine SHALL let callers select which checks or categories run, disable specific checks, and
override severities via configuration, without editing check code. Configuration SHALL be explicit
and recorded in the run result so a verdict is reproducible from it.

#### Scenario: A caller runs only temporal checks
- **WHEN** a caller requests only the `temporal` category
- **THEN** only temporal checks execute
- **AND** the run result records that the run was scoped to `temporal`

#### Scenario: A severity override is honored and recorded
- **WHEN** configuration downgrades a specific check from `error` to `warning`
- **THEN** that check's findings are reported as `warning`
- **AND** the override is recorded in the run result and reflected in the certificate's inputs

### Requirement: Deterministic, ordered verdicts
Given the same CDM, the same Veridex version, and the same configuration, the engine SHALL produce
byte-identical results, with findings in a stable, defined order independent of parallel execution.

#### Scenario: Parallel execution does not change output
- **WHEN** the same run is executed twice, once single-threaded and once multi-threaded
- **THEN** the two verdicts are byte-identical, including finding order
- **AND** both share the same result content hash

### Requirement: Findings are precise and actionable
Each finding SHALL identify the check ID, severity, the exact CDM location it concerns (episode
index, stream name, frame or time range where applicable), a human-readable message, and a stable
machine-readable code. Findings SHALL carry enough detail to locate the issue without rerunning.

#### Scenario: A finding pinpoints the offending episode and stream
- **WHEN** a check fails on episode 42's `observation.images.wrist` stream between two frames
- **THEN** the finding names check ID, episode 42, that stream, and the frame/time range
- **AND** a user can navigate to the exact location from the finding alone

### Requirement: Fault isolation
A check that errors internally SHALL NOT abort the run; the engine SHALL record the check as
`errored` and continue. A verdict SHALL clearly distinguish "check ran and found problems" from
"check failed to run."

#### Scenario: One check crashing does not sink the run
- **WHEN** a single check raises an internal error mid-run
- **THEN** the engine marks that check `errored`, continues the remaining checks, and completes
- **AND** the verdict lists the errored check separately from data findings

### Requirement: Run metadata for reproducibility
Every run SHALL record Veridex version, the CDM content hash, the effective configuration, the set
of checks executed with their versions, and the counts by severity. This metadata SHALL be
sufficient to reproduce the verdict.

#### Scenario: A verdict carries everything needed to reproduce it
- **WHEN** a run completes
- **THEN** its result includes Veridex version, CDM content hash, effective config, and executed
  check list with versions
- **AND** re-running with that recorded metadata reproduces the identical verdict

### Requirement: Incremental and cached validation
The engine SHALL support revalidating only the parts of a dataset that changed since a prior run,
using the CDM content structure to skip unchanged episodes/streams, while producing a verdict
identical to a full run over the current bytes. Incremental mode SHALL be an optimization only and
SHALL never change the verdict.

#### Scenario: Incremental run matches a full run
- **WHEN** a dataset is revalidated incrementally after a subset of episodes changed
- **THEN** the resulting verdict is identical to a full revalidation of the current dataset
- **AND** unchanged episodes are not needlessly reprocessed

### Requirement: Progress and cancellation
For long runs, the engine SHALL report progress and SHALL support cancellation that leaves no
partial or misleading verdict. A cancelled run SHALL be clearly marked incomplete.

#### Scenario: A cancelled run does not emit a pass
- **WHEN** a long validation run is cancelled partway
- **THEN** no verdict claims completion
- **AND** the result is marked incomplete/cancelled
