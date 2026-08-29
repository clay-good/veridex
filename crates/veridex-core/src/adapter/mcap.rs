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
//!   `TFMessage` → `Dataset.calibration`, `Odometry` → `Episode.ego_poses` (see the `cdr` module);
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
    Calibration, CameraIntrinsics, ClockKind, Dataset, EgoPose, Episode, Frame, Label, Modality,
    PointField, Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope, Stream, Transform,
    ValueRef,
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
pub(crate) fn infer_modality(schema_name: &str, topic: &str) -> Modality {
    let hay = format!(
        "{} {}",
        schema_name.to_ascii_lowercase(),
        topic.to_ascii_lowercase()
    );
    let has = |kw: &str| hay.contains(kw);

    // A `CameraInfo` channel carries a camera's calibration, not its imagery — and its cadence is
    // whatever the driver chose, commonly latched or 1 Hz. Typing it `Video` made it a *sensor*, and
    // the rig-sync check then compared a latched calibration topic's span against a LiDAR's and
    // reported a synchronized rig as drifting. Its content is not lost: Veridex decodes it into
    // `Dataset::calibration`, which is where a camera's intrinsics belong. Tested first, because the
    // topic name almost always contains "camera".
    if schema_is(schema_name, "CameraInfo") {
        Modality::ScalarState
    }
    // Camera imagery.
    else if has("image") || has("camera") || has("compressedimage") || has("video") {
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
    /// From the channel's `offered_qos_profiles` metadata, when rosbag2's MCAP writer recorded one.
    latched: Option<bool>,
    /// Per-point field layout, decoded from the first `PointCloud2` message on this topic (if any).
    point_fields: Option<Vec<PointField>>,
    /// The coordinate frame this topic's messages declare, from the first message whose body starts
    /// with a `std_msgs/Header`. First one wins: a topic that changes frame mid-recording is a rig
    /// fault, but recording the last one seen would hide it behind whichever message came last.
    frame_id: Option<String>,
}

/// Match a ROS message schema name (e.g. `sensor_msgs/msg/PointCloud2`) by its final type segment,
/// tolerant of the `pkg/msg/Type` and older `pkg/Type` spellings.
pub(crate) fn schema_is(schema_name: &str, ty: &str) -> bool {
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
    /// Sum of every chunk's *declared* uncompressed size. Read from the chunk headers without
    /// unpacking anything, so a decompression bomb can be refused before it costs memory.
    declared_uncompressed_bytes: u64,
}

/// The MCAP opcode for a Chunk record.
const OP_CHUNK: u8 = 0x06;

/// Prove every chunk decompresses to no more than the size it declares, before the reader is handed
/// the file.
///
/// The chunk *record header* declares an `uncompressed_size`, and the budget above charges the sum of
/// those. That bounds an honest file. It does not bound a corrupt one: the compressed stream inside
/// the chunk carries its own length claims, and the reader trusts those, not the header. One flipped
/// byte inside a zstd frame of the 7,756-byte demo log — with the record header still declaring a
/// truthful 17 KB — sent the reader into an allocation loop that had passed 700 MB after five
/// minutes and was still going, under both ingest budgets and under a 2 GB address-space limit. The
/// file never returns a verdict and never returns an error; the process just grows.
///
/// So each chunk is decompressed here first, into a buffer capped at one byte past what the chunk
/// itself declared. Overrunning that cap, or failing to decompress at all, means the two disagree —
/// which no valid file does — and the file is refused by name. This walks the record framing
/// directly rather than through the reader, so the check cannot be skipped by a record the reader
/// gives up on before reaching.
///
/// The budget is charged **here**, per chunk, rather than from the sum `read_records` collects.
/// That sum came from a reader whose loop stops at the first record it cannot parse, so every chunk
/// after a malformed one was charged nothing at all while this walk — which reaches them, by
/// design — went on to drain each up to its own self-declared size. One flipped magic byte in the
/// demo log took the charge to zero and let the chunks behind it decompress 16 GB over two seconds
/// before the file was refused; the expansion is linear in a number the attacker writes, so a
/// 500 MB file buys roughly half an hour of CPU. Charging on this walk closes the gap, and the cap
/// on each drain is the smaller of what the chunk declared and what the budget has left, so a
/// corrupt stream is stopped by the bound rather than merely billed for it afterwards.
pub(super) fn validate_chunks(
    bytes: &[u8],
    format_id: &'static str,
    budget: &mut super::DecompressionBudget,
) -> Result<(), IngestError> {
    let refuse = |detail: String| IngestError::Parse {
        format_id,
        message: detail,
    };
    // Past the 8-byte magic, a record is `opcode: u8, data_len: u64le, data`.
    let mut at = 8usize;
    while at + 9 <= bytes.len() {
        let opcode = bytes[at];
        let len = u64::from_le_bytes(bytes[at + 1..at + 9].try_into().expect("9 bytes read"));
        // A record claiming more bytes than the whole file holds cannot be read by anything, and
        // deferring it is not safe: the `mcap` reader's own framing arithmetic overflows on a
        // `u64::MAX` length and aborts the process, so a 17-byte hostile file killed the run before
        // any of Veridex's own guards were reached. Refused here, by name, while a merely truncated
        // record -- one whose length is plausible but runs off the end -- is still left to the
        // reader, which describes it better than this walk can.
        if len > bytes.len() as u64 {
            return Err(refuse(format!(
                "a record at byte {at} declares a {len}-byte payload, but the whole file is {} \
                 bytes; its framing is corrupt",
                bytes.len()
            )));
        }
        let len = len as usize;
        // Checked, not `+`: `usize::try_from` always succeeds on a 64-bit target, so an absurd
        // length overflowed the addition rather than being caught by the `get` below.
        let Some(end) = at.checked_add(9).and_then(|start| start.checked_add(len)) else {
            return Ok(());
        };
        let Some(payload) = bytes.get(at + 9..end) else {
            return Ok(()); // Truncated: the reader reports it, and stops before this chunk.
        };
        if opcode == OP_CHUNK {
            validate_one_chunk(payload, &refuse, format_id, budget)?;
        }
        at = end;
    }
    Ok(())
}

/// Decompress one Chunk record's payload into a buffer bounded by its own declared size.
fn validate_one_chunk(
    payload: &[u8],
    refuse: &dyn Fn(String) -> IngestError,
    format_id: &'static str,
    budget: &mut super::DecompressionBudget,
) -> Result<(), IngestError> {
    // start(8) end(8) uncompressed_size(8) uncompressed_crc(4) compression(4 + n) records_len(8)
    let Some(head) = payload.get(..32) else {
        return Ok(()); // Too short to be a chunk header; the reader reports the malformed record.
    };
    let declared = u64::from_le_bytes(head[16..24].try_into().expect("8 bytes read"));
    let name_len = u32::from_le_bytes(head[28..32].try_into().expect("4 bytes read")) as usize;
    let Some(name) = payload.get(32..32 + name_len) else {
        return Ok(());
    };
    let Some(records) = payload.get(32 + name_len + 8..) else {
        return Ok(());
    };
    // Charged before a byte is decompressed, and charged for every chunk this walk reaches — the
    // reader's own sum stops at the first record it cannot parse, so chunks behind a malformed one
    // used to cost nothing.
    budget.take(format_id, declared)?;

    // One byte past the declaration: reading it means the stream produced more than the chunk said
    // it holds, which is the disagreement being tested for. Bounded by what the budget has left as
    // well, so a corrupt stream is stopped by the cap instead of running to a number the file chose.
    let cap = declared.saturating_add(1).min(
        budget
            .remaining()
            .map_or(u64::MAX, |left| left.saturating_add(1)),
    );

    let produced = match name {
        // Stored uncompressed; its length is bounded by the file itself, and the outer walk in
        // `validate_chunks` already framed every record against the file size.
        b"" => return validate_inner_records(records, declared, refuse),
        b"zstd" => drain(
            zstd::stream::Decoder::new(records)
                .map_err(|e| refuse(format!("a chunk's zstd stream could not be opened: {e}")))?,
            cap,
        ),
        b"lz4" => drain(lz4_flex::frame::FrameDecoder::new(records), cap),
        other => {
            // An unknown codec is not decompressed here, and is not silently accepted either: the
            // reader will refuse it by name, which is the honest outcome.
            let _ = other;
            return Ok(());
        }
    };
    match produced {
        Ok((n, _)) if n > declared => Err(refuse(format!(
            "a chunk declares {declared} uncompressed bytes but its compressed stream produces \
             more; the chunk is corrupt and decompressing it would not terminate at the declared \
             size"
        ))),
        Ok((_, unpacked)) => validate_inner_records(&unpacked, declared, refuse),
        Err(e) => Err(refuse(format!(
            "a chunk's compressed stream is corrupt and could not be decompressed: {e}"
        ))),
    }
}

/// Frame-check the records *inside* a chunk, the way [`validate_chunks`] frames the records outside
/// one.
///
/// The outer walk refuses a record whose declared payload exceeds the file, because the `mcap`
/// reader sizes a buffer from that number before checking the bytes exist. Inside a chunk the same
/// number is read from decompressed bytes, and there the reader has no bound at all: of the crate's
/// four reader constructors only `LinearReader`'s sets `with_record_length_limit`, and the one
/// Veridex reads messages through -- `MessageStream` -> `RawMessageStream` -> `ChunkFlattener` --
/// is the one that omits it. So a `u64` length prefix hidden in a chunk's compressed body reaches
/// `RwBuf::reserve_exact` unchecked, and the allocator aborts the whole process: a 182-byte file
/// declaring a 117 TB record killed the run with SIGABRT, before any budget was consulted, and with
/// `--max-frames 1 --max-decompression-ratio 1` set. An abort is worse than a refusal in the way
/// that matters most here -- the process dies with no finding, no exit code a CI gate can read, and
/// nothing said about the file that did it.
///
/// The chunk's own declared uncompressed size is the bound: a record inside it cannot be longer
/// than the chunk that contains it. A merely truncated record -- plausible length, runs off the end
/// -- is still left to the reader, which describes it better than this walk can.
fn validate_inner_records(
    records: &[u8],
    declared: u64,
    refuse: &dyn Fn(String) -> IngestError,
) -> Result<(), IngestError> {
    let mut at = 0usize;
    while at + 9 <= records.len() {
        let opcode = records[at];
        let len = u64::from_le_bytes(records[at + 1..at + 9].try_into().expect("9 bytes read"));
        if len > declared {
            return Err(refuse(format!(
                "a record inside a chunk declares a {len}-byte payload, but the chunk holds only \
                 {declared} uncompressed bytes; its framing is corrupt"
            )));
        }
        let _ = opcode;
        // Checked, not `+`: a length just under `declared` on a huge chunk still overflows the
        // addition rather than failing the bound above.
        let Some(end) = at
            .checked_add(9)
            .and_then(|start| start.checked_add(len as usize))
        else {
            return Ok(());
        };
        if end > records.len() {
            return Ok(()); // Truncated: the reader reports it, and stops here.
        }
        at = end;
    }
    Ok(())
}

/// Read at most `cap` bytes out of `reader`, returning how many arrived. The bound is the point: a
/// corrupt stream is stopped by it rather than by exhausting memory.
/// Unpack at most `cap` bytes, returning how many were produced and the bytes themselves.
///
/// The bytes are kept rather than sunk because [`validate_inner_records`] has to read the record
/// framing they carry. `cap` is the chunk's declared size (plus one, to catch a stream that
/// overruns its own declaration) intersected with what the decompression budget has left, and the
/// budget was charged before a byte was unpacked -- so this materializes no more than the caller
/// already accounted for, and no more than the reader itself is about to.
fn drain(reader: impl std::io::Read, cap: u64) -> std::io::Result<(u64, Vec<u8>)> {
    let mut out = Vec::new();
    let n = std::io::copy(&mut std::io::Read::take(reader, cap), &mut out)?;
    Ok((n, out))
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
            Ok(mcap::records::Record::Chunk { header, .. }) => {
                out.declared_uncompressed_bytes = out
                    .declared_uncompressed_bytes
                    .saturating_add(header.uncompressed_size);
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
pub(crate) fn provenance_key_for(meta_key: &str) -> Option<&'static str> {
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

    /// An MCAP writes its own index at the end of the file: a Channel and a Schema record per
    /// topic, and a Statistics record carrying the message total, the per-channel totals and the
    /// recording's log-time span. That is a few kilobytes at a known offset in front of a recording
    /// that is routinely tens of gigabytes, and reading it opens no chunk.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    /// An MCAP is one recording, ingested as one episode, so there is no episode axis to sample.
    fn supports_sampling(&self) -> bool {
        false
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        // An MCAP recording becomes one episode, so there is nothing to sample along.
        let path = match source {
            Source::Local(p) => p,
            Source::Remote(_) => {
                return Err(IngestError::Parse {
                    format_id: "mcap",
                    message: "remote MCAP ingestion is not supported in v0.1".into(),
                })
            }
        };

        // The summary section is a few kilobytes at a known offset at the end of the file, so a
        // metadata-only run never reads the recording at all — three seeks rather than tens of
        // gigabytes.
        if options.metadata_only {
            return ingest_summary_only(path, read_summary(path)?);
        }

        // Read whole: the summary is at the end, the chunk walk below indexes the file directly, and
        // the reader seeks. So the file's size is the allocation, and one past what this ingest will
        // hold is refused here rather than by the OOM killer — `--metadata-only` answers from the
        // summary section at any size.
        let bytes = super::read_source_whole(
            path,
            "mcap",
            options,
            "check it from its summary section with --metadata-only, which reads three seeks \
             rather than the file, or raise the ceiling with --max-source-bytes (0 removes it)",
        )?;

        // Honest origin metadata: the header (library/profile), any producer-written Metadata
        // records, and Attachment summaries.
        let records = read_records(&bytes);
        let header = records.header.clone();

        // Chunks are decompressed on the way to messages, and the frame budget below counts frames,
        // not the bytes they arrive in: a 100 KB file can declare a gigabyte of chunk contents, and
        // one oversized message inside it is a single frame. Charge what the chunk headers declare
        // *before* anything is unpacked, then charge what actually arrives, so a header that
        // understates its expansion is caught too.
        // The two are charged against separate budgets of the same size rather than one shared
        // total, so an honest file is not charged twice for the same bytes.
        //
        // The declared charge is taken inside `validate_chunks`, per chunk, because that walk reads
        // the record framing directly and so reaches every chunk — including the ones behind a
        // record `read_records` gave up on, which is exactly where a hostile file puts them.
        let mut declared = super::DecompressionBudget::new(options, bytes.len() as u64);
        validate_chunks(&bytes, "mcap", &mut declared)?;
        let mut arrived = super::DecompressionBudget::new(options, bytes.len() as u64);

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
                    // rosbag2's MCAP writer carries each publisher's QoS on the channel, so a bag
                    // stored as MCAP declares a latched topic exactly as the `.db3` one does — and
                    // must be read the same way, or which storage plugin a team picked changes the
                    // verdict.
                    latched: message
                        .channel
                        .metadata
                        .get("offered_qos_profiles")
                        .and_then(|qos| super::rosbag2::declares_latched(qos)),
                    point_fields: None,
                    frame_id: None,
                });
            budget.take("mcap", 1)?;
            arrived.take("mcap", message.data.len() as u64)?;
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

            // Every header-first ROS message names the frame its data is expressed in. Recording it
            // is what lets a check ask whether this sensor is actually related to the others by the
            // TF tree, rather than only whether a TF tree exists at all.
            if builder.frame_id.is_none() {
                builder.frame_id = super::cdr::decode_header_frame_id(&message.data);
            }

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
                // Real recorded timestamps: every temporal check applies.
                clock_kind: ClockKind::Measured,
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
                latched: b.latched,
                point_fields: b.point_fields,
                // The coordinate frame the sensor declares, from its message headers.
                media: None,
                frame_id: b.frame_id,
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

        // The file's own summary index, checked against what the read actually yielded. An MCAP
        // closes with a Statistics record declaring how many messages it holds; a file truncated
        // after that record was written, or one whose chunks this reader could not walk to the end
        // of, yields fewer — and reading it as a complete recording is exactly the "silence reads as
        // a pass" failure this tool exists to prevent. Cheap: the summary sits at a known offset,
        // and the whole file is already in memory here.
        //
        // A file with no summary section is not a fault — a streaming writer legitimately omits one
        // — so it disables the reconciliation rather than failing the read, and says so.
        let mut unread_sources = Vec::new();
        let mut count_note = None;
        match read_summary(path).ok().and_then(|s| s.statistics) {
            Some(stats) => {
                let ingested: u64 = dataset.episodes[0]
                    .streams
                    .iter()
                    .map(|s| s.frames.len() as u64)
                    .sum();
                match stats.message_count.cmp(&ingested) {
                    std::cmp::Ordering::Greater => unread_sources.push(UnmappedField {
                        source_path: "summary Statistics.message_count".into(),
                        note: format!(
                            "the file's own summary declares {} message(s) but {ingested} were \
                             read — {} are missing from its chunks",
                            stats.message_count,
                            stats.message_count - ingested
                        ),
                    }),
                    // The other direction is not unread data: every message present was read. It is
                    // the summary that is wrong, which is worth saying and is not a coverage hole.
                    std::cmp::Ordering::Less => {
                        count_note = Some(UnmappedField {
                            source_path: "summary Statistics.message_count".into(),
                            note: format!(
                                "the file's own summary declares {} message(s) but {ingested} were \
                                 read; the CDM records the recording, and the summary's disagreeing \
                                 total is not represented in it",
                                stats.message_count
                            ),
                        })
                    }
                    std::cmp::Ordering::Equal => {}
                }
            }
            None => {
                count_note = Some(UnmappedField {
                    source_path: "summary Statistics record".into(),
                    note: "this file declares no message total (it was written without a summary \
                           section, or without statistics in one), so the read could not be \
                           reconciled against a count the file itself states"
                        .into(),
                })
            }
        }

        let report = IngestReport {
            unread_sources,
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
            unmapped_fields: {
                let mut u = vec![
                    UnmappedField {
                        source_path: "message.publish_time".into(),
                        note: "the CDM frame carries a single timestamp (log_time)".into(),
                    },
                    UnmappedField {
                        source_path: "message.sequence".into(),
                        note: "per-channel sequence numbers are not represented in the CDM".into(),
                    },
                ];
                u.extend(count_note);
                u
            },
            omitted_fields: vec![
                "episode-segmentation (MCAP has no episode concept; the whole file is one episode)"
                    .into(),
                "declared-rate (MCAP does not declare per-stream sample rates)".into(),
            ],
        };

        Ok(Ingested { dataset, report })
    }
}

