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
