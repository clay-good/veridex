# cli Specification

## Purpose

The CLI is the primary way people run Veridex, and the surface that must feel native in a
robot-data workflow and in CI. It exposes verification, certification, provenance, and inspection
over a single `veridex` binary, with output modes and exit codes designed for both humans and
pipelines. A matching Python API mirrors the CLI for the Python-first ecosystem.

## Requirements

### Requirement: Command surface
The CLI SHALL provide, at minimum: `veridex check` (run validation and report), `veridex certify`
(issue a signed certificate), `veridex verify` (verify a certificate against a dataset and key),
`veridex provenance` (extract/emit provenance as Croissant/PROV), `veridex inspect` (summarize a
dataset's CDM without full validation), `veridex diff` (compare two verdicts), and `veridex watch`
(continuously validate a dataset as it is being recorded, read-only). Each SHALL accept a dataset
path or a supported remote reference.

#### Scenario: Check runs against a local dataset
- **WHEN** a user runs `veridex check <path>`
- **THEN** Veridex ingests the dataset, runs the default checks, and prints a terminal report
- **AND** the exit code reflects the verdict status

#### Scenario: Verify confirms a certificate offline
- **WHEN** a user runs `veridex verify <dataset> --certificate <cert> --key <pub>` with no network
- **THEN** Veridex reports whether the certificate is valid and bound to that dataset
- **AND** it requires no network access

#### Scenario: Watch validates during recording without mutating
- **WHEN** a user runs `veridex watch <path>` while a dataset is being recorded
- **THEN** Veridex re-validates incrementally and surfaces new findings as they appear
- **AND** it never modifies the dataset being recorded

### Requirement: Config discovery and reproducibility flags
The CLI SHALL discover a project config file by convention, accept an explicit config path, allow
selecting a policy profile, and support a flag to print the effective merged configuration. The
effective configuration SHALL be recorded in outputs.

#### Scenario: The CLI prints the effective configuration
- **WHEN** a user requests the effective configuration for a run
- **THEN** the CLI prints the merged result of defaults, file, environment, and flags
- **AND** that same configuration is recorded in the verdict

### Requirement: Format autodetection with override
The CLI SHALL autodetect the source format when possible and SHALL accept an explicit
`--format` override. When detection is ambiguous, it SHALL ask the user to specify rather than
guess.

#### Scenario: Ambiguous format is not silently guessed
- **WHEN** a dataset could match more than one adapter and no `--format` is given
- **THEN** the CLI reports the ambiguity and the candidate formats
- **AND** it exits without emitting a verdict until the format is specified

### Requirement: CI-friendly exit codes and output
The CLI SHALL return distinct, documented exit codes for pass, pass-with-warnings, fail, and
tool-error, and SHALL support machine-readable output selectable by flag (`--json`, `--sarif`, `--html`; `--format`
is the adapter override, not an output selector).
Thresholds for what counts as failure SHALL be configurable.

#### Scenario: CI fails the build on an error verdict
- **WHEN** `veridex check` runs in CI and the verdict status is `fail`
- **THEN** the process exits with the documented failure code
- **AND** `--format sarif` emits results a code-scanning system can ingest

#### Scenario: Warning threshold is configurable
- **WHEN** a pipeline configures warnings to fail the build
- **THEN** a `pass-with-warnings` verdict returns the failure exit code
- **AND** the applied threshold is stated in the output

### Requirement: Non-mutating by contract
No CLI command SHALL modify the target dataset. Commands SHALL only read datasets and write
Veridex outputs (reports, certificates, provenance exports) to caller-specified locations.

#### Scenario: A check leaves the dataset untouched
- **WHEN** any `veridex` command runs against a dataset
- **THEN** the dataset's bytes are unchanged afterward
- **AND** all outputs are written only where the caller directed

### Requirement: Parity with the Python API
The Python package SHALL expose the same operations as the CLI (check, certify, verify,
provenance, inspect) with equivalent behavior and identical verdicts, so ecosystem users can call
Veridex in-process.

#### Scenario: Python and CLI produce the same verdict
- **WHEN** the same dataset is checked via `veridex check` and via the Python API with the same
  configuration
- **THEN** the two verdicts are identical, including trust score and content hash
- **AND** any certificate issued is interchangeable between the two paths