/// Ingest an MCAP from its summary section alone, opening no chunk.
///
/// What this covers: the topic inventory — every channel's topic, its schema name and so its
/// modality, and its message encoding — the file's declared message total and per-channel totals,
/// its first and last log time, and the library that wrote it. What it cannot cover is everything a
/// message carries: no timestamps, no message bytes, no content hashes, and no decoded rig
/// calibration or ego trajectory, all of which come from message *bodies*.
fn ingest_summary_only(path: &Path, summary: McapSummary) -> Result<Ingested, IngestError> {
    let refuse = |message: String| IngestError::Parse {
        format_id: "mcap",
        message,
    };
    // The inventory has to be whole, and the file's own total is what proves it. Presenting three
    // channels out of twelve as the recording's contents is invisible to the caller, so a summary
    // whose per-channel counts do not add up to its own total is refused rather than reported.
    if let Some(stats) = &summary.statistics {
        let counted: u64 = stats.channel_message_counts.values().copied().sum();
        if !stats.channel_message_counts.is_empty() && counted != stats.message_count {
            return Err(refuse(format!(
                "this file's MCAP summary declares {} message(s) in total but its per-channel \
                 counts account for {counted} across {} channel(s) — Veridex did not read the \
                 whole inventory, and will not present part of it as the recording's contents; \
                 drop --metadata-only to read the file instead",
                stats.message_count,
                stats.channel_message_counts.len()
            )));
        }
    }

    let streams: Vec<Stream> = summary
        .channels
        .iter()
        .map(|c| Stream {
            name: c.topic.clone(),
            modality: infer_modality(c.schema_name.as_deref().unwrap_or(""), &c.topic),
            declared_rate_hz: None,
            clock_id: CLOCK_ID.to_string(),
            // The clock describes the source — an MCAP stamps every message with a log time — and
            // this run simply did not read one. The temporal checks abstain here for want of
            // frames, which the coverage note states.
            clock_kind: ClockKind::Measured,
            // A channel's delivery policy is written in its own metadata map in some profiles and
            // in none in others, so nothing is claimed about it here.
            latched: None,
            dtype: None,
            shape: None,
            frames: Vec::new(),
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            point_fields: None,
            media: None,
            frame_id: None,
        })
        .collect();

    let mut metadata = vec![("source_format".into(), "mcap".to_string())];
    let mut elements = vec![ProvenanceElement {
        key: "source_format".into(),
        value: Some("mcap".to_string()),
        class: ProvenanceClass::Known,
    }];
    // The Metadata records the summary's index pointed at. This is where a producer writes the
    // licence, the sensor and the clock source, and reading them is what keeps a summary-only run
    // from reporting `provenance 0%` on a file that states its provenance perfectly well — a claim
    // about the read, not about the file. Mapped exactly as a full read maps them.
    let mut mapped: BTreeSet<&'static str> = BTreeSet::new();
    let mut scenario_dims: BTreeSet<&'static str> = BTreeSet::new();
    let mut scenario_labels: Vec<Label> = Vec::new();
    let mut sim_refs: Vec<(crate::simref::SimRefKind, String)> = Vec::new();
    let mut sim_kinds: BTreeSet<crate::simref::SimRefKind> = BTreeSet::new();
    for (name, pairs) in &summary.metadata {
        for (k, v) in pairs {
            if v.trim().is_empty() {
                continue;
            }
            metadata.push((format!("mcap_meta.{name}.{k}"), v.clone()));
            if let Some(pk) = provenance_key_for(k) {
                if mapped.insert(pk) {
                    elements.push(ProvenanceElement {
                        key: pk.into(),
                        value: Some(v.clone()),
                        class: ProvenanceClass::Known,
                    });
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
            if let Some(kind) = crate::simref::simref_key_for(k) {
                if sim_kinds.insert(kind) {
                    sim_refs.push((kind, v.clone()));
                }
            }
        }
    }
    // Scenario/map/simulation references, exactly as a full read maps them — except for the version,
    // which a full read prefers to take from the referenced sidecar's own ASAM header. Opening that
    // file is reading a second recording's data, which this mode does not do, so only a version the
    // recorded value itself carries is used, and the difference is disclosed.
    for (kind, value) in &sim_refs {
        elements.push(ProvenanceElement {
            key: kind.provenance_key().into(),
            value: Some(value.clone()),
            class: ProvenanceClass::Known,
        });
        if let Some(version_key) = kind.version_key() {
            if let Some(version) = crate::simref::version_from_value(value) {
                if mapped.insert(version_key) {
                    elements.push(ProvenanceElement {
                        key: version_key.into(),
                        value: Some(version),
                        class: ProvenanceClass::Known,
                    });
                }
            }
        }
    }
    // Attachments: their names and media types are in the summary's own index, so recording them
    // costs no extra read and opens no attachment. A calibration-looking *name* supplies the
    // `calibration` element the same way a full read does — classed `Asserted`, because it is a
    // name heuristic rather than extracted calibration content.
    for (name, media_type) in &summary.attachments {
        metadata.push((format!("mcap_attachment.{name}"), media_type.clone()));
        if name.to_ascii_lowercase().contains("calib") && mapped.insert("calibration") {
            elements.push(ProvenanceElement {
                key: "calibration".into(),
                value: Some(name.clone()),
                class: ProvenanceClass::Asserted,
            });
        }
    }
    if let Some(profile) = &summary.profile {
        metadata.push(("mcap_profile".into(), profile.clone()));
    }
    if let Some(library) = &summary.library {
        metadata.push(("mcap_library".into(), library.clone()));
        elements.push(ProvenanceElement {
            key: "recorder".into(),
            value: Some(library.clone()),
            class: ProvenanceClass::Known,
        });
    }
    // How the messages on each channel are serialized — `cdr`, `protobuf`, `json`. Recorded as the
    // set the file declares, because it is what a reader needs to know whether these bytes are ones
    // their tooling can open at all.
    let encodings: BTreeSet<&str> = summary
        .channels
        .iter()
        .map(|c| c.message_encoding.as_str())
        .filter(|e| !e.is_empty())
        .collect();
    if !encodings.is_empty() {
        metadata.push((
            "mcap_message_encodings".into(),
            encodings.into_iter().collect::<Vec<_>>().join(","),
        ));
    }

    let mut mapped_fields = vec![
        "summary Channel.topic -> stream.name".into(),
        "summary Schema.name -> stream.modality".into(),
        "summary Channel.message_encoding -> dataset metadata".into(),
        "header library -> provenance.recorder".into(),
    ];
    if !summary.metadata.is_empty() {
        mapped_fields.push(
            "summary MetadataIndex -> metadata records -> dataset metadata + provenance".into(),
        );
    }
    if !summary.attachments.is_empty() {
        mapped_fields.push("summary AttachmentIndex -> dataset metadata (+ calibration)".into());
    }
    if !sim_refs.is_empty() {
        mapped_fields.push("scenario/map/sim references -> provenance".into());
    }
    let mut unmapped_fields = Vec::new();
    if let Some(stats) = &summary.statistics {
        metadata.push(("mcap_message_count".into(), stats.message_count.to_string()));
        mapped_fields
            .push("summary Statistics -> declared message counts and log-time span".into());
        // A channel the Statistics record never counted is not a channel with no messages: it is a
        // count this file did not declare, and the difference has to reach the report. Without
        // this, a summary listing twelve channels and counting three reads as nine silent topics.
        let uncounted: Vec<&str> = summary
            .channels
            .iter()
            .filter(|c| !stats.channel_message_counts.contains_key(&c.id))
            .map(|c| c.topic.as_str())
            .collect();
        if !stats.channel_message_counts.is_empty() && !uncounted.is_empty() {
            unmapped_fields.push(UnmappedField {
                source_path: uncounted.join(", "),
                note:
                    "this file's summary lists the channel but its Statistics record declares no \
                       message count for it, so how much it carries is not stated"
                        .into(),
            });
        }
        unmapped_fields.push(UnmappedField {
            source_path: "message records".into(),
            note: format!(
                "the file's {} declared message(s), spanning log times {}..{}, were not read: this \
                 is a metadata-only ingest of {} chunk(s)",
                stats.message_count,
                stats.message_start_time,
                stats.message_end_time,
                stats.chunk_count
            ),
        });
    } else {
        // A summary section without a Statistics record is legal, and the difference matters: the
        // topic inventory is then all there is, with no total to check it against.
        unmapped_fields.push(UnmappedField {
            source_path: "summary Statistics record".into(),
            note: "this file's summary section carries no Statistics record, so it declares no \
                   message total and no log-time span"
                .into(),
        });
    }

    let dataset = Dataset {
        id: super::dataset_id_from_path(path, "mcap"),
        metadata,
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        }],
        episodes: vec![Episode {
            index: 0,
            // The summary's log-time span describes the recording, not the frames in this CDM.
            // Stamping it here would put a timeline on an episode with no frames to support it.
            start_ts: None,
            end_ts: None,
            streams,
            task: None,
            labels: scenario_labels,
            ego_poses: None,
            // The message total is a count across every channel, not this episode's frame count,
            // so it is recorded as metadata rather than as a claim the length check would grade.
            declared_frame_count: None,
        }],
        calibration: None,
    };

    Ok(Ingested {
        report: IngestReport {
            format_id: "mcap",
            source_version: summary.profile.clone(),
            coverage: Coverage::MetadataOnly {
                episodes_declared: 1,
            },
            mapped_fields,
            unmapped_fields,
            unread_sources: Vec::new(),
            omitted_fields: vec![
                "frames (no chunk was opened, so there are no timestamps, message bytes or content \
                 hashes)"
                    .into(),
                "rig calibration and ego trajectory (both are decoded from message bodies)".into(),
                "message schema contents (the summary names each schema; its definition is not \
                 read)"
                    .into(),
                "scenario/map/simulation sidecar versions (resolving one opens the referenced \
                 file beside the recording, which this mode does not do; a version the recorded \
                 value itself carries is still used)"
                    .into(),
                "attachment contents (the summary's index names each attachment and its media \
                 type; its bytes are never opened)"
                    .into(),
            ],
        },
        dataset,
    })
}

