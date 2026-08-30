//! The demo RLDS dataset, in the TFDS on-disk layout, for trying the CLI end-to-end
//! without downloading an Open X-Embodiment shard. [`VARIANTS`]:
//!
//! - (default) `clean` — three distinct 20-step episodes with a 7-DoF action, a 3-DoF state, an
//!   encoded camera image and a per-step instruction. Nothing about the data is wrong, so the only
//!   findings are the provenance gaps RLDS genuinely has (it records no sensor, clock, calibration
//!   or annotator).
//! - `truncated` — the same data, but `dataset_info.json` declares four episodes across its shard
//!   lengths while the shards hold three: a half-finished download →
//!   `STRUCTURAL.EPISODE_COUNT_MISMATCH`.
//! - `desynced` — one episode whose camera holds 19 images against 20 actions. The record
//!   contradicts the schema it was serialized against, so ingestion refuses it by name rather than
//!   quietly reporting a 19-step episode.
//! - `corrupt` — a clean dataset with one byte flipped inside a record. Only the TFRecord checksum
//!   notices, so this shows the shard being rejected instead of parsed past.
//!
//! Usage: `cargo run -p veridex-demo --example make_demo_rlds -- <output-dir> [clean|truncated|desynced|corrupt]`

use std::path::Path;

use crate::DemoError;

/// Every variant `write` accepts.
pub const VARIANTS: &[&str] = &["clean", "truncated", "desynced", "corrupt"];

// ---- CRC-32C, as TFRecord frames it ----

fn crc32c(data: &[u8]) -> u32 {
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

fn masked(data: &[u8]) -> u32 {
    crc32c(data).rotate_right(15).wrapping_add(0xA282_EAD8)
}

// ---- Just enough protobuf to write a `tf.train.Example` ----

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

fn delimited(field: u64, payload: &[u8], out: &mut Vec<u8>) {
    varint((field << 3) | 2, out);
    varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

enum Value {
    Bytes(Vec<Vec<u8>>),
    Floats(Vec<f32>),
    Ints(Vec<i64>),
}

impl Value {
    fn encode(&self) -> Vec<u8> {
        let mut feature = Vec::new();
        let mut list = Vec::new();
        match self {
            Value::Bytes(entries) => {
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
                delimited(1, &packed, &mut list);
                delimited(2, &list, &mut feature);
            }
            Value::Ints(values) => {
                let mut packed = Vec::new();
                for v in values {
                    varint(*v as u64, &mut packed);
                }
                delimited(1, &packed, &mut list);
                delimited(3, &list, &mut feature);
            }
        }
        feature
    }
}

fn example(features: Vec<(&str, Value)>) -> Vec<u8> {
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

// ---- The dataset ----

const STEPS: usize = 20;

/// One episode. `images` lets the `desynced` variant write a camera short of its own actions.
fn episode(index: usize, images: usize) -> Vec<u8> {
    let mut action = Vec::new();
    let mut state = Vec::new();
    for step in 0..STEPS {
        // A distinct trajectory per episode: identical episodes are themselves a finding.
        let base = index as f32 * 10.0 + step as f32 * 0.05;
        for dof in 0..7 {
            action.push(base + dof as f32 * 0.01);
        }
        for axis in 0..3 {
            state.push(base * 0.5 + axis as f32 * 0.02);
        }
    }
    example(vec![
        ("steps/action", Value::Floats(action)),
        (
            "steps/is_first",
            Value::Ints((0..STEPS).map(|s| i64::from(s == 0)).collect()),
        ),
        (
            "steps/is_last",
            Value::Ints((0..STEPS).map(|s| i64::from(s + 1 == STEPS)).collect()),
        ),
        (
            "steps/language_instruction",
            Value::Bytes(vec![b"pick up the red block".to_vec(); STEPS]),
        ),
        (
            "steps/observation/image",
            // Stand-in for an encoded JPEG: Veridex fingerprints these bytes, never decodes them.
            Value::Bytes(
                (0..images)
                    .map(|s| format!("jpeg-ep{index}-step{s}").into_bytes())
                    .collect(),
            ),
        ),
        ("steps/observation/state", Value::Floats(state)),
        (
            "episode_metadata/file_path",
            Value::Bytes(vec![format!("/raw/session_{index:03}.h5").into_bytes()]),
        ),
    ])
}

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
                    "is_last": {"tensor": {"shape": {}, "dtype": "bool"}},
                    "language_instruction": {"text": {}},
                    "observation": {"featuresDict": {"features": {
                        "image": {"image": {"shape": {"dimensions": ["256", "256", "3"]},
                                            "dtype": "uint8", "encodingFormat": "jpeg"}},
                        "state": {"tensor": {"shape": {"dimensions": ["3"]}, "dtype": "float32"}}
                    }}}
                }}}}
            }
        }}
    })
    .to_string()
}

fn dataset_info_json(declared_episodes: u64) -> String {
    serde_json::json!({
        "name": "demo_rlds",
        "version": "1.0.0",
        "description": "A synthetic RLDS demo dataset for trying Veridex end-to-end.",
        "citation": "@misc{veridex_demo_rlds}",
        "fileFormat": "tfrecord",
        "moduleName": "veridex.examples.demo_rlds",
        "redistributionInfo": {"license": "Apache-2.0"},
        "splits": [{
            "name": "train",
            "filepathTemplate": "{DATASET}-{SPLIT}.{FILEFORMAT}-{SHARD_X_OF_Y}",
            // Proto3 JSON writes int64 as a string, exactly as TFDS does.
            "shardLengths": [declared_episodes.to_string()],
        }],
    })
    .to_string()
}

/// Write the demo RLDS/TFDS export into `dir`, replacing anything already there.
pub fn write(dir: &Path, variant: &str) -> Result<(), DemoError> {
    crate::check_variant(variant, VARIANTS)?;
    crate::fresh_dir(dir)?;

    let (episodes, declared, images) = match variant {
        "clean" => (3, 3, STEPS),
        // The manifest promises one more episode than the shard holds.
        "truncated" => (3, 4, STEPS),
        // One camera image short of the actions it is paired with.
        "desynced" => (1, 1, STEPS - 1),
        "corrupt" => (3, 3, STEPS),
        _ => unreachable!("checked against VARIANTS above"),
    };

    let records: Vec<Vec<u8>> = (0..episodes).map(|e| episode(e, images)).collect();
    let mut bytes = shard(&records);
    if variant == "corrupt" {
        // Flip one bit deep inside the first record. Nothing but the checksum can see it.
        let at = bytes.len() / 4;
        bytes[at] ^= 0x01;
    }

    std::fs::write(dir.join("features.json"), features_json())?;
    std::fs::write(dir.join("dataset_info.json"), dataset_info_json(declared))?;
    std::fs::write(dir.join("demo_rlds-train.tfrecord-00000-of-00001"), &bytes)?;
    Ok(())
}

/// Steps per episode, for a caller that wants to report the shape it asked for.
pub const STEPS_PER_EPISODE: usize = STEPS;
