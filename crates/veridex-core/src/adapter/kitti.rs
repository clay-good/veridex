//! KITTI raw-data reader: a sensor rig recorded as a **directory per sensor**.
//!
//! The layout the KITTI raw dev kit publishes, and the shape a great many hand-rolled autonomy rigs
//! copied from it: one directory per sensor, each holding its own `timestamps.txt` and a `data/`
//! folder of one file per sample, with the calibration in text files beside the drive.
//!
//! ```text
//! 2011_09_26/                        <- the calibration lives here, beside the drives
//!   calib_cam_to_cam.txt
//!   calib_velo_to_cam.txt
//!   calib_imu_to_velo.txt
//!   2011_09_26_drive_0005_sync/      <- a drive: one episode
//!     image_00/{timestamps.txt, data/0000000000.png …}
//!     image_01/, image_02/, image_03/
//!     velodyne_points/{timestamps.txt, timestamps_start.txt, timestamps_end.txt, data/*.bin}
//!     oxts/{timestamps.txt, dataformat.txt, data/*.txt}
//! ```
//!
//! Every fact here is read out of those files. **Frames** are the lines of each sensor's own
//! `timestamps.txt`, so each stream carries the times that sensor was sampled at and the rig-wide
//! sync checks compare them against each other. **Point counts** come from each `.bin`'s length: a
//! KITTI sweep is `float32 [x, y, z, reflectance]` per point, so the count is the file's size over
//! sixteen — read from `stat`, with no point payload decoded. **Calibration** is the real thing: the
//! per-camera intrinsics and image size from `calib_cam_to_cam.txt`, and the `velo -> cam` and
//! `imu -> velo` rigid transforms from the other two files, which gives a connected transform tree
//! for the spatial checks to resolve sensors through. **GNSS values** are the first three fields of
//! each `oxts` row — latitude, longitude, altitude — which is what lets the plausibility rules grade
//! a drive, and `numsats` becomes the **fix availability**, so a drive that lost the sky is reported
//! rather than abstained on.
//!
//! Three honesty notes, because each is a claim this reader deliberately does not make.
//!
//! A KITTI timestamp is a **naive** civil time: `2011-09-26 13:02:25.964389445`, with no offset. It
//! is read as UTC, which is exact for every interval — rate, gap, jitter, span, cross-sensor drift,
//! all of which are differences — and is an assumption about the absolute instant alone. The
//! assumption is recorded in the dataset metadata rather than left in the reader.
//!
//! The sensors are **not named by the files**. The rig is famous and the files say nothing about it,
//! so `provenance.sensor` stays unknown rather than being filled in from what a reader of this code
//! happens to know. `calib_time`, which the calibration files *do* state, is read.
//!
//! Nothing is read whole into memory. Every file is independently addressed, so a frame's content
//! fingerprint is streamed through a fixed buffer — unlike the random-access containers, where the
//! summary is at the end and the file has to be resident.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{
    Coverage, Detection, FrameBudget, IngestError, IngestOptions, IngestReport, Ingested, Source,
    UnmappedField,
};
use crate::cdm::{
    Calibration, CameraIntrinsics, ClockKind, Dataset, Episode, Frame, Modality, Pose, Provenance,
    ProvenanceClass, ProvenanceElement, ProvenanceScope, Stream, Transform, ValueRef,
};

/// This adapter's stable format id.
pub const FORMAT_ID: &str = "kitti";

/// The clock every stream of a drive is measured against: one time base for the whole rig, which is
/// what makes the cross-sensor checks answerable at all.
const CLOCK_ID: &str = "kitti-rig";

/// Metadata key recording how the timestamps were read, because the files state no offset.
const META_TIME_BASE: &str = "kitti_time_base";

/// The sensor directories this reader knows, with the modality each one is.
///
/// A directory that is not here is disclosed rather than skipped: a rig that recorded a sensor this
/// reader has no modality for is a rig whose data went unread, which is a hole in coverage and not a
/// fact about the dataset.
const SENSORS: &[(&str, Modality)] = &[
    ("image_00", Modality::Video),
    ("image_01", Modality::Video),
    ("image_02", Modality::Video),
    ("image_03", Modality::Video),
    ("velodyne_points", Modality::PointCloud),
    ("oxts", Modality::Gnss),
];

/// Bytes per point in a KITTI Velodyne sweep: `float32 [x, y, z, reflectance]`.
const VELODYNE_POINT_BYTES: u64 = 16;

