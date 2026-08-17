# trust-certificate Specification

## Purpose

The trust certificate is Veridex's flagship artifact — the thing papers cite, hubs display, and
buyers demand. It is a portable, signed statement about a specific dataset: what was checked, what
was found, its trust score and grade, and its provenance coverage (known / asserted / unknown). It
binds to the dataset's CDM content hash so it cannot be silently transplanted onto different data,
and it verifies **offline** against a public key. Signing follows Invariant's signed-verdict pattern
in Veridex's own module: a detached Ed25519 signature over domain-separated canonical bytes (design
D6a). It is not a COSE or JWS document.

The certificate is a *nutrition label*, not a seal of approval: it states facts (checks run,
findings, score, provenance coverage) rather than blessing a dataset as "good."

## Requirements

### Requirement: Certificate content
A Veridex certificate SHALL record at minimum: the dataset identity and CDM content hash; the
Veridex version and effective configuration; the full list of checks executed with their versions;
findings summarized by severity and category; the trust score and grade; the provenance coverage
summary (known/asserted/unknown); and issuance metadata (issuer key ID, timestamp supplied by the
caller). It SHALL be serialized as JSON.

#### Scenario: A certificate states what was and was not checked
- **WHEN** a certificate is issued for a dataset
- **THEN** it lists every check that ran and every check category that was skipped
- **AND** a reader can tell exactly what the certificate does and does not cover

### Requirement: Binding to dataset content
A certificate SHALL bind to the CDM content hash of the exact dataset it was issued for.
Verification SHALL fail if the certificate is presented against a dataset whose content hash
differs.

#### Scenario: A transplanted certificate fails verification
- **WHEN** a certificate issued for dataset A is checked against dataset B
- **THEN** verification fails because the bound content hash does not match B
- **AND** the failure states that the certificate does not correspond to the presented dataset

### Requirement: Signing and offline verification
A certificate SHALL be cryptographically signed by the issuer and SHALL be verifiable offline
against the issuer's public key, with no network dependency. Signing SHALL use a detached Ed25519
signature over domain-separated canonical bytes, so that a verifier can reimplement verification
without a COSE, JOSE, or Veridex dependency.

#### Scenario: A certificate verifies offline
- **WHEN** a signed certificate and the issuer's public key are provided with no network access
- **THEN** verification confirms the signature and the bound content hash
- **AND** it reports the issuer key ID and issuance timestamp

#### Scenario: Tampering is detected
- **WHEN** any field of a signed certificate is altered after issuance
- **THEN** signature verification fails
- **AND** the altered certificate is rejected

### Requirement: Trust score and grade
Veridex SHALL compute a trust score and a letter grade from the verdict and provenance coverage
using a **documented, versioned, deterministic** rubric. The certificate SHALL record the rubric
version. Score comparisons across datasets SHALL only be valid within the same rubric version.

#### Scenario: The same dataset always scores the same
- **WHEN** the same dataset is certified twice with the same Veridex version and rubric
- **THEN** the trust score, grade, and rubric version are identical
- **AND** the certificate content hash is identical

#### Scenario: Rubric version is explicit
- **WHEN** a certificate reports a trust score
- **THEN** it names the rubric version used
- **AND** tooling refuses to compare scores computed under different rubric versions without saying
  so

### Requirement: Provenance coverage in the certificate
The certificate SHALL summarize provenance coverage, explicitly separating known, asserted, and
unknown elements, so that a high check score cannot mask missing provenance.

#### Scenario: Clean data with no provenance is not misrepresented
- **WHEN** a dataset passes all data checks but records no provenance
- **THEN** the certificate shows strong check results and an explicit provenance-coverage gap
- **AND** the overall presentation does not imply the dataset is fully trustworthy

### Requirement: Certificate is a statement of fact, not approval
A certificate SHALL present findings and coverage as facts and SHALL NOT assert that a dataset is
fit for any particular purpose. Any pass/fail status SHALL be tied to explicit, stated thresholds.

#### Scenario: Pass status names its thresholds
- **WHEN** a certificate reports a pass status
- **THEN** it states the thresholds that were applied to reach it
- **AND** it does not claim general fitness beyond those thresholds

### Requirement: Certificate schema versioning
Every certificate SHALL declare a schema version, and the schema SHALL evolve additively within a
major version so that older certificates remain parseable and verifiable by newer Veridex releases.

#### Scenario: An older certificate still verifies
- **WHEN** a certificate issued under an earlier schema minor version is verified by a newer Veridex
- **THEN** verification succeeds and all recorded fields are readable
- **AND** the schema version is reported

### Requirement: Revocation and supersession identity
A certificate SHALL carry stable identity (issuer, dataset lineage, content hash, issuance metadata)
sufficient to be matched against later revocation or supersession, so that a certificate can be
superseded by a newer one for the same dataset lineage.

#### Scenario: A newer certificate supersedes an older one
- **WHEN** a new certificate is issued for the same dataset lineage as an earlier one
- **THEN** the two can be related as supersede/superseded by their recorded identity
- **AND** verification of each remains independently valid offline

### Requirement: Human-readable nutrition label
Veridex SHALL be able to render a certificate as a concise, human-readable "nutrition label"
summary — grade, score, coverage, key findings, provenance known/asserted/unknown — suitable for a
dataset card, without requiring the reader to parse the raw certificate.

#### Scenario: A dataset card shows the nutrition label
- **WHEN** a certificate is rendered as a nutrition label
- **THEN** it shows grade, score, provenance coverage, and headline findings in a compact form
- **AND** it corresponds exactly to the signed certificate's contents
