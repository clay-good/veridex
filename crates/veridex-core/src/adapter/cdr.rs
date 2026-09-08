//! A minimal, bounds-checked reader for ROS 2 **CDR** (Common Data Representation) message bodies,
//! and decoders for the few AV message *headers* Veridex needs to populate the autonomy CDM.
//!
//! Scope and honesty: Veridex still never interprets the bulk payload of a message — the point cloud's
//! points, the image's pixels. It decodes only the small structural preamble that describes the rig:
//! a `PointCloud2`'s per-point field layout, a `CameraInfo`'s intrinsics, an `Odometry`'s pose, a
//! `TFMessage`'s transforms. Every read is length-checked and returns `None` on a short or malformed
//! buffer, so a corrupt message is skipped, never a panic (Veridex's job is to survive bad data).
//!
//! Encoding assumptions (ROS 2 default, `rmw_fastrtps` / XCDR1): a 4-byte encapsulation header whose
//! second byte selects little- vs big-endian; primitives aligned to their own size relative to the
//! start of the body (just past the header); strings are a `u32` byte length (including the trailing
//! NUL) followed by the bytes; sequences are a `u32` element count followed by the elements. Only the
//! little-endian representation (what ROS 2 emits by default) is decoded; a big-endian body is
//! declined (the caller simply gets no decoded metadata, exactly as if the field were absent).

use crate::cdm::{
    CameraIntrinsics, HeaderStamps, ImageDims, PointCounts, PointField, Pose, Transform,
};

/// Ceiling on any name this reader will return (coordinate frames, point-field names, distortion
/// models). ROS names are identifiers — tens of bytes — so this is generous by three orders of
/// magnitude while still bounding what an untrusted message can make the CDM retain.
const MAX_NAME_BYTES: usize = 4096;