// ---- Reading an MCAP's summary section, without reading the file ----

/// The MCAP opcodes this summary reader recognizes.
const OP_HEADER: u8 = 0x01;
const OP_FOOTER: u8 = 0x02;
const OP_SCHEMA: u8 = 0x03;
const OP_CHANNEL: u8 = 0x04;
const OP_STATISTICS: u8 = 0x0B;
const OP_METADATA: u8 = 0x0C;
const OP_METADATA_INDEX: u8 = 0x0D;
const OP_ATTACHMENT_INDEX: u8 = 0x0A;

/// The Footer record's fixed size on disk: opcode(1) + length(8) + payload(20).
const FOOTER_RECORD_LEN: u64 = 29;
/// The magic at both ends of an MCAP file.
const MAGIC_LEN: u64 = 8;

/// The ceiling on an MCAP's summary section.
///
/// The summary holds one Channel and one Schema record per topic plus a Statistics record — a few
/// kilobytes for the rigs this reads, and the offsets that delimit it come from the file itself.
/// A hostile footer can claim the summary starts at byte 0 of a 200 GB file; this is what stops
/// that claim from becoming a 200 GB read.
const MAX_SUMMARY_BYTES: u64 = 16 * 1024 * 1024;

/// Ceilings on the Metadata records a summary-only read will follow its index to.
///
/// A Metadata record is where a producer writes the licence, the sensor, the clock source — the
/// provenance that is 30% of the trust score, and that a summary-only read would otherwise report
/// as entirely absent. The index that names them is the file's own, so both the count and the total
/// size are bounded here rather than trusted.
const MAX_METADATA_RECORDS: usize = 256;
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