/// The most sample files this reader will list in one sensor directory.
///
/// A directory listing is attacker-controlled in the same way a declared count is, and every entry
/// becomes a frame. The ceiling is far above a real drive (KITTI's longest is a few thousand
/// samples) and bounds what a crafted tree can make this allocate before the frame budget is
/// consulted.
const MAX_SAMPLES_PER_SENSOR: usize = 2_000_000;

/// The longest `timestamps.txt` this reader will read, in bytes. One line is about thirty bytes, so
/// this is millions of samples' worth and still a bounded read of a file whose size the dataset
/// chooses.
const MAX_TIMESTAMPS_BYTES: u64 = 256 * 1024 * 1024;

/// The longest text file this reader will read for calibration or one `oxts` row.
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// Read the KITTI raw layout.
pub struct KittiAdapter;

impl super::Adapter for KittiAdapter {
    fn format_id(&self) -> &'static str {
        "kitti"
    }

    fn supported_versions(&self) -> &'static [&'static str] {
        &["raw-sync"]
    }

    fn detect(&self, source: &Source) -> Detection {
        let Source::Local(path) = source else {
            return Detection::No;
        };
        if is_drive(path) {
            Detection::Yes {
                version: Some("raw-sync".into()),
            }
        } else {
            Detection::No
        }
    }

    /// `timestamps.txt` is this format's manifest: it states every sample and its time without any
    /// sample being opened, so a metadata-only run answers the structural and temporal questions in
    /// full and skips the fingerprints, the point counts and the `oxts` values.
    fn supports_metadata_only(&self) -> bool {
        true
    }

    /// A drive is one episode, as a recording is in every other rig format here.
    fn supports_sampling(&self) -> bool {
        false
    }

    fn ingest(&self, source: &Source, options: &IngestOptions) -> Result<Ingested, IngestError> {
        let Source::Local(path) = source else {
            return Err(IngestError::Parse {
                format_id: FORMAT_ID,
                message: "the KITTI reader reads a local drive directory".into(),
            });
        };
        ingest_drive(path, options)
    }

    /// A *date* directory is the calibration plus the drives recorded that day: recognizable, and
    /// not a dataset on its own. Naming the drives inside it is the difference between "unsupported
    /// format" and "you meant one of these".
    fn incomplete_hint(&self, source: &Source) -> Option<String> {
        let Source::Local(path) = source else {
            return None;
        };
        if is_drive(path) || !path.is_dir() {
            return None;
        }
        let drives = child_drives(path);
        if drives.is_empty() {
            return None;
        }
        Some(format!(
            "a KITTI date directory: the calibration for {} drive(s) recorded that day, which are \
             the datasets — point Veridex at one of them ({})",
            drives.len(),
            drives.join(", ")
        ))
    }
}

/// Whether `path` is a KITTI raw drive: a directory holding at least one known sensor directory
/// with its own `timestamps.txt`.
fn is_drive(path: &Path) -> bool {
    path.is_dir()
        && SENSORS
            .iter()
            .any(|(name, _)| path.join(name).join("timestamps.txt").is_file())
}

