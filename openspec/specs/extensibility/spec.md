# extensibility Specification

## Purpose

Veridex becomes a standard only if the community can extend it without forking. This capability
defines the plugin surface: third parties can add **checks**, **format adapters**, and
**provenance extractors**, distribute them as **check-packs**, and rely on a stable, versioned
plugin API. Extensibility is what lets a specific robot lab, a hardware vendor, or a benchmark
maintainer encode their own dataset rules on top of the neutral core.

## Requirements

### Requirement: Stable plugin API
Veridex SHALL expose a stable, versioned API for registering custom checks, adapters, and
provenance extractors, from both Rust and Python, without modifying core code. The API version
SHALL be discoverable, and breaking changes SHALL only occur on a major version.

#### Scenario: A third-party check loads without core changes
- **WHEN** a user installs a check-pack that registers custom checks against the current plugin API
- **THEN** those checks appear in the registry and run by category and ID
- **AND** no change to Veridex core is required

#### Scenario: Plugin built for an incompatible API is refused
- **WHEN** a plugin declares a plugin-API version incompatible with the running Veridex
- **THEN** Veridex refuses to load it and reports the version mismatch
- **AND** it does not run with a partially loaded plugin

### Requirement: Custom checks are first-class
Custom checks SHALL carry the same metadata, severities, determinism, and finding structure as
built-in checks, and SHALL be distinguishable in results by a namespaced ID and their source pack.

#### Scenario: A custom finding is attributable to its pack
- **WHEN** a custom check emits a finding
- **THEN** the finding's check ID is namespaced to its pack and the report attributes it to that
  pack
- **AND** the finding meets the same determinism and location requirements as built-in checks

### Requirement: Custom adapters and extractors
Veridex SHALL allow third-party format adapters and provenance extractors that populate the CDM and
provenance model through the same contracts as built-in ones, including declaring supported
versions and recording unmapped fields.

#### Scenario: A vendor adapter maps a proprietary format into the CDM
- **WHEN** a vendor ships an adapter for their capture format
- **THEN** datasets in that format are validated and certified through the standard pipeline
- **AND** unmapped proprietary fields are recorded, not silently dropped

### Requirement: Check-pack distribution and provenance
Check-packs SHALL be distributable as versioned packages, and a verdict SHALL record which packs
(name and version) contributed checks, so a result is reproducible against the same pack set.

#### Scenario: A verdict pins its check-packs
- **WHEN** a run uses checks from external packs
- **THEN** the verdict records each pack's name and version
- **AND** re-running with the same core and pack versions reproduces the verdict

### Requirement: Untrusted plugins cannot forge trust
A plugin SHALL NOT be able to sign certificates, alter another check's findings, or mutate the
dataset. Certificate signing SHALL remain solely under Veridex's signing path and the issuer's key.

#### Scenario: A plugin cannot issue a certificate
- **WHEN** a loaded plugin attempts to produce or sign a certificate
- **THEN** the attempt is denied
- **AND** only Veridex's signing path, invoked by the issuer, can produce a certificate
