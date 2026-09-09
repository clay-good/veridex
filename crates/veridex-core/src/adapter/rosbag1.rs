//! ROS 1 rosbag (`.bag`) format v2.0.
//!
//! ROS 1 is still where a large share of the world's robot data sits, and until this reader a `.bag`
//! was refused at ingest — no report, no score, no certificate. That is the one failure a
//! cross-format verifier cannot have: the claim is that *which* format a team chose does not change
//! whether their data can be checked.
//!
//! A bag is a record stream, the same shape as the containers already read here. Past a
//! `#ROSBAG V2.0` line, every record is `header_len | header | data_len | data`, and the header is a
//! run of `field_len | name=value` pairs, one of which is the single-byte `op` naming the record's
//! kind. Connection records give a topic and its ROS message type; message records give a
//! connection, a timestamp on the recorder's clock, and the serialized body.
//!
//! Scope, stated rather than guessed at. **Read:** topics, their ROS types, the recorder's clock,
//! each message's bytes (fingerprinted), and the bodies themselves — through
//! `rosmsg::decode_body`, the one dispatch from a ROS message type to the CDM that the MCAP
//! adapter and both rosbag2 storage plugins also call. ROS 1 is a different *encoding* of the same
//! fields in the same order — no encapsulation header, no alignment padding between primitives, and
//! a `seq` at the front of every `std_msgs/Header` — and that difference lives in
//! `cdr::Encoding`, not here. Chunks are read uncompressed, **lz4** (`rosbag record --lz4`) and
//! **bz2** (`rosbag compress`'s default, and so most of what sits in an archive). **Unread** (a
//! `COVERAGE.SOURCE_UNREAD` warning in the verdict): a chunk in any other compression a future
//! rosbag writes, and one whose stream is corrupt or past this run's decompression budget —
//! because the messages are in the file and nobody read them.
//! **Unmapped** (a note about shape): the bulk payload of a body — an image's pixels, a cloud's
//! points — which is fingerprinted and never interpreted, as it is in every reader here.
//!
//! One field a bag carries that a rosbag2 does not: `header.seq`, the publisher's own count of what
//! it sent. A hole in it is the only direct evidence a recording holds of a message that never
//! reached the recorder.
//!
//! Every length in a bag is a number the file chose, so each one is bounded against what the buffer
//! actually holds before it is trusted: a corrupt or hostile bag is refused by name, never allocated
//! for.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{
    read_source_whole, Adapter, Coverage, Detection, FrameBudget, IngestError, IngestOptions,
    IngestReport, Ingested, Source, UnmappedField,
};
use crate::cdm::{
    Calibration, CameraIntrinsics, ClockKind, Dataset, EgoPose, Episode, Frame, PointField,
    Provenance, ProvenanceClass, ProvenanceElement, ProvenanceScope, Stream, Transform, ValueRef,
};

/// The format id this adapter reports, and the `source_format` it records.
const FORMAT_ID: &str = "rosbag1";

/// The clock every frame in a bag is timed on: one recording, one recorder's clock.
const CLOCK_ID: &str = "rosbag1-log";

/// The magic line every v2.0 bag opens with, newline included.
const MAGIC: &[u8] = b"#ROSBAG V2.0\n";

/// Record kinds, from the `op` header field.
const OP_MESSAGE_DATA: u8 = 0x02;
const OP_BAG_HEADER: u8 = 0x03;
const OP_CHUNK: u8 = 0x05;
const OP_CHUNK_INFO: u8 = 0x06;
const OP_CONNECTION: u8 = 0x07;

/// The most a single record's header or data may claim, as a first bound before the buffer's own
/// length is consulted.
///
/// A bag's records are small — a header is a handful of short fields, and a message is one ROS
/// message — while the number a corrupt file can put in a length prefix is `u32::MAX`. Every read
/// below is additionally bounded by the bytes actually present, so this is belt to that braces: it
/// stops a 4 GB claim from reaching an allocation at all.
const MAX_RECORD_BYTES: usize = 256 * 1024 * 1024;

/// The ROS 1 rosbag reader.
pub struct Rosbag1Adapter;

impl Rosbag1Adapter {
    /// Whether `path` is a file whose first bytes are a v2.0 bag's magic.
    ///
    /// By content, not by extension: a `.bag` is recognized because it says it is one, and a file
    /// named `.bag` that is something else is not claimed.
    fn is_bag(path: &Path) -> bool {
        use std::io::Read;
        let Ok(mut f) = std::fs::File::open(path) else {
            return false;
        };
        let mut head = [0u8; MAGIC.len()];
        f.read_exact(&mut head).is_ok() && head == MAGIC
    }
}