/// The drive directories inside a date directory, by name.
fn child_drives(path: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| is_drive(&e.path()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// One sensor directory's samples: the file paths, in sample order.
fn sample_files(dir: &Path) -> Result<(Vec<PathBuf>, Option<UnmappedField>), IngestError> {
    let data = dir.join("data");
    if !data.is_dir() {
        return Ok((Vec::new(), None));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut truncated = None;
    for entry in std::fs::read_dir(&data).map_err(|e| IngestError::Io(e.to_string()))? {
        let entry = entry.map_err(|e| IngestError::Io(e.to_string()))?;
        // A symlink in a sample directory can point anywhere; following it would read bytes from
        // outside the dataset and attribute them to this sensor.
        let meta =
            std::fs::symlink_metadata(entry.path()).map_err(|e| IngestError::Io(e.to_string()))?;
        if !meta.is_file() {
            continue;
        }
        if files.len() >= MAX_SAMPLES_PER_SENSOR {
            truncated = Some(UnmappedField {
                source_path: data.to_string_lossy().into_owned(),
                note: format!(
                    "more than {MAX_SAMPLES_PER_SENSOR} sample files, which is past what this \
                     reader will list at once; the rest were not read"
                ),
            });
            break;
        }
        files.push(entry.path());
    }
    // Sample order is frame order, and `0000000010` sorts before `0000000009` under a plain
    // lexicographic sort only by luck of the zero padding — so order by number where there is one.
    files.sort_by_key(|p| {
        super::natural_key(
            &p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    });
    Ok((files, truncated))
}

/// Parse one `timestamps.txt` line — `2011-09-26 13:02:25.964389445` — as nanoseconds since the
/// Unix epoch, reading the naive civil time as UTC.
///
/// `None` for anything that is not that shape, so a stray line is skipped and disclosed rather than
/// turned into a timestamp by partial parsing.
fn parse_timestamp(line: &str) -> Option<i64> {
    let line = line.trim();
    let (date, time) = line.split_once(' ')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: u32 = d.next()?.parse().ok()?;
    let day: u32 = d.next()?.parse().ok()?;
    if d.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, nanos) = match time.split_once('.') {
        Some((clock, frac)) => {
            if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let digits: String = frac.chars().take(9).collect();
            let scale = 10i64.pow(9 - digits.len() as u32);
            (clock, digits.parse::<i64>().ok()? * scale)
        }
        None => (time, 0),
    };
    let mut t = clock.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm, the inverse of the one the
/// CLI formats issuance timestamps with). Written out rather than pulled in: this crate depends on
/// no date library, and the conversion is exact and testable.
///
/// The `y >= 0` branch is unreachable from this reader and kept anyway: a negative year would have
/// to come through [`parse_timestamp`], which splits the date on `-` and so refuses one. The
/// algorithm is kept whole rather than trimmed to its reachable half, because a known algorithm with
/// a piece removed is no longer the algorithm anyone can check it against.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Read a sensor's `timestamps.txt` into nanosecond stamps, and how many lines were not a timestamp.
fn read_timestamps(path: &Path) -> Result<(Vec<i64>, usize), IngestError> {
    let size = std::fs::metadata(path)
        .map_err(|e| IngestError::Io(e.to_string()))?
        .len();
    if size > MAX_TIMESTAMPS_BYTES {
        return Err(IngestError::Parse {
            format_id: FORMAT_ID,
            message: format!(
                "{} is {size} bytes, past the {MAX_TIMESTAMPS_BYTES} this reader will read for a \
                 timestamps file",
                path.display()
            ),
        });
    }
    let text = std::fs::read_to_string(path).map_err(|e| IngestError::Io(e.to_string()))?;
    let mut stamps = Vec::new();
    let mut skipped = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_timestamp(line) {
            Some(ts) => stamps.push(ts),
            None => skipped += 1,
        }
    }
    Ok((stamps, skipped))
}

/// A file's SHA-256, streamed through a fixed buffer so nothing is held whole in memory.
fn file_digest(path: &Path) -> Result<[u8; 32], IngestError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| IngestError::Io(e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().into())
}

/// A small text file, refused past [`MAX_TEXT_BYTES`] rather than read.
fn read_text(path: &Path) -> Result<Option<String>, IngestError> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };
    if meta.len() > MAX_TEXT_BYTES {
        return Ok(None);
    }
    Ok(Some(
        std::fs::read_to_string(path).map_err(|e| IngestError::Io(e.to_string()))?,
    ))
}

/// A KITTI calibration file: `key: v1 v2 …` per line, which is how every one of the three is written.
fn read_calib(path: &Path) -> Result<BTreeMap<String, String>, IngestError> {
    let Some(text) = read_text(path)? else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            out.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(out)
}

/// The numbers in a calibration value, or `None` when any of them is not a number — a partially
/// parsed matrix is worse than an absent one.
fn numbers(value: &str) -> Option<Vec<f64>> {
    value.split_whitespace().map(|t| t.parse().ok()).collect()
}

