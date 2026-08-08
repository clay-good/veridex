# validation-engine — MVP delta

Scopes the north-star `validation-engine` to v0.1: registry, severities/categories, deterministic
verdicts, fault isolation, and run metadata. Full config surface may be minimal but must be
recorded.

## ADDED Requirements

### Requirement: MVP registry and deterministic verdict
Veridex SHALL run a registry of checks with unique IDs and produce a deterministic, stably ordered
verdict for a given CDM, Veridex version, and configuration, with findings ordered independently of
parallelism.

#### Scenario: Same CDM yields byte-identical verdict
- **WHEN** the same CDM is validated twice with identical version and config
- **THEN** the two verdicts are byte-identical including finding order
- **AND** they share a result content hash

### Requirement: MVP severities and status
Findings SHALL carry `error`/`warning`/`info`; the verdict status SHALL be `fail` on any error,
`pass-with-warnings` on warnings, and `pass` otherwise.

#### Scenario: An error yields fail status
- **WHEN** a run produces one or more `error` findings
- **THEN** the verdict status is `fail`

### Requirement: MVP fault isolation and run metadata
A check that errors internally SHALL be recorded as `errored` without aborting the run, and every
verdict SHALL record Veridex version, CDM content hash, effective config, and executed checks with
versions.

#### Scenario: A crashing check is isolated and reported
- **WHEN** one check raises internally during a run
- **THEN** it is recorded `errored`, remaining checks complete, and the verdict lists it separately
  from data findings