impl Adapter for Rosbag1Adapter {
    fn format_id(&self) -> &'static str {
        FORMAT_ID
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["2.0"]
    }

    /// A file that says it is a bag, or a directory holding at least one — the shape
    /// `rosbag record --split` leaves behind.
    fn detect(&self, source: &Source) -> Detection {
        match source {
            Source::Local(path) if !bag_files(path).is_empty() => Detection::Yes {
                version: Some("2.0".into()),
            },
            _ => Detection::No,
        }
    }

    /// A v2.0 bag ends with an **index section** that a metadata-only read can answer from: the
    /// header names its offset, and what sits there is every connection the recording declared plus
    /// one chunk-info record per chunk, carrying that chunk's time span and its per-connection
    /// message counts. So the topic inventory, the recording's span and how many messages each topic
    /// carries are all readable without unpacking a single chunk — which is the difference between
    /// inventorying a 40 GB archived bag and not being able to look at it at all.
    ///
    /// A bag whose header names no index — one a recorder was killed before it could finish writing
    /// — is refused by name rather than answered from a guess.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    /// One bag is one recording, ingested as one episode, so there is no episode axis to sample.
    fn supports_sampling(&self) -> bool {
        false
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        let path = match source {
            Source::Local(p) => p,
            Source::Remote(_) => {
                return Err(IngestError::Parse {
                    format_id: FORMAT_ID,
                    message: "a ROS 1 bag must be a local file".into(),
                })
            }
        };
        // One recording, however many files it was written to. `rosbag record --split` is how any
        // recording long enough to care about is made, and its parts are one session: reading them
        // separately gives each part its own verdict, its own score and its own certificate, and
        // leaves every cross-episode check with one episode to compare.
        let files = bag_files(path);
        if files.is_empty() {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: "not a ROS 1 bag (missing the `#ROSBAG V2.0` line)".into(),
            });
        }

        if options.metadata_only {
            return ingest_metadata_only(path, &files, options);
        }

        let mut walk = Walk::default();
        for file in &files {
            let bytes = read_source_whole(
                file,
                FORMAT_ID,
                options,
                "a bag's records are chained by length, so the stream is read whole",
            )?;
            // Each file is its own record stream, with its own connection ids: `conn 0` in the
            // second part of a split recording is whichever topic that part declared first, not the
            // one the first part called `conn 0`.
            walk.connections.clear();
            walk.file = display(file);
            // The magic was what identified the file; a file that changed underneath the detection
            // is read as the nothing it now is rather than sliced past its end.
            let Some(records) = bytes.get(MAGIC.len()..) else {
                continue;
            };
            walk.records(records, options, true)?;
        }

        let mut budget = FrameBudget::new(options);
        let mut streams: Vec<Stream> = Vec::new();
        let (mut min_ts, mut max_ts) = (i64::MAX, i64::MIN);
        let messages = std::mem::take(&mut walk.messages);
        for (name, mut acc) in messages {
            // A bag writes messages in chunk order, which is time order per chunk but not across
            // them once a recorder buffers — and across the files of a split recording, later parts
            // hold later messages but nothing in the format promises it.
            acc.msgs.sort_by_key(|(ts, _)| *ts);
            budget.take(FORMAT_ID, acc.msgs.len() as u64)?;
            let frames: Vec<Frame> = acc
                .msgs
                .iter()
                .map(|(ts, hash)| {
                    min_ts = min_ts.min(*ts);
                    max_ts = max_ts.max(*ts);
                    Frame {
                        ts: *ts,
                        value_ref: ValueRef {
                            uri: format!("rosbag1:{name}"),
                            byte_offset: None,
                            byte_len: None,
                            content_hash: Some(*hash),
                        },
                    }
                })
                .collect();
            if frames.is_empty() {
                continue;
            }
            // A topic whose values this read declined to summarize is disclosed, not left looking
            // like a topic that had nothing to say. See `StreamValues::refusal`.
            if let Some(why) = acc.values.refusal() {
                walk.unread.push(UnmappedField {
                    source_path: name.clone(),
                    note: why.into(),
                });
            }
            let measured = acc.values.finish();
            let values = measured.as_ref().map(|(a, _)| a);
            streams.push(Stream {
                // The same classifier the two ROS 2 readers use, over the same ROS type names: a rig
                // recorded to a bag types the way the same rig recorded to an MCAP does.
                modality: super::mcap::infer_modality(&acc.ros_type, &name),
                name,
                declared_rate_hz: None,
                // One bag is one recorder's clock, and every message time is on it.
                clock_id: CLOCK_ID.into(),
                clock_kind: ClockKind::Measured,
                dtype: None,
                shape: None,
                dim_names: measured.as_ref().and_then(|(_, n)| n.clone()),
                frames,
                stats: None,
                dim_stats: None,
                // Every message whose whole payload is its measurement — a `JointState`, an `Imu`,
                // a `NavSatFix` — is summarized here; every other topic's payload stays opaque and
                // says so through `STATISTICAL.UNMEASURED_VALUES`.
                observed_stats: values.and_then(|a| a.stats()),
                observed_saturation: values.and_then(|a| a.saturation()),
                observed_non_finite: values.map(|a| a.non_finite()),
                observed_dim_stats: values.and_then(|a| a.dim_stats()),
                latched: acc.latching,
                // A bag declares no range for a topic; there is nothing to compare values against.
                declared_range: None,
                point_fields: acc.point_fields,
                observed_point_counts: acc.point_counts.finish(),
                observed_image_dims: acc.image_dims.finish(),
                observed_body_decodes: acc.body_decodes.finish(),
                observed_header_stamps: acc.header_stamps.finish(),
                observed_sequence: acc.sequence.finish(),
                observed_fix_availability: acc.fix_availability.finish(),
                media: None,
                frame_id: acc.frame_id,
            });
        }
        streams.sort_by(|a, b| a.name.cmp(&b.name));

        if streams.is_empty() {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: "the bag declares no topic carrying messages this reader could place in \
                          time"
                    .into(),
            });
        }

        // The rig, as the recording's own messages describe it: the transform tree its `TFMessage`s
        // publish and the intrinsics its `CameraInfo`s carry, both decoded from bodies rather than
        // claimed by a sidecar.
        let calibration = if walk.transforms.is_empty() && walk.intrinsics.is_empty() {
            None
        } else {
            Some(Calibration {
                transforms: std::mem::take(&mut walk.transforms).into_values().collect(),
                intrinsics: std::mem::take(&mut walk.intrinsics).into_values().collect(),
            })
        };
        let ego_poses = if walk.ego_poses.is_empty() {
            None
        } else {
            let mut poses = std::mem::take(&mut walk.ego_poses);
            poses.sort_by_key(|p| p.ts);
            Some(poses)
        };
        // An edge the recording republished with a different pose: a rig whose frames moved, read as
        // one that stood still. Disclosed rather than dropped.
        if !walk.moved_frames.is_empty() {
            walk.unread.push(UnmappedField {
                source_path: "tf".into(),
                note: walk.moved_frames.note(),
            });
        }
        if walk.orphan_messages > 0 {
            walk.unread.push(UnmappedField {
                source_path: "message records".into(),
                note: format!(
                    "{} message(s) name a connection no connection record in their file declares, \
                     so the topic and type they belong to are unknown; they contribute no frames",
                    walk.orphan_messages
                ),
            });
        }

        let dataset = Dataset {
            id: super::dataset_id_from_path(path, FORMAT_ID),
            metadata: {
                let mut m = vec![("source_format".into(), FORMAT_ID.to_string())];
                if let Some(recorder) = &walk.recorder {
                    m.push(("recorder".into(), recorder.clone()));
                }
                m
            },
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements: {
                    let mut elements = vec![ProvenanceElement {
                        key: "source_format".into(),
                        value: Some(FORMAT_ID.to_string()),
                        class: ProvenanceClass::Known,
                    }];
                    // Only where the bag named one: a mapped field is a statement that this run read
                    // something, and most bags carry no `callerid` on their connections.
                    if let Some(recorder) = &walk.recorder {
                        elements.push(ProvenanceElement {
                            key: "recorder".into(),
                            value: Some(recorder.clone()),
                            class: ProvenanceClass::Known,
                        });
                    }
                    // A bag that carries its own transform tree and camera intrinsics identifies the
                    // calibration that produced it — in the recording, and bound into the CDM
                    // content hash.
                    if let Some(calib) = &calibration {
                        elements.push(ProvenanceElement {
                            key: "calibration".into(),
                            value: Some(super::in_band_calibration(calib)),
                            class: ProvenanceClass::Known,
                        });
                    }
                    elements
                },
            }],
            episodes: vec![Episode {
                index: 0,
                start_ts: (min_ts != i64::MAX).then_some(min_ts),
                end_ts: (max_ts != i64::MIN).then_some(max_ts),
                streams,
                task: None,
                labels: vec![],
                ego_poses,
                ego_frame: walk.ego_frame.clone(),
                declared_frame_count: None,
            }],
            calibration,
        };

        // What this run read, from the CDM it actually produced rather than from what the reader
        // hoped to find: a bag with no `CameraInfo` on it claims no calibration mapping.
        let mapped_fields = {
            let mut mapped = vec![
                "connection record topic + ROS type -> stream (and its modality)".into(),
                "message record time -> frame.ts".into(),
                "message body bytes -> frame.value_ref.content_hash (SHA-256)".into(),
                "std_msgs/Header stamp + frame_id -> stream.observed_header_stamps, stream.frame_id"
                    .into(),
                "std_msgs/Header seq -> stream.observed_sequence".into(),
            ];
            if dataset.calibration.is_some() {
                mapped.push("CameraInfo.k/d + TFMessage -> dataset.calibration".into());
            }
            if dataset.episodes.iter().any(|e| e.ego_poses.is_some()) {
                mapped.push("Odometry.pose -> episode.ego_poses".into());
            }
            if dataset
                .episodes
                .iter()
                .flat_map(|e| &e.streams)
                .any(|s| s.observed_stats.is_some())
            {
                mapped.push(
                    "JointState / Imu / NavSatFix / Twist / Wrench values -> stream.observed_stats"
                        .into(),
                );
            }
            mapped
        };

        Ok(Ingested {
            dataset,
            report: IngestReport {
                unread_sources: walk.unread,
                format_id: FORMAT_ID,
                source_version: Some("2.0".into()),
                coverage: Coverage::Full,
                mapped_fields,
                unmapped_fields: std::iter::once(UnmappedField {
                    source_path: "message data".into(),
                    note: "a message body is read only as far as the fields the CDM holds — a \
                           cloud's point layout and count, an image's dimensions, a rig's \
                           transforms and intrinsics, a sensor's readings. The bulk payload (the \
                           pixels, the points) is fingerprinted, never interpreted, and a message \
                           type with no typed decoder is fingerprinted whole"
                        .into(),
                })
                .chain(walk.unmapped)
                .collect(),
                omitted_fields: vec![
                    "episode segmentation (a bag records one continuous session)".into(),
                    "declared-rate (a bag declares no nominal rate for a topic)".into(),
                ],
            },
        })
    }
}

