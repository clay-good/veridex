# configuration Specification

## Purpose

Configuration makes Veridex's behavior explicit, portable, and reproducible. It governs which
checks run, at what severities and tolerances, what thresholds define pass/fail, and which output
and signing options apply. Configuration is a first-class input to a verdict: two teams sharing a
config file get the same judgment, and every verdict records the effective configuration it ran
under.

## Requirements

### Requirement: Config file and precedence
Veridex SHALL support a project config file (e.g. `veridex.toml`) and SHALL apply configuration in
a defined precedence order: built-in defaults, then config file, then environment, then explicit
command-line flags, with later sources overriding earlier ones. The effective merged configuration
SHALL be recorded in the verdict.

#### Scenario: Command-line overrides the config file
- **WHEN** a config file sets a tolerance and a command-line flag sets a different tolerance
- **THEN** the command-line value is used
- **AND** the verdict records the effective merged configuration, not just the file

### Requirement: Policy profiles
Veridex SHALL provide named policy profiles that bundle check selection, severities, and thresholds,
and SHALL allow user-defined profiles. A profile SHALL be selectable by name and fully expandable to
its concrete settings.

A profile SHALL only ever *tighten* a threshold relative to the configuration it is applied to.
There is therefore no `lenient` profile, and `--profile lenient` (or `relaxed`, or `permissive`) is
refused by name with that reason rather than reported as an unknown profile: a profile that loosens
a threshold raises the score without changing the data, which is the one thing a shareable verdict
must not let a flag do. Implemented today: `standard`, `strict`, and `world-model-ready`. See
[docs/profiles.md](../../../docs/profiles.md).

#### Scenario: A strict CI profile fails on warnings
- **WHEN** a pipeline selects the `strict` profile
- **THEN** warnings are treated as failures per that profile's thresholds
- **AND** the resolved profile settings appear in the verdict

### Requirement: Tolerances and thresholds are explicit
All numeric tolerances (e.g. clock-skew tolerance, rate-deviation tolerance) and pass/fail
thresholds SHALL be configurable and SHALL be recorded with the finding or verdict they affect, so a
result is never dependent on an unstated constant.

#### Scenario: A skew finding names the tolerance applied
- **WHEN** a cross-stream skew finding is emitted
- **THEN** the finding records the tolerance value that was applied
- **AND** changing that tolerance changes the finding reproducibly

### Requirement: Configuration validation
Veridex SHALL validate configuration before running and SHALL reject unknown keys, invalid values,
or references to unknown checks/profiles with a clear error, rather than silently ignoring them.

#### Scenario: An unknown check ID in config is rejected
- **WHEN** a config references a check ID that does not exist
- **THEN** Veridex reports the unknown ID and exits without running
- **AND** it does not silently drop the setting
