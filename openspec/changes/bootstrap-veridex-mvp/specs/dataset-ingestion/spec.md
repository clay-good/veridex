# dataset-ingestion — MVP delta

Scopes the north-star `dataset-ingestion` capability to v0.1: the CDM plus exactly two adapters
(LeRobot v3 and MCAP), chosen to prove cross-format neutrality.

## ADDED Requirements

### Requirement: MVP Canonical Dataset Model
Veridex SHALL implement the CDM covering dataset/episode/stream/frame with modalities video,
scalar-state, action, audio, and tactile/force-torque, per-frame timestamps, per-stream clock
identifiers and declared rates, and provenance/label attachment points, with deterministic
canonicalization and content hashing.

#### Scenario: CDM round-trips a LeRobot v3 dataset deterministically
- **WHEN** a LeRobot v3 dataset is ingested twice with the same Veridex version
- **THEN** both runs produce the same CDM content hash
- **AND** all episodes, streams, modalities, and timestamps are represented

### Requirement: LeRobot v3 and MCAP adapters
Veridex SHALL ship adapters for LeRobot v3 and MCAP that populate the CDM without loss of any
CDM-representable field and declare their supported versions. No other format is required in v0.1;
unsupported formats SHALL be rejected clearly.

#### Scenario: Equivalent CDM across the two MVP formats
- **WHEN** one logical dataset is ingested as LeRobot v3 and as MCAP
- **THEN** the CDMs are equivalent in episodes, streams, modalities, timestamps, and labels
- **AND** validation yields the same structural and temporal verdicts

#### Scenario: RLDS is reported unsupported in v0.1
- **WHEN** an RLDS dataset is supplied to v0.1
- **THEN** Veridex reports the format unsupported and lists LeRobot v3 and MCAP as supported
- **AND** it does not partially parse it

### Requirement: MVP streaming and remote metadata
Veridex SHALL ingest datasets larger than memory by streaming, and SHALL support metadata-only
ingestion of a remote LeRobot Hub dataset for structural checks without downloading stream
payloads.

#### Scenario: Structural check on a remote Hub dataset avoids payload download
- **WHEN** a caller runs structural-only checks against a remote Hub dataset
- **THEN** only metadata and required shards are fetched
- **AND** stream payloads unrelated to the requested checks are not downloaded