/// One topic, as its connection record declares it.
struct Topic {
    name: String,
    ros_type: String,
    /// `true` where the connection says the topic is latched — published once and retained rather
    /// than sampled, which several checks abstain on.
    latching: Option<bool>,
}

/// Everything one topic's messages contributed, accumulated as they were read.
///
/// Kept as running summaries rather than retained bodies: how many messages a topic carries is a
/// number the file chose, and a bag holds the whole recording.
#[derive(Default)]
struct TopicAccum {
    /// The ROS type the first connection record to name this topic declared, and whether that
    /// connection said the topic is latched.
    ros_type: String,
    latching: Option<bool>,
    /// Each message's time and the fingerprint of its body.
    msgs: Vec<(i64, [u8; 32])>,
    point_fields: Option<Vec<PointField>>,
    point_counts: super::cdr::PointCountAccum,
    image_dims: super::cdr::ImageDimAccum,
    body_decodes: super::cdr::BodyDecodeAccum,
    header_stamps: super::cdr::HeaderStampAccum,
    /// What this topic's publisher said about how many messages it sent, from the `seq` on each
    /// header. Empty for a topic whose bodies are not header-first.
    sequence: super::mcap::SequenceAccum,
    fix_availability: super::cdr::FixAvailabilityAccum,
    frame_id: Option<String>,
    values: super::stats::StreamValues,
}

