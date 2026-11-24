//! The demo KITTI raw drive: a sensor rig recorded as a directory per sensor.
//!
//! Written from the layout the KITTI raw dev kit publishes — a date directory holding the
//! calibration text files and one drive directory per recording, each sensor with its own
//! `timestamps.txt` and a `data/` folder of one file per sample — rather than from Veridex's reader,
//! for the reason every generator here is: a fixture that shared the reader's idea of the format
//! could not catch the reader being wrong about it.
//!
//! [`VARIANTS`]:
//!
//! - (default) `rig` — a four-camera, LiDAR and GPS/IMU drive of 20 samples: cameras at ~10 Hz,
//!   the LiDAR at ~10 Hz, the OXTS at ~10 Hz, all on one time base, with real intrinsics, a
//!   connected transform tree (`image_00 -> velodyne_points -> oxts`, plus the three other cameras
//!   off `image_00`), full sweeps and a plausible trajectory. Clean.
//! - `dead-lidar` — the same drive whose Velodyne files are all empty: every sweep is a
//!   well-formed file of zero points, on time, in the right frame, so the structural and temporal
//!   families pass → `AUTONOMY.POINT_CLOUD_EMPTY`.
//! - `unsynced` — the same drive whose LiDAR timestamps span only two thirds of the others', so the
//!   sensors drift apart on one shared clock → `AUTONOMY.RIG_SYNC`. Rig-wide rather than the
//!   pairwise `TEMPORAL.CLOCK_SKEW`, because the OXTS unit's three records (position, pose and
//!   inertial) plus the Velodyne put this drive over the rig threshold — one drifting sensor is
//!   then one finding about the rig instead of one per pair it is in.
//! - `null-island` — the same drive whose OXTS rows report latitude and longitude of exactly zero:
//!   a receiver that never acquired a fix, reported the way an unconfigured one reports it →
//!   `AUTONOMY.GNSS_UNSET`.
//! - `no-fix` — the same drive whose OXTS rows report **zero satellites tracked** for four fifths of
//!   it: a receiver that lost the sky under a bridge or in a tunnel, which the row states in
//!   `numsats` while still reporting a blended INS position. Every row arrives on time and the
//!   positions are plausible, so the stream's frame count, cadence and span are a healthy
//!   receiver's → `AUTONOMY.GNSS_NO_FIX`.
//! - `missing-samples` — the same drive whose `image_00/data/` holds five fewer files than its
//!   `timestamps.txt` states: a part-uploaded drive. The frames are the samples the manifest and the
//!   directory agree on, and the difference is disclosed as `COVERAGE.SOURCE_UNREAD` — so the run
//!   says which part it speaks for. That camera then genuinely ends before the others, which
//!   `TEMPORAL.END_OFFSET` reports too, and both facts are true.
//!
//! - `shrinking-camera` — the same drive whose `image_02` is re-encoded at half size half-way
//!   through: 1392x512 for the first ten samples and 696x256 for the rest. Every frame is present,
//!   on time and in the right frame, and the calibration still states the original size — so the
//!   presence, cadence and frame-count rules all pass while the intrinsics are wrong for half the
//!   recording → `AUTONOMY.IMAGE_RESIZED`.
//!
//! - `wrong-resolution` — the same drive whose `image_02` is recorded at half size throughout,
//!   while `calib_cam_to_cam.txt` still states the full `S_02`. The stream is consistent with
//!   itself, so nothing about it changed; the calibration is present, connected and finite, so
//!   nothing about it is unusable; and `cx = 690` sits well inside the 1392-wide image the
//!   calibration declares, so the principal-point rule is satisfied. Only the two sizes compared
//!   against each other show it → `AUTONOMY.CALIBRATION_RESOLUTION`.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_kitti -- <output-dir> [rig|dead-lidar|unsynced|null-island|no-fix|missing-samples|shrinking-camera|wrong-resolution]`
//!
//! Then: `veridex check <output-dir>/2011_09_26/2011_09_26_drive_0005_sync`.

