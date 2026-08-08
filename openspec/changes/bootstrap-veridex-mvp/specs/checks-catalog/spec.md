# checks-catalog — MVP delta

Scopes the north-star `checks-catalog` to v0.1: structural, temporal (including the headline
cross-stream skew check), statistical, and provenance-completeness families. Deep semantic and
video checks are deferred; basic media-integrity structural checks are included.

## ADDED Requirements

### Requirement: MVP structural checks
Veridex SHALL check episode-boundary integrity (including the corrupted-cumulative-length class
from lerobot#4143), missing shards/streams, degenerate (zero/one-frame) episodes, and dtype/shape
consistency.

#### Scenario: Corrupted episode boundary is an error, not a silent load
- **WHEN** episode-length metadata yields wrong cumulative boundaries
- **THEN** a structural check fails with an `error` naming affected episodes and the training risk

### Requirement: MVP temporal checks with cross-stream skew
Veridex SHALL check timestamp monotonicity, rate conformance, and gaps per stream, and SHALL
detect cross-stream clock skew for streams sharing an episode, using per-stream clock and declared
latency, flagging drift beyond a configurable tolerance.

#### Scenario: Cross-stream skew beyond tolerance fails
- **WHEN** two streams in one episode drift apart beyond tolerance
- **THEN** `TEMPORAL.CLOCK_SKEW` fails with an `error` reporting measured skew and the time range
- **AND** the finding notes the observation/action mismatch risk

#### Scenario: Non-monotonic timestamps are caught
- **WHEN** a stream's timestamps decrease or repeat within an episode
- **THEN** a temporal check fails naming the stream and frame indices

### Requirement: MVP statistical checks
Veridex SHALL check stored-vs-recomputed per-stream statistics, value range/sanity, and actuator
saturation.

#### Scenario: Stored stats disagree with data
- **WHEN** stored per-stream statistics differ from recomputed values
- **THEN** a statistical check fails and reports the mismatch per stream

### Requirement: MVP provenance-completeness checks
Veridex SHALL check for the presence and internal consistency of sensor/device, clock, calibration,
operator/annotator, license, and upstream lineage, surfacing missing elements as findings rather
than accepting them by default.

#### Scenario: Missing license and sensor provenance are surfaced
- **WHEN** a dataset records no license and no sensor identifiers
- **THEN** provenance-completeness checks emit findings for each missing element
- **AND** those gaps flow into the certificate's unknown section