/// What one walk of a recording's record streams collected.
///
/// One `Walk` spans every file of the recording, because a split one is still one recording: the
/// accumulators are keyed by **topic name**, which is what a topic is called in every file, rather
/// than by connection id, which is a per-file handle that different files reuse for different
/// topics.
#[derive(Default)]
struct Walk {
    /// The connections the file being read declares. Cleared between files.
    connections: BTreeMap<u32, Topic>,
    /// Per topic, across every file: everything its messages said.
    messages: BTreeMap<String, TopicAccum>,
    /// The `callerid` the bag header names, where it names one.
    recorder: Option<String>,
    /// The rig, as the recording's own messages describe it.
    ego_poses: Vec<EgoPose>,
    ego_frame: Option<String>,
    intrinsics: BTreeMap<String, CameraIntrinsics>,
    transforms: BTreeMap<(String, String), Transform>,
    moved_frames: super::cdr::MovingFrames,
    /// Messages naming a connection the file they are in never declared. There is no topic name to
    /// file them under, so they contribute no frames — and inventing one would attribute frames to a
    /// topic the recording does not have.
    orphan_messages: u64,
    /// The file being read, for the disclosures that name one.
    file: String,
    /// What the recording said that the CDM has one field for, and this run kept one of.
    unmapped: Vec<UnmappedField>,
    unread: Vec<UnmappedField>,
}