use std::fs;
use std::path::Path;

use crate::DemoError;

/// Every variant [`write`] accepts. `rig` is the default the docs show.
pub const VARIANTS: &[&str] = &[
    "rig",
    "dead-lidar",
    "unsynced",
    "null-island",
    "no-fix",
    "missing-samples",
    "shrinking-camera",
    "wrong-resolution",
];

/// The date directory a drive sits under, and the drive's own name — the shape KITTI publishes.
const DATE: &str = "2011_09_26";
const DRIVE: &str = "2011_09_26_drive_0005_sync";

/// Samples per sensor. Small on purpose: every sample is a file, and the point of the fixture is the
/// layout rather than the volume.
const SAMPLES: usize = 20;

/// Points in one demo sweep.
const POINTS: usize = 64;

/// The image size KITTI's `calib_cam_to_cam.txt` states in `S_0N`, and the one the camera samples
/// declare in their own `IHDR`. The two agreeing is what a clean drive looks like.
const IMAGE_WIDTH: u32 = 1392;
const IMAGE_HEIGHT: u32 = 512;

/// A PNG signature and `IHDR` for `width` x `height` — the header a reader probes, with no pixel
/// data behind it, because Veridex never decodes one.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&13u32.to_be_bytes()); // IHDR length
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&width.to_be_bytes());
    v.extend_from_slice(&height.to_be_bytes());
    // Bit depth 8, colour type 2 (truecolour), default compression, filter and interlace.
    v.extend_from_slice(&[8, 2, 0, 0, 0]);
    v
}