/// What an MCAP file says about itself in its own summary section.
struct McapSummary {
    /// `profile` and `library` from the Header record at the front of the file.
    profile: Option<String>,
    library: Option<String>,
    /// Every channel in the summary: topic, schema name, message encoding.
    channels: Vec<SummaryChannel>,
    /// The Statistics record, when the file carries one.
    statistics: Option<SummaryStatistics>,
    /// Every Metadata record the summary's index pointed at, as `(name, key/value pairs)`.
    metadata: Vec<(String, Vec<(String, String)>)>,
    /// Every attachment the summary indexes, as `(name, media type)`. Both are *in* the index
    /// record, so this costs no extra read and never opens an attachment's bytes.
    attachments: Vec<(String, String)>,
}

struct SummaryChannel {
    id: u16,
    topic: String,
    schema_name: Option<String>,
    message_encoding: String,
}

struct SummaryStatistics {
    message_count: u64,
    channel_message_counts: BTreeMap<u16, u64>,
    message_start_time: u64,
    message_end_time: u64,
    chunk_count: u32,
}

/// A bounds-checked cursor over a record payload.
///
/// Every length in an MCAP record is a number the file's author wrote, so each read is checked
/// against what is left rather than trusted. A short read returns `None`, which every caller turns
/// into "this record is malformed", never into a default value.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    /// A `u32` length followed by that many bytes of UTF-8.
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        Some(String::from_utf8_lossy(self.take(len)?).into_owned())
    }
    /// A `u32` byte-length followed by that many bytes.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