impl Walk {
    /// Walk a record stream, following chunks one level down.
    ///
    /// `top` distinguishes the file's own stream from a chunk's contents, because a chunk may not
    /// contain another chunk and a bag header appears only at the top. Bounded to those two levels
    /// by construction rather than by a depth counter.
    fn records(
        &mut self,
        mut buf: &[u8],
        options: &IngestOptions,
        top: bool,
    ) -> Result<(), IngestError> {
        while !buf.is_empty() {
            let Some((header, data, rest)) = split_record(buf) else {
                // A trailing fragment too short to be a record is where a truncated bag ends. The
                // records before it were read; saying so is better than refusing the whole file.
                if !buf.is_empty() {
                    self.unread.push(UnmappedField {
                        source_path: "records".into(),
                        note: format!(
                            "{} trailing byte(s) are too short to form a record — the bag is \
                             truncated, and anything after this point was not read",
                            buf.len()
                        ),
                    });
                }
                return Ok(());
            };
            buf = rest;
            let fields = header_fields(header);
            let Some(op) = fields.get("op").and_then(|v| v.first().copied()) else {
                continue; // a record naming no kind is not one this reader can place
            };
            match op {
                OP_BAG_HEADER if top => {
                    if let Some(id) = fields.get("callerid").and_then(|v| text(v)) {
                        // The first part of a split recording settles who recorded it. A later part
                        // naming someone else is not the same session, and quietly taking the last
                        // name would report the recording as that one's.
                        match &self.recorder {
                            None => self.recorder = Some(id),
                            Some(first) if *first != id => {
                                let note = format!(
                                    "`{}` names `{id}` as its recorder while the recording's first \
                                     file names `{first}`; the files were read as one recording and \
                                     the first name is the one reported",
                                    self.file
                                );
                                // Not unread data — every byte of both headers was read. It is a
                                // second answer the CDM holds one field for, which is what
                                // `unmapped` is: a note about shape, not a coverage hole.
                                self.unmapped.push(UnmappedField {
                                    source_path: "bag header callerid".into(),
                                    note,
                                });
                            }
                            Some(_) => {}
                        }
                    }
                }
                OP_CONNECTION => self.connection(&fields, data),
                OP_MESSAGE_DATA => self.message(&fields, data),
                OP_CHUNK if top => self.chunk(&fields, data, options)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// A connection record: the header carries the id, the *data* carries the topic's type.
    fn connection(&mut self, fields: &BTreeMap<String, Vec<u8>>, data: &[u8]) {
        if let (Some(conn), Some(topic)) = connection_of(fields, data) {
            self.connections.insert(conn, topic);
        }
    }

    /// A message record: its connection, its time on the recorder's clock, and its body.
    fn message(&mut self, fields: &BTreeMap<String, Vec<u8>>, data: &[u8]) {
        let (Some(conn), Some(ts)) = (
            fields.get("conn").and_then(|v| u32_le(v)),
            fields.get("time").and_then(|v| ros_time(v)),
        ) else {
            return;
        };
        // The bytes are fingerprinted whatever else happens to them — the discipline every container
        // reader here follows, and what gives the content-level checks something exact to compare.
        let hash: [u8; 32] = Sha256::digest(data).into();
        // Field by field, so the connection this message names can be read while its accumulator and
        // the bag-wide collections are written.
        let Self {
            connections,
            messages,
            ego_poses,
            ego_frame,
            intrinsics,
            transforms,
            moved_frames,
            orphan_messages,
            ..
        } = self;
        let Some(topic) = connections.get(&conn) else {
            // A message referring to a connection this file never declared names a topic nothing can
            // identify. Counted and reported rather than filed under a stream it cannot belong to —
            // inventing one would attribute frames to a topic the bag does not have.
            *orphan_messages += 1;
            return;
        };
        let acc = messages.entry(topic.name.clone()).or_default();
        if acc.msgs.is_empty() {
            acc.ros_type = topic.ros_type.clone();
            acc.latching = topic.latching;
        }
        acc.msgs.push((ts, hash));

        // The frame this sensor's data is expressed in, and the sensor's own clock against the
        // recorder's — both out of the `std_msgs/Header` a ROS message begins with, read here the
        // way the two ROS 2 readers read theirs.
        if acc.frame_id.is_none() {
            acc.frame_id = super::cdr::decode_header_frame_id(data, super::cdr::Encoding::Ros1);
        }
        if let Some(stamp) = super::cdr::decode_header_stamp(data, super::cdr::Encoding::Ros1) {
            acc.header_stamps.observe(ts, stamp);
        }
        // The publisher's own count of what it sent. ROS 1 keeps it on the header ROS 2 dropped, so
        // a `.bag` answers "did a message go missing before the recorder saw it?" — which the same
        // rig recorded to a `.db3` cannot.
        if let Some(seq) = super::cdr::decode_header_seq(data, super::cdr::Encoding::Ros1) {
            acc.sequence.observe(seq);
        }

        let decoded = super::rosmsg::decode_body(
            &mut super::rosmsg::BodyTargets {
                point_fields: &mut acc.point_fields,
                point_counts: &mut acc.point_counts,
                image_dims: &mut acc.image_dims,
                fix_availability: &mut acc.fix_availability,
                values: &mut acc.values,
                ego_poses,
                ego_frame,
                intrinsics,
                transforms,
                moved_frames,
            },
            super::cdr::Encoding::Ros1,
            &topic.ros_type,
            &topic.name,
            data,
            ts,
        );
        acc.body_decodes.observe(decoded);
    }

    /// A chunk record: a compressed or raw run of connection and message records.
    fn chunk(
        &mut self,
        fields: &BTreeMap<String, Vec<u8>>,
        data: &[u8],
        options: &IngestOptions,
    ) -> Result<(), IngestError> {
        let compression = fields
            .get("compression")
            .and_then(|v| text(v))
            .unwrap_or_else(|| "none".to_string());
        // What the chunk says it unpacks to, which is what the budget is charged against before a
        // decompressor is pointed at anything.
        let declared = fields.get("size").and_then(|v| u32_le(v)).unwrap_or(0) as usize;
        match compression.as_str() {
            "none" => self.records(data, options, false),
            // The two compressions `rosbag record --lz4` and `rosbag compress` write, read the same
            // way: charge the budget with what the chunk *declares* before a decompressor is pointed
            // at anything, then cap the read at one byte past that.
            "lz4" => {
                let unpacked = self.unpack(
                    "lz4",
                    declared,
                    options,
                    lz4_flex::frame::FrameDecoder::new(data),
                    data.len(),
                );
                match unpacked {
                    Some(out) => self.records(&out, options, false),
                    None => Ok(()),
                }
            }
            "bz2" => {
                let unpacked = self.unpack(
                    "bz2",
                    declared,
                    options,
                    bzip2::read::BzDecoder::new(data),
                    data.len(),
                );
                match unpacked {
                    Some(out) => self.records(&out, options, false),
                    None => Ok(()),
                }
            }
            other => {
                // The messages are in the file and nobody read them, which is a coverage hole rather
                // than a shape the CDM cannot hold.
                self.unread.push(UnmappedField {
                    source_path: "chunk".into(),
                    note: format!(
                        "a chunk compressed with `{other}` is not decompressed by this reader, so the {declared} byte(s) of messages it holds were not read"
                    ),
                });
                Ok(())
            }
        }
    }

    /// Decompress one chunk's stream under this run's decompression budget, or disclose why not.
    ///
    /// `None` means the messages in that chunk were not read, and the reason is already on
    /// [`Walk::unread`] — a coverage hole the verdict carries, never a silent skip.
    ///
    /// Three ways a chunk goes unread, and each is a defence rather than a nicety. The declared
    /// size is charged to the budget *before* a decompressor sees a byte, so a chunk that claims to
    /// unpack to more than this run allows costs nothing. The read is then capped at one byte past
    /// what the chunk declared, so a stream that keeps producing — the shape of a decompression
    /// bomb, and of a corrupt chunk — stops at a size the file cannot choose. And a stream that
    /// produces more than its own header promised is corrupt by its own account, so what it did
    /// produce is not read as messages.
    fn unpack(
        &mut self,
        compression: &str,
        declared: usize,
        options: &IngestOptions,
        reader: impl std::io::Read,
        compressed_len: usize,
    ) -> Option<Vec<u8>> {
        let mut budget = super::DecompressionBudget::new(options, compressed_len as u64);
        if budget.take(FORMAT_ID, declared as u64).is_err() {
            self.unread.push(UnmappedField {
                source_path: "chunk".into(),
                note: format!(
                    "a {compression} chunk declaring {declared} uncompressed byte(s) is past this run's decompression budget; its messages were not read"
                ),
            });
            return None;
        }
        let cap = (declared as u64).saturating_add(1).min(
            budget
                .remaining()
                .map_or(u64::MAX, |left| left.saturating_add(1)),
        );
        let mut out = Vec::new();
        match std::io::copy(&mut std::io::Read::take(reader, cap), &mut out) {
            Ok(n) if n as usize <= declared => Some(out),
            Ok(_) => {
                self.unread.push(UnmappedField {
                    source_path: "chunk".into(),
                    note: format!(
                        "a {compression} chunk declares {declared} uncompressed byte(s) but its stream produces more; the chunk is corrupt and its messages were not read"
                    ),
                });
                None
            }
            Err(e) => {
                self.unread.push(UnmappedField {
                    source_path: "chunk".into(),
                    note: format!(
                        "a {compression} chunk declaring {declared} byte(s) did not decompress ({e}); its messages contribute no frames"
                    ),
                });
                None
            }
        }
    }
}

/// One connection record, as its header and data declare it: the id, the topic, its ROS type, and
/// whether the topic is latched.
///
/// Read the same way wherever a connection record appears — inside a chunk during a full read, and
/// in the index section during a metadata-only one — so the two paths cannot disagree about what a
/// bag says its topics are.
fn connection_of(fields: &BTreeMap<String, Vec<u8>>, data: &[u8]) -> (Option<u32>, Option<Topic>) {
    let conn = fields.get("conn").and_then(|v| u32_le(v));
    let inner = header_fields(data);
    // The topic is written in both places; the connection header's copy is the one rosbag treats as
    // authoritative, and the data's is the fallback for a writer that omitted it.
    let name = fields
        .get("topic")
        .and_then(|v| text(v))
        .or_else(|| inner.get("topic").and_then(|v| text(v)));
    let topic = name.map(|name| Topic {
        name,
        ros_type: inner.get("type").and_then(|v| text(v)).unwrap_or_default(),
        latching: inner
            .get("latching")
            .and_then(|v| text(v))
            .map(|v| v.trim() == "1"),
    });
    (conn, topic)
}

/// What one bag's index section declares: its topics, how many messages each carries, and the span
/// the chunks were recorded over.
#[derive(Default)]
struct Index {
    /// Topic name -> (ROS type, message count).
    topics: BTreeMap<String, (String, u64)>,
    recorder: Option<String>,
    min_ts: Option<i64>,
    max_ts: Option<i64>,
    /// Messages the chunk-info records account for that name a connection the index never declared.
    orphan_messages: u64,
}

/// Read a bag's index section — the records past the offset its own header names — without touching
/// a chunk.
///
/// `None` for a bag whose header names no index: a recorder killed mid-write leaves `index_pos` at
/// zero, and there is nothing to answer from. The caller refuses rather than guessing, because the
/// alternative is presenting whatever the first chunk happened to declare as the bag's contents.
///
/// Every length here comes out of the file, so each is bounded before it is trusted, exactly as in
/// the full read: the index of a corrupt bag is refused, never allocated for.
fn read_index(path: &Path, options: &IngestOptions) -> Result<Option<Index>, IngestError> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = std::fs::File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
    // The bag header is the first record after the magic, and it is small.
    let mut head = vec![0u8; MAGIC.len() + 4096];
    let read = f
        .read(&mut head)
        .map_err(|e| IngestError::Io(e.to_string()))?;
    head.truncate(read);
    let Some((header, _, _)) = head.get(MAGIC.len()..).and_then(split_record) else {
        return Ok(None);
    };
    let fields = header_fields(header);
    if fields.get("op").and_then(|v| v.first().copied()) != Some(OP_BAG_HEADER) {
        return Ok(None);
    }
    let index_pos = fields
        .get("index_pos")
        .and_then(|v| v.get(..8))
        .and_then(|v| <[u8; 8]>::try_from(v).ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0);
    let size = f
        .metadata()
        .map_err(|e| IngestError::Io(e.to_string()))?
        .len();
    if index_pos == 0 || index_pos >= size {
        return Ok(None);
    }
    // Only the index is read, and it is charged against the same ceiling a whole-file read is: it is
    // a slice of the file, chosen by the file.
    let index_len = size - index_pos;
    super::check_source_size(
        index_len,
        FORMAT_ID,
        options,
        "a bag's index section is read whole to answer a metadata-only run",
    )?;
    f.seek(SeekFrom::Start(index_pos))
        .map_err(|e| IngestError::Io(e.to_string()))?;
    let mut buf = Vec::with_capacity(usize::try_from(index_len).unwrap_or(0));
    f.take(index_len)
        .read_to_end(&mut buf)
        .map_err(|e| IngestError::Io(e.to_string()))?;

    let mut index = Index {
        recorder: fields.get("callerid").and_then(|v| text(v)),
        ..Index::default()
    };
    // Connection ids are per file, and the index declares them all before the chunk-info records
    // that reference them.
    let mut connections: BTreeMap<u32, Topic> = BTreeMap::new();
    let mut counts: BTreeMap<u32, u64> = BTreeMap::new();
    let mut rest = buf.as_slice();
    while let Some((header, data, tail)) = split_record(rest) {
        rest = tail;
        let fields = header_fields(header);
        match fields.get("op").and_then(|v| v.first().copied()) {
            Some(OP_CONNECTION) => {
                if let (Some(conn), Some(topic)) = connection_of(&fields, data) {
                    connections.insert(conn, topic);
                }
            }
            Some(OP_CHUNK_INFO) => {
                // `start_time`/`end_time` are this chunk's span, and the data is one
                // `(conn, count)` pair per connection the chunk carries.
                if let Some(ts) = fields.get("start_time").and_then(|v| ros_time(v)) {
                    index.min_ts = Some(index.min_ts.map_or(ts, |m: i64| m.min(ts)));
                }
                if let Some(ts) = fields.get("end_time").and_then(|v| ros_time(v)) {
                    index.max_ts = Some(index.max_ts.map_or(ts, |m: i64| m.max(ts)));
                }
                for pair in data.chunks_exact(8) {
                    let Some(conn) = u32_at(pair, 0) else {
                        continue;
                    };
                    let Some(n) = u32_at(pair, 4) else { continue };
                    *counts.entry(conn).or_default() += u64::from(n);
                }
            }
            _ => {}
        }
    }
    for (conn, n) in counts {
        match connections.get(&conn) {
            Some(topic) => {
                let entry = index
                    .topics
                    .entry(topic.name.clone())
                    .or_insert_with(|| (topic.ros_type.clone(), 0));
                entry.1 += n;
            }
            // A count against a connection the index never declared belongs to a topic nothing
            // names, exactly as in the full read.
            None => index.orphan_messages += n,
        }
    }
    // A connection that carried nothing is still a topic the recording declared.
    for topic in connections.into_values() {
        index
            .topics
            .entry(topic.name)
            .or_insert((topic.ros_type, 0));
    }
    Ok(Some(index))
}

/// Ingest from the index sections alone, without opening a chunk.
///
/// What this covers, honestly: the topic inventory each bag's index declares — every topic's name,
/// its ROS type, and the modality that type implies — the recorder the header names, and how many
/// messages the recording holds. What it does not cover is everything a chunk would answer: no
/// timestamps on any frame, no message bytes, no content hashes, no decoded rig calibration or ego
/// trajectory. Every stream therefore carries zero frames *by request*, which is what
/// [`Coverage::MetadataOnly`] tells the checks that reason about frames, so they abstain rather than
/// reading that absence as a defect — and a certificate cannot be issued from it.
///
/// Refused, not approximated, when a file names no index: that bag's messages would simply be absent
/// from an inventory presented as the recording's, which is the shape of failure this tool exists to
/// prevent and the one a caller has no way to notice.
fn ingest_metadata_only(
    path: &Path,
    files: &[std::path::PathBuf],
    options: &IngestOptions,
) -> Result<Ingested, IngestError> {
    let mut topics: BTreeMap<String, (String, u64)> = BTreeMap::new();
    let mut recorder = None;
    let mut orphans = 0u64;
    for file in files {
        let Some(index) = read_index(file, options)? else {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: format!(
                    "`{}` names no index section — it is a bag whose writer did not finish, and \
                     there is nothing to inventory without opening its chunks; drop \
                     --metadata-only to read them",
                    display(file)
                ),
            });
        };
        if recorder.is_none() {
            recorder = index.recorder;
        }
        orphans += index.orphan_messages;
        for (name, (ros_type, count)) in index.topics {
            let entry = topics.entry(name).or_insert((ros_type, 0));
            entry.1 += count;
        }
    }
    if topics.is_empty() {
        return Err(IngestError::Parse {
            format_id: FORMAT_ID,
            message: "the bag's index section declares no topic, so there is nothing to check \
                      without opening its chunks"
                .into(),
        });
    }