/// Write the demo KITTI raw drive under `dir`, replacing anything already there.
///
/// Returns the drive directory, which is the dataset a reader points Veridex at.
pub fn write(dir: &Path, variant: &str) -> Result<(), DemoError> {
    crate::check_variant(variant, VARIANTS)?;
    let dead_lidar = variant == "dead-lidar";
    let unsynced = variant == "unsynced";
    let null_island = variant == "null-island";
    let no_fix = variant == "no-fix";
    let missing_samples = variant == "missing-samples";
    let shrinking_camera = variant == "shrinking-camera";
    let wrong_resolution = variant == "wrong-resolution";

    crate::fresh_dir(dir)?;
    let date_dir = dir.join(DATE);
    let drive = date_dir.join(DRIVE);
    for sensor in [
        "image_00",
        "image_01",
        "image_02",
        "image_03",
        "velodyne_points",
        "oxts",
    ] {
        fs::create_dir_all(drive.join(sensor).join("data")).map_err(DemoError::Io)?;
    }

    // One time base for the whole rig, as KITTI records it: a naive civil time with nanoseconds.
    // The LiDAR's span is compressed in `unsynced`, which is a drift on a shared clock.
    let stamp = |sample: usize, interval_ns: u64| -> String {
        let base_ns = 13 * 3_600 * 1_000_000_000u64 + 2 * 60 * 1_000_000_000u64 + 25_000_000_000u64;
        let ns = base_ns + sample as u64 * interval_ns;
        let secs = ns / 1_000_000_000;
        let frac = ns % 1_000_000_000;
        format!(
            "{DATE_DASHED} {:02}:{:02}:{:02}.{:09}",
            secs / 3_600,
            (secs % 3_600) / 60,
            secs % 60,
            frac
        )
    };
    const DATE_DASHED: &str = "2011-09-26";

    for sensor in ["image_00", "image_01", "image_02", "image_03"] {
        let mut lines = String::new();
        for i in 0..SAMPLES {
            lines.push_str(&stamp(i, 100_000_000));
            lines.push('\n');
        }
        fs::write(drive.join(sensor).join("timestamps.txt"), lines).map_err(DemoError::Io)?;
        // A camera sample is a PNG, and its `IHDR` carries the real thing: the 1392x512 the
        // calibration's `S_0N` was computed for, which is what makes the image rules answer on this
        // format instead of abstaining. The payload behind the header varies per sample, so frames
        // are content-distinct the way a real recording's are — Veridex fingerprints a camera file
        // and decodes no pixel of it.
        let written = if missing_samples && sensor == "image_00" {
            SAMPLES - 5
        } else {
            SAMPLES
        };
        for i in 0..written {
            // `shrinking-camera` re-encodes the second half of `image_02` at half size: a
            // resolution that changed part-way through one recording, which every presence,
            // cadence and frame-count test passes.
            let (w, h) = if shrinking_camera && sensor == "image_02" && i * 2 >= SAMPLES {
                (IMAGE_WIDTH / 2, IMAGE_HEIGHT / 2)
            } else if wrong_resolution && sensor == "image_02" {
                // Every sample at half size, while the calibration still states the full one: a rig
                // recording downscaled against intrinsics nobody re-computed.
                (IMAGE_WIDTH / 2, IMAGE_HEIGHT / 2)
            } else {
                (IMAGE_WIDTH, IMAGE_HEIGHT)
            };
            let mut bytes = png_header(w, h);
            bytes.extend_from_slice(&(i as u32).to_le_bytes());
            bytes.resize(96, i as u8);
            fs::write(
                drive.join(sensor).join("data").join(format!("{i:010}.png")),
                bytes,
            )
            .map_err(DemoError::Io)?;
        }
    }

    // The LiDAR. `dead-lidar` writes every sweep empty — a well-formed file of no points, which is
    // what a driver that lost its sensor leaves behind.
    let lidar_interval = if unsynced { 66_000_000 } else { 100_000_000 };
    let mut lines = String::new();
    for i in 0..SAMPLES {
        lines.push_str(&stamp(i, lidar_interval));
        lines.push('\n');
    }
    fs::write(drive.join("velodyne_points").join("timestamps.txt"), lines)
        .map_err(DemoError::Io)?;
    for i in 0..SAMPLES {
        // `float32 [x, y, z, reflectance]` per point, which is what makes the count readable from
        // the file's length alone.
        let mut bytes = Vec::new();
        if !dead_lidar {
            for p in 0..POINTS {
                let angle = (p as f32) * 0.1 + (i as f32) * 0.01;
                for v in [angle.cos() * 10.0, angle.sin() * 10.0, 0.2, 0.5] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
        }
        fs::write(
            drive
                .join("velodyne_points")
                .join("data")
                .join(format!("{i:010}.bin")),
            bytes,
        )
        .map_err(DemoError::Io)?;
    }

    // The GPS/IMU. Thirty whitespace-separated fields per sample, as `dataformat.txt` defines them:
    // the position first, then the orientation and velocities the navigation solution reports, then
    // the inertial measurements, then the accuracy and mode flags.
    let mut lines = String::new();
    for i in 0..SAMPLES {
        lines.push_str(&stamp(i, 100_000_000));
        lines.push('\n');
    }
    fs::write(drive.join("oxts").join("timestamps.txt"), lines).map_err(DemoError::Io)?;
    fs::write(
        drive.join("oxts").join("dataformat.txt"),
        "lat:   latitude of the oxts-unit (deg)\nlon:   longitude of the oxts-unit (deg)\n\
         alt:   altitude of the oxts-unit (m)\nroll:  roll angle (rad)\npitch: pitch angle (rad)\n\
         yaw:   heading (rad)\nvf:    forward velocity (m/s)\nvl:    leftward velocity (m/s)\n\
         vu:    upward velocity (m/s)\nax:    acceleration in x, wrt. the vehicle frame (m/s^2)\n\
         ay:    acceleration in y, wrt. the vehicle frame (m/s^2)\n\
         az:    acceleration in z, wrt. the vehicle frame (m/s^2)\n\
         wx:    angular rate around x (rad/s)\nwy:    angular rate around y (rad/s)\n\
         wz:    angular rate around z (rad/s)\nnumsats: number of satellites tracked\n",
    )
    .map_err(DemoError::Io)?;
    for i in 0..SAMPLES {
        let (lat, lon, alt) = if null_island {
            (0.0, 0.0, 0.0)
        } else {
            (49.015 + i as f64 * 9.0e-6, 8.434, 112.0)
        };
        // A vehicle driving gently straight ahead: a small steady acceleration, gravity on the
        // down axis, a yaw rate that eases off as it comes out of a bend, and a heading that turns
        // with it. All of it rational arithmetic in the sample index — no `sin`, whose last bit is
        // the platform's libm's business and would make this fixture hash differently on two
        // machines. Only a real profile is worth writing: a column of zeros is a dead sensor, and
        // the statistical family would be right to say so.
        let t = i as f64;
        let yaw_rate = 0.12 - t * 0.004;
        let yaw = 1.42 + t * 0.012;
        let forward = 8.4 + t * 0.05;
        let mut fields: Vec<String> = vec![
            format!("{lat:.12}"),
            format!("{lon:.12}"),
            format!("{alt:.6}"),
            // roll, pitch, yaw
            format!("{:.9}", 0.004 + t * 0.0002),
            format!("{:.9}", -0.011 + t * 0.0001),
            format!("{yaw:.9}"),
            // vn, ve — the same motion in map coordinates, which this reader leaves unmapped
            format!("{:.9}", forward * 0.98),
            format!("{:.9}", forward * 0.19),
            // vf, vl, vu — the body-frame velocity triple
            format!("{forward:.9}"),
            format!("{:.9}", 0.06 - t * 0.002),
            format!("{:.9}", 0.01),
            // ax, ay, az — vehicle frame, gravity included on the vertical axis
            format!("{:.9}", 0.5 + t * 0.01),
            format!("{:.9}", forward * yaw_rate),
            format!("{:.9}", 9.81 + t * 0.001),
            // af, al, au — the same accelerations forward/left/up, left unmapped
            format!("{:.9}", 0.5 + t * 0.01),
            format!("{:.9}", forward * yaw_rate),
            format!("{:.9}", 9.81),
            // wx, wy, wz — angular rates in the vehicle frame
            format!("{:.9}", 0.002),
            format!("{:.9}", -0.003),
            format!("{yaw_rate:.9}"),
            // wf, wl, wu — the same rates forward/left/up, left unmapped
            format!("{:.9}", 0.002),
            format!("{:.9}", -0.003),
            format!("{yaw_rate:.9}"),
        ];
        // The two accuracy estimates, present because the format defines thirty fields and a row of
        // another width is not an OXTS record.
        for _ in 23..25 {
            fields.push("0.030000".into());
        }
        // The five integer mode fields the format ends with: navstat, numsats, posmode, velmode,
        // orimode. `no-fix` sets `numsats` to zero for four fifths of the drive — the receiver
        // saying it tracked no satellite, while the row still reports a position.
        let satellites = if no_fix && i * 5 >= SAMPLES {
            "0"
        } else {
            "11"
        };
        for v in ["4", satellites, "5", "5", "5"] {
            fields.push(v.into());
        }
        fs::write(
            drive.join("oxts").join("data").join(format!("{i:010}.txt")),
            format!("{}\n", fields.join(" ")),
        )
        .map_err(DemoError::Io)?;
    }

    write_calibration(&date_dir)?;
    Ok(())
}

/// The three calibration files, in the date directory where KITTI keeps them.
fn write_calibration(date_dir: &Path) -> Result<(), DemoError> {
    // `S_0N` is the image size, `K_0N` the 3x3 intrinsic matrix, `D_0N` the five plumb-bob
    // coefficients, and `R_0N`/`T_0N` the rotation and translation from camera 0.
    let mut cam = String::from("calib_time: 09-Jan-2012 13:57:47\ncorner_dist: 9.950000e-02\n");
    for index in 0..4 {
        cam.push_str(&format!("S_{index:02}: 1.392000e+03 5.120000e+02\n"));
        cam.push_str(&format!(
            "K_{index:02}: 9.842439e+02 0.000000e+00 6.900000e+02 0.000000e+00 9.808141e+02 \
             2.331966e+02 0.000000e+00 0.000000e+00 1.000000e+00\n"
        ));
        cam.push_str(&format!(
            "D_{index:02}: -3.728755e-01 2.037299e-01 2.219027e-03 1.383707e-03 -7.233722e-02\n"
        ));
        // Camera 0 is the reference, so its own transform is the identity; the others sit along the
        // baseline to its right.
        let tx = index as f64 * -0.06;
        cam.push_str(&format!(
            "R_{index:02}: 1.000000e+00 0.000000e+00 0.000000e+00 0.000000e+00 1.000000e+00 \
             0.000000e+00 0.000000e+00 0.000000e+00 1.000000e+00\n"
        ));
        cam.push_str(&format!(
            "T_{index:02}: {tx:.6e} 0.000000e+00 0.000000e+00\n"
        ));
    }
    fs::write(date_dir.join("calib_cam_to_cam.txt"), cam).map_err(DemoError::Io)?;

    // The two rigid files: `R` row-major 3x3 and `T` 3x1.
    fs::write(
        date_dir.join("calib_velo_to_cam.txt"),
        "calib_time: 15-Mar-2012 11:37:16\n\
         R: 1.000000e+00 0.000000e+00 0.000000e+00 0.000000e+00 1.000000e+00 0.000000e+00 \
         0.000000e+00 0.000000e+00 1.000000e+00\n\
         T: -4.069766e-03 -7.631618e-02 -2.717806e-01\n\
         delta_f: 0.000000e+00 0.000000e+00\n\
         delta_c: 0.000000e+00 0.000000e+00\n",
    )
    .map_err(DemoError::Io)?;
    fs::write(
        date_dir.join("calib_imu_to_velo.txt"),
        "calib_time: 25-May-2012 16:47:16\n\
         R: 1.000000e+00 0.000000e+00 0.000000e+00 0.000000e+00 1.000000e+00 0.000000e+00 \
         0.000000e+00 0.000000e+00 1.000000e+00\n\
         T: -8.086759e-01 3.195559e-01 -7.997231e-01\n",
    )
    .map_err(DemoError::Io)?;
    Ok(())
}

/// One line describing what `variant` produces and which finding it is meant to fire.
pub fn describe(variant: &str) -> Result<&'static str, DemoError> {
    crate::check_variant(variant, VARIANTS)?;
    Ok(match variant {
        "dead-lidar" => {
            "dead-lidar (every Velodyne sweep holds no points → AUTONOMY.POINT_CLOUD_EMPTY)"
        }
        "unsynced" => {
            "unsynced (the LiDAR spans two thirds of the other sensors → TEMPORAL.CLOCK_SKEW)"
        }
        "null-island" => "null-island (every OXTS fix is exactly (0, 0) → AUTONOMY.GNSS_UNSET)",
        "no-fix" => {
            "no-fix (the receiver tracked no satellite for four fifths of the drive → \
             AUTONOMY.GNSS_NO_FIX)"
        }
        "missing-samples" => {
            "missing-samples (image_00 holds five fewer files than its timestamps state → \
             COVERAGE.SOURCE_UNREAD, and that camera ends early → TEMPORAL.CLOCK_SKEW)"
        }
        _ => "rig (a clean four-camera, LiDAR and GPS/IMU drive)",
    })
}

/// The drive directory inside `dir` that [`write`] produces — the path a reader checks.
pub fn drive_path(dir: &Path) -> std::path::PathBuf {
    dir.join(DATE).join(DRIVE)
}
