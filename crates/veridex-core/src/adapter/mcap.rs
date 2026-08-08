//! MCAP adapter: maps an MCAP recording into the CDM (design D3).
//!
//! MCAP is the cross-domain container (ROS 2 / NVIDIA Isaac default). Shipping it alongside the
//! LeRobot adapter forces the CDM to be a real neutral substrate, not a LeRobot wrapper.
//!
//! Mapping:
//! - each MCAP **channel** (topic) → a CDM [`Stream`], with modality inferred from the schema name
//!   and topic;
//! - each **message** → a [`Frame`] whose timestamp is the message `log_time` (ns);
//! - all channels share the single MCAP log clock (`clock_id = "mcap-log"`); MCAP does not separate
//!   per-sensor clocks, so cross-stream skew is inferred from duration drift (the `TEMPORAL.CLOCK_SKEW`
//!   check compares spanned durations, which needs no shared epoch);
//! - MCAP has no episode concept, so the whole file maps to a single episode (index 0). The
//!   [`IngestReport`] records this and the MCAP fields the CDM does not carry.
//!
//! Rich provenance extraction (metadata records, calibration) is deferred to the provenance
//! milestone; this adapter records only the source format.

use std::collections::BTreeMap;
use std::path::Path;

use crate::cdm::{
    Dataset, Episode, Frame, Modality, Provenance, ProvenanceClass, ProvenanceElement,
    ProvenanceScope, Stream, ValueRef,
};

use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};

const CLOCK_ID: &str = "mcap-log";

/// Adapter for MCAP files (`.mcap`).
pub struct McapAdapter;

impl McapAdapter {
    fn is_mcap_path(path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("mcap")
    }
}

/// Infer a CDM modality from an MCAP schema name and topic using conservative keyword heuristics.
fn infer_modality(schema_name: &str, topic: &str) -> Modality {
    let hay = format!(
        "{} {}",
        schema_name.to_ascii_lowercase(),
        topic.to_ascii_lowercase()
    );
    let has = |kw: &str| hay.contains(kw);

    if has("image") || has("camera") || has("compressedimage") || has("video") {
        Modality::Video
    } else if has("audio") {
        Modality::Audio
    } else if has("wrench") || has("force") || has("torque") || has("tactile") || has("contact") {
        Modality::TactileForceTorque
    } else if has("command") || has("/cmd") || has("action") || has("setpoint") || has("control") {
        Modality::Action
    } else {
        // joint states, IMU, odometry, generic telemetry
        Modality::ScalarState
    }
}

/// A stream being accumulated during ingestion.
struct StreamBuilder {
    modality: Modality,
    frames: Vec<Frame>,
}

impl Adapter for McapAdapter {
    fn format_id(&self) -> &'static str {
        "mcap"
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["0"]
    }

    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(path) if McapAdapter::is_mcap_path(path) => Detection::Yes {
                version: Some("0".into()),
            },
            _ => Detection::No,
        }
    }

    fn ingest(&self, source: &Source, _options: &IngestOptions) -> Result<Ingested, IngestError> {
        let path = match source {
            Source::Local(p) => p,
            Source::Remote(_) => {
                return Err(IngestError::Parse {
                    format_id: "mcap",
                    message: "remote MCAP ingestion is not supported in v0.1".into(),
                })
            }
        };

        let bytes = std::fs::read(path).map_err(|e| IngestError::Io(e.to_string()))?;

        // Accumulate streams by topic (BTreeMap keeps a deterministic order before canonicalization).
        let mut streams: BTreeMap<String, StreamBuilder> = BTreeMap::new();
        let mut min_ts: Option<i64> = None;
        let mut max_ts: Option<i64> = None;

        for message in mcap::MessageStream::new(&bytes).map_err(|e| IngestError::Parse {
            format_id: "mcap",
            message: e.to_string(),
        })? {
            let message = message.map_err(|e| IngestError::Parse {
                format_id: "mcap",
                message: e.to_string(),
            })?;

            let topic = message.channel.topic.clone();
            let schema_name = message
                .channel
                .schema
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("");
            let ts = message.log_time as i64;
            min_ts = Some(min_ts.map_or(ts, |m| m.min(ts)));
            max_ts = Some(max_ts.map_or(ts, |m| m.max(ts)));

            let builder = streams
                .entry(topic.clone())
                .or_insert_with(|| StreamBuilder {
                    modality: infer_modality(schema_name, &topic),
                    frames: Vec::new(),
                });
            builder.frames.push(Frame {
                ts,
                value_ref: ValueRef {
                    uri: topic,
                    byte_offset: None,
                    byte_len: Some(message.data.len() as u64),
                    content_hash: None,
                },
            });
        }

        let cdm_streams: Vec<Stream> = streams
            .into_iter()
            .map(|(name, b)| Stream {
                name,
                modality: b.modality,
                declared_rate_hz: None,
                clock_id: CLOCK_ID.to_string(),
                frames: b.frames,
            })
            .collect();

        let dataset = Dataset {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mcap")
                .to_string(),
            metadata: vec![("source_format".into(), "mcap".into())],
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![ProvenanceElement {
                    key: "source_format".into(),
                    value: Some("mcap".into()),
                    class: ProvenanceClass::Known,
                }],
            }],
            episodes: vec![Episode {
                index: 0,
                start_ts: min_ts,
                end_ts: max_ts,
                streams: cdm_streams,
                task: None,
                labels: vec![],
            }],
        };

        let report = IngestReport {
            format_id: "mcap",
            source_version: Some("0".into()),
            coverage: Coverage::Full,
            mapped_fields: vec![
                "channel.topic -> stream.name".into(),
                "message.log_time -> frame.ts".into(),
                "schema.name -> stream.modality".into(),
                "message.data.len -> frame.value_ref.byte_len".into(),
            ],
            unmapped_fields: vec![
                UnmappedField {
                    source_path: "message.publish_time".into(),
                    note: "the CDM frame carries a single timestamp (log_time)".into(),
                },
                UnmappedField {
                    source_path: "message.sequence".into(),
                    note: "per-channel sequence numbers are not represented in the CDM".into(),
                },
            ],
            omitted_fields: vec![
                "episode-segmentation (MCAP has no episode concept; the whole file is one episode)"
                    .into(),
                "declared-rate (MCAP does not declare per-stream sample rates)".into(),
            ],
        };

        Ok(Ingested { dataset, report })
    }
}
