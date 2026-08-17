# security Specification

## Purpose

Veridex makes trust claims, so its own trustworthiness must be explicit. This capability defines key
management, the signing trust model, the boundaries of what a certificate does and does not
guarantee, and the threat model. It reuses Invariant's proven cryptographic substrate rather than
inventing new primitives.

## Requirements

### Requirement: Key management
Veridex SHALL support generating issuer keypairs, keeping private keys outside of any committed or
shared artifact, and referencing keys for signing and verification. Private key material SHALL never
appear in a certificate, report, verdict, or log.

#### Scenario: A certificate never contains private key material
- **WHEN** a certificate is issued
- **THEN** it contains only the issuer's public key identifier and the signature
- **AND** no private key material appears in the certificate or any Veridex output

### Requirement: Signing trust model
Certificate signing SHALL use Ed25519 with the issuer's private key over domain-separated canonical
bytes, binding the signature to the CDM content hash. The signature algorithm SHALL be fixed, not
selected from a field inside the document: verification SHALL reject any certificate declaring an
algorithm other than the one this build signs with, rather than dispatching on it. Verification SHALL
confirm the signature and the binding offline, and SHALL report the issuer key ID so a verifier can
decide whether they trust that issuer.

#### Scenario: Verification reports the issuer for trust decisions
- **WHEN** a certificate is verified
- **THEN** the result names the issuer key ID and confirms the content-hash binding
- **AND** trusting that issuer is left to the verifier, not assumed by Veridex

### Requirement: Guarantees and non-guarantees are explicit
Veridex SHALL document precisely what a certificate attests (that these checks, at these versions
and thresholds, produced these findings on data with this content hash, and this provenance was
known/asserted/unknown) and what it does NOT attest (fitness for a purpose, correctness of asserted
provenance, or absence of issues no check covers).

#### Scenario: A passing certificate does not imply fitness
- **WHEN** a dataset receives a passing certificate
- **THEN** the certificate states the checks and thresholds behind that status
- **AND** it explicitly disclaims any guarantee of fitness for a particular use

### Requirement: Tamper evidence and audit
Veridex SHALL maintain a tamper-evident, append-only audit log of certificate issuance and
verification actions, reusing Invariant's audit-log approach, so issuance history is independently
checkable.

#### Scenario: Issuance is recorded in a tamper-evident log
- **WHEN** a certificate is issued
- **THEN** an append-only audit entry records the issuance with the content hash and issuer
- **AND** altering a prior entry is detectable

### Requirement: Threat model
Veridex SHALL document its threat model, including: a compromised or malicious dataset producer
asserting false provenance; a tampered certificate; a transplanted certificate; a malicious plugin;
and supply-chain risks in adapters. For each, the spec SHALL state the mitigation or the explicit
residual risk.

#### Scenario: False asserted provenance is bounded, not hidden
- **WHEN** a producer asserts provenance that is untrue
- **THEN** the certificate marks it `asserted` and attributes it to that producer's key
- **AND** Veridex does not represent asserted provenance as independently verified fact

### Requirement: Safe handling of untrusted dataset content
Veridex SHALL treat dataset bytes as untrusted input and parse them defensively, so a malformed or
adversarial dataset causes a clean tool-error rather than a crash, hang, or unsafe behavior.

#### Scenario: A malformed dataset yields a clean tool-error
- **WHEN** an adversarially malformed dataset is ingested
- **THEN** Veridex fails with a tool-error exit code and a clear message
- **AND** it does not crash unsafely or hang indefinitely