/// A cursor over a CDR message body (the bytes *after* the 4-byte encapsulation header). `pos` is the
/// offset from the body start, which is the origin all alignment is measured against.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Open a reader over a full CDR message, validating the encapsulation header and requiring the
    /// little-endian representation. Returns `None` for a truncated header or a big-endian body.
    fn new(data: &'a [u8]) -> Option<Reader<'a>> {
        if data.len() < 4 {
            return None;
        }
        // Representation identifier byte 1: 0x01 = CDR_LE, 0x03 = PL_CDR_LE (both little-endian).
        // 0x00 / 0x02 are the big-endian variants, which we decline.
        match data[1] {
            0x01 | 0x03 => Some(Reader {
                buf: &data[4..],
                pos: 0,
            }),
            _ => None,
        }
    }

    /// Advance `pos` to the next multiple of `n` (the size of the primitive about to be read).
    fn align(&mut self, n: usize) {
        self.pos = (self.pos + n - 1) & !(n - 1);
    }

    /// Bytes left in the body past the cursor.
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.align(4);
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    fn f32(&mut self) -> Option<f32> {
        self.align(4);
        let b = self.take(4)?;
        Some(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn f64(&mut self) -> Option<f64> {
        self.align(8);
        let b = self.take(8)?;
        Some(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// A CDR string: `u32` length (including the trailing NUL) + bytes. The NUL is stripped.
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        // Drop the trailing NUL terminator if present.
        let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
        let bytes = &bytes[..end];
        // Every string this reader returns is a *name* — a coordinate frame, a point field, a
        // distortion model — and every one is retained in the CDM. Two reasons for the cap. The slice
        // is bounded by the message body, but invalid UTF-8 expands 3x on the way out (each bad byte
        // becomes a 3-byte U+FFFD), and the ingest budget charges the raw body, not the decoded
        // string: 63 channels each carrying 1 MiB of 0xFF measured 198 MB retained from a 19.8 KB
        // file, right past a budget meant to cap exactly that. And a name this long is not a name.
        if bytes.len() > MAX_NAME_BYTES {
            return None;
        }
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// The `builtin_interfaces/Time` at the front of a `std_msgs/Header`, as `(sec, nanosec)`.
    fn stamp(&mut self) -> Option<(i32, u32)> {
        Some((self.i32()?, self.u32()?))
    }

    /// Skip a `std_msgs/Header`: `{ int32 sec, uint32 nanosec }` then a `string frame_id`. Returns the
    /// `frame_id` (some messages, e.g. a `TransformStamped`, use it as the parent frame).
    fn header(&mut self) -> Option<String> {
        self.stamp()?;
        self.string()
    }
}

/// Recover the `header.stamp` of any message that begins with a `std_msgs/Header` — the time the
/// **sensor** says its data was sampled, as distinct from the time the **recorder** wrote it to the
/// bag, which is the only clock a bag's frame timestamps carry.
///
/// Returned in nanoseconds since the epoch. Zero is not an error: it is what a driver that never
/// stamped its messages publishes, and telling that apart from a stamp that was set is the point.
///
/// Declines a body that is not header-shaped. Eight bytes read as a time are plausible in almost any
/// payload, so two of the message's own invariants have to hold before the pair is believed: a
/// normalized `builtin_interfaces/Time` keeps `nanosec` under a full second and no recording carries
/// a `sec` before 1970, and the `frame_id` string behind the stamp has to decode. A fabricated clock
/// reading would be a finding about honest data, which is worse than reading no clock at all.
pub fn decode_header_stamp(data: &[u8]) -> Option<i64> {
    let mut r = Reader::new(data)?;
    let (sec, nanosec) = r.stamp()?;
    if sec < 0 || nanosec >= 1_000_000_000 {
        return None;
    }
    r.string()?;
    Some(i64::from(sec) * 1_000_000_000 + i64::from(nanosec))
}

/// Accumulates a stream's `header.stamp` readings against the log times they were recorded at.
///
/// Kept as a running summary rather than a `Vec`, for the same reason [`PointCountAccum`] is: the
/// number of messages on a topic is chosen by the file.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeaderStampAccum {
    messages: u64,
    stamped: u64,
    unset: u64,
    min_offset: i64,
    max_offset: i64,
    regressions: u64,
    last: Option<i64>,
}

impl HeaderStampAccum {
    /// Fold in one message: the log time the recorder wrote it at, and the stamp it carried.
    pub fn observe(&mut self, log_ts: i64, stamp_ns: i64) {
        self.messages += 1;
        if stamp_ns == 0 {
            self.unset += 1;
            return;
        }
        // Saturating: both sides come out of the file, and a stamp far in the past against a log time
        // far in the future is exactly the corrupt case this summary exists to report.
        let offset = log_ts.saturating_sub(stamp_ns);
        if self.stamped == 0 {
            self.min_offset = offset;
            self.max_offset = offset;
        } else {
            self.min_offset = self.min_offset.min(offset);
            self.max_offset = self.max_offset.max(offset);
        }
        if self.last.is_some_and(|prev| stamp_ns < prev) {
            self.regressions += 1;
        }
        self.last = Some(stamp_ns);
        self.stamped += 1;
    }

    /// The summary, or `None` when no message's stamp was read — a stream whose stamps were never
    /// decoded and a stream whose stamps were all zero are opposite verdicts.
    pub fn finish(self) -> Option<HeaderStamps> {
        (self.messages > 0).then_some(HeaderStamps {
            message_count: self.messages,
            unset: self.unset,
            min_offset_ns: self.min_offset,
            max_offset_ns: self.max_offset,
            regressions: self.regressions,
        })
    }
}

/// The `sensor_msgs/msg/PointField` datatype enum → a CDM dtype string.
/// The byte width of a `PointField` datatype tag, or `None` for a tag the spec does not define.
///
/// Used to check that a cloud's point record layout fits the stride it declares. A tag outside the
/// eight the message defines is not a width to guess at.
fn point_datatype_width(tag: u8) -> Option<u64> {
    match tag {
        1..=2 => Some(1),
        3..=4 => Some(2),
        5..=7 => Some(4),
        8 => Some(8),
        _ => None,
    }
}

fn point_datatype(tag: u8) -> &'static str {
    match tag {
        1 => "int8",
        2 => "uint8",
        3 => "int16",
        4 => "uint16",
        5 => "int32",
        6 => "uint32",
        7 => "float32",
        8 => "float64",
        _ => "unknown",
    }
}

/// Recover the `header.frame_id` of any message that begins with a `std_msgs/Header` — the
/// coordinate frame the sensor's data is expressed in, and the name that has to appear in the TF
/// tree for the sensor to be relatable to any other.
///
/// Returns `None` for a message that is not header-first, is truncated, or names an empty frame:
/// an empty `frame_id` is what an unconfigured driver publishes, and recording it as a frame would
/// turn "this sensor declares no frame" into "this sensor declares the frame `""`".
pub fn decode_header_frame_id(data: &[u8]) -> Option<String> {
    let mut r = Reader::new(data)?;
    let frame_id = r.header()?;
    (!frame_id.is_empty()).then_some(frame_id)
}

/// Decode a `sensor_msgs/msg/PointCloud2` body far enough to recover its per-point field layout
/// (`fields`): `Header`, `uint32 height`, `uint32 width`, then a sequence of `PointField`
/// `{ string name, uint32 offset, uint8 datatype, uint32 count }`. The bulk `data` blob is never read.
pub fn decode_point_cloud2_fields(data: &[u8]) -> Option<Vec<PointField>> {
    let mut r = Reader::new(data)?;
    r.header()?; // header (stamp + frame_id)
    let _height = r.u32()?;
    let _width = r.u32()?;
    let count = r.u32()? as usize;
    // Guard against a corrupt length claiming more fields than the buffer could hold — bounded by the
    // smallest a `PointField` can encode (name length + offset + datatype + count), not by bytes.
    const MIN_POINT_FIELD_BYTES: usize = 13;
    if count > data.len() / MIN_POINT_FIELD_BYTES {
        return None;
    }
    let mut fields = Vec::with_capacity(count);
    for _ in 0..count {
        let name = r.string()?;
        let _offset = r.u32()?;
        let datatype = r.u8()?;
        let _elem_count = r.u32()?;
        fields.push(PointField {
            name,
            dtype: Some(point_datatype(datatype).to_string()),
        });
    }
    Some(fields)
}

/// Accumulates what a receiver said about its own fix into a [`crate::cdm::FixAvailability`].
///
/// Only `NavSatFix` bodies that decoded reach this: a body that could not be parsed is not the
/// receiver saying anything, and counting it as a no-fix would report a decode failure as an
/// outage.
#[derive(Debug, Default, Clone, Copy)]
pub struct FixAvailabilityAccum {
    messages: u64,
    unfixed: u64,
}

impl FixAvailabilityAccum {
    /// Fold in one decoded `NavSatFix`.
    pub fn observe(&mut self, sample: &NavSatSample) {
        self.messages += 1;
        if matches!(sample, NavSatSample::NoFix) {
            self.unfixed += 1;
        }
    }

    /// The summary, or `None` for a stream that carried no `NavSatFix` at all — which is every
    /// stream but a GNSS one, and is not the same fact as a receiver with no fix.
    pub fn finish(self) -> Option<crate::cdm::FixAvailability> {
        (self.messages > 0).then_some(crate::cdm::FixAvailability {
            message_count: self.messages,
            unfixed: self.unfixed,
        })
    }
}

/// Accumulates a camera stream's frame dimensions, as a running summary rather than a `Vec` for the
/// same reason [`PointCountAccum`] is one: the number of messages on a topic is chosen by the file.
///
/// `min`/`max` are taken over the frames that carried pixels. An empty frame has no resolution to
/// compare, and folding its zeros into the range would report every dead camera as one that also
/// changed resolution.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImageDimAccum {
    messages: u64,
    empty: u64,
    sized: u64,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

impl ImageDimAccum {
    /// Fold in one frame's dimensions.
    pub fn observe(&mut self, width: u32, height: u32) {
        self.messages += 1;
        if width == 0 || height == 0 {
            self.empty += 1;
            return;
        }
        if self.sized == 0 {
            self.min_width = width;
            self.max_width = width;
            self.min_height = height;
            self.max_height = height;
        } else {
            self.min_width = self.min_width.min(width);
            self.max_width = self.max_width.max(width);
            self.min_height = self.min_height.min(height);
            self.max_height = self.max_height.max(height);
        }
        self.sized += 1;
    }

    /// The summary, or `None` when no frame's dimensions were read — "nothing was measured" and "a
    /// stream of empty frames" are opposite verdicts and must not render the same.
    pub fn finish(self) -> Option<ImageDims> {
        (self.messages > 0).then_some(ImageDims {
            message_count: self.messages,
            empty: self.empty,
            min_width: self.min_width,
            max_width: self.max_width,
            min_height: self.min_height,
            max_height: self.max_height,
        })
    }
}

/// Accumulates the point counts of a stream's `PointCloud2` messages into a [`PointCounts`].
///
/// Kept as a running summary rather than a `Vec` of counts: the number of messages on a topic is
/// chosen by the file, so holding one entry per message is a memory cost a bag controls.
#[derive(Debug, Default, Clone, Copy)]
pub struct PointCountAccum {
    messages: u64,
    min: u64,
    max: u64,
    empty: u64,
}

impl PointCountAccum {
    /// Fold in one message's point count.
    ///
    /// Only counts that were *read* reach here: how many bodies failed to decode is a property of
    /// the stream rather than of the density summary, and is recorded once for all decoders by
    /// [`BodyDecodeAccum`].
    pub fn observe(&mut self, points: u64) {
        if self.messages == 0 {
            self.min = points;
        } else {
            self.min = self.min.min(points);
        }
        self.messages += 1;
        self.max = self.max.max(points);
        if points == 0 {
            self.empty += 1;
        }
    }

    /// The summary, or `None` when no message's count was read — an empty summary and a stream of
    /// empty clouds are opposite verdicts, so "nothing was measured" must not render as a count.
    pub fn finish(self) -> Option<PointCounts> {
        (self.messages > 0).then_some(PointCounts {
            message_count: self.messages,
            min: self.min,
            max: self.max,
            empty: self.empty,
        })
    }
}

/// Counts how many of a stream's message bodies the reader could decode, for
/// [`crate::cdm::BodyDecodes`].
///
/// One accumulator for every typed decoder on the stream, rather than one per decoder, because the
/// fact it records is a property of the *recording* — bytes that did not survive it — and not of any
/// one message type. It also means a decoder added later is covered by the check that reads this
/// without any further work, which is the failure mode this exists to close: each strict decoder
/// returns `Option` so a corrupt body cannot yield a fabricated reading, and every call site then
/// wrote `if let Some(v) = decode(..)` and threw the failures away.
#[derive(Debug, Default, Clone, Copy)]
pub struct BodyDecodeAccum {
    attempted: u64,
    failed: u64,
}

impl BodyDecodeAccum {
    /// Fold in one message: `Some(true)` if its body decoded, `Some(false)` if it did not, and
    /// `None` for a schema this reader has no typed decoder for.
    ///
    /// The three-way answer is deliberate. A stream the reader only fingerprints has nothing that
    /// *could* have failed, and recording it as "zero failures" would report a question that was
    /// never asked as an answer of "all well" — the distinction this tool exists to make.
    pub fn observe(&mut self, decoded: Option<bool>) {
        let Some(decoded) = decoded else { return };
        self.attempted += 1;
        if !decoded {
            self.failed += 1;
        }
    }

    /// The summary, or `None` for a stream no typed decoder was ever run against.
    pub fn finish(self) -> Option<crate::cdm::BodyDecodes> {
        (self.attempted > 0).then_some(crate::cdm::BodyDecodes {
            attempted: self.attempted,
            failed: self.failed,
        })
    }
}

/// The most `PointField` entries a body may declare before the point-count decode declines it.
///
/// A real `PointCloud2` declares a handful — `x`, `y`, `z`, `intensity`, `ring`, `time`. The count
/// is a `uint32` out of the file, and the decode below walks the list to reach the length invariants
/// that prove the body is a cloud at all, so an uncapped count is a per-message cost the file
/// chooses. 64 is far above any real layout and far below anything expensive.
const MAX_POINT_FIELDS: usize = 64;

/// The most returns a `sensor_msgs/msg/LaserScan` may declare, as a bound on what one message can
/// make this reader allocate and walk.
///
/// A planar scanner publishes hundreds to a few thousand returns a sweep; the densest catalogue
/// models are under 10,000. This sits far above that and far below a length a corrupt header could
/// use to spend the run.
const MAX_LASER_RETURNS: usize = 1 << 20;

/// Decode a `sensor_msgs/msg/LaserScan` body far enough to count the returns that measured
/// something.
///
/// Layout: `Header`, `float32 angle_min`, `angle_max`, `angle_increment`, `time_increment`,
/// `scan_time`, `range_min`, `range_max`, then `float32[] ranges` and `float32[] intensities`.
///
/// The count is of returns that fall **inside the scanner's own declared `[range_min, range_max]`**,
/// which is how REP 117 says a driver reports "nothing there": a return outside that window, or an
/// infinity, or a NaN, is a direction the beam came back from with no measurement. So a planar
/// scanner whose driver lost its sensor — publishing a full, well-formed, correctly-timed sweep of
/// infinities — counts zero, and `autonomy.point-cloud-density` reports it exactly as it reports a
/// dead 3-D LiDAR. Nothing else can: the messages have the schema, the rate and the coordinate frame
/// of a working scanner.
///
/// Declined rather than guessed at where the body does not prove it is a scan: a sweep with no
/// returns at all, an `angle_increment` of zero (no sweep to speak of), a `[range_min, range_max]`
/// window that is not a window, and an `intensities` array that is neither absent nor one-per-return
/// — the message definition allows only those two. Those are the invariants an arbitrary buffer does
/// not satisfy, and a fabricated return count would be a finding about honest data.
pub fn decode_laser_scan_returns(data: &[u8]) -> Option<u64> {
    let mut r = Reader::new(data)?;
    r.header()?;
    r.f32()?; // angle_min
    r.f32()?; // angle_max
    let angle_increment = r.f32()?;
    r.f32()?; // time_increment
    r.f32()?; // scan_time
    let range_min = r.f32()?;
    let range_max = r.f32()?;
    if !angle_increment.is_finite() || angle_increment == 0.0 {
        return None;
    }
    if !(range_min.is_finite() && range_max.is_finite() && range_min < range_max) {
        return None;
    }
    let count = r.u32()? as usize;
    if count == 0 || count > MAX_LASER_RETURNS {
        return None;
    }
    let mut measured = 0u64;
    for _ in 0..count {
        let range = r.f32()?;
        if range.is_finite() && range >= range_min && range <= range_max {
            measured += 1;
        }
    }
    // `intensities` is either empty or parallel to `ranges`; anything else is not this message.
    let intensities = r.u32()? as usize;
    if intensities != 0 && intensities != count {
        return None;
    }
    r.take(intensities.checked_mul(4)?)?;
    Some(measured)
}

/// The most marker segments this reader will walk looking for a JPEG's frame header.
///
/// A JPEG's `SOFn` sits within a handful of segments of the start; a file that has not reached one
/// after this many is not one this reader will keep scanning, because the segment lengths come out
/// of the file and a crafted chain is otherwise a loop the run pays for.
const MAX_JPEG_SEGMENTS: usize = 64;

/// The dimensions a JPEG declares in its frame header (`SOFn`), or `None` for a buffer that is not
/// one.
///
/// Walks the marker chain from `SOI`, skipping each segment by the length it declares, until it
/// reaches a start-of-frame marker — `SOF0`/`SOF1`/`SOF2` (baseline, extended, progressive) and the
/// lossless and arithmetic-coded variants beside them, all of which carry
/// `[precision u8][height u16][width u16]` in the same place. `DHT`, `DQT`, `APPn` and the rest are
/// skipped; the entropy-coded scan is never reached, so no pixel is decoded.
fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2usize;
    for _ in 0..MAX_JPEG_SEGMENTS {
        // Markers may be preceded by any number of `0xFF` fill bytes.
        while data.get(at) == Some(&0xFF) && data.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        if *data.get(at)? != 0xFF {
            return None;
        }
        let marker = *data.get(at + 1)?;
        at += 2;
        match marker {
            // Standalone markers: no length, nothing to skip.
            0x01 | 0xD0..=0xD7 => continue,
            // Start of scan, and end of image: the frame header should have come first.
            0xDA | 0xD9 => return None,
            _ => {}
        }
        let length = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]) as usize;
        // A segment's length includes its own two bytes, so anything under two is not a length.
        if length < 2 {
            return None;
        }
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC); // DHT, JPG and DAC are not frame headers
        if is_sof {
            let height = u16::from_be_bytes([*data.get(at + 3)?, *data.get(at + 4)?]);
            let width = u16::from_be_bytes([*data.get(at + 5)?, *data.get(at + 6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        at = at.checked_add(length)?;
    }
    None
}

/// The dimensions a PNG declares in its `IHDR`, or `None` for a buffer that is not one.
///
/// `IHDR` is the first chunk by the spec, at a fixed offset behind the 8-byte signature, so there is
/// no chain to walk and no pixel to decode.
fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" || data.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
    };
    Some((be(16)?, be(20)?))
}

