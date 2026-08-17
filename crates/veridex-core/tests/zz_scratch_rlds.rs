//! RLDS / TFDS adapter tests.
//!
//! Every fixture is written the way TFDS writes one: a `dataset_info.json`, a `features.json`, and
//! `*.tfrecord-XXXXX-of-YYYYY` shards of length-prefixed, CRC-framed `tf.train.Example` records.
//! The TFRecord framing and the protobuf are built here from the wire formats — deliberately a
//! second, independent implementation, so a shared bug cannot make a broken parser look correct.

use std::path::Path;

use veridex_core::adapter::rlds::RldsAdapter;
use veridex_core::adapter::{
    default_registry, Adapter, Coverage, Detection, IngestError, IngestOptions, Sample, Source,
};
use veridex_core::canonical::content_hash;
use veridex_core::cdm::{Modality, ProvenanceClass, ProvenanceScope};

// ---- Writing the wire formats (independent of the adapter's readers) ----

fn crc32c(data: &[u8]) -> u32 {
    // Bitwise, reflected polynomial 0x82F63B78 — no shared table with the implementation under test.
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// Spelled out as the TFRecord format documents the mask, rather than as the rotate it happens to
// be — the point of this file is to encode the spec independently of the implementation.
#[allow(clippy::manual_rotate)]
fn masked(data: &[u8]) -> u32 {
    let crc = crc32c(data);
    ((crc >> 15) | (crc << 17)).wrapping_add(0xA282_EAD8)
}

fn varint(value: u64, out: &mut Vec<u8>) {
    let mut value = value;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            return;
        }
    }
}

fn tag(field: u64, wire: u8, out: &mut Vec<u8>) {
    varint((field << 3) | wire as u64, out);
}

