//! One dispatch from a serialized ROS message body to the CDM, shared by every reader that carries
//! ROS messages: the MCAP adapter, both rosbag2 storage plugins, and the ROS 1 bag reader.
//!
//! It existed twice before this module — inline in the MCAP adapter's ingest loop, and as
//! `rosbag2::decode_body` — two hand-written chains over the same list of message types. A decoder
//! added to one and missed in the other is a stream measured through one container and merely
//! fingerprinted through the other: the same rig grading differently because of how it was
//! recorded, which is the one thing a cross-format verifier must not do. There is one chain now,
//! and the encoding a body is in is a parameter of it rather than a reason to write it again.

use std::collections::BTreeMap;

use crate::cdm::{CameraIntrinsics, EgoPose, PointField, Transform};

use super::cdr::Encoding;

/// Everything one message body can contribute to, borrowed field by field.
///
/// Some of it belongs to the stream the message was recorded on (its point layout, its measured
/// values) and some to the recording as a whole (the rig's calibration, the ego trajectory), and
/// the two live in different places in every caller. Borrowing each one separately is what lets a
/// single dispatch write to both without the callers having to hold them in one structure.
pub(crate) struct BodyTargets<'a> {
    /// Per-point field layout, from the first `PointCloud2` on this stream that decoded.
    pub point_fields: &'a mut Option<Vec<PointField>>,
    pub point_counts: &'a mut super::cdr::PointCountAccum,
    pub image_dims: &'a mut super::cdr::ImageDimAccum,
    pub fix_availability: &'a mut super::cdr::FixAvailabilityAccum,
    pub values: &'a mut super::stats::StreamValues,
    pub ego_poses: &'a mut Vec<EgoPose>,
    /// The body frame the trajectory is of; the first message that names one settles it.
    pub ego_frame: &'a mut Option<String>,
    pub intrinsics: &'a mut BTreeMap<String, CameraIntrinsics>,
    pub transforms: &'a mut BTreeMap<(String, String), Transform>,
    pub moved_frames: &'a mut super::cdr::MovingFrames,
}