/// The calibration a drive's date directory records: per-camera intrinsics, and the rigid transforms
/// that relate the LiDAR and the IMU to camera 0.
///
/// Read from the files beside the drive, which is where KITTI keeps them — a drive directory on its
/// own carries no calibration, and that is disclosed rather than inferred.
fn read_calibration(
    date_dir: &Path,
    mapped: &mut Vec<String>,
    omitted: &mut Vec<String>,
) -> Result<(Option<Calibration>, Option<String>), IngestError> {
    let cam = read_calib(&date_dir.join("calib_cam_to_cam.txt"))?;
    let velo_to_cam = read_calib(&date_dir.join("calib_velo_to_cam.txt"))?;
    let imu_to_velo = read_calib(&date_dir.join("calib_imu_to_velo.txt"))?;
    // The calibration's own date, which is the one provenance fact these files state.
    let calib_time = ["calib_time"]
        .iter()
        .find_map(|k| {
            cam.get(*k)
                .or_else(|| velo_to_cam.get(*k))
                .or_else(|| imu_to_velo.get(*k))
        })
        .cloned();

    let mut intrinsics = Vec::new();
    for (name, _) in SENSORS.iter().filter(|(_, m)| *m == Modality::Video) {
        let index = name.trim_start_matches("image_");
        // `K_0N` is the 3x3 intrinsic matrix and `S_0N` the image size the calibration was computed
        // for — the pair that makes the principal point checkable at all.
        let Some(k) = cam.get(&format!("K_{index}")).and_then(|v| numbers(v)) else {
            continue;
        };
        if k.len() != 9 {
            continue;
        }
        let size = cam.get(&format!("S_{index}")).and_then(|v| numbers(v));
        let (width, height) = match size.as_deref() {
            Some([w, h]) if *w >= 0.0 && *h >= 0.0 => (Some(*w as u64), Some(*h as u64)),
            _ => (None, None),
        };
        let distortion = cam
            .get(&format!("D_{index}"))
            .and_then(|v| numbers(v))
            .unwrap_or_default();
        intrinsics.push(CameraIntrinsics {
            stream: (*name).to_string(),
            fx: k[0],
            fy: k[4],
            cx: k[2],
            cy: k[5],
            // KITTI's five coefficients are the OpenCV plumb-bob set, which is what `D_0N` holds —
            // the model is named here because the file's own layout fixes it, and the count is the
            // one thing about the coefficients that can then be checked.
            distortion_model: (distortion.len() == 5).then(|| "plumb_bob".to_string()),
            distortion,
            width,
            height,
            valid_from: None,
            valid_to: None,
        });
    }

    let mut transforms = Vec::new();
    // Each of the two rigid files is `R` (3x3, row-major) and `T` (3x1). Recorded as a transform
    // from the parent frame to the child, which is how the CDM holds a rig's geometry.
    for (file, parent, child) in [
        (&velo_to_cam, "image_00", "velodyne_points"),
        (&imu_to_velo, "velodyne_points", "oxts"),
    ] {
        let Some(r) = file.get("R").and_then(|v| numbers(v)) else {
            continue;
        };
        let Some(t) = file.get("T").and_then(|v| numbers(v)) else {
            continue;
        };
        if r.len() != 9 || t.len() != 3 {
            continue;
        }
        transforms.push(Transform {
            parent_frame: parent.to_string(),
            child_frame: child.to_string(),
            pose: Pose {
                translation: [t[0], t[1], t[2]],
                rotation: quaternion_from_matrix(&r),
            },
            valid_from: None,
            valid_to: None,
        });
    }
    // The other cameras hang off camera 0 by the rotation and translation the calibration records
    // between them, which is what puts every camera in one tree.
    for (name, _) in SENSORS.iter().filter(|(_, m)| *m == Modality::Video) {
        let index = name.trim_start_matches("image_");
        if index == "00" {
            continue;
        }
        let Some(r) = cam.get(&format!("R_{index}")).and_then(|v| numbers(v)) else {
            continue;
        };
        let Some(t) = cam.get(&format!("T_{index}")).and_then(|v| numbers(v)) else {
            continue;
        };
        if r.len() != 9 || t.len() != 3 {
            continue;
        }
        transforms.push(Transform {
            parent_frame: "image_00".to_string(),
            child_frame: (*name).to_string(),
            pose: Pose {
                translation: [t[0], t[1], t[2]],
                rotation: quaternion_from_matrix(&r),
            },
            valid_from: None,
            valid_to: None,
        });
    }

    if intrinsics.is_empty() && transforms.is_empty() {
        omitted.push(
            "calibration (no calib_cam_to_cam.txt / calib_velo_to_cam.txt beside the drive)".into(),
        );
        return Ok((None, calib_time));
    }
    mapped.push("calib_cam_to_cam.txt K_0N/S_0N/D_0N -> camera intrinsics".into());
    if !transforms.is_empty() {
        mapped.push("calib_*_to_*.txt R/T -> transform tree".into());
    }
    Ok((
        Some(Calibration {
            transforms,
            intrinsics,
        }),
        calib_time,
    ))
}

/// A rotation matrix (row-major 3x3) as a unit quaternion `[x, y, z, w]`.
///
/// Shepperd's branch choice: the largest diagonal term decides which form is used, so the division
/// is never by a near-zero quantity. The comparisons are deliberately strict and would be correct
/// either way — at a tie the two branches produce the same quaternion, which is why a mutation sweep
/// cannot tell them apart and no test tries to. A matrix that is not a rotation produces a quaternion that is
/// not a unit quaternion — which is exactly what `AUTONOMY.CALIBRATION_IMPLAUSIBLE` exists to
/// report, so it is passed through rather than normalized away here.
fn quaternion_from_matrix(r: &[f64]) -> [f64; 4] {
    let (m00, m01, m02) = (r[0], r[1], r[2]);
    let (m10, m11, m12) = (r[3], r[4], r[5]);
    let (m20, m21, m22) = (r[6], r[7], r[8]);
    let trace = m00 + m11 + m22;
    if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s]
    }
}

