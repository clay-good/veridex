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

use veridex_core::adapter::lerobot::LeRobotAdapter;
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

/// The four ways a user reaches one directory. Every one must name it the same, because the name is
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

    for (label, ds) in [
        ("absolute", &absolute),
        ("trailing `.`", &with_dot),
        ("trailing slash", &trailing_slash),
        ("through `..`", &via_parent),
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
    ] {
        assert_eq!(
            content_hash(ds),
            baseline,
            "{label} hashes differently from the absolute path — a certificate issued against one \
             would be rejected against the other, on identical bytes"
        );
    }
}