/// Decode a `sensor_msgs/msg/CompressedImage` body far enough to recover the frame's dimensions.
///
/// Layout: `Header`, `string format`, then the `uint8[] data` blob — the compressed bytes, of which
/// only the codec's own frame header is read.
///
/// Most real bags record cameras compressed; without this a `/camera/image_raw/compressed` topic was
/// unmeasured while the raw topic beside it was graded, so the same dead camera was caught on one
/// spelling of the topic and not the other.
///
/// Three answers, and the difference between the last two matters:
/// - `None` — the body is not a `CompressedImage`, or it names a codec this reader *does* read and
///   its header cannot be read all the same: a truncated write or a dropped chunk, which is a body
///   that broke.
/// - `Some(None)` — it is one, in a codec this reader does not read. Nothing was measured and nothing
///   failed; the caller reports it as a schema with no decoder rather than as a body that broke.
/// - `Some(Some((w, h)))` — the frame's size, `(0, 0)` for a frame carrying no compressed bytes at
///   all, which is a driver that lost its sensor and needs no codec to recognize.
pub fn decode_compressed_image_dimensions(data: &[u8]) -> Option<Option<(u32, u32)>> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let format = r.string()?;
    // The one field an all-zero body cannot satisfy: a driver publishing empty frames still names
    // the codec it would have published in. ROS spells this either bare (`jpeg`) or as the full
    // `rgb8; jpeg compressed bgr8`, so the codec is looked for *in* the string.
    if format.is_empty() {
        return None;
    }
    let payload = r.u32()? as usize;
    let bytes = r.take(payload)?;
    // Nothing compressed is nothing recorded, whatever the codec.
    if bytes.is_empty() {
        return Some(Some((0, 0)));
    }
    let format = format.to_ascii_lowercase();
    if format.contains("jpeg") || format.contains("jpg") {
        // A frame whose codec is named and whose header cannot be read is a body that broke — a
        // truncated write, a dropped chunk — not a codec this reader declined. `None` says so.
        jpeg_dimensions(bytes).map(Some)
    } else if format.contains("png") {
        png_dimensions(bytes).map(Some)
    } else {
        // A codec this reader has no header parser for. Not a failure: nothing was tried.
        Some(None)
    }
}

/// The most bytes per pixel any `sensor_msgs/msg/Image` encoding uses, as a sanity bound on `step`.
///
/// The widest ROS encodings are 4-channel 32-bit float (`32FC4`), which is 16. Doubling that leaves
/// room for an encoding this table does not know while still refusing a `step` that could only come
/// from a body that is not an image.
const MAX_IMAGE_BYTES_PER_PIXEL: u64 = 32;

/// Decode a `sensor_msgs/msg/Image` body far enough to recover the frame's **dimensions**.
///
/// Layout: `Header`, `uint32 height`, `uint32 width`, `string encoding`, `uint8 is_bigendian`,
/// `uint32 step`, then the `uint8[] data` blob — which is never read, only proved to be present at
/// the length the message states.
///
/// Returns `(width, height)`, and `(0, 0)` is a real answer rather than a failure: a camera driver
/// that lost its sensor keeps publishing well-formed zero-sized frames at its configured rate, and
/// that is exactly what this exists to let a check see.
///
/// What separates that from an arbitrary buffer is the message's own length invariants, because
/// every one of them holds trivially at zero. A non-empty `encoding` is the anchor — a body of
/// zeroes yields the empty string and is declined — and beyond it a row is at least one byte per
/// pixel and no more than [`MAX_IMAGE_BYTES_PER_PIXEL`], `data` is exactly `step × height` bytes,
/// and those bytes are there. A fabricated resolution is worse than silence: it is a finding about
/// honest data.
pub fn decode_image_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let height = r.u32()?;
    let width = r.u32()?;
    let encoding = r.string()?;
    // The one field an all-zero body cannot satisfy. A driver publishing empty frames still names
    // the encoding it would have published in.
    if encoding.is_empty() {
        return None;
    }
    let _is_bigendian = r.u8()?;
    let step = r.u32()? as u64;
    let data_len = r.u32()? as u64;
    // A row covers its own pixels: at least one byte each, and no more than the widest encoding a
    // ROS image uses. An empty frame has no row to size, so the bound applies only where there is
    // one.
    if width > 0 && (step < u64::from(width) || step > u64::from(width) * MAX_IMAGE_BYTES_PER_PIXEL)
    {
        return None;
    }
    // `data` is exactly `step × height` bytes, per the message definition — the invariant an
    // arbitrary body will not satisfy by accident — and the bytes are actually present, so a stub
    // body claiming a full frame is declined even when its numbers agree with each other.
    if data_len != step.saturating_mul(u64::from(height)) {
        return None;
    }
    r.take(usize::try_from(data_len).ok()?)?;
    Some((width, height))
}

/// Decode a `sensor_msgs/msg/PointCloud2` body far enough to recover its **point count** — `height ×
/// width` — or `None` when the body is not a `PointCloud2` at all.
///
/// The count itself is the first two `uint32`s after the header, but reading only those believes
/// whatever bytes happen to sit there. A channel's declared schema is not proof of its bodies: a
/// mislabelled topic, a truncated write, or a recorder that stubbed the payload all present as a
/// `PointCloud2` channel, and a fabricated count would be reported as a real one — a finding about
/// honest data, which is worse than the silence it replaces. So the decode continues to the fields
/// and the three length values behind them, and returns a count only when the message's own
/// invariants hold: `row_step` covers a row of `width` points, `data` is `row_step × height` bytes,
/// and the buffer actually holds them. An empty cloud satisfies all three with zeroes, which is the
/// case this exists to find.
///
/// Run per message, unlike [`decode_point_cloud2_fields`]: the layout is a property of the stream
/// and the first message settles it, while whether a sweep held any points is a property of each
/// message. Nothing here reads the point payload — `data`'s length is its `uint32` prefix, and the
/// bytes are only bounds-checked.
pub fn decode_point_cloud2_point_count(data: &[u8]) -> Option<u64> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let height = r.u32()? as u64;
    let width = r.u32()? as u64;
    let field_count = r.u32()? as usize;
    // A cloud that declares no per-point fields is not describing points, so there is nothing for a
    // count to be a count *of*. Together with the `point_step` rule below this is what an all-zero
    // body fails: every length invariant holds trivially at zero, and without these two a buffer of
    // zeroes reads as a well-formed empty cloud and is reported as a dead sensor.
    if field_count == 0 || field_count > MAX_POINT_FIELDS {
        return None;
    }
    // The record layout, kept so it can be checked against the stride below. A field's extent is
    // `offset + width × count`, and a layout whose fields run past the stride or overlap each other
    // describes a record no consumer can read the same way twice — a hand-rolled publisher that got
    // an offset wrong, which produces garbage for one field and correct values for the rest.
    let mut extents: Vec<(u64, u64)> = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        r.string()?; // name
        let offset = r.u32()? as u64;
        let width = point_datatype_width(r.u8()?)?;
        let count = r.u32()? as u64;
        if count == 0 {
            return None; // a field holding no elements occupies nothing and means nothing
        }
        extents.push((offset, offset.saturating_add(width.saturating_mul(count))));
    }
    let _is_bigendian = r.u8()?;
    let point_step = r.u32()? as u64;
    let row_step = r.u32()? as u64;
    let data_len = r.u32()? as u64;
    // A point occupies bytes, in an empty cloud as much as a full one — a driver that publishes no
    // returns still declares the stride of the point it would have published.
    if point_step == 0 {
        return None;
    }
    // Every field lies inside the point, and no two fields share a byte. Padding between them is
    // normal (alignment); overlap is not, and neither is a field running past the stride.
    extents.sort_unstable();
    let mut prev_end = 0u64;
    for (start, end) in &extents {
        if *start < prev_end || *end > point_step {
            return None;
        }
        prev_end = *end;
    }
    // A row holds `width` points, so it is at least `point_step × width` bytes — the message may pad
    // beyond that but cannot fall short of it.
    if row_step < point_step.saturating_mul(width) {
        return None;
    }
    // `data` is exactly `row_step × height` bytes, per the message definition. This is the one
    // invariant an arbitrary body will not satisfy by accident.
    if data_len != row_step.saturating_mul(height) {
        return None;
    }
    // ...and the bytes are actually there. A stub body claiming a full cloud fails here even if its
    // numbers are self-consistent.
    r.take(usize::try_from(data_len).ok()?)?;
    Some(height.saturating_mul(width))
}

/// Decode a `sensor_msgs/msg/CameraInfo` body far enough to recover intrinsics for `stream`: `Header`,
/// `uint32 height`, `uint32 width`, `string distortion_model`, `float64[] d` (sequence), then the
/// row-major 3×3 intrinsic matrix `float64[9] k` (`fx=k0, fy=k4, cx=k2, cy=k5`). The image
/// distortion model name and the image dimensions are carried through onto the intrinsics — the message states them alongside the
/// matrix, and they are what makes `cx`/`cy` checkable as the pixel coordinates they are. A zero is
/// the field's unset value and becomes `None`. `valid_from`/`_to` are left open — the caller stamps
/// the validity range from the message time if it wishes.
pub fn decode_camera_info(data: &[u8], stream: &str) -> Option<CameraIntrinsics> {
    let mut r = Reader::new(data)?;
    r.header()?;
    // Recorded, not discarded: `cx`/`cy` are pixel coordinates, and these are the only thing that
    // says which image they are coordinates *in*. A driver that has not been configured publishes
    // 0, which is the field's unset value rather than a one-pixel-wide camera, so it maps to `None`
    // and the checks that read the dimensions abstain instead of inventing an image.
    let height = r.u32()?;
    let width = r.u32()?;
    // Kept: the coefficients below are recorded verbatim and never interpreted, and this is the
    // only thing that says how many of them there should be. An empty string is a source that named
    // no model, not a model named "".
    let distortion_model = r.string()?;
    let d_len = r.u32()? as usize;
    // Each distortion coefficient is an 8-byte f64; a count beyond that can't be honored.
    if d_len > data.len() / 8 {
        return None;
    }
    let mut distortion = Vec::with_capacity(d_len);
    for _ in 0..d_len {
        distortion.push(r.f64()?);
    }
    // k is a fixed-size array of 9 doubles (no length prefix).
    let mut k = [0.0f64; 9];
    for slot in &mut k {
        *slot = r.f64()?;
    }
    Some(CameraIntrinsics {
        stream: stream.to_string(),
        fx: k[0],
        fy: k[4],
        cx: k[2],
        cy: k[5],
        distortion,
        distortion_model: (!distortion_model.is_empty()).then_some(distortion_model),
        width: (width > 0).then_some(width as u64),
        height: (height > 0).then_some(height as u64),
        valid_from: None,
        valid_to: None,
    })
}

/// Decode a `geometry_msgs` `Pose` (`{ Point position {f64 x,y,z}, Quaternion orientation
/// {f64 x,y,z,w} }`) from the reader at its current position.
fn read_pose(r: &mut Reader) -> Option<Pose> {
    let x = r.f64()?;
    let y = r.f64()?;
    let z = r.f64()?;
    let qx = r.f64()?;
    let qy = r.f64()?;
    let qz = r.f64()?;
    let qw = r.f64()?;
    Some(Pose {
        translation: [x, y, z],
        rotation: [qx, qy, qz, qw],
    })
}