fn delimited(field: u64, payload: &[u8], out: &mut Vec<u8>) {
    tag(field, 2, out);
    varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// One `tf.train.Feature` value list.
enum Value {
    Bytes(Vec<Vec<u8>>),
    Floats(Vec<f32>),
    Ints(Vec<i64>),
}

impl Value {
    fn encode(&self) -> Vec<u8> {
        let mut feature = Vec::new();
        match self {
            Value::Bytes(entries) => {
                let mut list = Vec::new();
                for entry in entries {
                    delimited(1, entry, &mut list);
                }
                delimited(1, &list, &mut feature);
            }
            Value::Floats(values) => {
                let mut packed = Vec::new();
                for v in values {
                    packed.extend_from_slice(&v.to_le_bytes());
                }
                let mut list = Vec::new();
                delimited(1, &packed, &mut list);
                delimited(2, &list, &mut feature);
            }
            Value::Ints(values) => {
                let mut packed = Vec::new();
                for v in values {
                    varint(*v as u64, &mut packed);
                }
                let mut list = Vec::new();
                delimited(1, &packed, &mut list);
                delimited(3, &list, &mut feature);
            }
        }
        feature
    }
}

/// A `tf.train.Example` holding the given flattened keys.
fn example(features: &[(&str, Value)]) -> Vec<u8> {
    let mut map = Vec::new();
    for (key, value) in features {
        let mut entry = Vec::new();
        delimited(1, key.as_bytes(), &mut entry);
        delimited(2, &value.encode(), &mut entry);
        delimited(1, &entry, &mut map);
    }
    let mut out = Vec::new();
    delimited(1, &map, &mut out);
    out
}

/// Frame records into one TFRecord shard.
fn shard(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for record in records {
        let length = (record.len() as u64).to_le_bytes();
        out.extend_from_slice(&length);
        out.extend_from_slice(&masked(&length).to_le_bytes());
        out.extend_from_slice(record);
        out.extend_from_slice(&masked(record).to_le_bytes());
    }
    out
}

// ---- Fixture builders ----

/// The features.json of a small OXE-shaped dataset: a 7-DoF action, a camera image, a state vector,
/// a per-step instruction, and the episode's source file.
fn features_json() -> String {
    serde_json::json!({
        "pythonClassName": "tensorflow_datasets.core.features.features_dict.FeaturesDict",
        "featuresDict": {"features": {
            "episode_metadata": {"featuresDict": {"features": {
                "file_path": {"text": {}}
            }}},
            "steps": {
                "pythonClassName": "tensorflow_datasets.core.features.dataset_feature.Dataset",
                "sequence": {"feature": {"featuresDict": {"features": {
                    "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
                    "is_first": {"tensor": {"shape": {}, "dtype": "bool"}},
                    "language_instruction": {"text": {}},
                    "observation": {"featuresDict": {"features": {
                        "image": {"image": {"shape": {"dimensions": ["64", "64", "3"]},
                                            "dtype": "uint8", "encodingFormat": "jpeg"}},
                        "state": {"tensor": {"shape": {"dimensions": ["3"]}, "dtype": "float32"}}
                    }}}
                }}}}
            }
        }}
    })
    .to_string()
}

fn dataset_info_json(shard_lengths: Option<Vec<u64>>, file_format: &str) -> String {
    let mut info = serde_json::json!({
        "name": "demo_rlds",
        "version": "0.1.0",
        "fileFormat": file_format,
        "moduleName": "tensorflow_datasets.robotics.demo_rlds",
        "citation": "@article{demo}",
        "redistributionInfo": {"license": "Apache-2.0"},
    });
    if let Some(lengths) = shard_lengths {
        info["splits"] = serde_json::json!([{
            "name": "train",
            // Proto3 JSON writes int64 as a string, exactly as TFDS does.
            "shardLengths": lengths.iter().map(|n| n.to_string()).collect::<Vec<_>>(),
        }]);
    }
    info.to_string()
}

/// One episode's record: `steps` of the given length, with the given instruction per step.
/// `offset` shifts the values so two episodes of a dataset are not byte-identical trajectories.
fn episode_record(steps: usize, instructions: &[&str], source_file: &str, offset: f32) -> Vec<u8> {
    let mut action = Vec::new();
    let mut state = Vec::new();
    for step in 0..steps {
        for dof in 0..7 {
            action.push(offset + step as f32 + dof as f32 * 0.1);
        }
        for axis in 0..3 {
            state.push(offset + step as f32 * 0.5 + axis as f32);
        }
    }
    example(&[
        ("steps/action", Value::Floats(action)),
        (
            "steps/is_first",
            Value::Ints((0..steps).map(|s| i64::from(s == 0)).collect()),
        ),
        (
            "steps/language_instruction",
            Value::Bytes(
                (0..steps)
                    .map(|s| {
                        instructions[s.min(instructions.len() - 1)]
                            .as_bytes()
                            .to_vec()
                    })
                    .collect(),
            ),
        ),
        (
            "steps/observation/image",
            Value::Bytes(
                (0..steps)
                    .map(|s| format!("jpeg-{s}").into_bytes())
                    .collect(),
            ),
        ),
        ("steps/observation/state", Value::Floats(state)),
        (
            "episode_metadata/file_path",
            Value::Bytes(vec![source_file.as_bytes().to_vec()]),
        ),
    ])
}

/// A complete TFDS directory holding `episodes` episodes of `steps` steps each.
fn write_dataset(dir: &Path, episodes: usize, steps: usize) {
    std::fs::write(dir.join("features.json"), features_json()).unwrap();
    std::fs::write(
        dir.join("dataset_info.json"),
        dataset_info_json(Some(vec![episodes as u64]), "tfrecord"),
    )
    .unwrap();
    let records: Vec<Vec<u8>> = (0..episodes)
        .map(|e| {
            // Each episode is a distinct trajectory — identical episodes are a defect Veridex
            // reports, so a fixture must not accidentally build one.
            episode_record(
                steps,
                &["pick up the block"],
                &format!("/raw/ep{e}.h5"),
                e as f32 * 100.0,
            )
        })
        .collect();
    std::fs::write(
        dir.join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&records),
    )
    .unwrap();
}

fn ingest(dir: &Path) -> Result<veridex_core::adapter::Ingested, IngestError> {
    RldsAdapter.ingest(&Source::Local(dir.to_path_buf()), &IngestOptions::default())
}

// ---- Tests ----

#[test]

// ---- scratch probes ----

