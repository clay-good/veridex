//! The dataset id — and so the content hash — is a property of the dataset, not of how it was typed.
//!
//! `Path::file_name` answers about the path as written and returns `None` for one ending in `.`, so
//! `veridex check .` run from inside a dataset used to fall through to the adapter's fallback
//! string. The id is bound into the CDM content hash, so the same bytes hashed two ways and a
//! `verify` run from inside the dataset rejected a genuine certificate.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;

use veridex_core::adapter::candbc::CanDbcAdapter;
use veridex_core::adapter::lerobot::LeRobotAdapter;
use veridex_core::adapter::rosbag2::Rosbag2Adapter;
use veridex_core::adapter::{Adapter, IngestOptions, Source};
use veridex_core::cdm::Dataset;
use veridex_core::content_hash;

fn write_dataset(dir: &Path) {
    fs::create_dir_all(dir.join("meta")).unwrap();
    fs::create_dir_all(dir.join("data/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 30.0,
        "features": { "observation.state": { "dtype": "float32", "shape": [1] } },
    });
    fs::write(
        dir.join("meta/info.json"),
        serde_json::to_string(&info).unwrap(),
    )
    .unwrap();

    let schema = Arc::new(Schema::new(vec![
        Field::new("episode_index", DataType::Int64, false),
        Field::new("frame_index", DataType::Int64, false),
        Field::new("timestamp", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![0i64, 0])) as ArrayRef,
            Arc::new(Int64Array::from(vec![0i64, 1])),
            Arc::new(Float64Array::from(vec![0.0f64, 0.0333])),
        ],
    )
    .unwrap();
    let path = dir.join("data/chunk-000/file-000.parquet");
    let mut writer = ArrowWriter::try_new(fs::File::create(path).unwrap(), schema, None).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

fn ingest(path: &Path) -> Dataset {
    LeRobotAdapter
        .ingest(
            &Source::Local(path.to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("lerobot ingest")
        .dataset
}

/// A path spelling that genuinely leaves [`Path::file_name`] with no answer.
///
/// The obvious spellings do not: Rust normalizes a trailing `.` away, so `Path::new("/a/b/.")
/// `.file_name()` is `Some("b")` and `/a/b/` is too. Only a bare `.` — which needs the process
/// working directory, and so cannot be used from a threaded test without racing its neighbours —
/// and a path *terminating* in `..` return `None`. `<dir>/sub/..` names `<dir>` exactly as `cd
/// <dir> && veridex verify .` does, reaches the same `None` branch, and needs no `chdir`.
fn unnameable_spelling_of(dir: &Path) -> std::path::PathBuf {
    fs::create_dir_all(dir.join("sub")).unwrap();
    dir.join("sub").join("..")
}

/// The ways a user reaches one directory. Every one must name it the same, because the name is
/// in the hash the certificate is bound to.
#[test]
fn every_spelling_of_one_path_yields_one_id_and_one_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("my_dataset");
    write_dataset(&dir);

    let absolute = ingest(&dir);
    let with_dot = ingest(&dir.join("."));
    let trailing_slash = ingest(Path::new(&format!("{}/", dir.display())));
    let via_parent = ingest(&dir.join("..").join("my_dataset"));
    // The spelling that actually exercises the defect. Without it this test passed with the fix
    // fully reverted: none of the four above leave `file_name()` without an answer, so for all its
    // thoroughness it never tested its own invariant.
    let unnameable = ingest(&unnameable_spelling_of(&dir));

    for (label, ds) in [
        ("absolute", &absolute),
        ("trailing `.`", &with_dot),
        ("trailing slash", &trailing_slash),
        ("through `..`", &via_parent),
        ("terminating in `..`", &unnameable),
    ] {
        assert_eq!(
            ds.id, "my_dataset",
            "{label} must name the directory the dataset is in, not the adapter's fallback"
        );
    }

    let baseline = content_hash(&absolute);
    for (label, ds) in [
        ("trailing `.`", &with_dot),
        ("trailing slash", &trailing_slash),
        ("through `..`", &via_parent),
        ("terminating in `..`", &unnameable),
    ] {
        assert_eq!(
            content_hash(ds),
            baseline,
            "{label} hashes differently from the absolute path — a certificate issued against one \
             would be rejected against the other, on identical bytes"
        );
    }
}

const DBC: &str = "\
BO_ 256 EngineData: 8 ECU
 SG_ EngineSpeed : 0|16@1+ (0.25,0) [0|16383.75] \"rpm\" Vector__XXX
";

const LOG: &str = "\
(1000.000000) can0 100#4001000012343412
(1000.100000) can0 100#80020000ABCDCDAB
";

/// The same invariant for the other directory-shaped adapters. `dataset_id_from_path` was written
/// for this and applied to LeRobot and Zarr; CAN+DBC and RLDS kept a raw `file_name()`, so `veridex
/// certify mycandata` and `cd mycandata && veridex verify .` disagreed on the hash and the genuine
/// certificate was rejected against its own dataset.
#[test]
fn a_candbc_directory_is_named_the_same_from_inside_it() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mycandata");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("vehicle.dbc"), DBC).unwrap();
    fs::write(dir.join("drive.log"), LOG).unwrap();

    let ingest = |p: &Path| {
        CanDbcAdapter
            .ingest(&Source::Local(p.to_path_buf()), &IngestOptions::default())
            .expect("candbc ingest")
            .dataset
    };

    let absolute = ingest(&dir);
    let from_inside = ingest(&unnameable_spelling_of(&dir));

    assert_eq!(
        absolute.id, "mycandata",
        "an absolute path must name the directory the CAN log is in"
    );
    assert_eq!(
        from_inside.id, "mycandata",
        "a path with no `file_name()` must still name the directory, not fall through to the \
         adapter's `candbc` fallback"
    );
    assert_eq!(
        content_hash(&from_inside),
        content_hash(&absolute),
        "the same CAN dataset hashed two ways depending on how the path was spelled — a \
         certificate issued from outside the directory is rejected from inside it"
    );
}

/// The rosbag2 fixtures, which are real Python-`sqlite3` output committed under the core crate.
fn bag(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rosbag2")
        .join(name)
}

fn ingest_bag(p: &Path) -> Dataset {
    Rosbag2Adapter
        .ingest(&Source::Local(p.to_path_buf()), &IngestOptions::default())
        .expect("rosbag2 ingest")
        .dataset
}

#[test]
fn a_bag_directory_is_named_the_same_from_inside_it() {
    // Copied to a temp directory: `unnameable_spelling_of` creates a subdirectory, and a committed
    // fixture is not something a test writes into.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("clean_rig");
    fs::create_dir_all(&dir).unwrap();
    for entry in fs::read_dir(bag("clean_rig")).unwrap().flatten() {
        fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
    }

    let absolute = ingest_bag(&dir);
    // `<dir>/<something>/..` is the shape `veridex check .` reaches the adapter as: it resolves to
    // the directory but has no `file_name()`.
    let from_inside = ingest_bag(&unnameable_spelling_of(&dir));

    assert_eq!(absolute.id, "clean_rig");
    assert_eq!(
        from_inside.id, "clean_rig",
        "a path with no `file_name()` must still name the bag, not fall through to the `rosbag2` \
         fallback"
    );
    assert_eq!(
        content_hash(&from_inside),
        content_hash(&absolute),
        "a certificate issued for a bag from outside its directory must verify from inside it"
    );
}

#[test]
fn a_bare_shard_is_named_for_the_recording_however_it_is_stored() {
    // A shard is identified by the recording, not by the file: `x.db3` and `x.db3.zstd` are one
    // recording stored two ways, and the id is bound into the content hash — so naming the
    // compressed one `compressed_rig_0.db3` (the file stem, which keeps an extension) would make a
    // certificate issued over the uncompressed copy fail against the compressed one.
    assert_eq!(ingest_bag(&bag("bare.db3")).id, "bare");
    assert_eq!(
        ingest_bag(&bag("compressed_rig/compressed_rig_0.db3.zstd")).id,
        "compressed_rig_0"
    );

    // And a relative spelling names it the same as an absolute one.
    let relative = bag("compressed_rig").join("..").join("bare.db3");
    assert_eq!(ingest_bag(&relative).id, "bare");
    assert_eq!(
        content_hash(&ingest_bag(&relative)),
        content_hash(&ingest_bag(&bag("bare.db3")))
    );
}