/// Decode a `nav_msgs/msg/Odometry` body far enough to recover the ego pose: `Header`,
/// `string child_frame_id`, then `pose.pose` (a `Pose`). The covariance and twist are ignored.
/// Decode a `nav_msgs/msg/Odometry` body into its pose and the coordinate frame it tracks.
///
/// `child_frame_id` is the vehicle body (`base_link`, `base_footprint`) — the frame the pose *is
/// of*, as distinct from the header's `frame_id`, which is the reference frame the pose is
/// expressed *in* (`odom`, `map`). Both matter and they answer different questions: the reference
/// frame is joined to the body dynamically, while the body frame is what every sensor's extrinsics
/// hang off, so it has to appear in the static transform tree. It was read and discarded, which
/// left nothing able to ask whether the trajectory and the sensors describe the same vehicle.
///
/// An empty `child_frame_id` is what an unconfigured publisher emits, and becomes `None` rather than
/// a frame named `""` — the same rule [`decode_header_frame_id`] follows.
pub fn decode_odometry(data: &[u8]) -> Option<(Pose, Option<String>)> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let child_frame_id = r.string()?;
    let pose = read_pose(&mut r)?;
    Some((pose, (!child_frame_id.is_empty()).then_some(child_frame_id)))
}

/// Decode a `sensor_msgs/msg/JointState` body far enough to recover its joint `name`s and its
/// `position` array: `Header`, `string[] name`, then `float64[] position` (the `velocity` and
/// `effort` arrays that follow are not read). The names are what let a finding say which joint
/// saturated instead of which index.
///
/// This is the one ROS message whose *whole* payload is the measurement — a handful of joint angles,
/// not a bulk blob — so reading it is not a departure from the rule this module states above. It is
/// also the actuator signal on a robot arm recorded to a bag, and without it every statistical check
/// abstains on the stream that would show a joint pinned at its limit.
///
/// Returns `None` for a message that is truncated, big-endian, or publishes no positions at all (a
/// `JointState` may carry effort alone); an empty result would otherwise read as "measured, and
/// there was nothing there".
pub fn decode_joint_state(data: &[u8]) -> Option<(Vec<String>, Vec<f64>)> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let name_count = r.u32()? as usize;
    // A declared count is attacker-controlled. The smallest a CDR string can encode is its 4-byte
    // length prefix, and the smallest an f64 can is 8 bytes, so bound each sequence by what the body
    // could actually hold rather than trusting the count.
    if name_count > data.len() / 4 {
        return None;
    }
    let mut names = Vec::with_capacity(name_count);
    for _ in 0..name_count {
        names.push(r.string()?);
    }
    let count = r.u32()? as usize;
    if count > data.len() / 8 {
        return None;
    }
    let mut positions = Vec::with_capacity(count);
    for _ in 0..count {
        positions.push(r.f64()?);
    }
    (!positions.is_empty()).then_some((names, positions))
}

/// The `(parent, child)` edges a recording republished with a **different** pose, and so the frames
/// that moved during it.
///
/// `Transform` is time-scoped by design — a rig is recalibrated and coordinate frames move within a
/// log — but a bag's `/tf` topic hands each edge over as an open-ended transform per message, with
/// nothing to bound one sample from the next. So the readers keep the first pose seen for each edge
/// and drop the rest, which is right for the `/tf_static` an unmoving rig publishes and wrong for a
/// pan-tilt head, an articulated trailer or an arm: every spatial result is then judged against the
/// rig's geometry at the start of the log, and nothing says so.
///
/// This is what says so. It records which edges moved, so the run can disclose the geometry it did
/// not read rather than presenting one instant as the whole recording.
#[derive(Debug, Default, Clone)]
pub struct MovingFrames {
    edges: std::collections::BTreeSet<String>,
}

impl MovingFrames {
    /// Whether any edge was republished with a different pose.
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// The disclosure, naming the first few edges that moved.
    pub fn note(&self) -> String {
        let shown = self
            .edges
            .iter()
            .take(4)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let listed = match self.edges.len().saturating_sub(4) {
            0 => shown,
            rest => format!("{shown} and {rest} more"),
        };
        format!(
            "{} transform edge(s) were republished with a different pose during the recording ({listed}); only the first pose of each was read, so every result that places a sensor — the frame resolution, the calibration completeness, anything projected between sensors — is judged against the rig's geometry at the start of the log rather than at the time of each frame",
            self.edges.len()
        )
    }
}

/// Fold one transform into a bag-wide `(parent, child)` map, noting the edge when the recording has
/// already given that edge a **different** pose.
///
/// The first pose wins, which is what a `/tf_static` republished unchanged means. A later pose that
/// differs is the frame moving, and is recorded in `moved` rather than silently dropped — the value
/// of the disclosure is that a moving rig read as a still one is invisible in every other result.
pub fn insert_transform(
    transforms: &mut std::collections::BTreeMap<(String, String), Transform>,
    moved: &mut MovingFrames,
    t: Transform,
) {
    let key = (t.parent_frame.clone(), t.child_frame.clone());
    match transforms.get(&key) {
        // Republished unchanged: what an unmoving rig's `/tf_static` does every message.
        Some(existing) if existing.pose == t.pose => {}
        Some(_) => {
            moved.edges.insert(format!("{} -> {}", key.0, key.1));
        }
        None => {
            transforms.insert(key, t);
        }
    }
}

/// The name of each scalar [`decode_twist_values`] returns, in the same order.
pub const TWIST_DIM_NAMES: [&str; 6] = [
    "linear.x",
    "linear.y",
    "linear.z",
    "angular.x",
    "angular.y",
    "angular.z",
];

/// Decode a `geometry_msgs/msg/Twist` (or `TwistStamped`) body into its six velocity components.
///
/// Layout: `Vector3 linear { float64 x, y, z }`, `Vector3 angular { float64 x, y, z }` — six doubles
/// and nothing else. `TwistStamped` is the same, behind a `std_msgs/Header`; pass `stamped`.
///
/// This is a mobile robot's **action** channel: `/cmd_vel` is to a base what `/joint_states` is to
/// an arm, and it went unread. A recording whose commanded velocity is pinned at its rail for the
/// whole run — the one fault the statistical family exists to catch on an actuator — carried no
/// values to grade, so the stream reported `STATISTICAL.UNMEASURED_VALUES` and the run scored clean.
///
/// A `Twist` has no invariants of its own to prove it is one: six doubles are six doubles, and any
/// value they hold is legal (a NaN in a velocity command is a real fault, not a parse failure, and
/// `STATISTICAL.NON_FINITE_OBSERVED` is what reports it). Its **length** is the invariant instead —
/// the message is exactly six doubles, so a body carrying more than its own padding past them is not
/// a `Twist`, and reading one would summarize whatever else it is as a velocity.
pub fn decode_twist_values(data: &[u8], stamped: bool) -> Option<Vec<Option<f64>>> {
    six_doubles(data, stamped)
}

/// The name of each scalar [`decode_wrench_values`] returns, in the same order.
pub const WRENCH_DIM_NAMES: [&str; 6] = [
    "force.x", "force.y", "force.z", "torque.x", "torque.y", "torque.z",
];

/// Decode a `geometry_msgs/msg/Wrench` (or `WrenchStamped`) body into its six force/torque
/// components.
///
/// Layout: `Vector3 force { float64 x, y, z }`, `Vector3 torque { float64 x, y, z }` — the same
/// shape as a [`decode_twist_values`], and read by the same rule.
///
/// A force/torque sensor is a manipulation recording's contact channel, and it went unread: a sensor
/// clipped at its rail through a whole run of contact-rich episodes — the exact fault
/// `STATISTICAL.SATURATED` exists for — carried no values to grade.
pub fn decode_wrench_values(data: &[u8], stamped: bool) -> Option<Vec<Option<f64>>> {
    six_doubles(data, stamped)
}

/// Six doubles behind an optional `std_msgs/Header`, and nothing else in the body.
///
/// Shared by `Twist` and `Wrench`, which have the same shape. Neither has an invariant of its own to
/// prove a body is one — six doubles are six doubles, and every value they can hold is legal (a NaN
/// in a velocity command or a force reading is a fault to report, not a parse failure). The
/// message's **length** is the invariant instead: a body carrying more than its own padding past
/// those six is not one of these, and reading it would summarize whatever else it is as a
/// measurement.
fn six_doubles(data: &[u8], stamped: bool) -> Option<Vec<Option<f64>>> {
    let mut r = Reader::new(data)?;
    if stamped {
        r.header()?;
    }
    let values: Vec<Option<f64>> = (0..6)
        .map(|_| r.f64().map(Some))
        .collect::<Option<Vec<_>>>()?;
    // CDR pads a body to its own alignment, never beyond it.
    (r.remaining() < 8).then_some(values)
}

/// The dimension name for a one-scalar `sensor_msgs` measurement, or `None` for a schema that is not
/// one of them.
///
/// A **closed** table, and it names the quantity rather than calling every one of them `value`: a
/// finding that says a stream's `temperature` is pinned at its rail tells a reader what is wrong,
/// and `value` does not. Same rule as every other open namespace this reader judges — a schema
/// outside the table is declined, never guessed at.
pub fn scalar_measurement_name(schema_name: &str) -> Option<&'static str> {
    if super::mcap::schema_is(schema_name, "Temperature") {
        Some("temperature")
    } else if super::mcap::schema_is(schema_name, "FluidPressure") {
        Some("fluid_pressure")
    } else if super::mcap::schema_is(schema_name, "RelativeHumidity") {
        Some("relative_humidity")
    } else if super::mcap::schema_is(schema_name, "Illuminance") {
        Some("illuminance")
    } else {
        None
    }
}

/// Decode a one-scalar `sensor_msgs` measurement: `Header`, `float64 <value>`, `float64 variance`.
///
/// That is the whole of `Temperature`, `FluidPressure`, `RelativeHumidity` and `Illuminance` — four
/// schemas, one layout, and a robot recording carries them wherever it carries an environment. Each
/// was fingerprinted rather than measured, so a probe frozen at a constant, railed at its limit, or
/// publishing a NaN reported nothing at all.
///
/// The variance is read only to prove the body ends where the message says it does; the value it
/// holds is the sensor's own uncertainty, which is not a measurement of the world.
pub fn decode_scalar_measurement(data: &[u8]) -> Option<f64> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let value = r.f64()?;
    r.f64()?; // variance
    (r.remaining() < 8).then_some(value)
}

/// Decode a `sensor_msgs/msg/Range`: `Header`, `uint8 radiation_type`, `float32 field_of_view`,
/// `float32 min_range`, `float32 max_range`, `float32 range`.
///
/// Returns the reading, or `None` for a reading the sensor's **own** window disowns. A sonar or IR
/// rangefinder reports "nothing there" by publishing a value outside `[min_range, max_range]` (or an
/// infinity), the same convention `LaserScan` uses, so recording one as a distance would report a
/// beam that saw nothing as a measurement — and a probe that saw nothing for a whole run as a
/// perfectly steady one.
///
/// The outer `Option` says whether the body is a `Range` at all; the inner one whether that message
/// measured something.
pub fn decode_range_value(data: &[u8]) -> Option<Option<f64>> {
    let mut r = Reader::new(data)?;
    r.header()?;
    r.u8()?; // radiation_type
    let field_of_view = r.f32()?;
    let min_range = r.f32()?;
    let max_range = r.f32()?;
    let range = r.f32()?;
    if r.remaining() >= 4 {
        return None;
    }
    // The sensor's own window has to be a window, and a beam has to have a width; without both there
    // is nothing to judge the reading against.
    if !(field_of_view.is_finite()
        && min_range.is_finite()
        && max_range.is_finite()
        && min_range < max_range)
    {
        return None;
    }
    let measured = range.is_finite() && range >= min_range && range <= max_range;
    Some(measured.then(|| f64::from(range)))
}

