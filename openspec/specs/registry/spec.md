# registry Specification

> **Status: built as a static registry in v0.1; a hosted service remains roadmap.** Every
> requirement below is implemented against a **directory** — `index.json` plus one record per
> dataset content hash — which any static host can serve and `veridex registry` reads over a local
> path. That covers publish, look-up by hash or dataset identity, revocation and supersession
> status, badges, and verification that is the same computation as offline. What stays roadmap is
> the *service* around it: an HTTP API, accounts and authorization, continuous monitoring, and the
> commercial layer the purpose below describes. Those are deployment, not core, and the core must
> never require them — which is the last requirement here and is asserted as a test.

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

<!-- Implemented in v0.1 as a static registry: `veridex registry publish --registry <dir>
     --certificate <cert.json>` writes `<dir>/by-hash/<cdm hash>.json` and refreshes `<dir>/index.json`,
     and `veridex registry lookup` finds a record by content hash or by dataset identity. A lookup by
     *name* answers with every certificate published under it, newest first, because a dataset id is
     not unique over time — re-recording produces different bytes under the same name, and a single
     answer would hide the others.

     The record carries the signed certificate **verbatim**, so a lookup hands back the document
     rather than a summary of one, and the consumer verifies it offline exactly as if it had arrived
     by email. Publication verifies first, with the same function `veridex verify` calls: a registry
     that accepts an unverifiable document is a place where one looks verified. See
     crates/veridex-core/src/registry.rs and crates/veridex-core/tests/registry.rs. -->

### Requirement: Public verification endpoint
The registry SHALL offer verification of a submitted certificate and dataset reference without
requiring the caller to run Veridex locally, returning the same verdict offline verification would.

#### Scenario: Hosted verification matches offline verification
- **WHEN** a certificate is verified via the registry and again offline with the same inputs
- **THEN** both return the same validity result and issuer identity

<!-- Implemented in v0.1, and the implementation is the point: `registry::verify_record` calls the
     same `certificate::verify` that `veridex verify` does, with the same transplant and issuer
     checks. There is no second code path to disagree, and a test asserts the two results are equal
     rather than merely both successful. A *hosted* endpoint is then deployment — the computation it
     would run is this one. -->

### Requirement: Revocation and supersession
The registry SHALL support marking a certificate revoked or superseded by a newer certificate for
the same dataset lineage, and lookups SHALL surface that status. Offline certificates SHALL carry
enough identity to be matched against registry revocation status.

#### Scenario: A revoked certificate is flagged on lookup
- **WHEN** an issuer revokes a certificate and a user later looks it up
- **THEN** the registry reports it revoked, with the reason if provided
- **AND** any superseding certificate is linked

<!-- Implemented in v0.1. `veridex registry revoke --reason` records the issuer's withdrawal beside
     the certificate, and a lookup reports it and exits 20 — found, and not the issuer's current
     word, which a release gate should not wave through. The certificate itself still verifies
     afterwards and the rendered answer says so: bytes that have left cannot be recalled, and a trust
     tool that implied otherwise would be lying about the one thing it exists to be honest about.

     Supersession is not a second mechanism: publishing a newer certificate for the same dataset
     identity relates the two with `certificate::relate` — the same code `veridex supersedes` uses —
     and marks the older record `superseded`, by content hash, keeping it. A registry whose history
     can be edited is a registry whose claims cannot be checked. -->

### Requirement: Badges
The registry SHALL render a status badge for a dataset (e.g. grade and verification state) suitable
for embedding in a README or dataset card, backed by the current registry status.

#### Scenario: A badge reflects current status
- **WHEN** a dataset's certificate is published and later revoked
- **THEN** the badge reflects the current status on refresh
- **AND** the badge links back to the certificate record

<!-- Implemented in v0.1. `veridex registry badge` renders an SVG from the record: the grade and
     score in the verdict's colour while the certificate is current, and a grey `revoked` or
     `superseded` otherwise — a withdrawn certificate must not show a green grade to someone reading
     a README at a glance. It is rendered locally rather than fetched from a badge service, because
     an image request to a third party's host tells that host who is reading which dataset's page.
     The record it is rendered from lives at a stable path (`by-hash/<hash>.json`), which is what a
     README links to. -->

### Requirement: Neutrality and offline independence preserved
The registry SHALL NOT become a required dependency for validation or certification, and SHALL host
certificates across all supported source formats without privileging any one. Offline verification
SHALL never require the registry.

<!-- Implemented in v0.1, and asserted rather than claimed: `tests/registry.rs` runs a check, an
     issuance and an offline verification with no registry anywhere and shows the certificate a
     registry would hold is the same document either way. Nothing in `check`, `certify` or `verify`
     reads a registry; publishing is an extra step a producer takes, and a consumer who never looks
     loses nothing a certificate gave them. -->

#### Scenario: Core works with no registry
- **WHEN** a team validates, certifies, and verifies entirely offline
- **THEN** all operations succeed without contacting the registry
- **AND** publishing to the registry is an optional, separate step