/// Decode one message body into the autonomy CDM, returning whether the body decoded — or `None`
/// for a schema there is no typed decoder for, whose body nothing tried to read.
///
/// `enc` is the wire encoding the container recorded: CDR for a ROS 2 recording, ROS 1 for a `.bag`.
/// The message types, and everything read out of them, are the same either way.
///
/// The three-way return is what [`super::cdr::BodyDecodeAccum`] records. Each decoder here is
/// strict — it yields a reading only once the body's own invariants prove it is the message it
/// claims to be — and the failures have to be counted, not dropped: everything derived from the
/// bodies is otherwise summarized from whichever ones survived the recording and reported as a
/// property of the stream.
pub(crate) fn decode_body(
    t: &mut BodyTargets<'_>,
    enc: Encoding,
    ros_type: &str,
    topic: &str,
    data: &[u8],
    ts: i64,
) -> Option<bool> {
    if super::mcap::schema_is(ros_type, "PointCloud2") {
        if t.point_fields.is_none() {
            *t.point_fields = super::cdr::decode_point_cloud2_fields(data, enc);
        }
        // Per message, unlike the layout above: the layout is a property of the stream and the
        // first message settles it, while whether a sweep held any points is a property of each
        // message and only the messages can settle it.
        match super::cdr::decode_point_cloud2_point_count(data, enc) {
            Some(n) => {
                t.point_counts.observe(n);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Twist")
        || super::mcap::schema_is(ros_type, "TwistStamped")
    {
        // A mobile base's action channel. `/cmd_vel` is to a base what `/joint_states` is to an arm,
        // and a commanded velocity pinned at its rail is exactly what the statistical family exists
        // to catch on an actuator.
        match super::cdr::decode_twist_values(
            data,
            enc,
            super::mcap::schema_is(ros_type, "TwistStamped"),
        ) {
            Some(values) => {
                t.values.push_fixed(&values, &super::cdr::TWIST_DIM_NAMES);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Wrench")
        || super::mcap::schema_is(ros_type, "WrenchStamped")
    {
        // A manipulation recording's contact channel, and the same shape as a `Twist`.
        match super::cdr::decode_wrench_values(
            data,
            enc,
            super::mcap::schema_is(ros_type, "WrenchStamped"),
        ) {
            Some(values) => {
                t.values.push_fixed(&values, &super::cdr::WRENCH_DIM_NAMES);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "MagneticField") {
        // The third instrument in the IMU package, and the one heading is estimated from.
        match super::cdr::decode_magnetic_field(data, enc) {
            Some(values) => {
                t.values
                    .push_fixed(&values, &super::cdr::MAGNETIC_FIELD_DIM_NAMES);
                Some(true)
            }
            None => Some(false),
        }
    } else if let Some(name) = super::cdr::scalar_measurement_name(ros_type) {
        // Four schemas, one layout: a header, the reading, and its variance.
        match super::cdr::decode_scalar_measurement(data, enc) {
            Some(value) => {
                t.values.push_fixed(&[Some(value)], &[name]);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Range") {
        // A reading the rangefinder's own window disowns is "nothing there", not a distance.
        match super::cdr::decode_range_value(data, enc) {
            Some(value) => {
                t.values.push_fixed(&[value], &["range"]);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "LaserScan") {
        // A planar scanner's returns feed the same density summary a 3-D cloud's points do: the
        // fault is the same one, and a `LaserScan` is what most mobile robots publish.
        match super::cdr::decode_laser_scan_returns(data, enc) {
            Some(n) => {
                t.point_counts.observe(n);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "CompressedImage") {
        // Most real bags record their cameras compressed, and without this a
        // `/camera/image_raw/compressed` topic went unmeasured while the raw topic beside it was
        // graded — the same dead camera caught on one spelling of the topic and not the other.
        // Only the codec's own frame header is read; no pixel is decoded.
        match super::cdr::decode_compressed_image_dimensions(data, enc) {
            Some(Some((w, h))) => {
                t.image_dims.observe(w, h);
                Some(true)
            }
            // A `CompressedImage` in a codec this reader has no header parser for. Nothing was
            // tried, so this is not a body that failed — it is a schema with no decoder, and the
            // image rules abstain on the stream out loud.
            Some(None) => None,
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Image") {
        // The camera counterpart of the point count above, and there for the same fault: a driver
        // that lost its sensor keeps publishing well-formed frames at its configured rate with no
        // pixels in them. Read from the message's own `height`/`width`; the pixel blob is never
        // opened.
        match super::cdr::decode_image_dimensions(data, enc) {
            Some((w, h)) => {
                t.image_dims.observe(w, h);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "CameraInfo") {
        match super::cdr::decode_camera_info(data, enc, topic) {
            Some(ci) => {
                // First successfully-decoded intrinsics per camera topic wins.
                t.intrinsics.entry(topic.to_string()).or_insert(ci);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Odometry") {
        match super::cdr::decode_odometry(data, enc) {
            Some(sample) => {
                t.ego_poses.push(EgoPose {
                    ts,
                    pose: sample.pose,
                });
                if t.ego_frame.is_none() {
                    *t.ego_frame = sample.child_frame;
                }
                // The ego's own velocity, where the message carries it: a vehicle's speed and yaw
                // rate are measurements, and this stream had none to grade.
                if let Some(twist) = sample.twist {
                    t.values.push_fixed(&twist, &super::cdr::TWIST_DIM_NAMES);
                }
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "JointState") {
        // The one message whose entire payload is the measurement: a handful of joint angles.
        // Measuring them is what lets the statistical family grade an arm recorded to a bag.
        match super::cdr::decode_joint_state(data, enc) {
            Some(sample) => {
                t.values.push_joint_state(sample);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "Imu") {
        // Thirty-seven doubles and no bulk payload: an IMU message is entirely its own
        // measurement. Slots a `-1` covariance declares absent are held out, not read as zeros.
        match super::cdr::decode_imu_values(data, enc) {
            Some(values) => {
                t.values.push_fixed(&values, &super::cdr::IMU_DIM_NAMES);
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "NavSatFix") {
        // The last AV message body that went unread. A GNSS stream was fingerprinted rather than
        // measured, so a receiver frozen at one fix, publishing NaNs, or railed at a coordinate
        // limit reported nothing — while the same faults on the IMU beside it were caught. A
        // message declaring no fix carries fields the driver left behind, not a position.
        match super::cdr::decode_nav_sat_fix(data, enc) {
            Some(sample) => {
                t.fix_availability.observe(&sample);
                if let super::cdr::NavSatSample::Fix(values) = sample {
                    t.values
                        .push_fixed(&values, &super::cdr::NAV_SAT_FIX_DIM_NAMES);
                }
                Some(true)
            }
            None => Some(false),
        }
    } else if super::mcap::schema_is(ros_type, "TFMessage") {
        match super::cdr::decode_tf_message(data, enc) {
            Some(edges) => {
                for edge in edges {
                    super::cdr::insert_transform(t.transforms, t.moved_frames, edge);
                }
                Some(true)
            }
            None => Some(false),
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One field of a ROS message, written independently of how it is encoded.
    ///
    /// The point of the tests below is that a decoder reads the *same message* out of either
    /// encoding, so each fixture is described once and rendered twice. Writing the two by hand would
    /// let a fixture drift into describing two different messages, which is the one way this test
    /// could pass while the claim it guards is false.
    enum F<'a> {
        U8(u8),
        U32(u32),
        F32(f32),
        F64(f64),
        Str(&'a str),
        /// A `std_msgs/Header`: the ROS 1 `seq` where there is one, the stamp, and the frame.
        Header(&'a str),
        /// `n` bytes of payload — a cloud's points, an image's pixels. Never interpreted.
        Bytes(usize),
    }

    /// Render one message in `enc`.
    ///
    /// CDR aligns every primitive to its own size from the start of the body, counts a string's NUL
    /// in its length, and opens with a four-byte encapsulation header. ROS 1 does none of that and
    /// puts a `uint32 seq` in front of every header. Both are written here from the specifications,
    /// not from the reader.
    fn render(fields: &[F], enc: Encoding) -> Vec<u8> {
        let mut buf: Vec<u8> = match enc {
            Encoding::Cdr => vec![0x00, 0x01, 0x00, 0x00],
            Encoding::Ros1 => Vec::new(),
        };
        let origin = buf.len();
        let align = |buf: &mut Vec<u8>, n: usize| {
            if enc == Encoding::Cdr {
                while (buf.len() - origin) % n != 0 {
                    buf.push(0);
                }
            }
        };
        let put = |buf: &mut Vec<u8>, f: &F| match f {
            F::U8(v) => buf.push(*v),
            F::U32(v) => {
                align(buf, 4);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            F::F32(v) => {
                align(buf, 4);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            F::F64(v) => {
                align(buf, 8);
                buf.extend_from_slice(&v.to_le_bytes());
            }
            F::Str(s) => {
                align(buf, 4);
                let len = match enc {
                    Encoding::Cdr => s.len() + 1,
                    Encoding::Ros1 => s.len(),
                };
                buf.extend_from_slice(&(len as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
                if enc == Encoding::Cdr {
                    buf.push(0);
                }
            }
            F::Header(frame) => {
                if enc == Encoding::Ros1 {
                    buf.extend_from_slice(&7u32.to_le_bytes()); // seq
                }
                align(buf, 4);
                buf.extend_from_slice(&1_767_225_600u32.to_le_bytes()); // stamp.sec
                align(buf, 4);
                buf.extend_from_slice(&250_000_000u32.to_le_bytes()); // stamp.nanosec
                let len = match enc {
                    Encoding::Cdr => frame.len() + 1,
                    Encoding::Ros1 => frame.len(),
                };
                align(buf, 4);
                buf.extend_from_slice(&(len as u32).to_le_bytes());
                buf.extend_from_slice(frame.as_bytes());
                if enc == Encoding::Cdr {
                    buf.push(0);
                }
            }
            F::Bytes(n) => buf.extend(std::iter::repeat_n(0x5au8, *n)),
        };
        for f in fields {
            put(&mut buf, f);
        }
        buf
    }

    /// Everything one body contributed, as text — the comparison the parity assertion makes.
    #[derive(Default)]
    struct Sink {
        point_fields: Option<Vec<PointField>>,
        point_counts: super::super::cdr::PointCountAccum,
        image_dims: super::super::cdr::ImageDimAccum,
        fix_availability: super::super::cdr::FixAvailabilityAccum,
        values: super::super::stats::StreamValues,
        ego_poses: Vec<EgoPose>,
        ego_frame: Option<String>,
        intrinsics: BTreeMap<String, CameraIntrinsics>,
        transforms: BTreeMap<(String, String), Transform>,
        moved_frames: super::super::cdr::MovingFrames,
    }

    impl Sink {
        fn decode(&mut self, ros_type: &str, data: &[u8], enc: Encoding) -> Option<bool> {
            decode_body(
                &mut BodyTargets {
                    point_fields: &mut self.point_fields,
                    point_counts: &mut self.point_counts,
                    image_dims: &mut self.image_dims,
                    fix_availability: &mut self.fix_availability,
                    values: &mut self.values,
                    ego_poses: &mut self.ego_poses,
                    ego_frame: &mut self.ego_frame,
                    intrinsics: &mut self.intrinsics,
                    transforms: &mut self.transforms,
                    moved_frames: &mut self.moved_frames,
                },
                enc,
                ros_type,
                "/topic",
                data,
                1_767_225_600_250_000_000,
            )
        }

        /// What the body left behind, rendered so two runs can be compared exactly.
        fn summary(self) -> String {
            let measured = self.values.finish();
            format!(
                "fields={:?} points={:?} images={:?} fix={:?} values={:?} poses={:?} frame={:?} intrinsics={:?} transforms={:?}",
                self.point_fields,
                self.point_counts.finish(),
                self.image_dims.finish(),
                self.fix_availability.finish(),
                measured.map(|(a, names)| (a.stats(), a.dim_stats(), names)),
                self.ego_poses,
                self.ego_frame,
                self.intrinsics,
                self.transforms,
            )
        }
    }

    /// A `sensor_msgs/PointCloud2` of one `float32 x` field and `width` points.
    fn point_cloud(width: u32) -> Vec<F<'static>> {
        let mut f = vec![
            F::Header("lidar_top"),
            F::U32(1),
            F::U32(width),
            F::U32(1),
            F::Str("x"),
            F::U32(0),
            F::U8(7),
            F::U32(1),
            F::U8(0),
            F::U32(4),
            F::U32(4 * width),
            F::U32(4 * width),
        ];
        f.push(F::Bytes(4 * width as usize));
        f.push(F::U8(1));
        f
    }

    /// Every message type the dispatch decodes, each as one body described once.
    ///
    /// A schema missing from here is a decoder nothing holds to the parity claim, which is how a
    /// reader that quietly depends on one encoding's field offsets would get in.
    fn fixtures() -> Vec<(&'static str, Vec<F<'static>>)> {
        let covariance = |n: usize| std::iter::repeat_with(|| F::F64(0.0)).take(n);
        vec![
            ("sensor_msgs/PointCloud2", point_cloud(4)),
            (
                "geometry_msgs/Twist",
                vec![
                    F::F64(1.5),
                    F::F64(0.0),
                    F::F64(0.0),
                    F::F64(0.0),
                    F::F64(0.0),
                    F::F64(0.2),
                ],
            ),
            (
                "geometry_msgs/TwistStamped",
                std::iter::once(F::Header("base_link"))
                    .chain([1.5, 0.0, 0.0, 0.0, 0.0, 0.2].map(F::F64))
                    .collect(),
            ),
            (
                "geometry_msgs/Wrench",
                vec![
                    F::F64(3.0),
                    F::F64(0.0),
                    F::F64(-9.0),
                    F::F64(0.0),
                    F::F64(0.1),
                    F::F64(0.0),
                ],
            ),
            (
                "geometry_msgs/WrenchStamped",
                std::iter::once(F::Header("ft_sensor"))
                    .chain([3.0, 0.0, -9.0, 0.0, 0.1, 0.0].map(F::F64))
                    .collect(),
            ),
            (
                "sensor_msgs/MagneticField",
                std::iter::once(F::Header("imu_link"))
                    .chain([2.1e-5, -1.4e-5, 4.6e-5].map(F::F64))
                    .chain(covariance(9))
                    .collect(),
            ),
            (
                "sensor_msgs/Temperature",
                vec![F::Header("probe"), F::F64(21.5), F::F64(0.01)],
            ),
            (
                "sensor_msgs/FluidPressure",
                vec![F::Header("baro"), F::F64(101_325.0), F::F64(1.0)],
            ),
            (
                "sensor_msgs/Range",
                vec![
                    F::Header("sonar"),
                    F::U8(0),
                    F::F32(0.5),
                    F::F32(0.2),
                    F::F32(4.0),
                    F::F32(1.5),
                ],
            ),
            (
                "sensor_msgs/LaserScan",
                vec![
                    F::Header("laser"),
                    F::F32(-1.5),
                    F::F32(1.5),
                    F::F32(0.01),
                    F::F32(0.0),
                    F::F32(0.1),
                    F::F32(0.1),
                    F::F32(30.0),
                    F::U32(3),
                    F::F32(1.0),
                    F::F32(2.0),
                    F::F32(f32::INFINITY),
                    F::U32(0),
                ],
            ),
            (
                "sensor_msgs/CompressedImage",
                vec![
                    F::Header("camera_front"),
                    F::Str("rgb8; jpeg compressed bgr8"),
                    F::U32(0),
                ],
            ),
            (
                "sensor_msgs/Image",
                vec![
                    F::Header("camera_front"),
                    F::U32(48),
                    F::U32(64),
                    F::Str("rgb8"),
                    F::U8(0),
                    F::U32(192),
                    F::U32(192 * 48),
                    F::Bytes(192 * 48),
                ],
            ),
            (
                "sensor_msgs/CameraInfo",
                std::iter::once(F::Header("camera_front"))
                    .chain([F::U32(48), F::U32(64), F::Str("plumb_bob"), F::U32(5)])
                    .chain(covariance(5))
                    .chain([640.0, 0.0, 32.0, 0.0, 640.0, 24.0, 0.0, 0.0, 1.0].map(F::F64))
                    .collect(),
            ),
            (
                "nav_msgs/Odometry",
                std::iter::once(F::Header("odom"))
                    .chain([F::Str("base_link")])
                    .chain([2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0].map(F::F64))
                    .chain(covariance(36))
                    .chain([1.5, 0.0, 0.0, 0.0, 0.0, 0.05].map(F::F64))
                    .collect(),
            ),
            (
                "sensor_msgs/JointState",
                vec![
                    F::Header("base_link"),
                    F::U32(2),
                    F::Str("head_pan"),
                    F::Str("head_tilt"),
                    F::U32(2),
                    F::F64(0.4),
                    F::F64(-0.2),
                    F::U32(0),
                    F::U32(0),
                ],
            ),
            (
                "sensor_msgs/Imu",
                std::iter::once(F::Header("imu_link"))
                    .chain([0.0, 0.0, 0.0, 1.0].map(F::F64))
                    .chain(covariance(9))
                    .chain([0.1, 0.2, 0.3].map(F::F64))
                    .chain(covariance(9))
                    .chain([0.0, 0.0, 9.81].map(F::F64))
                    .chain(covariance(9))
                    .collect(),
            ),
            (
                "sensor_msgs/NavSatFix",
                vec![
                    F::Header("gnss"),
                    F::U8(0),
                    F::U8(0),
                    F::U8(1),
                    F::F64(37.4),
                    F::F64(-122.1),
                    F::F64(30.0),
                ],
            ),
            (
                "tf2_msgs/TFMessage",
                std::iter::once(F::U32(1))
                    .chain([F::Header("base_link"), F::Str("lidar_top")])
                    .chain([0.0, 0.0, 1.6, 0.0, 0.0, 0.0, 1.0].map(F::F64))
                    .collect(),
            ),
        ]
    }

    #[test]
    fn every_message_reads_the_same_in_both_encodings() {
        // The neutrality claim at the level it is actually made: which generation of ROS recorded a
        // rig must not change what Veridex sees on it. A decoder that reached a field through one
        // encoding's offsets would pass every test written against that encoding alone.
        for (ros_type, fields) in fixtures() {
            let mut as_cdr = Sink::default();
            let cdr = as_cdr.decode(ros_type, &render(&fields, Encoding::Cdr), Encoding::Cdr);
            let mut as_ros1 = Sink::default();
            let ros1 = as_ros1.decode(ros_type, &render(&fields, Encoding::Ros1), Encoding::Ros1);

            assert_eq!(
                cdr, ros1,
                "{ros_type}: the two encodings disagree about whether the body decoded"
            );
            assert_eq!(
                cdr,
                Some(true),
                "{ros_type}: the fixture does not decode at all, so it proves nothing"
            );
            assert_eq!(
                as_cdr.summary(),
                as_ros1.summary(),
                "{ros_type}: the same message yields a different CDM depending on its encoding"
            );
        }
    }

    #[test]
    fn a_body_in_the_other_encoding_is_not_read_as_a_reading() {
        // The other half of the claim: the encodings really are different, so reading a body the
        // wrong way must not quietly produce the right answer. Where a wrong read still decodes —
        // a run of doubles satisfies few invariants — it must at least not agree with the truth.
        for (ros_type, fields) in fixtures() {
            let mut truth = Sink::default();
            truth.decode(ros_type, &render(&fields, Encoding::Ros1), Encoding::Ros1);
            let truth = truth.summary();

            let mut wrong = Sink::default();
            wrong.decode(ros_type, &render(&fields, Encoding::Ros1), Encoding::Cdr);
            assert_ne!(
                wrong.summary(),
                truth,
                "{ros_type}: a ROS 1 body read as CDR produced the right answer, so this fixture \
                 cannot tell the two encodings apart — give it a frame name whose length forces \
                 CDR padding"
            );
        }
    }
}
