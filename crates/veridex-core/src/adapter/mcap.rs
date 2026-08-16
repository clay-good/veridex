//! MCAP adapter: maps an MCAP recording into the CDM (design D3).
//!
//! MCAP is the cross-domain container (ROS 2 / NVIDIA Isaac default). Shipping it alongside the
//! LeRobot adapter forces the CDM to be a real neutral substrate, not a LeRobot wrapper.
//!
//! Mapping:
//! - each MCAP **channel** (topic) → a CDM [`Stream`], with modality inferred from the schema name
//!   and topic;
//! - for the common ROS 2 autonomy message types, the message **header** (never the bulk payload) is
//!   CDR-decoded to populate the rig CDM: `PointCloud2` → `Stream.point_fields`, `CameraInfo` and
//!   `TFMessage` → `Dataset.calibration`, `Odometry` → `Episode.ego_poses` (see [`super::cdr`]);
//! - each **message** → a [`Frame`] whose timestamp is the message `log_time` (ns);
//! - all channels share the single MCAP log clock (`clock_id = "mcap-log"`); MCAP does not separate
//!   per-sensor clocks, so cross-stream skew is inferred from duration drift (the `TEMPORAL.CLOCK_SKEW`
//!   check compares spanned durations, which needs no shared epoch);
//! - MCAP has no episode concept, so the whole file maps to a single episode (index 0). The
//!   [`IngestReport`] records this and the MCAP fields the CDM does not carry.
//!
//! Provenance: the adapter records the source format, the MCAP header's writing `library` (as a
//! `recorder` element) and `profile`, every producer-written **Metadata** record (preserved in
//! dataset metadata, with well-known keys mapped to typed provenance — the core license / sensor /
//! calibration / operator / upstream, plus the autonomy rig lineage firmware / calibration_session /
//! platform / drive / region / map_version / redaction / consent), and a summary of any
//! **Attachments** (a calibration-looking attachment supplies the `calibration` element). Everything
//! is read as-is; nothing is fabricated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cdm::{
    Calibration, CameraIntrinsics, Dataset, EgoPose, Episode, Frame, Label, Modality, PointField,
    Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope, Stream, Transform, ValueRef,
};

use super::{
    Adapter, Coverage, Detection, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};
use sha2::{Digest, Sha256};

const CLOCK_ID: &str = "mcap-log";

/// Adapter for MCAP files (`.mcap`).
pub struct McapAdapter;

impl McapAdapter {
    fn is_mcap_path(path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("mcap")
    }
}

/// Infer a CDM modality from an MCAP schema name and topic using conservative keyword heuristics.
///
/// Recognizes the common ROS/ROS 2 autonomy sensor message types (PointCloud2/LaserScan, Imu,
/// NavSatFix, Odometry, CAN frames) so an AV rig log's streams are typed as the rig modalities the
/// autonomy checks expect, falling back to the manipulation modalities and finally `ScalarState`.
/// The order matters: more specific message types are tested first.
fn infer_modality(schema_name: &str, topic: &str) -> Modality {
    let hay = format!(
        "{} {}",
        schema_name.to_ascii_lowercase(),
        topic.to_ascii_lowercase()
    );
    let has = |kw: &str| hay.contains(kw);

    // Camera imagery first (a CameraInfo channel is camera-related telemetry, still Video here).
    if has("image") || has("camera") || has("compressedimage") || has("video") {
        Modality::Video
    } else if has("pointcloud")
        || has("laserscan")
        || has("lidar")
        || has("radar")
        || has("velodyne")
    {
        Modality::PointCloud
    } else if has("imu") {
        Modality::Imu
    } else if has("navsatfix") || has("gnss") || has("gps") || has("/fix") {
        Modality::Gnss
    } else if has("odometry") || has("odom") {
        Modality::EgoPose
    } else if has("can_msgs") || has("can_frame") || has("canbus") || has("/can/") {
        Modality::CanSignal
    } else if has("audio") {
        Modality::Audio
    } else if has("wrench") || has("force") || has("torque") || has("tactile") || has("contact") {
        Modality::TactileForceTorque
    } else if has("command") || has("/cmd") || has("action") || has("setpoint") || has("control") {
        Modality::Action
    } else {
        // joint states, generic telemetry
        Modality::ScalarState
    }
}

/// A stream being accumulated during ingestion.
struct StreamBuilder {
    modality: Modality,
    frames: Vec<Frame>,
    /// Per-point field layout, decoded from the first `PointCloud2` message on this topic (if any).
    point_fields: Option<Vec<PointField>>,
}

