# reporting Specification

## Purpose

Reporting turns a verdict into output humans read and machines consume. It is distinct from the
certificate: a **report** is diagnostics (what's wrong, where, how to fix it), while a
**certificate** is a signed attestation. Reporting SHALL cover terminal, JSON, HTML, and SARIF
outputs, roll findings up to dataset/episode/stream summaries, and support diffing two verdicts.

## Requirements

### Requirement: Multiple output formats
Veridex SHALL render a verdict as: a human-readable terminal report; machine-readable JSON; a
self-contained HTML report; and SARIF for code-scanning/CI integrations. All formats SHALL derive
from the same verdict so they never disagree.

#### Scenario: All formats reflect the same findings
- **WHEN** a verdict is rendered to terminal, JSON, HTML, and SARIF
- **THEN** the set of findings, severities, and counts is identical across all four
- **AND** no format adds or omits a finding present in the verdict

### Requirement: Rollup summaries
Reports SHALL summarize findings at dataset, episode, and stream scope, including counts by
severity and category and a ranked list of the worst episodes, so a user can triage without
reading every finding.

#### Scenario: Worst episodes are surfaced first
- **WHEN** a dataset has findings spread unevenly across episodes
- **THEN** the report ranks episodes by severity and count and lists the worst ones first
- **AND** each entry links to that episode's findings

### Requirement: Machine-readable output is stable
The JSON and SARIF schemas SHALL be versioned and stable within a major version, so CI pipelines
can depend on them. Schema changes SHALL be additive within a major version.

#### Scenario: CI parses output across a minor upgrade
- **WHEN** a CI pipeline consumes Veridex JSON and Veridex is upgraded within the same major
  version
- **THEN** previously parsed fields remain present with unchanged meaning
- **AND** any new fields are additive

<!-- Implemented in v0.1: `veridex diff` over two report JSONs (introduced/resolved/unchanged
     findings + score delta); see crates/veridex-core/src/diff.rs. -->

### Requirement: Verdict diffing
Veridex SHALL compare two verdicts for the same dataset lineage and report what changed — findings
introduced, resolved, or unchanged, and score movement — so users can see whether a dataset
improved or regressed.

#### Scenario: A diff shows introduced and resolved findings
- **WHEN** two verdicts for successive versions of a dataset are diffed
- **THEN** the diff lists findings newly introduced, findings resolved, and the change in trust
  score
- **AND** unchanged findings are grouped separately

### Requirement: Reports never leak beyond what is shared
When a report is produced for sharing, Veridex SHALL support redacting sample values and paths so a
shared report reveals findings and structure without exposing dataset contents.

#### Scenario: A shared report omits raw sample values
- **WHEN** a report is generated in shareable mode
- **THEN** it includes findings, locations, and counts but omits raw sample values and absolute
  local paths
- **AND** the redaction is stated in the report

### Requirement: Remediation guidance
Reports SHALL, for each finding, include the check's documented risk and suggested remedy, so a
report tells the user not only what is wrong but what to do about it.

#### Scenario: A finding in the report carries a remedy
- **WHEN** a report lists a finding
- **THEN** it includes the training-time risk and a suggested remedy for that check
- **AND** the guidance matches the check's documented rationale

### Requirement: Coverage disclosure
Every report SHALL disclose what was and was not covered: checks/categories run vs. skipped, whether
coverage was a sample or the full dataset, and any unmapped source fields, so a clean report is
never mistaken for exhaustive.

#### Scenario: A report states its coverage limits
- **WHEN** a report is produced after a sampled run that skipped a category
- **THEN** the report states the sample coverage and the skipped category
- **AND** it lists any unmapped source fields