#[test]
fn probe_a_sample_larger_than_the_dataset_without_shard_lengths() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 4);
    // Same as the existing test, but the manifest declares no shard lengths (TFDS omits `splits`
    // in plenty of published dirs, and the adapter itself has a code path for exactly that).
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(None, "tfrecord"),
    )
    .unwrap();
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions {
            sample: Sample::FirstEpisodes(10),
            ..IngestOptions::default()
        },
    )
    .unwrap();
    let declared = out
        .ingested
        .dataset
        .metadata
        .iter()
        .find(|(k, _)| k == veridex_core::cdm::META_DECLARED_EPISODES)
        .map(|(_, v)| v.clone());
    eprintln!("declared={declared:?} episodes={}", out.ingested.dataset.episodes.len());
    let codes: Vec<&str> = out.verdict.findings.iter().map(|f| f.code.as_str()).collect();
    eprintln!("codes={codes:?}");
    assert!(
        !codes.contains(&"STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "sound dataset failed on the user's own flag"
    );
}

#[test]
fn probe_sampled_report_claims_crc_verified_on_every_record() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 5, 3);
    let ingested = RldsAdapter
        .ingest(
            &Source::Local(tmp.path().to_path_buf()),
            &IngestOptions {
                sample: Sample::FirstEpisodes(2),
                ..IngestOptions::default()
            },
        )
        .unwrap();
    eprintln!("mapped={:#?}", ingested.report.mapped_fields);
    eprintln!("omitted={:#?}", ingested.report.omitted_fields);
}

#[test]
fn probe_wire_kind_contradicting_declared_dtype() {
    // features.json declares steps/action as float32[7]; the record serializes it as int64.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let record = example(&[
        ("steps/action", Value::Ints((0..14).collect())),
        ("steps/is_first", Value::Ints(vec![1, 0])),
        (
            "steps/language_instruction",
            Value::Bytes(vec![b"go".to_vec(), b"go".to_vec()]),
        ),
        (
            "steps/observation/image",
            Value::Bytes(vec![b"a".to_vec(), b"b".to_vec()]),
        ),
        ("steps/observation/state", Value::Floats(vec![0.0; 6])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let out = ingest(tmp.path()).unwrap();
    let a = out.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "action")
        .unwrap();
    eprintln!("dtype={:?} shape={:?} frames={}", a.dtype, a.shape, a.frames.len());
    eprintln!("unmapped={:#?}", out.report.unmapped_fields);
    eprintln!("omitted={:#?}", out.report.omitted_fields);
}

#[test]
fn probe_duplicate_flattened_path() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let feats = serde_json::json!({
        "featuresDict": {"features": {
            "steps": {"sequence": {"feature": {"featuresDict": {"features": {
                "observation/state": {"tensor": {"shape": {"dimensions": ["3"]}, "dtype": "float32"}},
                "observation": {"featuresDict": {"features": {
                    "state": {"tensor": {"shape": {"dimensions": ["3"]}, "dtype": "float32"}}
                }}}
            }}}}}
        }}
    });
    std::fs::write(tmp.path().join("features.json"), feats.to_string()).unwrap();
    let record = example(&[("steps/observation/state", Value::Floats(vec![0.0; 6]))]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let out = ingest(tmp.path()).unwrap();
    let names: Vec<&str> = out.dataset.episodes[0].streams.iter().map(|s| s.name.as_str()).collect();
    eprintln!("names={names:?}");
}

#[test]
fn probe_shape_as_array_not_object() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let feats = serde_json::json!({
        "featuresDict": {"features": {
            "steps": {"sequence": {"feature": {"featuresDict": {"features": {
                "action": {"tensor": {"shape": ["7"], "dtype": "float32"}}
            }}}}}
        }}
    });
    std::fs::write(tmp.path().join("features.json"), feats.to_string()).unwrap();
    let record = example(&[("steps/action", Value::Floats(vec![0.0; 14]))]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let out = ingest(tmp.path()).unwrap();
    let a = &out.dataset.episodes[0].streams[0];
    eprintln!("shape={:?} frames={} (2 steps of 7 expected)", a.shape, a.frames.len());
}

#[test]
fn probe_empty_shard_no_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(None, "tfrecord"),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        Vec::<u8>::new(),
    )
    .unwrap();
    match ingest(tmp.path()) {
        Ok(o) => {
            eprintln!("OK, episodes={} coverage={:?}", o.dataset.episodes.len(), o.report.coverage);
        }
        Err(e) => eprintln!("Err {e:?}"),
    }
}
