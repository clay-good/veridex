# trust-certificate — MVP delta

Scopes the north-star `trust-certificate` to v0.1: content, content-hash binding, deterministic v1
rubric, COSE/JWS signing reusing Invariant, and offline verification with tamper/transplant
rejection.

## ADDED Requirements

### Requirement: MVP certificate content and binding
A v0.1 certificate SHALL record dataset identity and CDM content hash, Veridex version and config,
checks executed with versions, findings by severity/category, trust score and grade with
`rubric_version`, provenance coverage (known/asserted/unknown), and caller-supplied issuance
metadata; and SHALL bind to the exact CDM content hash.

#### Scenario: Certificate states coverage and binds to content
- **WHEN** a certificate is issued
- **THEN** it lists checks run and categories skipped, and records the CDM content hash it is bound
  to
- **AND** it reports provenance coverage split into known/asserted/unknown

### Requirement: MVP signing and offline verification
A certificate SHALL be COSE/JWS-signed (reusing Invariant's substrate) and verifiable offline
against the issuer public key, rejecting both tampering and presentation against a different
dataset.

#### Scenario: Offline verify accepts a valid certificate
- **WHEN** `veridex verify` is given a valid signed certificate, the dataset, and the public key,
  with no network
- **THEN** verification succeeds and reports issuer key ID and issuance timestamp

#### Scenario: Tampered or transplanted certificate is rejected
- **WHEN** a certificate field is altered, or the certificate is checked against a different
  dataset
- **THEN** verification fails, citing signature mismatch or content-hash mismatch respectively

### Requirement: MVP deterministic v1 rubric
Veridex SHALL compute the trust score and grade with a documented, versioned, deterministic v1
rubric recorded in the certificate; the same dataset and version SHALL always yield the same score
and certificate content hash.

#### Scenario: Certification is reproducible
- **WHEN** the same dataset is certified twice with the same version and rubric
- **THEN** trust score, grade, rubric version, and certificate content hash are identical
