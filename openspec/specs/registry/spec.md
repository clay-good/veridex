# registry Specification

> **Status: Roadmap (post-core).** This capability captures the full vision but is deliberately
> **not** part of the open-source core MVP. It gets its own change proposal once the core has
> adoption. It is specified here so the north star is complete, not because it is next.

## Purpose

The registry is how Veridex certificates become a **standard that compounds**: a hosted service
where certificates are published, looked up by dataset content hash, verified by anyone, revoked or
superseded when needed, and surfaced as README/Hub **badges**. This is also the natural commercial
layer (hosted verification, continuous monitoring, enterprise attestation) sitting atop the MIT
core — the open-standard / commercial-service split.

The registry is optional: every Veridex certificate remains fully verifiable **offline** without it.

## Requirements

### Requirement: Publish and look up certificates
The registry SHALL let an issuer publish a certificate and let anyone look it up by dataset content
hash or dataset identity, returning the certificate and its verification status.

#### Scenario: Look up a dataset's certificate by content hash
- **WHEN** a user queries the registry with a dataset's CDM content hash
- **THEN** the registry returns any published certificate bound to that hash and its status
- **AND** the certificate remains independently verifiable offline

### Requirement: Public verification endpoint
The registry SHALL offer verification of a submitted certificate and dataset reference without
requiring the caller to run Veridex locally, returning the same verdict offline verification would.

#### Scenario: Hosted verification matches offline verification
- **WHEN** a certificate is verified via the registry and again offline with the same inputs
- **THEN** both return the same validity result and issuer identity

### Requirement: Revocation and supersession
The registry SHALL support marking a certificate revoked or superseded by a newer certificate for
the same dataset lineage, and lookups SHALL surface that status. Offline certificates SHALL carry
enough identity to be matched against registry revocation status.

#### Scenario: A revoked certificate is flagged on lookup
- **WHEN** an issuer revokes a certificate and a user later looks it up
- **THEN** the registry reports it revoked, with the reason if provided
- **AND** any superseding certificate is linked

### Requirement: Badges
The registry SHALL render a status badge for a dataset (e.g. grade and verification state) suitable
for embedding in a README or dataset card, backed by the current registry status.

#### Scenario: A badge reflects current status
- **WHEN** a dataset's certificate is published and later revoked
- **THEN** the badge reflects the current status on refresh
- **AND** the badge links back to the certificate record

### Requirement: Neutrality and offline independence preserved
The registry SHALL NOT become a required dependency for validation or certification, and SHALL host
certificates across all supported source formats without privileging any one. Offline verification
SHALL never require the registry.

#### Scenario: Core works with no registry
- **WHEN** a team validates, certifies, and verifies entirely offline
- **THEN** all operations succeed without contacting the registry
- **AND** publishing to the registry is an optional, separate step
