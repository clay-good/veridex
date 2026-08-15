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

use crate::cdm::{CameraIntrinsics, PointField, Pose, Transform};

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
        Some(String::from_utf8_lossy(&bytes[..end]).into_owned())
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

/// Decode a `sensor_msgs/msg/PointCloud2` body far enough to recover its per-point field layout
/// (`fields`): `Header`, `uint32 height`, `uint32 width`, then a sequence of `PointField`
/// `{ string name, uint32 offset, uint8 datatype, uint32 count }`. The bulk `data` blob is never read.
pub fn decode_point_cloud2_fields(data: &[u8]) -> Option<Vec<PointField>> {
    let mut r = Reader::new(data)?;
    r.header()?; // header (stamp + frame_id)
    let _height = r.u32()?;
    let _width = r.u32()?;
    let count = r.u32()? as usize;
    // Guard against a corrupt length claiming more fields than the buffer could hold.
    if count > data.len() {
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

/// Decode a `sensor_msgs/msg/CameraInfo` body far enough to recover intrinsics for `stream`: `Header`,
/// `uint32 height`, `uint32 width`, `string distortion_model`, `float64[] d` (sequence), then the
/// row-major 3×3 intrinsic matrix `float64[9] k` (`fx=k0, fy=k4, cx=k2, cy=k5`). `valid_from`/`_to`
/// are left open — the caller stamps the validity range from the message time if it wishes.
pub fn decode_camera_info(data: &[u8], stream: &str) -> Option<CameraIntrinsics> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let _height = r.u32()?;
    let _width = r.u32()?;
    let _distortion_model = r.string()?;
    let d_len = r.u32()? as usize;
    if d_len > data.len() {
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
pub fn decode_odometry_pose(data: &[u8]) -> Option<Pose> {
    let mut r = Reader::new(data)?;
    r.header()?;
    let _child_frame_id = r.string()?;
    read_pose(&mut r)
}

/// Decode a `tf2_msgs/msg/TFMessage` body: a sequence of `TransformStamped`
/// `{ Header header (frame_id = parent), string child_frame_id, Transform { Vector3 translation,
/// Quaternion rotation } }`. Returns each edge as a CDM [`Transform`] with open validity.
pub fn decode_tf_message(data: &[u8]) -> Option<Vec<Transform>> {
    let mut r = Reader::new(data)?;
    let count = r.u32()? as usize;
    if count > data.len() {
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
        let pose = decode_odometry_pose(&w.buf).expect("decode");
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
    fn malformed_or_big_endian_bodies_are_declined_not_panicked() {
        // Big-endian encapsulation.
        assert!(decode_odometry_pose(&[0x00, 0x00, 0x00, 0x00]).is_none());
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
}
