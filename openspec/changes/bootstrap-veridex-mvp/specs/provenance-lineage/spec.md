# provenance-lineage — MVP delta

Scopes the north-star `provenance-lineage` to v0.1: the provenance model with known/asserted/unknown
classification, extraction from the two MVP formats, and Croissant emit. Rich producer-attestation
UX and full PROV querying are deferred; minimal PROV lineage is included.

## ADDED Requirements

### Requirement: MVP provenance model with honest classification
Veridex SHALL represent provenance at dataset/episode/stream scope and classify each element as
known (extracted), asserted (attested), or unknown (absent), never presenting asserted or unknown
as known and never fabricating values.

#### Scenario: Absent operator identity stays unknown
- **WHEN** a source encodes no operator identity and none is attested
- **THEN** operator identity is reported `unknown` with no substituted value

### Requirement: MVP extraction and Croissant emit
Veridex SHALL extract available provenance from LeRobot v3 and MCAP sources and emit valid
MLCommons Croissant metadata, plus a minimal W3C PROV lineage representation.

#### Scenario: Extracted provenance emits as valid Croissant
- **WHEN** provenance export is requested for an ingested dataset
- **THEN** Veridex produces valid Croissant carrying the extracted provenance
- **AND** a minimal PROV lineage is produced