    let declared: u64 = topics.values().map(|(_, n)| n).sum();
    let streams: Vec<Stream> = topics
        .iter()
        .map(|(name, (ros_type, _))| Stream {
            name: name.clone(),
            modality: super::mcap::infer_modality(ros_type, name),
            declared_rate_hz: None,
            clock_id: CLOCK_ID.to_string(),
            clock_kind: ClockKind::Measured,
            dtype: None,
            shape: None,
            dim_names: None,
            frames: Vec::new(),
            stats: None,
            dim_stats: None,
            observed_stats: None,
            observed_saturation: None,
            observed_non_finite: None,
            observed_dim_stats: None,
            // The index carries no QoS, so nothing is claimed about latching: the connection records
            // that state it are the ones inside the chunks.
            latched: None,
            declared_range: None,
            point_fields: None,
            observed_point_counts: None,
            observed_image_dims: None,
            observed_body_decodes: None,
            observed_header_stamps: None,
            observed_sequence: None,
            observed_fix_availability: None,
            media: None,
            frame_id: None,
        })
        .collect();

    let mut unread = vec![UnmappedField {
        source_path: "chunks".into(),
        note: format!(
            "the recording's {declared} declared message(s) were not read: this is a \
             metadata-only ingest"
        ),
    }];
    if orphans > 0 {
        unread.push(UnmappedField {
            source_path: "index chunk info".into(),
            note: format!(
                "{orphans} message(s) are counted against a connection the index never declares, so \
                 the topic they belong to is unknown"
            ),
        });
    }

