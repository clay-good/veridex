# dataset-ingestion Specification

## Purpose

Ingestion is Veridex's neutrality layer. It maps any supported source format into one **Canonical
Dataset Model (CDM)** so that every downstream capability — validation, provenance, certification,
reporting — operates on a single representation and never has to know the source format. This is
the foundation of the "Switzerland" position: a format is supported when, and only when, it has an
adapter that faithfully populates the CDM.

The CDM's core entities: a **Dataset** contains **Episodes**; an Episode contains time-ordered
**Frames** across one or more named **Streams**; each Stream carries a modality, a sample clock,
and a rate; a **Timeline** relates streams onto a common time base; **Provenance** and **Label
tracks** attach at the dataset, episode, and stream levels.

## Requirements

### Requirement: Canonical Dataset Model
Veridex SHALL define a single Canonical Dataset Model that all adapters populate and all
downstream capabilities consume. The CDM SHALL represent, at minimum: dataset-level metadata;
episodes with explicit start/end and frame counts; named streams each with a modality
(video, scalar-state, action, audio, tactile/force-torque, event/label), a declared sample rate,
and a clock identifier; per-frame timestamps; and attachment points for provenance and label
tracks at dataset, episode, and stream scope.

#### Scenario: A parsed dataset is fully represented in the CDM
- **WHEN** any supported source dataset is ingested
- **THEN** the resulting CDM instance exposes every episode, stream, modality, declared rate, and
  per-frame timestamp present in the source
- **AND** no downstream capability needs to read the original file to obtain that information

#### Scenario: Modalities beyond video and state are preserved
- **WHEN** a source encodes tactile / force-torque or audio streams alongside video and state
- **THEN** those streams appear in the CDM with their correct modality tags and rates
- **AND** they are available to checks and to the certificate

### Requirement: Format adapters
Veridex SHALL support ingestion through pluggable adapters, one per source format. Each adapter
SHALL translate its format into the CDM without loss of any field the CDM can represent, and SHALL
declare which format versions it supports. The set SHALL include, over time, LeRobot
(v2.0/2.1/3.0), MCAP, RLDS/TFDS, and HDF5/Zarr.

#### Scenario: The same dataset in two formats yields equivalent CDMs
- **WHEN** the identical logical dataset is provided once as LeRobot v3 and once as MCAP
- **THEN** the two resulting CDM instances are equivalent in episodes, streams, modalities,
  timestamps, and labels, up to documented format-specific metadata
- **AND** running validation on each produces the same structural and temporal verdicts

#### Scenario: An unsupported format is rejected clearly
- **WHEN** a dataset in a format with no adapter is supplied
- **THEN** Veridex reports the unsupported format and lists the formats it does support
- **AND** it does not partially parse or emit a misleading verdict

### Requirement: Streaming and scale
Veridex SHALL ingest datasets far larger than memory by streaming, and SHALL support ingesting
directly from a remote source (e.g. a Hugging Face Hub dataset) without a full local download when
the source permits range reads.

#### Scenario: A dataset larger than memory is ingested
- **WHEN** a dataset whose total size exceeds available memory is ingested
- **THEN** ingestion completes without loading the entire dataset into memory at once
- **AND** downstream checks can run over the full dataset

#### Scenario: Metadata-only ingestion for a remote dataset
- **WHEN** the caller requests only structural/metadata validation of a remote Hub dataset
- **THEN** Veridex streams only the metadata and shards required for those checks
- **AND** it does not download stream payloads that no requested check needs

### Requirement: Fidelity and adapter self-declaration
Each adapter SHALL record, in the CDM, exactly which source fields it mapped, which it could not
represent, and which the source omitted. Ingestion SHALL never silently discard information that
affects a verdict.

#### Scenario: Unmapped source fields are surfaced, not dropped
- **WHEN** a source contains a field the CDM cannot represent
- **THEN** the CDM records that the field existed and was not mapped
- **AND** reporting can list unmapped fields so users know the certificate's coverage limits

### Requirement: Deterministic ingestion
Given identical source bytes and an identical Veridex version, ingestion SHALL produce an
identical CDM, including a stable content hash over the canonicalized CDM.

#### Scenario: Re-ingesting the same bytes yields the same content hash
- **WHEN** the same dataset bytes are ingested twice with the same Veridex version
- **THEN** the two CDM content hashes are identical
- **AND** any later certificate referencing that hash verifies against both runs

### Requirement: Spatial and calibration metadata
When a source encodes camera intrinsics/extrinsics, coordinate frames, robot kinematics (e.g.
URDF), or sensor-to-sensor transforms, the CDM SHALL represent them and associate them with the
relevant streams, so cross-stream and spatial checks can use them and the certificate can attest
their presence.

#### Scenario: Camera calibration is carried into the CDM
- **WHEN** a multi-camera dataset encodes intrinsics/extrinsics and coordinate frames
- **THEN** the CDM associates that calibration with the corresponding video streams
- **AND** checks and provenance can reference it

### Requirement: Sampling-based ingestion
Veridex SHALL support ingesting and validating a defined sample of a dataset (e.g. the first N
episodes or a deterministic random subset) so very large datasets can be triaged quickly, and the
verdict SHALL record that it covered a sample rather than the whole dataset.

#### Scenario: A sampled run is labeled as partial coverage
- **WHEN** a caller validates a deterministic 5% sample of a large dataset
- **THEN** the run completes over only that sample
- **AND** the verdict and any certificate state that coverage was a sample, not the full dataset