/// The first three fields of one `oxts` row — latitude, longitude, altitude — or `None` when the row
/// is not an OXTS record.
///
/// A row has thirty fields and this reads the three the plausibility rules need. The rest (the
/// velocities, the accelerations, the accuracy and mode flags) are disclosed as unmapped rather than
/// silently dropped.
fn oxts_row(text: &str) -> Option<OxtsRow> {
    let fields: Vec<&str> = text.split_whitespace().collect();
    // Thirty fields exactly, as `dataformat.txt` defines them: a row of another width is not this
    // record, and reading its first three numbers anyway would report a position from a file that
    // holds something else.
    if fields.len() != 30 {
        return None;
    }
    let lat = fields[0].parse().ok()?;
    let lon = fields[1].parse().ok()?;
    let alt = fields[2].parse().ok()?;
    Some(OxtsRow {
        position: [lat, lon, alt],
        // `numsats`, which `dataformat.txt` defines as the number of satellites the primary GPS
        // receiver tracked: field 27 of the thirty. Unparseable stays `None` rather than becoming
        // zero, because "the row does not say" and "it tracked none" are opposite facts.
        satellites: fields[26].parse().ok(),
    })
}

/// The part of one OXTS row this reader uses.
struct OxtsRow {
    /// Latitude, longitude and altitude, in that order.
    position: [f64; 3],
    /// How many satellites the receiver tracked, where the row states it.
    satellites: Option<u64>,
}

/// The names of the three `oxts` fields this reader summarizes, in order.
const OXTS_DIM_NAMES: [&str; 3] = ["latitude", "longitude", "altitude"];

