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
//! Provenance: the adapter records the source format, the MCAP header's writing `library` (as a
//! `recorder` element) and `profile`, every producer-written **Metadata** record (preserved in
//! dataset metadata, with well-known keys — license/sensor/calibration/operator/upstream — mapped to
//! typed provenance), and a summary of any **Attachments** (a calibration-looking attachment supplies
//! the `calibration` element). Everything is read as-is; nothing is fabricated.

use std::collections::{BTreeMap, BTreeSet};
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

/// Honest provenance records read from an MCAP file: the header, any Metadata records (a name plus a
/// key/value map the producer wrote), and a summary of any Attachments (name + media type). All are
/// read as-is and never fabricated.
#[derive(Default)]
struct McapRecords {
    header: Option<mcap::records::Header>,
    metadata: Vec<mcap::records::Metadata>,
    attachments: Vec<(String, String)>, // (name, media_type)
}

/// Read the header, Metadata records, and Attachment summaries in a single linear pass. Absent or
/// malformed records are skipped, never fabricated (the pass stops at the first read error).
fn read_records(bytes: &[u8]) -> McapRecords {
    let mut out = McapRecords::default();
    let Ok(reader) = mcap::read::LinearReader::new(bytes) else {
        return out;
    };
    for rec in reader {
        match rec {
            Ok(mcap::records::Record::Header(h)) => out.header = Some(h),
            Ok(mcap::records::Record::Metadata(m)) => out.metadata.push(m),
            Ok(mcap::records::Record::Attachment { header, .. }) => {
                out.attachments
                    .push((header.name.clone(), header.media_type.clone()));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    out
}

/// Map an MCAP Metadata key (case-insensitive) to a CDM provenance key, when it names one Veridex
/// tracks. Conservative: only well-known spellings map, so a producer's arbitrary keys aren't
/// misread as typed provenance (they are still preserved in dataset metadata).
fn provenance_key_for(meta_key: &str) -> Option<&'static str> {
    match meta_key.trim().to_ascii_lowercase().as_str() {
        "license" | "spdx" | "spdx_license" | "license_id" => Some("license"),
        "sensor" | "sensors" | "device" | "camera_model" | "lidar_model" | "hardware" => {
            Some("sensor")
        }
        "calibration" | "calibration_id" | "calib" | "calibration_version" => Some("calibration"),
        "operator" | "annotator" | "author" | "recorded_by" | "operator_id" => Some("annotator"),
        "upstream" | "derived_from" | "source_dataset" | "parent_dataset" | "upstream_dataset" => {
            Some("upstream")
        }
        _ => None,
    }
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

        // Honest origin metadata: the header (library/profile), any producer-written Metadata
        // records, and Attachment summaries.
        let records = read_records(&bytes);
        let header = records.header.clone();

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
                dtype: None,
                shape: None,
                frames: b.frames,
                stats: None,
            })
            .collect();

        // Dataset metadata and provenance: the format plus whatever the header honestly records.
        let mut metadata = vec![("source_format".into(), "mcap".into())];
        let mut elements = vec![ProvenanceElement {
            key: "source_format".into(),
            value: Some("mcap".into()),
            class: ProvenanceClass::Known,
        }];
        if let Some(h) = &header {
            // The writing library (e.g. "mcap-rs 0.25") is recorded provenance about who produced
            // the file. Empty strings are treated as absent, never fabricated.
            if !h.library.trim().is_empty() {
                metadata.push(("mcap_library".into(), h.library.clone()));
                elements.push(ProvenanceElement {
                    key: "recorder".into(),
                    value: Some(h.library.clone()),
                    class: ProvenanceClass::Known,
                });
            }
            // The profile (e.g. "ros2") identifies the message ecosystem.
            if !h.profile.trim().is_empty() {
                metadata.push(("mcap_profile".into(), h.profile.clone()));
            }
        }

        // Producer-written Metadata records: preserve every non-empty key/value in dataset metadata
        // (namespaced by record name), and map well-known keys to typed provenance. First value wins
        // per provenance key so the result is deterministic in file order.
        let mut mapped: BTreeSet<&'static str> = BTreeSet::new();
        for m in &records.metadata {
            for (k, v) in &m.metadata {
                if v.trim().is_empty() {
                    continue;
                }
                metadata.push((format!("mcap_meta.{}.{}", m.name, k), v.clone()));
                if let Some(pk) = provenance_key_for(k) {
                    if mapped.insert(pk) {
                        elements.push(ProvenanceElement {
                            key: pk.into(),
                            value: Some(v.clone()),
                            class: ProvenanceClass::Known,
                        });
                    }
                }
            }
        }
        // Attachments: record their presence, and let a calibration-looking attachment supply the
        // `calibration` element when no metadata key already did.
        for (name, media_type) in &records.attachments {
            metadata.push((format!("mcap_attachment.{name}"), media_type.clone()));
            if name.to_ascii_lowercase().contains("calib") && mapped.insert("calibration") {
                elements.push(ProvenanceElement {
                    key: "calibration".into(),
                    value: Some(name.clone()),
                    class: ProvenanceClass::Known,
                });
            }
        }

        let dataset = Dataset {
            id: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("mcap")
                .to_string(),
            metadata,
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements,
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
            mapped_fields: {
                let mut m = vec![
                    "channel.topic -> stream.name".into(),
                    "message.log_time -> frame.ts".into(),
                    "schema.name -> stream.modality".into(),
                    "message.data.len -> frame.value_ref.byte_len".into(),
                ];
                if header
                    .as_ref()
                    .is_some_and(|h| !h.library.trim().is_empty())
                {
                    m.push("header.library -> provenance.recorder".into());
                }
                if !records.metadata.is_empty() {
                    m.push("metadata records -> dataset metadata + provenance".into());
                }
                if !records.attachments.is_empty() {
                    m.push("attachment names -> dataset metadata (+ calibration)".into());
                }
                m
            },
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
