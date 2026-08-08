# provenance-lineage Specification

## Purpose

Provenance is Veridex's second differentiator and the one no incumbent owns. This capability
captures, verifies, and represents **where a dataset and each of its parts came from** — sensor,
device, clock, calibration, firmware, operator/annotator, capture session, license, and upstream
dataset lineage — and emits it in portable, standard forms (MLCommons Croissant and W3C PROV).

A guiding principle: a provenance record is always explicit about what is **known** (extracted from
the source), **asserted** (attested by the producer), and **unknown** (absent). Veridex never
fabricates provenance and never presents an assertion as an extracted fact.

## Requirements

### Requirement: Provenance model
Veridex SHALL define a provenance model attachable at dataset, episode, and stream scope, covering
at minimum: sensor/device identity and configuration; clock/time-source identity; calibration
reference; firmware/software versions; operator and annotator identity; capture session/date;
license; and references to upstream datasets or tools a dataset was derived from.

#### Scenario: Provenance attaches at multiple scopes
- **WHEN** a dataset records one license at dataset scope and different sensors per stream
- **THEN** the provenance model represents the dataset-scope license and the per-stream sensors
  distinctly
- **AND** downstream emit and certificate reflect both scopes

### Requirement: Known / asserted / unknown separation
Every provenance element SHALL be classified as extracted-from-source (known), producer-attested
(asserted), or absent (unknown). Veridex SHALL never present asserted or unknown provenance as
known.

#### Scenario: A certificate distinguishes extracted from attested provenance
- **WHEN** a source encodes sensor identity but the producer attests calibration separately
- **THEN** sensor identity is marked `known` and calibration is marked `asserted`
- **AND** any element neither present nor attested is marked `unknown`

### Requirement: Lineage graph
Veridex SHALL represent dataset lineage as a directed graph linking a dataset to the datasets,
recordings, or tools it was derived from, when that information is available or attested. The graph
SHALL support querying a dataset's upstream ancestry.

#### Scenario: Derived dataset records its parent
- **WHEN** a dataset is declared or attested as filtered/merged from named upstream datasets
- **THEN** the lineage graph links it to those upstreams
- **AND** a caller can enumerate the dataset's ancestry from the graph

### Requirement: Standard emit (Croissant and PROV)
Veridex SHALL export provenance and dataset metadata as MLCommons Croissant, and lineage as W3C
PROV, so that other tools and hubs consume it without adopting a Veridex-specific format. Veridex
SHALL NOT invent a rival metadata interchange format.

#### Scenario: Provenance is emitted as Croissant
- **WHEN** a caller requests provenance export for an ingested dataset
- **THEN** Veridex produces valid Croissant metadata carrying the dataset's provenance
- **AND** it produces a W3C PROV representation of the lineage graph

### Requirement: Producer attestation
Veridex SHALL let a dataset producer attest provenance elements by signing them with their key, so
that asserted provenance is cryptographically attributable. Attested provenance SHALL be verifiable
offline against the producer's public key.

#### Scenario: An attested calibration is attributable and verifiable
- **WHEN** a producer signs a calibration attestation for a dataset
- **THEN** the attestation is bound to the dataset's content hash and the producer's key
- **AND** any party can verify offline that this producer made that assertion about that dataset

### Requirement: License compatibility for derived datasets
When a dataset is derived from multiple upstreams with recorded licenses, Veridex SHALL evaluate
license compatibility across the lineage and surface conflicts (e.g. combining incompatibly
licensed sources), reporting the licenses involved. Veridex SHALL present this as information, not
legal advice.

#### Scenario: Incompatible upstream licenses are surfaced
- **WHEN** a merged dataset draws from upstreams with conflicting licenses
- **THEN** Veridex reports the conflict and the licenses involved
- **AND** it states this is informational and not legal advice

### Requirement: Provenance is part of the signed attestation
The provenance summary (known/asserted/unknown and lineage) SHALL be included in what the
certificate binds and signs, so a certificate's provenance claims are tamper-evident alongside its
findings.

#### Scenario: Altering provenance invalidates the certificate
- **WHEN** the provenance summary bound into a signed certificate is altered
- **THEN** certificate verification fails
- **AND** the alteration is detectable

### Requirement: Provenance never fabricated
Veridex SHALL NOT infer or invent provenance values. When provenance is absent, it SHALL be
reported as unknown rather than guessed or defaulted.

#### Scenario: Absent provenance stays unknown
- **WHEN** a source encodes no operator identity and none is attested
- **THEN** operator identity is reported `unknown`
- **AND** no placeholder or inferred value is substituted
