# cli — MVP delta

Scopes the north-star `cli` to v0.1: the five commands, autodetect with override, CI exit codes,
non-mutation, and Python parity. HTML/SARIF and diffing are deferred to reporting changes.

## ADDED Requirements

### Requirement: MVP command surface
The CLI SHALL provide `veridex check`, `veridex certify`, `veridex verify`, `veridex provenance`,
and `veridex inspect`, each accepting a local path or a supported remote reference, with terminal
and JSON output.

#### Scenario: Check-to-certify-to-verify round trip
- **WHEN** a user runs `veridex check`, then `veridex certify`, then `veridex verify` on a dataset
- **THEN** check reports a verdict, certify issues a signed certificate bound to the dataset, and
  verify confirms it offline
- **AND** exit codes reflect each command's outcome

### Requirement: MVP autodetect, exit codes, non-mutation
The CLI SHALL autodetect LeRobot v3 vs. MCAP with a `--format` override, refuse to guess on
ambiguity, return distinct documented exit codes for pass / pass-with-warnings / fail / tool-error
with a configurable failure threshold, and never modify the target dataset.

#### Scenario: CI fails on error verdict without touching data
- **WHEN** `veridex check` runs in CI on a dataset with an `error` finding
- **THEN** the process exits with the documented failure code
- **AND** the dataset bytes are unchanged afterward

#### Scenario: Ambiguous format is not guessed
- **WHEN** a source matches both adapters and no `--format` is given
- **THEN** the CLI reports the ambiguity and exits without a verdict

### Requirement: MVP Python parity
The `veridex` Python package SHALL expose the five operations with behavior and verdicts identical
to the CLI.

#### Scenario: Python and CLI agree
- **WHEN** the same dataset is checked via CLI and via the Python API with the same config
- **THEN** verdicts, trust scores, and content hashes are identical
- **AND** a certificate issued by one verifies via the other
