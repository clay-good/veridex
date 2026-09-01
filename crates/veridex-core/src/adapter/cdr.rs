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

use crate::cdm::{CameraIntrinsics, PointCounts, PointField, Pose, Transform};

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

    /// Skip a `std_msgs/Header`: `{ int32 sec, uint32 nanosec }` then a `string frame_id`. Returns the
    /// `frame_id` (some messages, e.g. a `TransformStamped`, use it as the parent frame).
    fn header(&mut self) -> Option<String> {
        let _sec = self.i32()?;
        let _nanosec = self.u32()?;
        self.string()
    }
}

/// The `sensor_msgs/msg/PointField` datatype enum → a CDM dtype string.
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

/// The most `PointField` entries a body may declare before the point-count decode declines it.
///
/// A real `PointCloud2` declares a handful — `x`, `y`, `z`, `intensity`, `ring`, `time`. The count
/// is a `uint32` out of the file, and the decode below walks the list to reach the length invariants
/// that prove the body is a cloud at all, so an uncapped count is a per-message cost the file
/// chooses. 64 is far above any real layout and far below anything expensive.
const MAX_POINT_FIELDS: usize = 64;

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
    for _ in 0..field_count {
        r.string()?; // name
        r.u32()?; // offset
        r.u8()?; // datatype
        r.u32()?; // count
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

/// The name of each scalar [`decode_nav_sat_fix_values`] returns, in the same order.
pub const NAV_SAT_FIX_DIM_NAMES: [&str; 3] = ["latitude", "longitude", "altitude"];

/// The `NavSatStatus.status` value a receiver publishes when it has no fix at all.
///
/// ROS defines `STATUS_NO_FIX = -1`. The message still carries latitude, longitude and altitude
/// fields, and a driver with no fix leaves them at whatever it last had or at zero — so recording
/// them as measurements reports a vehicle parked at Null Island, or frozen at its last known
/// position, as a fact about the drive. It is not one.
const NAV_SAT_STATUS_NO_FIX: i8 = -1;

/// Decode a `sensor_msgs/msg/NavSatFix` body into `[latitude, longitude, altitude]`.
///
/// Layout: `Header`, `NavSatStatus { int8 status, uint16 service }`, `float64 latitude`,
/// `float64 longitude`, `float64 altitude`, `float64[9] position_covariance`,
/// `uint8 position_covariance_type`. Fixed size, no bulk payload to decline.
///
/// This is the one AV message body the decoder did not read, so a rig's GNSS stream was
/// fingerprinted rather than measured: a receiver frozen at one fix, one publishing NaNs, or one
/// railed at a coordinate limit reported nothing at all, while the same faults on the IMU beside it
/// were caught. `None` when the message declares no fix, so the fields it leaves behind are not
/// recorded as a position.
pub fn decode_nav_sat_fix_values(data: &[u8]) -> Option<Vec<Option<f64>>> {
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
        return None;
    }
    Some(vec![Some(latitude), Some(longitude), Some(altitude)])
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
            self.i32(0); // stamp.sec
            self.u32(0); // stamp.nanosec
            self.string(frame_id);
        }
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
            let _ = decode_nav_sat_fix_values(b);
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
            super::decode_nav_sat_fix_values(&w.buf),
            Some(vec![Some(37.4), Some(-122.1), Some(12.5)])
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
        assert_eq!(super::decode_nav_sat_fix_values(&w.buf), None);
    }

    #[test]
    fn a_truncated_nav_sat_fix_yields_nothing_rather_than_a_partial_position() {
        let mut w = W::new();
        w.header("gnss");
        w.u8(0);
        w.align(2);
        w.buf.extend_from_slice(&1u16.to_le_bytes());
        w.f64(37.4); // latitude only
        assert_eq!(super::decode_nav_sat_fix_values(&w.buf), None);
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