/// Decode a `sensor_msgs/msg/Imu` body into its ten measured scalars, in the order
/// `[qx, qy, qz, qw, wx, wy, wz, ax, ay, az]` — orientation, angular velocity, linear acceleration.
///
/// Layout: `Header`, `Quaternion orientation`, `float64[9] orientation_covariance`,
/// `Vector3 angular_velocity`, `float64[9] angular_velocity_covariance`,
/// `Vector3 linear_acceleration`, `float64[9] linear_acceleration_covariance`. Everything is a fixed
/// number of doubles, so the whole message is 37 values — it has no bulk payload to decline.
///
/// A field whose covariance begins with `-1` is one the driver declares it does **not** provide, and
/// ROS leaves its slot zero-filled. Those slots come back as `None` rather than as zeros: recording
/// them as measurements would report a driver that publishes no orientation as an IMU whose
/// orientation is frozen at the origin — a defect it does not have, hiding the ones it might.
/// The name of each scalar [`decode_imu_values`] returns, in the same order.
pub const IMU_DIM_NAMES: [&str; 10] = [
    "orientation.x",
    "orientation.y",
    "orientation.z",
    "orientation.w",
    "angular_velocity.x",
    "angular_velocity.y",
    "angular_velocity.z",
    "linear_acceleration.x",
    "linear_acceleration.y",
    "linear_acceleration.z",
];

pub fn decode_imu_values(data: &[u8]) -> Option<Vec<Option<f64>>> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let read = |r: &mut Reader, n: usize| -> Option<Vec<f64>> { (0..n).map(|_| r.f64()).collect() };
    let orientation = read(&mut r, 4)?;
    let orientation_cov0 = r.f64()?;
    read(&mut r, 8)?;
    let angular = read(&mut r, 3)?;
    let angular_cov0 = r.f64()?;
    read(&mut r, 8)?;
    let linear = read(&mut r, 3)?;
    let linear_cov0 = r.f64()?;

    // `covariance[0] == -1` is the ROS convention for "this field is not provided".
    let provided = |cov0: f64, vs: Vec<f64>| -> Vec<Option<f64>> {
        let keep = cov0 != -1.0;
        vs.into_iter().map(|v| keep.then_some(v)).collect()
    };
    let values: Vec<Option<f64>> = provided(orientation_cov0, orientation)
        .into_iter()
        .chain(provided(angular_cov0, angular))
        .chain(provided(linear_cov0, linear))
        .collect();
    values.iter().any(Option::is_some).then_some(values)
}

/// The name of each scalar [`decode_nav_sat_fix`] returns for a fix, in the same order.
pub const NAV_SAT_FIX_DIM_NAMES: [&str; 3] = ["latitude", "longitude", "altitude"];

/// The `NavSatStatus.status` value a receiver publishes when it has no fix at all.
///
/// ROS defines `STATUS_NO_FIX = -1`. The message still carries latitude, longitude and altitude
/// fields, and a driver with no fix leaves them at whatever it last had or at zero — so recording
/// them as measurements reports a vehicle parked at Null Island, or frozen at its last known
/// position, as a fact about the drive. It is not one.
const NAV_SAT_STATUS_NO_FIX: i8 = -1;

/// What one `NavSatFix` body turned out to be.
///
/// The distinction a plain value decode cannot express: one `Option` answers `None` for
/// a body that is not a `NavSatFix` at all *and* for one whose receiver declared no fix, and those
/// are opposite facts. The first is a message Veridex could not read; the second is a message that
/// read perfectly and says the sensor had nothing to report. Counting the second is the only way a
/// report can distinguish a receiver that lost the sky from one that never did.
#[derive(Debug, Clone, PartialEq)]
pub enum NavSatSample {
    /// A fix: `[latitude, longitude, altitude]`.
    Fix(Vec<Option<f64>>),
    /// The receiver stamped `STATUS_NO_FIX`. The coordinates in the body are what the driver left
    /// behind, so they are deliberately not returned.
    NoFix,
}

/// Decode a `sensor_msgs/msg/NavSatFix` body, keeping the receiver's own verdict on it.
///
/// Layout: `Header`, `NavSatStatus { int8 status, uint16 service }`, `float64 latitude`,
/// `float64 longitude`, `float64 altitude`, `float64[9] position_covariance`,
/// `uint8 position_covariance_type`. Fixed size, no bulk payload to decline.
///
/// The coordinates are read before the status is judged, so a truncated body is `None` — not a
/// no-fix. A message that cannot be parsed is not the receiver saying anything.
pub fn decode_nav_sat_fix(data: &[u8]) -> Option<NavSatSample> {
    let mut r = Reader::new(data)?;
    r.header()?;
    // `NavSatStatus`: int8 then uint16. The uint16 is 2-aligned, and the f64 that follows is
    // 8-aligned, both of which `Reader` handles when the field is read.
    let status = r.u8()? as i8;
    r.align(2);
    let _service = r.take(2)?;
    let latitude = r.f64()?;
    let longitude = r.f64()?;
    let altitude = r.f64()?;
    if status == NAV_SAT_STATUS_NO_FIX {
        return Some(NavSatSample::NoFix);
    }
    Some(NavSatSample::Fix(vec![
        Some(latitude),
        Some(longitude),
        Some(altitude),
    ]))
}