    let dataset = Dataset {
        id: super::dataset_id_from_path(path, FORMAT_ID),
        metadata: {
            let mut m = vec![("source_format".into(), FORMAT_ID.to_string())];
            if let Some(r) = &recorder {
                m.push(("recorder".into(), r.clone()));
            }
            m
        },
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements: {
                let mut elements = vec![ProvenanceElement {
                    key: "source_format".into(),
                    value: Some(FORMAT_ID.to_string()),
                    class: ProvenanceClass::Known,
                }];
                if let Some(r) = &recorder {
                    elements.push(ProvenanceElement {
                        key: "recorder".into(),
                        value: Some(r.clone()),
                        class: ProvenanceClass::Known,
                    });
                }
                elements
            },
        }],
        episodes: vec![Episode {
            index: 0,
            // No frames were read, so there is no measured span to state. The index's own chunk
            // times describe the recording, not the streams in this CDM, and stamping them here
            // would put a timeline on an episode with no frames to support it.
            start_ts: None,
            end_ts: None,
            streams,
            task: None,
            labels: Vec::new(),
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
        calibration: None,
    };

    Ok(Ingested {
        dataset,
        report: IngestReport {
            unread_sources: unread,
            format_id: FORMAT_ID,
            source_version: Some("2.0".into()),
            coverage: Coverage::MetadataOnly {
                episodes_declared: 1,
            },
            mapped_fields: vec![
                "index connection record topic + ROS type -> stream (and its modality)".into(),
                "bag header callerid -> recorder".into(),
            ],
            unmapped_fields: Vec::new(),
            omitted_fields: vec![
                "episode segmentation (a bag records one continuous session)".into(),
                "everything a chunk holds (frames, bodies, calibration, trajectory): not read by \
                 request"
                    .into(),
            ],
        },
    })
}