/// Read the summary section of the MCAP at `path` — and nothing else of it.
///
/// This is the `--metadata-only` reader. An MCAP writes its own index at the end of the file: one
/// Channel and one Schema record per topic, and a Statistics record carrying the message total, the
/// per-channel totals, and the recording's first and last log time. That is a few kilobytes at a
/// known offset in front of a recording that is routinely tens of gigabytes, and reading it needs
/// three seeks — no chunk is opened and nothing is decompressed.
///
/// Every offset used here comes out of the file, so every one is bounds-checked against the file's
/// real length before it is used. A file with no summary section is refused by name rather than
/// read as a recording with no topics.
fn read_summary(path: &Path) -> Result<McapSummary, IngestError> {
    use std::io::{Read, Seek, SeekFrom};

    let refuse = |message: String| IngestError::Parse {
        format_id: "mcap",
        message,
    };
    let mut file = std::fs::File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
    let len = file
        .metadata()
        .map_err(|e| IngestError::Io(e.to_string()))?
        .len();
    if len < MAGIC_LEN * 2 + FOOTER_RECORD_LEN {
        return Err(refuse(format!(
            "this file is {len} bytes, too short to hold an MCAP footer, so it declares no summary \
             section to read"
        )));
    }

    // The Footer is the last record before the closing magic, and it is fixed-size, so its offset
    // is arithmetic rather than a scan.
    let footer_at = len - MAGIC_LEN - FOOTER_RECORD_LEN;
    let mut footer = [0u8; FOOTER_RECORD_LEN as usize];
    file.seek(SeekFrom::Start(footer_at))
        .and_then(|_| file.read_exact(&mut footer))
        .map_err(|e| IngestError::Io(e.to_string()))?;
    if footer[0] != OP_FOOTER {
        return Err(refuse(
            "this file does not end in an MCAP footer record; it is truncated, or it was still \
             being written — drop --metadata-only to read what is there"
                .into(),
        ));
    }
    let mut cur = Cursor::new(&footer[9..]);
    let (Some(summary_start), Some(_summary_offset_start)) = (cur.u64(), cur.u64()) else {
        return Err(refuse("this file's MCAP footer is malformed".into()));
    };
    if summary_start == 0 {
        return Err(IngestError::NotImplemented {
            what: "metadata-only ingestion of an MCAP with no summary section",
            hint: "this file was written without a summary index — a streaming writer that never \
                   finalized, or one configured not to write one — so the topics and counts exist \
                   only in the records themselves; drop --metadata-only to read them",
        });
    }
    if summary_start < MAGIC_LEN || summary_start > footer_at {
        return Err(refuse(format!(
            "this file's MCAP footer places the summary section at byte {summary_start}, which is \
             outside the file's own {len} bytes; its framing is corrupt"
        )));
    }
    let summary_len = footer_at - summary_start;
    if summary_len > MAX_SUMMARY_BYTES {
        return Err(refuse(format!(
            "this file declares a {summary_len}-byte summary section, over the \
             {MAX_SUMMARY_BYTES}-byte ceiling for a metadata-only read — drop --metadata-only to \
             read the file itself"
        )));
    }
    let mut summary = vec![0u8; summary_len as usize];
    file.seek(SeekFrom::Start(summary_start))
        .and_then(|_| file.read_exact(&mut summary))
        .map_err(|e| IngestError::Io(e.to_string()))?;

    // The Header is the first record after the opening magic, and it is the only thing worth
    // reading from the front: the library that wrote the file, and the profile it claims.
    let mut head = vec![0u8; (len - MAGIC_LEN).min(64 * 1024) as usize];
    file.seek(SeekFrom::Start(MAGIC_LEN))
        .and_then(|_| file.read_exact(&mut head))
        .map_err(|e| IngestError::Io(e.to_string()))?;
    let (mut profile, mut library) = (None, None);
    if let Some((OP_HEADER, payload)) = first_record(&head) {
        let mut cur = Cursor::new(payload);
        profile = cur.string().filter(|s| !s.is_empty());
        library = cur.string().filter(|s| !s.is_empty());
    }

    let mut channels: Vec<SummaryChannel> = Vec::new();
    let mut schemas: BTreeMap<u16, String> = BTreeMap::new();
    let mut statistics = None;
    let mut metadata_index: Vec<(u64, u64)> = Vec::new(); // (offset, length)
    let mut attachments: Vec<(String, String)> = Vec::new();
    let mut pending: Vec<(u16, u16)> = Vec::new(); // (channel id, schema id)
    for (opcode, payload) in records(&summary) {
        match opcode {
            OP_SCHEMA => {
                let mut cur = Cursor::new(payload);
                if let (Some(id), Some(name)) = (cur.u16(), cur.string()) {
                    schemas.insert(id, name);
                }
            }
            OP_CHANNEL => {
                let mut cur = Cursor::new(payload);
                let (Some(id), Some(schema_id), Some(topic), Some(encoding)) =
                    (cur.u16(), cur.u16(), cur.string(), cur.string())
                else {
                    continue;
                };
                pending.push((id, schema_id));
                channels.push(SummaryChannel {
                    id,
                    topic,
                    schema_name: None,
                    message_encoding: encoding,
                });
            }
            OP_STATISTICS => {
                let mut cur = Cursor::new(payload);
                let (
                    Some(message_count),
                    Some(_schema_count),
                    Some(_channel_count),
                    Some(_attachment_count),
                    Some(_metadata_count),
                    Some(chunk_count),
                    Some(message_start_time),
                    Some(message_end_time),
                ) = (
                    cur.u64(),
                    cur.u16(),
                    cur.u32(),
                    cur.u32(),
                    cur.u32(),
                    cur.u32(),
                    cur.u64(),
                    cur.u64(),
                )
                else {
                    continue;
                };
                let mut channel_message_counts = BTreeMap::new();
                if let Some(pairs) = cur.bytes() {
                    let mut pairs = Cursor::new(pairs);
                    while let (Some(id), Some(count)) = (pairs.u16(), pairs.u64()) {
                        channel_message_counts.insert(id, count);
                    }
                }
                statistics = Some(SummaryStatistics {
                    message_count,
                    channel_message_counts,
                    message_start_time,
                    message_end_time,
                    chunk_count,
                });
            }
            OP_ATTACHMENT_INDEX => {
                // offset, length, log_time, create_time, data_size, then the name and media type —
                // which are in the index itself, so nothing is read from the attachment.
                let mut cur = Cursor::new(payload);
                let (Some(_), Some(_), Some(_), Some(_), Some(_)) =
                    (cur.u64(), cur.u64(), cur.u64(), cur.u64(), cur.u64())
                else {
                    continue;
                };
                if let (Some(name), Some(media_type)) = (cur.string(), cur.string()) {
                    if attachments.len() < MAX_METADATA_RECORDS {
                        attachments.push((name, media_type));
                    }
                }
            }
            OP_METADATA_INDEX => {
                let mut cur = Cursor::new(payload);
                if let (Some(offset), Some(length)) = (cur.u64(), cur.u64()) {
                    if metadata_index.len() < MAX_METADATA_RECORDS {
                        metadata_index.push((offset, length));
                    }
                }
            }
            _ => {}
        }
    }

    // Follow the index to the Metadata records themselves. This is where a producer writes the
    // licence, the sensor and the clock source — the provenance a summary-only read would otherwise
    // report as entirely absent, which is a claim about the file rather than about the read. Each
    // record is a few hundred bytes at an offset the *file* chose, so every one is bounds-checked
    // against the file's real length and the whole set is capped.
    let mut metadata = Vec::new();
    let mut spent = 0u64;
    for (offset, length) in metadata_index {
        if offset < MAGIC_LEN || length == 0 || offset.saturating_add(length) > footer_at {
            // An entry pointing outside the file is skipped rather than followed: the rest of the
            // index is still usable, and refusing the whole read over one bad pointer would make a
            // slightly-wrong file unreadable when its topics are perfectly legible.
            continue;
        }
        spent = spent.saturating_add(length);
        if spent > MAX_METADATA_BYTES {
            break;
        }
        let mut record = vec![0u8; length as usize];
        if file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut record))
            .is_err()
        {
            continue;
        }
        let Some((OP_METADATA, payload)) = first_record(&record) else {
            continue;
        };
        let mut cur = Cursor::new(payload);
        let Some(name) = cur.string() else { continue };
        let mut pairs = Vec::new();
        if let Some(map) = cur.bytes() {
            let mut map = Cursor::new(map);
            while let (Some(key), Some(value)) = (map.string(), map.string()) {
                pairs.push((key, value));
            }
        }
        metadata.push((name, pairs));
    }

    // A Schema record may follow the Channel that refers to it, so the names are resolved after the
    // whole section is walked rather than as each channel is read.
    for (channel, (_, schema_id)) in channels.iter_mut().zip(pending) {
        channel.schema_name = schemas.get(&schema_id).cloned();
    }
    if channels.is_empty() {
        return Err(refuse(
            "this file's MCAP summary section declares no channels, so there is no topic inventory \
             to check without reading the file — drop --metadata-only"
                .into(),
        ));
    }
    channels.sort_by(|a, b| a.topic.cmp(&b.topic));
    Ok(McapSummary {
        profile,
        library,
        channels,
        statistics,
        metadata,
        attachments,
    })
}

/// The first record in `bytes`, as `(opcode, payload)`.
fn first_record(bytes: &[u8]) -> Option<(u8, &[u8])> {
    records(bytes).next()
}

/// Walk `bytes` as a sequence of `opcode: u8, len: u64le, payload` records, stopping at the first
/// one that does not fit. A length is the file author's number, so it is checked against what is
/// left rather than used to index.
fn records(bytes: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let opcode = *bytes.get(at)?;
        let len = u64::from_le_bytes(bytes.get(at + 1..at + 9)?.try_into().ok()?);
        let start = at.checked_add(9)?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        let payload = bytes.get(start..end)?;
        at = end;
        Some((opcode, payload))
    })
}