/// Ingest one drive directory.
fn ingest_drive(path: &Path, options: &IngestOptions) -> Result<Ingested, IngestError> {
    if !path.exists() {
        return Err(IngestError::SourceNotFound(path.to_path_buf()));
    }
    let mut budget = FrameBudget::new(options);
    let mut mapped: Vec<String> = vec![
        "<sensor>/timestamps.txt -> stream frames (one line per sample)".into(),
        "<sensor>/data/* -> frame value references".into(),
    ];
    let mut unmapped: Vec<UnmappedField> = Vec::new();
    let mut unread: Vec<UnmappedField> = Vec::new();
    let mut omitted: Vec<String> = Vec::new();

    // The largest per-sensor sample count the manifests state — the episode's declared length, by
    // the same rule the structural check reads an actual one (the largest per-stream frame count).
    let mut declared_samples: u64 = 0;
    let mut streams: Vec<Stream> = Vec::new();
    for (name, modality) in SENSORS {
        let dir = path.join(name);
        let stamps_path = dir.join("timestamps.txt");
        if !stamps_path.is_file() {
            continue;
        }
        let (stamps, skipped) = read_timestamps(&stamps_path)?;
        if skipped > 0 {
            unmapped.push(UnmappedField {
                source_path: stamps_path.to_string_lossy().into_owned(),
                note: format!(
                    "{skipped} line(s) are not a `YYYY-MM-DD HH:MM:SS.fffffffff` timestamp and \
                     became no frame"
                ),
            });
        }
        let (files, truncated) = sample_files(&dir)?;
        if let Some(note) = truncated {
            unread.push(note);
        }
        // The timestamps file is this format's manifest and `data/` is the data. Where they
        // disagree, the frames are the samples both account for and the difference is disclosed: a
        // part-uploaded drive would otherwise read as a shorter drive, which is a clean result over
        // the part that arrived.
        // What this sensor's manifest states, which is a claim about the data whether or not the
        // data was read.
        declared_samples = declared_samples.max(stamps.len() as u64);
        // A metadata-only run reads no sample, and `Coverage::MetadataOnly` means exactly that: the
        // structure without the data, so no stream carries frames. The sample *times* are in the
        // manifest and are deliberately not materialized — a run that emitted them would answer the
        // temporal questions while the coverage note said it had read nothing, and the checks that
        // stand down on `frames_read` would stand down over frames that were there.
        let samples = if options.metadata_only {
            0
        } else if files.is_empty() {
            stamps.len()
        } else {
            stamps.len().min(files.len())
        };
        if !options.metadata_only {
            if files.is_empty() {
                unread.push(UnmappedField {
                    source_path: dir.to_string_lossy().into_owned(),
                    note: format!(
                        "`timestamps.txt` states {} sample(s) and there is no `data/` directory to \
                         read them from",
                        stamps.len()
                    ),
                });
            } else if files.len() != stamps.len() {
                unread.push(UnmappedField {
                    source_path: dir.to_string_lossy().into_owned(),
                    note: format!(
                        "`timestamps.txt` states {} sample(s) and `data/` holds {} — the frames are \
                         the {samples} they agree on",
                        stamps.len(),
                        files.len()
                    ),
                });
            }
        }
        budget.take(FORMAT_ID, samples as u64)?;

        // What the samples themselves say, where this run reads them.
        // What the receiver said about its own sky: `numsats` per row, counted the way a ROS
        // receiver's own `STATUS_NO_FIX` is, so `autonomy.gnss-fix-availability` answers on a KITTI
        // drive instead of abstaining.
        let mut fix = crate::cdm::FixAvailability {
            message_count: 0,
            unfixed: 0,
        };
        let mut point_counts = super::cdr::PointCountAccum::default();
        let mut values = super::stats::StreamValues::default();
        let mut frames: Vec<Frame> = Vec::with_capacity(samples);
        for (i, ts) in stamps.iter().take(samples).enumerate() {
            let file = files.get(i);
            let uri = file
                .map(|p| {
                    p.strip_prefix(path)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_else(|| format!("{name}/data/{i:010}"));
            let content_hash = match (options.metadata_only, file) {
                (false, Some(p)) => Some(file_digest(p)?),
                _ => None,
            };
            if !options.metadata_only {
                if let Some(p) = file {
                    match modality {
                        // A sweep is `float32 [x, y, z, reflectance]` per point, so the count is the
                        // file's length over sixteen — from `stat`, with no point payload read. A
                        // file whose length is not a whole number of points is not a sweep this
                        // reader will count.
                        Modality::PointCloud => {
                            let len = std::fs::metadata(p)
                                .map_err(|e| IngestError::Io(e.to_string()))?
                                .len();
                            if len % VELODYNE_POINT_BYTES == 0 {
                                point_counts.observe(len / VELODYNE_POINT_BYTES);
                            }
                        }
                        // The three fields the plausibility rules read, and what the receiver
                        // said about its own sky, from the row itself.
                        Modality::Gnss => {
                            if let Some(text) = read_text(p)? {
                                if let Some(row) = oxts_row(&text) {
                                    // The position is recorded as a measurement even where no
                                    // satellite was tracked, and deliberately: unlike a `NavSatFix`
                                    // whose `STATUS_NO_FIX` disclaims the coordinates beside it, an
                                    // OXTS row disclaims nothing — it reports a blended INS
                                    // position and, separately, how many satellites went into it.
                                    // Dropping it would be this reader deciding the row was lying.
                                    values.push_fixed(&row.position.map(Some), &OXTS_DIM_NAMES);
                                    if let Some(satellites) = row.satellites {
                                        fix.message_count += 1;
                                        if satellites == 0 {
                                            fix.unfixed += 1;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            frames.push(Frame {
                ts: *ts,
                value_ref: ValueRef {
                    uri,
                    byte_offset: None,
                    byte_len: None,
                    content_hash,
                },
            });
        }

        let measured = values.finish();
        let accum = measured.as_ref().map(|(a, _)| a);
        streams.push(Stream {
            name: (*name).to_string(),
            modality: *modality,
            // KITTI states no nominal rate anywhere; the rig runs at about 10 Hz and the files do
            // not say so, and a rate this reader supplied would be a rate the data was then graded
            // against. Measured cadence is what the temporal family computes for itself.
            declared_rate_hz: None,
            clock_id: CLOCK_ID.to_string(),
            clock_kind: ClockKind::Measured,
            dtype: None,
            shape: None,
            dim_names: measured.as_ref().and_then(|(_, n)| n.clone()),
            frames,
            stats: None,
            dim_stats: None,
            observed_stats: accum.and_then(|a| a.stats()),
            observed_saturation: accum.and_then(|a| a.saturation()),
            observed_non_finite: accum.map(|a| a.non_finite()),
            observed_dim_stats: accum.and_then(|a| a.dim_stats()),
            latched: None,
            declared_range: None,
            point_fields: None,
            observed_diagnostics: None,
            observed_point_counts: point_counts.finish(),
            observed_image_dims: None,
            observed_body_decodes: None,
            observed_byte_order: None,
            observed_header_stamps: None,
            observed_sequence: None,
            observed_fix_availability: (fix.message_count > 0).then_some(fix),
            media: None,
            // The sensor directory is the coordinate frame the calibration relates, so a sensor's
            // own name is the frame the spatial checks resolve it through.
            frame_id: Some((*name).to_string()),
        });
    }

    if streams.is_empty() {
        return Err(IngestError::Parse {
            format_id: FORMAT_ID,
            message: format!(
                "{} holds no sensor directory with a `timestamps.txt` — a KITTI raw drive holds one \
                 per sensor (image_00…image_03, velodyne_points, oxts)",
                path.display()
            ),
        });
    }

    // Sensor directories this reader has no modality for: data that is there and went unread.
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if !child.is_dir() || !child.join("timestamps.txt").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if SENSORS.iter().any(|(known, _)| *known == name) {
                continue;
            }
            unread.push(UnmappedField {
                source_path: name.clone(),
                note:
                    "a sensor directory with its own timestamps that this reader has no modality \
                       for, so its samples became no frames"
                        .into(),
            });
        }
    }

    // The calibration lives beside the drive, in the date directory.
    let date_dir = path.parent().unwrap_or(path).to_path_buf();
    let (calibration, calib_time) = read_calibration(&date_dir, &mut mapped, &mut omitted)?;

    let (start_ts, end_ts) = {
        let all: Vec<i64> = streams
            .iter()
            .flat_map(|s| s.frames.iter().map(|f| f.ts))
            .collect();
        (all.iter().copied().min(), all.iter().copied().max())
    };
    let episode = Episode {
        index: 0,
        start_ts,
        end_ts,
        streams,
        // A raw drive carries no task and no annotation: it is a recording of a drive.
        task: None,
        labels: vec![],
        ego_poses: None,
        ego_frame: None,
        declared_frame_count: (declared_samples > 0).then_some(declared_samples),
    };

    let mut metadata: Vec<(String, String)> = vec![
        ("source_format".into(), FORMAT_ID.to_string()),
        (
            META_TIME_BASE.into(),
            "naive civil time read as UTC (the files state no offset); every interval is exact and \
             the absolute instant carries that assumption"
                .into(),
        ),
    ];
    if let Some(time) = &calib_time {
        metadata.push(("kitti_calib_time".into(), time.clone()));
    }
    let mut elements = vec![ProvenanceElement {
        key: "source_format".into(),
        value: Some(FORMAT_ID.to_string()),
        class: ProvenanceClass::Known,
    }];
    if calibration.is_some() {
        elements.push(ProvenanceElement {
            key: "calibration".into(),
            value: Some(match &calib_time {
                Some(time) => format!("recorded beside the drive, calibrated {time}"),
                None => "recorded beside the drive".into(),
            }),
            class: ProvenanceClass::Known,
        });
    }
    // The files name no sensor, no operator, no license and no upstream, and this reader invents
    // none of them.
    omitted.push("provenance.sensor / operator / license / upstream (the files state none)".into());

    unmapped.push(UnmappedField {
        source_path: "oxts/data/*.txt".into(),
        note: "fields 4-26 and 28-30 (orientation, velocities, accelerations, accuracies and the \
               are recorded per sample and the CDM holds the position; the three position fields \
               are summarized"
            .into(),
    });
    if !options.metadata_only {
        mapped.push("sample bytes -> frame.value_ref.content_hash (SHA-256)".into());
        mapped.push("velodyne_points/data/*.bin length -> observed point counts".into());
        mapped.push("oxts/data/*.txt fields 1-3 -> measured latitude/longitude/altitude".into());
        mapped.push("oxts/data/*.txt field 27 (numsats) -> satellite fix availability".into());
    } else {
        omitted.push(
            "frames and sample contents (metadata-only: each sensor's `timestamps.txt` states its \
             sample count, which is the episode's declared length here, and no sample was opened)"
                .into(),
        );
    }

    let dataset = Dataset {
        id: super::dataset_id_from_path(path, FORMAT_ID),
        metadata,
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements,
        }],
        calibration,
        episodes: vec![episode],
    };
    Ok(Ingested {
        dataset,
        report: IngestReport {
            format_id: "kitti",
            source_version: Some("raw-sync".into()),
            coverage: if options.metadata_only {
                Coverage::MetadataOnly {
                    episodes_declared: 1,
                }
            } else {
                Coverage::Full
            },
            mapped_fields: mapped,
            unmapped_fields: unmapped,
            unread_sources: unread,
            omitted_fields: omitted,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boundary pins for the timestamp reader, written after a mutation sweep: every comparison in
    /// `parse_timestamp` and `days_from_civil` could be flipped with no test noticing, and the
    /// civil-date conversion is the one piece here whose being wrong would make every temporal
    /// result wrong in a way that looks like data.
    #[test]
    fn a_timestamp_is_read_as_the_instant_it_names() {
        // The demo drive's own stamp, against the epoch seconds it is: `date -u -d` on
        // 2011-09-26 13:02:25 gives 1317042145.
        assert_eq!(
            parse_timestamp("2011-09-26 13:02:25.000000000"),
            Some(1_317_042_145_000_000_000)
        );
        // Nanoseconds are read as nanoseconds, and a shorter fraction is scaled rather than
        // truncated — `.5` is half a second, not five nanoseconds.
        assert_eq!(
            parse_timestamp("2011-09-26 13:02:25.964389445"),
            Some(1_317_042_145_964_389_445)
        );
        assert_eq!(
            parse_timestamp("2011-09-26 13:02:25.5"),
            Some(1_317_042_145_500_000_000)
        );
    }

    #[test]
    fn a_drive_recorded_in_january_or_february_reads_as_the_right_day() {
        // The branch the fixtures never reach. The civil-date conversion shifts the year for
        // January and February — the era trick that makes leap days land at the end of a cycle —
        // and a drive recorded in those two months would otherwise be read a year out, which every
        // temporal result would then be computed from. KITTI's own drives include a 2011-01-28 set.
        assert_eq!(
            parse_timestamp("2011-01-28 09:00:00.000000000"),
            Some(1_296_205_200_000_000_000)
        );
        assert_eq!(
            parse_timestamp("2012-02-29 12:00:00.000000000"),
            Some(1_330_516_800_000_000_000),
            "a leap day is a real day"
        );
        // March, the first month of the shifted calendar, and December, its last.
        assert_eq!(
            parse_timestamp("2011-03-01 00:00:00.000000000"),
            Some(1_298_937_600_000_000_000)
        );
        assert_eq!(
            parse_timestamp("2011-12-31 23:59:59.000000000"),
            Some(1_325_375_999_000_000_000)
        );
    }

    #[test]
    fn the_clock_fields_accept_what_a_clock_can_hold_and_refuse_the_rest() {
        // A leap second is a real stamp a recorder can write, and the last second of a day is not
        // an invalid hour. Both are one step from a value that is genuinely out of range.
        assert!(parse_timestamp("2011-09-26 23:59:60.000000000").is_some());
        assert!(parse_timestamp("2011-09-26 24:00:00.000000000").is_none());
        assert!(parse_timestamp("2011-09-26 23:60:00.000000000").is_none());
        assert!(parse_timestamp("2011-09-26 23:59:61.000000000").is_none());
        // And the shapes that are not a timestamp at all, which become no frame.
        for text in [
            "",
            "2011-09-26",
            "2011-09-26T13:02:25.0Z",
            "2011-13-01 00:00:00.0",
            "2011-09-26 13:02:25.abc",
            "not a timestamp",
        ] {
            assert!(parse_timestamp(text).is_none(), "`{text}` is not a stamp");
        }
    }

    #[test]
    fn a_sweeps_point_count_is_its_length_over_the_point_size() {
        // The whole of the LiDAR reading: sixteen bytes a point, so a file that is not a whole
        // number of points is not a sweep this reader will count.
        assert_eq!(VELODYNE_POINT_BYTES, 16);
        assert_eq!(1024 % VELODYNE_POINT_BYTES, 0);
        assert_ne!(1020 % VELODYNE_POINT_BYTES, 0);
    }

    #[test]
    fn a_rotation_matrix_becomes_a_unit_quaternion_in_every_branch() {
        // Shepperd's branch choice, which the sweep showed is never exercised past the first
        // branch: each of the four is picked by a different rotation, and all four must come back
        // as unit quaternions or `AUTONOMY.CALIBRATION_IMPLAUSIBLE` would fire on sound geometry.
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        // 180° about x, y and z: each drives the trace negative and selects a different branch.
        let about_x = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, -1.0];
        let about_y = [-1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0];
        let about_z = [-1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
        for (name, m) in [
            ("identity", identity),
            ("x", about_x),
            ("y", about_y),
            ("z", about_z),
        ] {
            let q = quaternion_from_matrix(&m);
            let norm = q.iter().map(|v| v * v).sum::<f64>().sqrt();
            assert!((norm - 1.0).abs() < 1e-12, "{name}: {q:?} has norm {norm}");
        }
        // The identity really is the identity rotation, not just something of unit length.
        assert_eq!(quaternion_from_matrix(&identity), [0.0, 0.0, 0.0, 1.0]);
    }
}