/// Match a ROS message schema name (e.g. `sensor_msgs/msg/PointCloud2`) by its final type segment,
/// tolerant of the `pkg/msg/Type` and older `pkg/Type` spellings.
fn schema_is(schema_name: &str, ty: &str) -> bool {
    schema_name
        .rsplit('/')
        .next()
        .map(|last| last == ty)
        .unwrap_or(false)
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
///
/// Covers the core keys (license/sensor/calibration/operator/upstream) plus the autonomy sensor-rig
/// lineage (design A3): sensor firmware, the calibration session, the platform/vehicle and drive/run
/// identity, the capture region, the HD-map version, and the redaction/consent status — the last two
/// being acute for public-road capture (design A7).
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
        // --- autonomy rig lineage (A3) ---
        "firmware" | "firmware_version" | "fw" | "fw_version" => Some("firmware"),
        "calibration_session" | "calib_session" | "calibration_session_id" => {
            Some("calibration_session")
        }
        "platform" | "vehicle" | "vehicle_id" | "platform_id" => Some("platform"),
        "drive" | "drive_id" | "run_id" | "session_id" | "log_id" => Some("drive"),
        "region" | "geo_region" | "country" | "locale" => Some("region"),
        "map_version" | "map" | "hdmap_version" | "map_id" => Some("map_version"),
        "redaction" | "redacted" | "redaction_status" | "pii_redaction" => Some("redaction"),
        "consent" | "consent_status" | "data_consent" => Some("consent"),
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

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
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

        // Autonomy rig metadata decoded from AV message *headers* (not their bulk payload): the ego
        // trajectory from Odometry, camera intrinsics from CameraInfo (first per camera topic), and the
        // static transform tree from TFMessage (first per parent→child edge). See `super::cdr`.
        let mut ego_poses: Vec<EgoPose> = Vec::new();
        let mut intrinsics: BTreeMap<String, CameraIntrinsics> = BTreeMap::new();
        let mut transforms: BTreeMap<(String, String), Transform> = BTreeMap::new();

        // One message is one frame, but the message count comes from the file — and a chunked MCAP
        // can expand a 100 KB file into a gigabyte of payload, all of which gets hashed. The budget
        // bounds that work.
        let mut budget = super::FrameBudget::new(options);
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
            // `log_time` is a u64 nanosecond stamp; the CDM stores i64. Saturate rather than wrap so a
            // value past i64::MAX (nanoseconds beyond ~year 2262, i.e. corrupt) can't flip negative and
            // corrupt min/max/ordering. Real timestamps are far below the cap, so this is lossless.
            let ts = i64::try_from(message.log_time).unwrap_or(i64::MAX);
            min_ts = Some(min_ts.map_or(ts, |m| m.min(ts)));
            max_ts = Some(max_ts.map_or(ts, |m| m.max(ts)));

            let builder = streams
                .entry(topic.clone())
                .or_insert_with(|| StreamBuilder {
                    modality: infer_modality(schema_name, &topic),
                    frames: Vec::new(),
                    point_fields: None,
                });
            budget.take("mcap", 1)?;
            builder.frames.push(Frame {
                ts,
                value_ref: ValueRef {
                    uri: topic.clone(),
                    byte_offset: None,
                    byte_len: Some(message.data.len() as u64),
                    // Fingerprint the raw message bytes (a hash, not a decode of the payload). This
                    // gives content-level checks (e.g. duplicate-episode detection) something exact to
                    // compare, and records provenance of the bytes.
                    content_hash: Some(Sha256::digest(&message.data).into()),
                },
            });

            // Decode the AV message header (never the bulk payload) to populate the autonomy CDM.
            if schema_is(schema_name, "PointCloud2") {
                if builder.point_fields.is_none() {
                    builder.point_fields = super::cdr::decode_point_cloud2_fields(&message.data);
                }
            } else if schema_is(schema_name, "CameraInfo") {
                // First successfully-decoded intrinsics per camera topic wins.
                if !intrinsics.contains_key(&topic) {
                    if let Some(ci) = super::cdr::decode_camera_info(&message.data, &topic) {
                        intrinsics.insert(topic.clone(), ci);
                    }
                }
            } else if schema_is(schema_name, "Odometry") {
                if let Some(pose) = super::cdr::decode_odometry_pose(&message.data) {
                    ego_poses.push(EgoPose { ts, pose });
                }
            } else if schema_is(schema_name, "TFMessage") {
                if let Some(edges) = super::cdr::decode_tf_message(&message.data) {
                    for t in edges {
                        transforms
                            .entry((t.parent_frame.clone(), t.child_frame.clone()))
                            .or_insert(t);
                    }
                }
            }
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
                dim_stats: None,
                // MCAP message payloads are opaque bytes; Veridex fingerprints them but never decodes
                // numeric values, so there are no recomputed statistics.
                observed_stats: None,
                observed_saturation: None,
                observed_non_finite: None,
                observed_dim_stats: None,
                // Per-point field layout decoded from a PointCloud2 header, when this is a cloud stream.
                point_fields: b.point_fields,
            })
            .collect();

        // Assemble the decoded rig calibration (transform tree + camera intrinsics), if any.
        let calibration = if transforms.is_empty() && intrinsics.is_empty() {
            None
        } else {
            Some(Calibration {
                transforms: transforms.into_values().collect(),
                intrinsics: intrinsics.into_values().collect(),
            })
        };
        // The ego trajectory, ordered by timestamp (messages arrive in log order, but sort to be safe).
        let ego_poses = if ego_poses.is_empty() {
            None
        } else {
            ego_poses.sort_by_key(|p| p.ts);
            Some(ego_poses)
        };

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
        // Recognized scenario-dimension tags become episode labels (first value per dimension wins),
        // for the descriptive scenario-coverage report (design A3/A6).
        let mut scenario_dims: BTreeSet<&'static str> = BTreeSet::new();
        let mut scenario_labels: Vec<Label> = Vec::new();
        // Scenario/map/simulation references (design A3, format priority 4), collected in file order
        // so the first value per kind wins deterministically.
        let mut sim_refs: Vec<(crate::simref::SimRefKind, String)> = Vec::new();
        let mut sim_kinds: BTreeSet<crate::simref::SimRefKind> = BTreeSet::new();
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
                if let Some(kind) = crate::simref::simref_key_for(k) {
                    if sim_kinds.insert(kind) {
                        sim_refs.push((kind, v.clone()));
                    }
                }
                if let Some(dim) = crate::scenario::scenario_dim_for(k) {
                    if scenario_dims.insert(dim) {
                        scenario_labels.push(Label {
                            key: dim.into(),
                            value: v.clone(),
                            ts: None,
                        });
                    }
                }
            }
        }
        // Simulation references become provenance, plus the version each one declares. The version
        // is read from the referenced sidecar's own ASAM header when that file exists next to the
        // log; otherwise it is whatever dotted version the recorded value itself carries. Either way
        // it comes from recorded bytes, so it is `Known` — and an explicitly recorded `map_version`
        // always wins over an OpenDRIVE header revision, because `mapped` already holds the key.
        let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        for (kind, value) in &sim_refs {
            elements.push(ProvenanceElement {
                key: kind.provenance_key().into(),
                value: Some(value.clone()),
                class: ProvenanceClass::Known,
            });
            let Some(version_key) = kind.version_key() else {
                continue;
            };
            let version = crate::simref::sidecar_version(root, *kind, value)
                .or_else(|| crate::simref::version_from_value(value));
            if let Some(version) = version {
                if mapped.insert(version_key) {
                    elements.push(ProvenanceElement {
                        key: version_key.into(),
                        value: Some(version),
                        class: ProvenanceClass::Known,
                    });
                }
            }
        }

        // Labels are canonicalized by (key, value, ts), so a stable order in is not required.
        // Attachments: record their presence, and let a calibration-looking attachment supply the
        // `calibration` element when no metadata key already did. This is inferred from the file
        // *name* (a "calib" substring), not extracted calibration content, so it is classed
        // `Asserted`, not `Known` — the honest distinction, and it keeps a name-heuristic guess from
        // inflating the `known` coverage count.
        for (name, media_type) in &records.attachments {
            metadata.push((format!("mcap_attachment.{name}"), media_type.clone()));
            if name.to_ascii_lowercase().contains("calib") && mapped.insert("calibration") {
                elements.push(ProvenanceElement {
                    key: "calibration".into(),
                    value: Some(name.clone()),
                    class: ProvenanceClass::Asserted,
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
                labels: scenario_labels,
                ego_poses,
                declared_frame_count: None,
            }],
            calibration,
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
                    "message.data -> frame.value_ref.content_hash (SHA-256)".into(),
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
                if !sim_refs.is_empty() {
                    m.push("scenario/map/sim references -> provenance (+ declared version)".into());
                }
                if !records.attachments.is_empty() {
                    m.push("attachment names -> dataset metadata (+ calibration)".into());
                }
                // AV message-body decode (CDR headers, never the bulk payload).
                if dataset
                    .episodes
                    .iter()
                    .flat_map(|e| &e.streams)
                    .any(|s| s.point_fields.is_some())
                {
                    m.push("PointCloud2.fields -> stream.point_fields".into());
                }
                if dataset.calibration.is_some() {
                    m.push("CameraInfo.k/d + TFMessage -> dataset.calibration".into());
                }
                if dataset.episodes.iter().any(|e| e.ego_poses.is_some()) {
                    m.push("Odometry.pose -> episode.ego_poses".into());
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