/// Every bag file this source names, in the order they were recorded.
///
/// A single `.bag` is itself; a directory is every `.bag`-shaped file directly inside it, ordered by
/// [`super::natural_key`] so `foo_10.bag` follows `foo_9.bag` rather than `foo_1.bag` — which is the
/// order `rosbag record --split` wrote them in, and so the order the recording ran in. Files are
/// recognized by their `#ROSBAG V2.0` line, never by their name, so a directory holding a bag beside
/// unrelated files still reads as one recording of the bags.
fn bag_files(path: &Path) -> Vec<std::path::PathBuf> {
    if path.is_file() {
        return if Rosbag1Adapter::is_bag(path) {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        };
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut found: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && Rosbag1Adapter::is_bag(p))
        .collect();
    found.sort_by_key(|p| super::natural_key(&display(p)));
    found
}

/// A path as it should appear in a disclosure: the file name, which is what identifies one part of a
/// split recording, rather than the caller's whole path.
fn display(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("<bag>")
        .to_string()
}

/// Split one `header_len | header | data_len | data` record off the front of `buf`.
///
/// `None` for a buffer too short to hold one, which is how a truncated bag ends. Both lengths come
/// out of the file, so both are bounded by [`MAX_RECORD_BYTES`] and by the bytes actually present
/// before anything is sliced.
fn split_record(buf: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let header_len = u32_at(buf, 0)? as usize;
    if header_len > MAX_RECORD_BYTES {
        return None;
    }
    let header = buf.get(4..4usize.checked_add(header_len)?)?;
    let data_at = 4 + header_len;
    let data_len = u32_at(buf, data_at)? as usize;
    if data_len > MAX_RECORD_BYTES {
        return None;
    }
    let start = data_at.checked_add(4)?;
    let end = start.checked_add(data_len)?;
    let data = buf.get(start..end)?;
    Some((header, data, &buf[end..]))
}

/// A record header: a run of `field_len | name=value` pairs.
///
/// A malformed field ends the parse rather than failing the record — the fields read before it are
/// still what the file said, and a header this reader cannot finish is one whose remaining fields
/// simply do not reach the caller.
fn header_fields(mut buf: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    while let Some(len) = u32_at(buf, 0) {
        let len = len as usize;
        if len > MAX_RECORD_BYTES {
            break;
        }
        let Some(field) = buf.get(4..4 + len) else {
            break;
        };
        buf = &buf[4 + len..];
        let Some(eq) = field.iter().position(|b| *b == b'=') else {
            break;
        };
        let name = String::from_utf8_lossy(&field[..eq]).into_owned();
        out.insert(name, field[eq + 1..].to_vec());
    }
    out
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

fn u32_le(v: &[u8]) -> Option<u32> {
    u32_at(v, 0)
}

/// A ROS 1 `time`: seconds then nanoseconds, both `u32` little-endian, as nanoseconds.
fn ros_time(v: &[u8]) -> Option<i64> {
    let secs = u32_at(v, 0)? as i64;
    let nsecs = u32_at(v, 4)? as i64;
    secs.checked_mul(1_000_000_000)?.checked_add(nsecs)
}

/// A header field's value as text, or `None` where it is not valid UTF-8 worth keeping.
fn text(v: &[u8]) -> Option<String> {
    (!v.is_empty()).then(|| String::from_utf8_lossy(v).into_owned())
}