/// Decode a `tf2_msgs/msg/TFMessage` body: a sequence of `TransformStamped`
/// `{ Header header (frame_id = parent), string child_frame_id, Transform { Vector3 translation,
/// Quaternion rotation } }`. Returns each edge as a CDM [`Transform`] with open validity.
pub fn decode_tf_message(data: &[u8]) -> Option<Vec<Transform>> {
    let mut r = Reader::new(data)?;
    let count = r.u32()? as usize;
    // A declared element count is attacker-controlled. Bound it by the smallest a `TransformStamped`
    // can encode (its header plus 7 f64s), so a tiny message can never reserve gigabytes; comparing
    // against the byte length alone would allow ~13 GB from a 100 MB body.
    const MIN_TRANSFORM_BYTES: usize = 60;
    if count > data.len() / MIN_TRANSFORM_BYTES {
        return None;
    }
    let mut transforms = Vec::with_capacity(count);
    for _ in 0..count {
        let parent = r.header()?; // header.frame_id is the parent frame
        let child = r.string()?;
        // Transform = Vector3 translation {x,y,z} + Quaternion rotation {x,y,z,w}.
        let tx = r.f64()?;
        let ty = r.f64()?;
        let tz = r.f64()?;
        let qx = r.f64()?;
        let qy = r.f64()?;
        let qz = r.f64()?;
        let qw = r.f64()?;
        transforms.push(Transform {
            parent_frame: parent,
            child_frame: child,
            pose: Pose {
                translation: [tx, ty, tz],
                rotation: [qx, qy, qz, qw],
            },
            valid_from: None,
            valid_to: None,
        });
    }
    Some(transforms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny CDR writer mirroring the reader's alignment rules, so tests encode bytes byte-identical
    /// to what a ROS 2 publisher would emit for these message layouts.
    struct W {
        buf: Vec<u8>,
    }
    impl W {
        fn new() -> W {
            // Encapsulation header: CDR_LE.
            W {
                buf: vec![0x00, 0x01, 0x00, 0x00],
            }
        }
        fn body_pos(&self) -> usize {
            self.buf.len() - 4
        }
        fn align(&mut self, n: usize) {
            while self.body_pos() % n != 0 {
                self.buf.push(0);
            }
        }
        fn u8(&mut self, v: u8) {
            self.buf.push(v);
        }
        fn u32(&mut self, v: u32) {
            self.align(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        fn i32(&mut self, v: i32) {
            self.u32(v as u32);
        }
        fn f32(&mut self, v: f32) {
            self.align(4);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        fn f64(&mut self, v: f64) {
            self.align(8);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
        fn string(&mut self, s: &str) {
            self.u32((s.len() + 1) as u32);
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(0);
        }
        fn header(&mut self, frame_id: &str) {
            self.header_at(frame_id, 0);
        }
        /// A header carrying a real capture stamp, in nanoseconds.
        fn header_at(&mut self, frame_id: &str, stamp_ns: u64) {
            self.i32((stamp_ns / 1_000_000_000) as i32); // stamp.sec
            self.u32((stamp_ns % 1_000_000_000) as u32); // stamp.nanosec
            self.string(frame_id);
        }
    }

    #[test]
    fn a_wrench_is_six_force_and_torque_components() {
        let mut w = W::new();
        for v in [1.0, 2.0, 3.0, 0.1, 0.2, 0.3] {
            w.f64(v);
        }
        let values = decode_wrench_values(&w.buf, false).expect("decodes");
        assert_eq!(values[0], Some(1.0));
        assert_eq!(values[5], Some(0.3));
        assert_eq!(WRENCH_DIM_NAMES.len(), 6);
        // Behind a header it is a `WrenchStamped`, and the same bytes read the other way are not
        // one of these at all: the length no longer fits.
        let mut w = W::new();
        w.header("ft_sensor");
        for _ in 0..6 {
            w.f64(0.0);
        }
        assert!(decode_wrench_values(&w.buf, true).is_some());
        assert_eq!(decode_wrench_values(&w.buf, false), None);
    }

    #[test]
    fn a_one_scalar_measurement_is_its_reading_and_its_variance() {
        let mut w = W::new();
        w.header("probe");
        w.f64(21.5); // the reading
        w.f64(0.01); // variance
        assert_eq!(decode_scalar_measurement(&w.buf), Some(21.5));

        // The table that says which schemas have this shape is closed, and names the quantity
        // rather than calling all four of them `value`.
        assert_eq!(
            scalar_measurement_name("sensor_msgs/msg/Temperature"),
            Some("temperature")
        );
        assert_eq!(
            scalar_measurement_name("sensor_msgs/RelativeHumidity"),
            Some("relative_humidity")
        );
        assert_eq!(scalar_measurement_name("sensor_msgs/msg/Imu"), None);

        // Its length is what says a body is one of them.
        let mut w = W::new();
        w.header("probe");
        w.f64(21.5);
        assert_eq!(decode_scalar_measurement(&w.buf), None, "no variance");
        let mut w = W::new();
        w.header("probe");
        for _ in 0..4 {
            w.f64(0.0);
        }
        assert_eq!(decode_scalar_measurement(&w.buf), None, "too long");
    }

    /// A `sensor_msgs/msg/Range` body, with the rangefinder's own window under the caller's control.
    fn range_msg(min_range: f32, max_range: f32, range: f32) -> W {
        let mut w = W::new();
        w.header("sonar");
        w.u8(0); // radiation_type = ULTRASOUND
        w.f32(0.5); // field_of_view
        w.f32(min_range);
        w.f32(max_range);
        w.f32(range);
        w
    }

    #[test]
    fn a_rangefinder_reading_outside_its_own_window_is_not_a_distance() {
        // A sonar reports "nothing there" by publishing outside `[min_range, max_range]`. Recording
        // that as a distance would report a beam that saw nothing as a measurement — and a probe
        // that saw nothing all run as a perfectly steady one.
        assert_eq!(
            decode_range_value(&range_msg(0.2, 4.0, 1.5).buf),
            Some(Some(1.5))
        );
        assert_eq!(
            decode_range_value(&range_msg(0.2, 4.0, 9.0).buf),
            Some(None)
        );
        assert_eq!(
            decode_range_value(&range_msg(0.2, 4.0, 0.05).buf),
            Some(None)
        );
        assert_eq!(
            decode_range_value(&range_msg(0.2, 4.0, f32::INFINITY).buf),
            Some(None)
        );
        // And a body whose window is not a window, or that is not a `Range` at all, yields nothing.
        assert_eq!(decode_range_value(&range_msg(4.0, 0.2, 1.5).buf), None);
        let mut w = range_msg(0.2, 4.0, 1.5);
        w.f32(0.0);
        assert_eq!(decode_range_value(&w.buf), None, "too long");
    }

    #[test]
    fn a_twist_is_six_velocity_components_and_nothing_else() {
        let mut w = W::new();
        for v in [0.5, 0.0, 0.0, 0.0, 0.0, -0.25] {
            w.f64(v);
        }
        assert_eq!(
            decode_twist_values(&w.buf, false),
            Some(vec![
                Some(0.5),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(-0.25)
            ])
        );
        // A NaN in a velocity command is a fault the statistical family reports, not a parse
        // failure — reading it is the only way anything can say it is there.
        let mut w = W::new();
        w.f64(f64::NAN);
        for _ in 0..5 {
            w.f64(0.0);
        }
        assert!(decode_twist_values(&w.buf, false).expect("decodes")[0]
            .expect("a value")
            .is_nan());
    }

    #[test]
    fn a_stamped_twist_carries_the_same_six_behind_a_header() {
        let mut w = W::new();
        w.header_at("base_link", 1_767_225_600_000_000_000);
        for v in [1.0, 2.0, 3.0, 4.0, 5.0, 6.0] {
            w.f64(v);
        }
        let values = decode_twist_values(&w.buf, true).expect("decodes");
        assert_eq!(values[0], Some(1.0));
        assert_eq!(values[5], Some(6.0));
        // Read without the header, the same bytes are not a `Twist`: the length no longer fits.
        assert_eq!(decode_twist_values(&w.buf, false), None);
    }

    #[test]
    fn a_body_that_is_not_a_twist_yields_no_values() {
        // Six doubles are six doubles and any value they hold is legal, so the message's *length*
        // is the only thing that says a body is one. Without it a mislabelled topic would have
        // whatever it carries summarized as a velocity command.
        let mut w = W::new();
        for _ in 0..5 {
            w.f64(0.0);
        }
        assert_eq!(decode_twist_values(&w.buf, false), None, "too short");
        let mut w = W::new();
        for _ in 0..8 {
            w.f64(0.0);
        }
        assert_eq!(decode_twist_values(&w.buf, false), None, "too long");
        assert_eq!(TWIST_DIM_NAMES.len(), 6);
    }

    /// A `sensor_msgs/msg/LaserScan` body over `ranges`, with the scanner's own window under the
    /// caller's control.
    fn laser_scan(range_min: f32, range_max: f32, ranges: &[f32], intensities: usize) -> W {
        let mut w = W::new();
        w.header("laser");
        w.f32(-1.57); // angle_min
        w.f32(1.57); // angle_max
        w.f32(0.01); // angle_increment
        w.f32(0.0); // time_increment
        w.f32(0.1); // scan_time
        w.f32(range_min);
        w.f32(range_max);
        w.u32(ranges.len() as u32);
        for r in ranges {
            w.f32(*r);
        }
        w.u32(intensities as u32);
        for _ in 0..intensities {
            w.f32(0.0);
        }
        w
    }

    #[test]
    fn a_laser_scan_counts_only_the_returns_that_measured_something() {
        // REP 117: a direction the beam came back from with nothing in it is reported as an
        // infinity, a NaN, or a value outside the scanner's own window — never as a distance.
        let w = laser_scan(
            0.1,
            10.0,
            &[1.0, f32::INFINITY, 2.5, f32::NAN, 20.0, 0.05],
            0,
        );
        assert_eq!(decode_laser_scan_returns(&w.buf), Some(2));
        // An intensities array parallel to the ranges is the other legal shape.
        let w = laser_scan(0.1, 10.0, &[1.0, 2.0, 3.0], 3);
        assert_eq!(decode_laser_scan_returns(&w.buf), Some(3));
    }

    #[test]
    fn a_scanner_that_measured_nothing_reads_as_zero_rather_than_as_absent() {
        // The whole point of the check this feeds: a driver that lost its sensor publishes a full,
        // well-formed, correctly-timed sweep of infinities, and that has to be distinguishable from
        // a scan nobody counted.
        let w = laser_scan(0.1, 10.0, &[f32::INFINITY; 8], 0);
        assert_eq!(decode_laser_scan_returns(&w.buf), Some(0));
    }

    #[test]
    fn a_body_that_is_not_a_laser_scan_yields_no_count() {
        // Each of these is an invariant of the message that an arbitrary buffer does not satisfy,
        // and a fabricated return count would be a finding about honest data.
        assert_eq!(
            decode_laser_scan_returns(&[0x00, 0x01, 0x00, 0x00][..]),
            None
        );
        let w = laser_scan(0.1, 10.0, &[], 0);
        assert_eq!(
            decode_laser_scan_returns(&w.buf),
            None,
            "a sweep of nothing"
        );
        let w = laser_scan(10.0, 0.1, &[1.0], 0);
        assert_eq!(decode_laser_scan_returns(&w.buf), None, "window inverted");
        let w = laser_scan(0.1, f32::INFINITY, &[1.0], 0);
        assert_eq!(decode_laser_scan_returns(&w.buf), None, "window unbounded");
        let w = laser_scan(0.1, 10.0, &[1.0, 2.0], 1);
        assert_eq!(
            decode_laser_scan_returns(&w.buf),
            None,
            "intensities neither absent nor one per return"
        );
        let mut w = laser_scan(0.1, 10.0, &[1.0, 2.0], 2);
        w.buf.truncate(w.buf.len() - 1);
        assert_eq!(decode_laser_scan_returns(&w.buf), None, "body cut short");
    }

    /// A `sensor_msgs/msg/Image` body, with every field the decode reads under the caller's control
    /// so each invariant can be broken one at a time.
    fn image(frame_id: &str, height: u32, width: u32, encoding: &str, step: u32, data: u32) -> W {
        let mut w = W::new();
        w.header(frame_id);
        w.u32(height);
        w.u32(width);
        w.string(encoding);
        w.u8(0); // is_bigendian
        w.u32(step);
        w.u32(data);
        w.buf.extend(std::iter::repeat(0u8).take(data as usize));
        w
    }

    /// A `sensor_msgs/msg/CompressedImage` body: header, format string, then the compressed bytes.
    fn compressed_image(format: &str, payload: &[u8]) -> W {
        let mut w = W::new();
        w.header("camera_front");
        w.string(format);
        w.u32(payload.len() as u32);
        w.buf.extend_from_slice(payload);
        w
    }

    /// The smallest byte sequence a JPEG frame header needs: `SOI`, an `APP0` to be skipped, then a
    /// baseline `SOF0` declaring `height x width`.
    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]); // APP0, length 4
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]); // SOF0, length 17, precision 8
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&[0u8; 10]); // component spec, never read
        v
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, colour type, and the rest
        v
    }

    #[test]
    fn a_compressed_frame_gives_up_its_size_without_decoding_a_pixel() {
        let w = compressed_image("jpeg", &jpeg(1920, 1080));
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            Some(Some((1920, 1080)))
        );
        // ROS also spells the format in full, and the codec is looked for inside the string.
        let w = compressed_image("rgb8; jpeg compressed bgr8", &jpeg(640, 480));
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            Some(Some((640, 480)))
        );
        let w = compressed_image("png", &png(320, 240));
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            Some(Some((320, 240)))
        );
    }

    #[test]
    fn a_compressed_frame_carrying_nothing_needs_no_codec_to_recognize() {
        // The fault this feeds: a driver that lost its sensor publishes a well-formed message at the
        // right rate with nothing compressed in it. No codec has to be understood to see that.
        let w = compressed_image("jpeg", &[]);
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            Some(Some((0, 0)))
        );
        let w = compressed_image("h264", &[]);
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            Some(Some((0, 0)))
        );
    }

    #[test]
    fn a_codec_this_reader_does_not_read_is_untried_rather_than_broken() {
        // The distinction the caller acts on: nothing was measured *and nothing failed*, so the
        // stream abstains out loud rather than being accused of carrying bodies that broke.
        let w = compressed_image("h264", &[0x00, 0x00, 0x01, 0x67, 0x42]);
        assert_eq!(decode_compressed_image_dimensions(&w.buf), Some(None));
    }

    #[test]
    fn a_named_codec_whose_header_is_unreadable_is_a_body_that_broke() {
        // A JPEG topic whose payload is not a JPEG is a truncated write or a dropped chunk — a
        // fault, and distinct from a codec this reader declined.
        let w = compressed_image("jpeg", &[0xFF, 0xD8, 0xFF, 0xC0]);
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            None,
            "truncated"
        );
        let w = compressed_image("jpeg", &[0x89, 0x50, 0x4E, 0x47]);
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            None,
            "not a jpeg"
        );
        let w = compressed_image("png", &png(320, 240)[..12]);
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            None,
            "not a png"
        );
        // And a body that is not a `CompressedImage` at all.
        assert_eq!(
            decode_compressed_image_dimensions(&[0x00, 0x01, 0x00, 0x00][..]),
            None
        );
        let w = compressed_image("", &jpeg(64, 64));
        assert_eq!(
            decode_compressed_image_dimensions(&w.buf),
            None,
            "no format"
        );
    }

    #[test]
    fn a_jpeg_marker_chain_cannot_be_walked_forever() {
        // The segment lengths come out of the file, so a chain of empty segments is a loop the run
        // would otherwise pay for. It ends in "not a frame header this reader found".
        let mut v = vec![0xFF, 0xD8];
        for _ in 0..(MAX_JPEG_SEGMENTS + 10) {
            v.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x02]);
        }
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0, 64, 0, 64]);
        assert_eq!(jpeg_dimensions(&v), None);
    }

    #[test]
    fn an_image_body_yields_the_size_it_declares() {
        let w = image("camera_front", 720, 1280, "mono8", 1280, 1280 * 720);
        assert_eq!(decode_image_dimensions(&w.buf), Some((1280, 720)));
        // Three bytes a pixel is an ordinary `rgb8` frame, not a suspicious stride.
        let w = image("camera_front", 4, 8, "rgb8", 24, 96);
        assert_eq!(decode_image_dimensions(&w.buf), Some((8, 4)));
    }

    #[test]
    fn an_empty_frame_is_a_reading_rather_than_a_refusal() {
        // The whole point of the check this feeds: a driver that lost its sensor publishes a
        // well-formed frame declaring no pixels, and that has to be distinguishable from a body
        // whose size was never read.
        let w = image("camera_front", 0, 0, "mono8", 0, 0);
        assert_eq!(decode_image_dimensions(&w.buf), Some((0, 0)));
    }

    #[test]
    fn a_body_that_is_not_an_image_yields_no_size() {
        // A buffer of zeroes satisfies every length invariant trivially, so the non-empty
        // `encoding` is what stands between it and being reported as a dead camera.
        assert_eq!(decode_image_dimensions(&[0x00, 0x01, 0x00, 0x00][..]), None);
        let w = image("camera_front", 4, 8, "", 8, 32);
        assert_eq!(decode_image_dimensions(&w.buf), None, "no encoding named");
        // A row that cannot hold its own pixels, and one far too wide to be an image's.
        let w = image("camera_front", 4, 8, "mono8", 4, 16);
        assert_eq!(decode_image_dimensions(&w.buf), None, "step below width");
        let w = image("camera_front", 4, 8, "mono8", 8 * 33, 8 * 33 * 4);
        assert_eq!(decode_image_dimensions(&w.buf), None, "step absurdly wide");
        // `data` is exactly `step × height` — the invariant an arbitrary body will not satisfy by
        // accident.
        let w = image("camera_front", 4, 8, "mono8", 8, 31);
        assert_eq!(
            decode_image_dimensions(&w.buf),
            None,
            "data length disagrees"
        );
        // ...and the bytes are actually there. A stub prefix claiming a full frame is declined even
        // when its numbers agree with each other.
        let mut w = image("camera_front", 4, 8, "mono8", 8, 32);
        w.buf.truncate(w.buf.len() - 1);
        assert_eq!(decode_image_dimensions(&w.buf), None, "pixels not present");
    }

    #[test]
    fn decodes_a_header_stamp() {
        let mut w = W::new();
        w.header_at("lidar_top", 1_767_225_600_123_456_789);
        assert_eq!(decode_header_stamp(&w.buf), Some(1_767_225_600_123_456_789));
    }

    #[test]
    fn an_unstamped_header_reads_as_zero_not_as_absent() {
        // The whole point of the check this feeds: a driver that never stamped publishes the epoch,
        // and that has to be distinguishable from a body whose stamp was never read.
        let mut w = W::new();
        w.header("lidar_top");
        assert_eq!(decode_header_stamp(&w.buf), Some(0));
    }

    #[test]
    fn a_body_that_is_not_header_shaped_yields_no_stamp() {
        // Eight bytes read as a time are plausible in almost any payload. A denormalized `nanosec`
        // (at or past a full second), a `sec` before 1970, and a truncated `frame_id` each say the
        // bytes were something else — and a fabricated clock reading is worse than no reading.
        let mut w = W::new();
        w.i32(0);
        w.u32(1_000_000_000); // nanosec at a full second: not a normalized ROS time
        w.string("lidar_top");
        assert_eq!(decode_header_stamp(&w.buf), None);

        let mut w = W::new();
        w.i32(-1); // a stamp before 1970
        w.u32(0);
        w.string("lidar_top");
        assert_eq!(decode_header_stamp(&w.buf), None);

        let mut w = W::new();
        w.i32(5);
        w.u32(0); // and then nothing where the frame_id should be
        assert_eq!(decode_header_stamp(&w.buf), None);
    }

    #[test]
    fn the_stamp_accumulator_separates_unset_offset_and_regression() {
        let mut a = HeaderStampAccum::default();
        a.observe(1_000_000_000, 995_000_000); // 5 ms behind the recorder
        a.observe(1_100_000_000, 0); // unstamped
        a.observe(1_200_000_000, 1_194_000_000); // 6 ms behind
        a.observe(1_300_000_000, 1_100_000_000); // stamp steps backwards
        let h = a.finish().expect("stamps were read");
        assert_eq!(h.message_count, 4);
        assert_eq!(h.unset, 1);
        assert_eq!(h.min_offset_ns, 5_000_000);
        assert_eq!(h.max_offset_ns, 200_000_000);
        assert_eq!(h.regressions, 1);
    }

    #[test]
    fn a_stream_whose_stamps_were_never_read_summarizes_to_nothing() {
        // "No stamp was decoded" and "every stamp was zero" are opposite verdicts: one is a format
        // that does not carry capture times, the other is a driver that never set them.
        assert!(HeaderStampAccum::default().finish().is_none());
    }

    #[test]
    fn decodes_point_cloud2_fields() {
        let mut w = W::new();
        w.header("lidar");
        w.u32(1); // height
        w.u32(1000); // width
        w.u32(4); // 4 fields
        for name in ["x", "y", "z", "intensity"] {
            w.string(name);
            w.u32(0); // offset
            w.u8(7); // datatype FLOAT32
            w.u32(1); // count
        }
        let fields = decode_point_cloud2_fields(&w.buf).expect("decode");
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["x", "y", "z", "intensity"]);
        assert!(fields.iter().all(|f| f.dtype.as_deref() == Some("float32")));
    }

    #[test]
    fn decodes_camera_info_intrinsics() {
        let mut w = W::new();
        w.header("cam");
        w.u32(480);
        w.u32(640);
        w.string("plumb_bob");
        w.u32(5); // d: 5 coeffs
        for v in [0.1, -0.2, 0.0, 0.0, 0.0] {
            w.f64(v);
        }
        // k (row-major 3x3): fx=600 at 0, cx=320 at 2, fy=600 at 4, cy=240 at 5.
        for v in [600.0, 0.0, 320.0, 0.0, 600.0, 240.0, 0.0, 0.0, 1.0] {
            w.f64(v);
        }
        let ci = decode_camera_info(&w.buf, "/cam/info").expect("decode");
        assert_eq!(ci.fx, 600.0);
        assert_eq!(ci.fy, 600.0);
        assert_eq!(ci.cx, 320.0);
        assert_eq!(ci.cy, 240.0);
        assert_eq!(ci.distortion.len(), 5);
        assert_eq!(ci.stream, "/cam/info");
    }

    #[test]
    fn decodes_odometry_pose() {
        let mut w = W::new();
        w.header("odom");
        w.string("base_link"); // child_frame_id
        for v in [1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0] {
            w.f64(v);
        }
        let (pose, child) = decode_odometry(&w.buf).expect("decode");
        assert_eq!(child.as_deref(), Some("base_link"));
        assert_eq!(pose.translation, [1.0, 2.0, 3.0]);
        assert_eq!(pose.rotation, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn decodes_tf_message() {
        let mut w = W::new();
        w.u32(1); // one transform
        w.header("base_link"); // header.frame_id = parent
        w.string("lidar_top"); // child_frame_id
        for v in [0.1, 0.2, 0.3, 0.0, 0.0, 0.0, 1.0] {
            w.f64(v);
        }
        let ts = decode_tf_message(&w.buf).expect("decode");
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].parent_frame, "base_link");
        assert_eq!(ts[0].child_frame, "lidar_top");
        assert_eq!(ts[0].pose.translation, [0.1, 0.2, 0.3]);
    }

    #[test]
    fn decodes_joint_state_positions() {
        let mut w = W::new();
        w.header("");
        w.u32(3); // name[]
        for n in ["shoulder", "elbow", "gripper"] {
            w.string(n);
        }
        w.u32(3); // position[]
        for v in [0.5, -1.25, 0.0] {
            w.f64(v);
        }
        w.u32(0); // velocity[]
        w.u32(0); // effort[]
        assert_eq!(
            decode_joint_state(&w.buf).expect("decode").1,
            vec![0.5, -1.25, 0.0]
        );
    }

    #[test]
    fn a_joint_state_without_positions_is_not_a_measurement() {
        // A publisher that reports effort only: `position` is empty. Returning `Some(vec![])` would
        // record the stream as measured when nothing was measured.
        let mut w = W::new();
        w.header("");
        w.u32(1);
        w.string("elbow");
        w.u32(0); // position[] empty
        w.u32(0); // velocity[]
        w.u32(1); // effort[]
        w.f64(2.5);
        assert!(decode_joint_state(&w.buf).is_none());
    }

    #[test]
    fn a_joint_state_with_an_absurd_count_is_declined() {
        // Both sequence counts are attacker-controlled and must be bounded by what the body holds.
        let mut w = W::new();
        w.header("");
        w.u32(4_000_000_000); // name[] count
        assert!(decode_joint_state(&w.buf).is_none());

        let mut w = W::new();
        w.header("");
        w.u32(0); // name[]
        w.u32(4_000_000_000); // position[] count
        assert!(decode_joint_state(&w.buf).is_none());
    }

    /// One `sensor_msgs/msg/Imu` body. Each `*_cov0` is that field's `covariance[0]`; `-1.0` is the
    /// ROS convention for "this field is not provided".
    fn imu(
        orientation: [f64; 4],
        orientation_cov0: f64,
        angular: [f64; 3],
        angular_cov0: f64,
        linear: [f64; 3],
        linear_cov0: f64,
    ) -> Vec<u8> {
        let mut w = W::new();
        w.header("imu_link");
        for v in orientation {
            w.f64(v);
        }
        w.f64(orientation_cov0);
        for _ in 0..8 {
            w.f64(0.0);
        }
        for v in angular {
            w.f64(v);
        }
        w.f64(angular_cov0);
        for _ in 0..8 {
            w.f64(0.0);
        }
        for v in linear {
            w.f64(v);
        }
        w.f64(linear_cov0);
        for _ in 0..8 {
            w.f64(0.0);
        }
        w.buf
    }

    #[test]
    fn decodes_the_imu_measurements_in_order() {
        let body = imu(
            [0.0, 0.0, 0.0, 1.0],
            0.01,
            [0.1, 0.2, 0.3],
            0.02,
            [0.0, 0.0, 9.81],
            0.03,
        );
        assert_eq!(
            decode_imu_values(&body).expect("decode"),
            vec![
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(1.0),
                Some(0.1),
                Some(0.2),
                Some(0.3),
                Some(0.0),
                Some(0.0),
                Some(9.81),
            ]
        );
    }

    #[test]
    fn a_field_the_driver_declares_absent_is_held_out_not_read_as_zero() {
        // The common case: a gyro/accelerometer with no orientation estimate. ROS leaves the
        // quaternion zero-filled and sets `orientation_covariance[0] = -1`. Summarizing those zeros
        // would report a frozen orientation the IMU never claimed to have.
        let body = imu(
            [0.0, 0.0, 0.0, 0.0],
            -1.0,
            [0.1, 0.2, 0.3],
            0.02,
            [0.0, 0.0, 9.81],
            0.03,
        );
        let values = decode_imu_values(&body).expect("decode");
        assert_eq!(&values[..4], &[None, None, None, None]);
        assert_eq!(values[4], Some(0.1));
        assert_eq!(values[9], Some(9.81));
    }

    #[test]
    fn an_imu_that_provides_nothing_is_not_a_measurement() {
        let body = imu([0.0; 4], -1.0, [0.0; 3], -1.0, [0.0; 3], -1.0);
        assert!(decode_imu_values(&body).is_none());
        // And a body that stops short of the ten values is declined rather than half-read.
        assert!(decode_imu_values(&body[..40]).is_none());
    }

    /// Every decoder in this module, over every truncation and a spread of byte flips of a valid
    /// body of each message type.
    ///
    /// These are the only parsers in Veridex pointed at bytes a *publisher* chose: a message body
    /// arrives from whatever node was on the bus, and the counts and lengths inside it steer this
    /// reader's arithmetic and its allocations. The sweep over damaged *files* reaches them only
    /// through a container that usually fails first, so it never gets this far. The assertion is the
    /// one that matters for a tool whose job is to survive bad data: return `None`, never unwind.
    #[test]
    fn no_damaged_message_body_takes_the_process_down() {
        let mut jointstate = W::new();
        jointstate.header("");
        jointstate.u32(2);
        for n in ["shoulder", "elbow"] {
            jointstate.string(n);
        }
        jointstate.u32(2);
        jointstate.f64(0.5);
        jointstate.f64(-1.25);
        jointstate.u32(0);
        jointstate.u32(0);

        let mut pc = W::new();
        pc.header("lidar");
        pc.u32(1);
        pc.u32(1000);
        pc.u32(2);
        for name in ["x", "y"] {
            pc.string(name);
            pc.u32(0);
            pc.u8(7);
            pc.u32(1);
        }

        let mut ci = W::new();
        ci.header("cam");
        ci.u32(480);
        ci.u32(640);
        ci.string("plumb_bob");
        ci.u32(2);
        ci.f64(0.1);
        ci.f64(-0.2);
        for v in [600.0, 0.0, 320.0, 0.0, 600.0, 240.0, 0.0, 0.0, 1.0] {
            ci.f64(v);
        }

        let mut odom = W::new();
        odom.header("odom");
        odom.string("base_link");
        for v in [1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0] {
            odom.f64(v);
        }

        let mut tf = W::new();
        tf.u32(1);
        tf.header("base_link");
        tf.string("lidar_top");
        for v in [0.1, 0.2, 0.3, 0.0, 0.0, 0.0, 1.0] {
            tf.f64(v);
        }

        let bodies = [
            jointstate.buf,
            pc.buf,
            ci.buf,
            odom.buf,
            imu(
                [0.0, 0.0, 0.0, 1.0],
                0.01,
                [0.1, 0.2, 0.3],
                0.02,
                [0.0, 0.0, 9.81],
                0.03,
            ),
            tf.buf,
        ];

        // Every decoder is run over every body, not only its own: a topic's declared schema is also
        // content, so a `CameraInfo` decoder can be handed a `PointCloud2` body by a mislabelled
        // channel, and must decline it rather than misread it into a panic.
        let decode_all = |b: &[u8]| {
            let _ = decode_header_frame_id(b);
            let _ = decode_point_cloud2_fields(b);
            let _ = decode_camera_info(b, "/topic");
            let _ = decode_odometry(b);
            let _ = decode_joint_state(b);
            let _ = decode_imu_values(b);
            let _ = decode_nav_sat_fix(b);
            let _ = decode_tf_message(b);
        };

        for body in &bodies {
            // Every prefix, including the empty one: a truncated message is what a half-written
            // shard and a dropped connection both leave behind.
            for cut in 0..=body.len() {
                decode_all(&body[..cut]);
            }
            // Byte flips, from a fixed linear congruential generator so a failure is reproducible
            // from the index alone. These land in the length and count fields as readily as in the
            // payload, which is the point.
            let mut state = 0x5eed_u64;
            for _ in 0..512 {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let at = (state >> 33) as usize % body.len();
                let xor = ((state >> 20) & 0xFF) as u8;
                let mut damaged = body.clone();
                damaged[at] ^= xor;
                decode_all(&damaged);
            }
        }
    }

    #[test]
    fn a_nav_sat_fix_decodes_to_its_three_coordinates() {
        let mut w = W::new();
        w.header("gnss");
        w.u8(0); // NavSatStatus.status = STATUS_FIX
        w.align(2);
        w.buf.extend_from_slice(&1u16.to_le_bytes()); // service = SERVICE_GPS
        for v in [37.4, -122.1, 12.5] {
            w.f64(v);
        }
        for _ in 0..9 {
            w.f64(0.0);
        }
        w.u8(0); // position_covariance_type
        assert_eq!(
            super::decode_nav_sat_fix(&w.buf),
            Some(super::NavSatSample::Fix(vec![
                Some(37.4),
                Some(-122.1),
                Some(12.5)
            ]))
        );
    }

    #[test]
    fn a_receiver_with_no_fix_contributes_no_position() {
        // `STATUS_NO_FIX` means the coordinates are whatever the driver left behind — zeros, or the
        // last known position. Recording them would report a vehicle parked at Null Island, or
        // frozen where it last had signal, as a fact about the drive.
        let mut w = W::new();
        w.header("gnss");
        w.u8(0xFF); // status = STATUS_NO_FIX (-1)
        w.align(2);
        w.buf.extend_from_slice(&0u16.to_le_bytes());
        for v in [0.0, 0.0, 0.0] {
            w.f64(v);
        }
        // Read as the receiver's own verdict, not as an unreadable message: those are opposite
        // facts, and only the first can be counted.
        assert_eq!(
            super::decode_nav_sat_fix(&w.buf),
            Some(super::NavSatSample::NoFix)
        );
    }

    #[test]
    fn a_truncated_nav_sat_fix_yields_nothing_rather_than_a_partial_position() {
        let mut w = W::new();
        w.header("gnss");
        w.u8(0);
        w.align(2);
        w.buf.extend_from_slice(&1u16.to_le_bytes());
        w.f64(37.4); // latitude only
        assert_eq!(super::decode_nav_sat_fix(&w.buf), None);
    }

    #[test]
    fn malformed_or_big_endian_bodies_are_declined_not_panicked() {
        // Big-endian encapsulation.
        assert!(decode_odometry(&[0x00, 0x00, 0x00, 0x00]).is_none());
        // Truncated after the header.
        assert!(decode_point_cloud2_fields(&[0x00, 0x01, 0x00, 0x00, 0x01]).is_none());
        // A field count far larger than the buffer must not over-allocate or panic.
        let mut w = W::new();
        w.header("x");
        w.u32(1);
        w.u32(1);
        w.u32(4_000_000_000); // absurd field count
        assert!(decode_point_cloud2_fields(&w.buf).is_none());
        // Empty input.
        assert!(decode_tf_message(&[]).is_none());
    }

    /// A `PointCloud2` point count is believed only when the body proves it is a `PointCloud2`.
    ///
    /// The count is the first two `uint32`s after the header, so a decode that read only those would
    /// believe whatever bytes happen to sit there — and a channel's declared schema is not proof of
    /// its bodies. A recorder that stubs the payload, a mislabelled topic, a truncated write: each
    /// presents as a `PointCloud2` channel, and a fabricated count reaches the report as a finding
    /// about honest data. So the decode continues to the message's own length invariants.
    #[test]
    fn a_body_that_is_not_a_point_cloud_yields_no_point_count() {
        const POINT_STEP: u32 = 16;
        let cloud = |height: u32, width: u32, data_len: u32| {
            let mut w = W::new();
            w.header("lidar");
            w.u32(height);
            w.u32(width);
            w.u32(1);
            w.string("x");
            w.u32(0);
            w.u8(7);
            w.u32(1);
            w.u8(0); // is_bigendian
            w.u32(POINT_STEP);
            w.u32(POINT_STEP * width);
            w.u32(data_len);
            w.buf.resize(w.buf.len() + data_len as usize, 0);
            w.buf
        };
        // A real cloud, and a real *empty* cloud: both counted. The empty one is the case the
        // check exists for, so it must survive every rule above.
        assert_eq!(
            decode_point_cloud2_point_count(&cloud(1, 100, 1600)),
            Some(100)
        );
        assert_eq!(decode_point_cloud2_point_count(&cloud(1, 0, 0)), Some(0));

        // The stub body a demo recorder writes: a header and a payload that is not a cloud. Read as
        // two `uint32`s it yields a count; read as a message it is not one.
        let mut stub = W::new();
        stub.header("lidar");
        stub.buf.extend_from_slice(&0u64.to_le_bytes());
        stub.buf.extend_from_slice(&[0u8; 32]);
        assert!(decode_point_cloud2_point_count(&stub.buf).is_none());

        // `data` shorter than `row_step × height` claims — the shape a truncated write leaves.
        assert!(decode_point_cloud2_point_count(&cloud(1, 100, 0)).is_none());
        // ...and a `data` length the buffer does not actually hold.
        let mut short = cloud(1, 100, 1600);
        short.truncate(short.len() - 1);
        assert!(decode_point_cloud2_point_count(&short).is_none());

        // A record layout that does not fit the stride it declares: `intensity` at offset 12 is
        // four bytes wide, so it runs to 16 in a 12-byte point. A hand-rolled publisher that adds a
        // field and forgets to widen `point_step` writes exactly this, and every consumer reads
        // garbage for one field and correct values for the rest — so the count is declined rather
        // than reported over a record nothing can read.
        let layout = |offsets: &[(u32, u8)], point_step: u32| {
            let mut w = W::new();
            w.header("lidar");
            w.u32(1);
            w.u32(1);
            w.u32(offsets.len() as u32);
            for (i, (offset, datatype)) in offsets.iter().enumerate() {
                w.string(&format!("f{i}"));
                w.u32(*offset);
                w.u8(*datatype);
                w.u32(1);
            }
            w.u8(0);
            w.u32(point_step);
            w.u32(point_step);
            w.u32(point_step);
            w.buf.resize(w.buf.len() + point_step as usize, 0);
            w.buf
        };
        // Three float32s in twelve bytes is exactly right.
        assert_eq!(
            decode_point_cloud2_point_count(&layout(&[(0, 7), (4, 7), (8, 7)], 12)),
            Some(1)
        );
        // A fourth that runs past the stride is not.
        assert!(
            decode_point_cloud2_point_count(&layout(&[(0, 7), (4, 7), (8, 7), (12, 7)], 12))
                .is_none()
        );
        // Nor is one that overlaps the field before it.
        assert!(decode_point_cloud2_point_count(&layout(&[(0, 7), (2, 7), (8, 7)], 12)).is_none());
        // Nor a datatype tag the message definition does not define — that is not a width to guess.
        assert!(decode_point_cloud2_point_count(&layout(&[(0, 99)], 12)).is_none());

        // A field count past the cap is declined rather than walked: it is a `uint32` out of the
        // file, and the walk to the length invariants is a per-message cost.
        let mut many = W::new();
        many.header("lidar");
        many.u32(1);
        many.u32(1);
        many.u32(4_000_000_000);
        assert!(decode_point_cloud2_point_count(&many.buf).is_none());
    }
}
