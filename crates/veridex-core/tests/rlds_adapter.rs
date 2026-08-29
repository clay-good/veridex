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

/// A TFDS export whose `dataset_info.json` omits `name`, so the id falls back to the path.
#[test]
fn an_unnamed_export_is_identified_by_its_directory_however_the_path_is_spelled() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("openx_export");
    std::fs::create_dir_all(&dir).unwrap();
    write_dataset(&dir, 1, 3);
    // `name` is what the id normally comes from; without it the adapter falls back to the path,
    // and that fallback used a raw `file_name()`. `Path::file_name` answers about the path as
    // written and has no answer for one terminating in `..` — the spelling `cd <dir> && veridex
    // verify .` produces — so the same export hashed one way from outside and another from
    // inside, and a genuine certificate was rejected against its own dataset.
    let mut info: serde_json::Value =
        serde_json::from_str(&dataset_info_json(Some(vec![1]), "tfrecord")).unwrap();
    info.as_object_mut().unwrap().remove("name");
    std::fs::write(dir.join("dataset_info.json"), info.to_string()).unwrap();

    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let from_outside = ingest(&dir).expect("rlds ingest").dataset;
    let from_inside = ingest(&dir.join("sub").join(".."))
        .expect("rlds ingest")
        .dataset;

    assert_eq!(from_outside.id, "openx_export");
    assert_eq!(
        from_inside.id, "openx_export",
        "a path with no `file_name()` must still name the directory, not fall through to the \
         adapter's `rlds` fallback"
    );
    assert_eq!(
        veridex_core::content_hash(&from_inside),
        veridex_core::content_hash(&from_outside),
        "the same export hashed two ways depending on how the path was spelled"
    );
}

// ---- Tests ----

#[test]
fn a_tfds_directory_becomes_episodes_streams_and_frames() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 5);
    let ingested = ingest(tmp.path()).unwrap();
    let ds = &ingested.dataset;

    assert_eq!(ds.episodes.len(), 2);
    assert_eq!(ds.id, "demo_rlds");
    let ep = &ds.episodes[0];
    let names: Vec<&str> = ep.streams.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "action",
            "is_first",
            "language_instruction",
            "observation/image",
            "observation/state",
        ],
        "the `steps/` prefix is the timeline, not part of the stream's name"
    );
    for stream in &ep.streams {
        assert_eq!(
            stream.frames.len(),
            5,
            "stream `{}` must carry one frame per step",
            stream.name
        );
        // The flattened list divides evenly into steps, and each step is stamped with its index.
        let stamps: Vec<i64> = stream.frames.iter().map(|f| f.ts).collect();
        assert_eq!(stamps, vec![0, 1, 2, 3, 4]);
        assert!(stream
            .frames
            .iter()
            .all(|f| f.value_ref.content_hash.is_some()));
    }
    let action = ep.streams.iter().find(|s| s.name == "action").unwrap();
    assert_eq!(action.modality, Modality::Action);
    assert_eq!(action.dtype.as_deref(), Some("float32"));
    assert_eq!(action.shape, Some(vec![7]));
    let image = ep
        .streams
        .iter()
        .find(|s| s.name == "observation/image")
        .unwrap();
    assert_eq!(image.modality, Modality::Video);
    assert_eq!(image.shape, Some(vec![64, 64, 3]));
    assert_eq!(ep.start_ts, Some(0));
    assert_eq!(ep.end_ts, Some(4));
    assert_eq!(ep.task.as_deref(), Some("pick up the block"));
    assert_eq!(ingested.report.coverage, Coverage::Full);
}

#[test]
fn the_timeline_is_the_step_index_and_the_report_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 3);
    let ingested = ingest(tmp.path()).unwrap();
    for stream in &ingested.dataset.episodes[0].streams {
        assert_eq!(stream.clock_id, "rlds-step-index");
        assert_eq!(
            stream.declared_rate_hz, None,
            "RLDS declares no rate, and none may be invented from the step count"
        );
    }
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("no wall clock")),
        "the missing clock has to be disclosed, not left for a reader to infer: {:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn a_corrupt_shard_is_reported_rather_than_parsed_past() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 4);
    let path = tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001");
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip one bit deep inside the first record's payload — the checksum is the only thing that
    // notices, and a parser that skipped it would hand back a plausible, wrong episode.
    let middle = bytes.len() / 4;
    bytes[middle] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                message.contains("checksum") && message.contains("corrupt"),
                "the error must name the corruption: {message}"
            );
        }
        other => panic!("a corrupt shard must be refused, got {other:?}"),
    }
}

#[test]
fn a_truncated_shard_is_refused_rather_than_read_as_a_short_dataset() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 4);
    let path = tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001");
    let bytes = std::fs::read(&path).unwrap();
    // Cut the second record in half. Silence at the end of a file must not read as "that's all".
    std::fs::write(&path, &bytes[..bytes.len() - 20]).unwrap();

    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                message.contains("past the end") || message.contains("truncated"),
                "the error must name the truncation: {message}"
            );
        }
        other => panic!("a truncated shard must be refused, got {other:?}"),
    }
}

#[test]
fn a_record_whose_step_features_disagree_on_length_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 4);
    // 4 steps of action and state, but only 3 images — the desync class, inside one record.
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 7 * 4])),
        ("steps/is_first", Value::Ints(vec![1, 0, 0, 0])),
        (
            "steps/language_instruction",
            Value::Bytes(vec![b"go".to_vec(); 4]),
        ),
        (
            "steps/observation/image",
            Value::Bytes(vec![b"jpeg".to_vec(); 3]),
        ),
        ("steps/observation/state", Value::Floats(vec![0.0; 3 * 4])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                message.contains("observation/image") && message.contains("disagree"),
                "the error must name the feature that disagrees: {message}"
            );
        }
        other => panic!("a self-contradicting record must be refused, got {other:?}"),
    }
}

#[test]
fn a_feature_that_is_not_a_whole_multiple_of_its_element_size_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 4);
    // 27 floats cannot be a whole number of 7-DoF actions — the tensor is truncated mid-step, and
    // floor division would quietly report 3 steps of a 4-step episode.
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 27])),
        (
            "steps/observation/image",
            Value::Bytes(vec![b"jpeg".to_vec(); 4]),
        ),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                message.contains("steps/action") && message.contains("whole multiple"),
                "the error must name the feature and the arithmetic: {message}"
            );
        }
        other => panic!("a part-step tensor must be refused, got {other:?}"),
    }
}

#[test]
fn an_array_record_dataset_is_refused_by_name_not_misparsed() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 3);
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![1]), "array_record"),
    )
    .unwrap();

    match ingest(tmp.path()) {
        Err(IngestError::UnsupportedVersion {
            version, supported, ..
        }) => {
            assert_eq!(version.as_deref(), Some("array_record"));
            assert_eq!(supported, &["tfrecord"]);
        }
        other => panic!("a non-TFRecord backend must be refused, got {other:?}"),
    }
}

#[test]
fn the_declared_episode_total_comes_from_the_split_shard_lengths() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 2);
    // The manifest promises four episodes across two shards; the data holds three.
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![2, 2]), "tfrecord"),
    )
    .unwrap();
    let ingested = ingest(tmp.path()).unwrap();
    let declared = ingested
        .dataset
        .metadata
        .iter()
        .find(|(k, _)| k == veridex_core::cdm::META_DECLARED_EPISODES)
        .map(|(_, v)| v.as_str());
    assert_eq!(
        declared,
        Some("4"),
        "shard lengths sum to the declared total, so the truncation check has something to test"
    );
    assert_eq!(ingested.dataset.episodes.len(), 3);
}

#[test]
fn a_first_episodes_sample_reads_only_those_episodes() {
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
    assert_eq!(ingested.dataset.episodes.len(), 2);
    assert_eq!(
        ingested.report.coverage,
        Coverage::Sample {
            sample: Sample::FirstEpisodes(2),
            episodes_ingested: 2,
        },
        "a sample must never be reported as full coverage"
    );
    // Under a sample the dataset-level total is not comparable, so what is recorded is the count
    // the sample itself selected.
    let declared = ingested
        .dataset
        .metadata
        .iter()
        .find(|(k, _)| k == veridex_core::cdm::META_DECLARED_EPISODES)
        .map(|(_, v)| v.as_str());
    assert_eq!(declared, Some("2"));
}

#[test]
fn a_fraction_sample_draws_the_same_episodes_every_time() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 6, 2);
    let options = IngestOptions {
        sample: Sample::Fraction {
            fraction: 0.5,
            seed: 7,
        },
        ..IngestOptions::default()
    };
    let first = RldsAdapter
        .ingest(&Source::Local(tmp.path().to_path_buf()), &options)
        .unwrap();
    let second = RldsAdapter
        .ingest(&Source::Local(tmp.path().to_path_buf()), &options)
        .unwrap();
    let indices: Vec<u64> = first.dataset.episodes.iter().map(|e| e.index).collect();
    assert_eq!(indices.len(), 3);
    assert_eq!(
        indices,
        second
            .dataset
            .episodes
            .iter()
            .map(|e| e.index)
            .collect::<Vec<_>>(),
        "the same seed over the same episode set must draw the same episodes"
    );
}

#[test]
fn a_fraction_sample_without_declared_shard_lengths_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 4, 2);
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(None, "tfrecord"),
    )
    .unwrap();
    match RldsAdapter.ingest(
        &Source::Local(tmp.path().to_path_buf()),
        &IngestOptions {
            sample: Sample::Fraction {
                fraction: 0.5,
                seed: 1,
            },
            ..IngestOptions::default()
        },
    ) {
        Err(IngestError::SamplingUnsupported { reason, .. }) => {
            assert!(reason.contains("shard lengths"), "reason: {reason}");
        }
        other => panic!("a draw over an unknown episode set must be refused, got {other:?}"),
    }
}

#[test]
fn an_instruction_that_changes_mid_episode_becomes_a_language_label() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 4);
    let record = episode_record(
        4,
        &["pick up the block", "pick up the block", "stack it"],
        "/raw/a.h5",
        0.0,
    );
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let ingested = ingest(tmp.path()).unwrap();
    let ep = &ingested.dataset.episodes[0];
    assert_eq!(ep.task.as_deref(), Some("pick up the block"));
    let labels: Vec<(&str, Option<i64>)> =
        ep.labels.iter().map(|l| (l.value.as_str(), l.ts)).collect();
    assert_eq!(
        labels,
        vec![("stack it", Some(2))],
        "only the change is an event, and it is stamped with the step it happens at"
    );
}

#[test]
fn the_episode_source_file_becomes_upstream_provenance() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 2);
    let ingested = ingest(tmp.path()).unwrap();
    let upstream: Vec<(&ProvenanceScope, &str)> = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|p| {
            p.elements
                .iter()
                .filter(|e| e.key == "upstream")
                .map(move |e| (&p.scope, e.value.as_deref().unwrap_or("")))
        })
        .collect();
    assert_eq!(
        upstream,
        vec![
            (&ProvenanceScope::Episode(0), "/raw/ep0.h5"),
            (&ProvenanceScope::Episode(1), "/raw/ep1.h5"),
        ]
    );
    // And the manifest's license is extracted, not asserted.
    let license = ingested
        .dataset
        .provenance
        .iter()
        .flat_map(|p| &p.elements)
        .find(|e| e.key == "license")
        .unwrap();
    assert_eq!(license.value.as_deref(), Some("Apache-2.0"));
    assert_eq!(license.class, ProvenanceClass::Known);
}

#[test]
fn a_changed_step_value_changes_the_content_hash() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    write_dataset(one.path(), 1, 3);
    write_dataset(two.path(), 1, 3);
    // Same features, same shapes, same step count — one different action value. The hash has to
    // move, or a check could not bind a verdict to the content it read.
    let record_with = |action_13: f32| {
        let mut action = vec![0.0f32; 21];
        action[13] = action_13;
        example(&[
            ("steps/action", Value::Floats(action)),
            (
                "steps/observation/image",
                Value::Bytes(vec![
                    b"jpeg-0".to_vec(),
                    b"jpeg-1".to_vec(),
                    b"jpeg-2".to_vec(),
                ]),
            ),
        ])
    };
    for (dir, action_13) in [(one.path(), 0.0), (two.path(), 42.0)] {
        std::fs::write(
            dir.join("demo_rlds-train.tfrecord-00000-of-00001"),
            shard(&[record_with(action_13)]),
        )
        .unwrap();
    }

    let mut a = ingest(one.path()).unwrap().dataset;
    let mut b = ingest(two.path()).unwrap().dataset;
    a.canonicalize_order();
    b.canonicalize_order();
    assert_ne!(content_hash(&a), content_hash(&b));
}

#[test]
fn a_key_the_records_carry_but_the_manifest_never_declared_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 14])),
        (
            "steps/observation/image",
            Value::Bytes(vec![b"a".to_vec(), b"b".to_vec()]),
        ),
        // Present in the data, absent from features.json.
        ("steps/reward", Value::Floats(vec![0.0, 1.0])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let ingested = ingest(tmp.path()).unwrap();
    // A `steps/*` key is a per-step value: it is in the records, no stream represents it, and its
    // values went unread. That is a hole in the run's coverage, so it has to reach the verdict —
    // which only `unread_sources` does. (An undeclared `episode_metadata/*` key is the other case:
    // information the CDM has no shape for, costing no coverage, and ordinary in the OXE corpus.)
    assert!(
        ingested
            .report
            .unread_sources
            .iter()
            .any(|f| f.source_path.contains("steps/reward")),
        "an undeclared per-step key must be disclosed as unread: {:?}",
        ingested.report.unread_sources
    );
    // And the declared features the record does not carry are reported the other way round.
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("steps/observation/state")),
        "a declared-but-absent feature must be disclosed: {:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn a_ragged_feature_is_disclosed_rather_than_guessed_at() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let features = serde_json::json!({
        "featuresDict": {"features": {
            "steps": {"sequence": {"feature": {"featuresDict": {"features": {
                "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
                // A variable-length inner dimension: no element size, so no step count.
                "points": {"tensor": {"shape": {"dimensions": ["-1", "3"]}, "dtype": "float32"}}
            }}}}}
        }}
    });
    std::fs::write(tmp.path().join("features.json"), features.to_string()).unwrap();
    let record = example(&[("steps/action", Value::Floats(vec![0.0; 14]))]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    assert_eq!(ingested.dataset.episodes[0].streams.len(), 1);
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.source_path.contains("points") && f.note.contains("unknown")),
        "an unsizable feature must be named, never silently skipped: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn a_directory_with_no_steps_sequence_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let features = serde_json::json!({
        "featuresDict": {"features": {
            "image": {"image": {"shape": {"dimensions": ["8", "8", "3"]}, "dtype": "uint8"}}
        }}
    });
    std::fs::write(tmp.path().join("features.json"), features.to_string()).unwrap();
    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("no per-step features"), "{message}");
        }
        other => panic!("a non-RLDS TFDS dataset must be refused, got {other:?}"),
    }
}

#[test]
fn the_value_uri_uses_forward_slashes_whatever_the_platform() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let ingested = ingest(tmp.path()).unwrap();
    let uri = &ingested.dataset.episodes[0].streams[0].frames[0]
        .value_ref
        .uri;
    assert!(
        !uri.contains('\\'),
        "the uri is bound into the content hash, so it must not carry a host separator: {uri}"
    );
    assert!(
        uri.starts_with("demo_rlds-train.tfrecord-00000-of-00001#0/steps/"),
        "{uri}"
    );
}

#[test]
fn the_frame_budget_refuses_a_record_before_it_allocates() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 50);
    match RldsAdapter.ingest(
        &Source::Local(tmp.path().to_path_buf()),
        &IngestOptions {
            max_frames: Some(10),
            ..IngestOptions::default()
        },
    ) {
        Err(IngestError::FrameBudgetExceeded { limit, .. }) => assert_eq!(limit, 10),
        other => panic!("the frame budget must bound an ingest, got {other:?}"),
    }
}

#[test]
fn the_registry_detects_a_tfds_directory_unambiguously() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let source = Source::Local(tmp.path().to_path_buf());
    assert_eq!(
        RldsAdapter.detect(&source),
        Detection::Yes { version: None }
    );
    let ingested = default_registry()
        .ingest(&source, &IngestOptions::default())
        .expect("exactly one adapter must recognize a TFDS directory");
    assert_eq!(ingested.report.format_id, "rlds");
    assert_eq!(ingested.report.source_version.as_deref(), Some("tfrecord"));
}

#[test]
fn a_clean_rlds_dataset_passes_the_standard_checks_without_false_findings() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 20);
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    // Nothing about the data itself is wrong, so no structural or temporal check may fire. A
    // step-index timeline is uniform by construction: a clock Veridex cannot measure has to produce
    // silence, not accusations.
    let noise: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .map(|f| f.code.as_str())
        .filter(|code| !code.starts_with("PROVENANCE."))
        .filter(|code| *code != "TEMPORAL.UNMEASURED_CLOCK")
        // The other half of the same disclosure, in both its forms: TFDS stores no summary
        // statistics of its own (so the stored-vs-observed comparison has nothing to compare to),
        // and a `bytes_list` leaf — an image, an instruction string — is fingerprinted rather than
        // interpreted (so nothing measured it). Both say what a passing statistical result is
        // evidence of; neither accuses the data of anything.
        .filter(|code| *code != "STATISTICAL.NO_STORED_STATS")
        .filter(|code| *code != "STATISTICAL.UNMEASURED_VALUES")
        .collect();
    assert!(
        noise.is_empty(),
        "a well-formed RLDS dataset must not be accused of anything: {noise:?}"
    );
    // The provenance gaps, by contrast, are real and must be reported: RLDS records no sensor,
    // clock, calibration or annotator, and saying so is the point of the provenance score.
    let missing: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .map(|f| f.code.as_str())
        .filter(|code| code.starts_with("PROVENANCE."))
        .collect();
    assert!(
        missing.contains(&"PROVENANCE.MISSING_CLOCK"),
        "a format with no wall clock must be reported as having none: {missing:?}"
    );
}

#[test]
fn a_run_over_an_unmeasured_timeline_says_so_in_the_verdict() {
    // Without this, the temporal checks compare step indices — flawlessly monotonic, perfectly
    // regular, identical across every stream — and all of them pass. The verdict then carries zero
    // temporal findings and the certificate records ten temporal checks executed with nothing
    // skipped, which reads as "these sensors are synchronized" rather than "nothing here was ever
    // measured". The abstention has to be a finding, because a finding is the only disclosure that
    // reaches the JSON, the SARIF, the HTML, and the signed certificate.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 4, 20);
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    let unmeasured: Vec<&veridex_core::check::Finding> = out
        .verdict
        .findings
        .iter()
        .filter(|f| f.code == "TEMPORAL.UNMEASURED_CLOCK")
        .collect();
    assert_eq!(
        unmeasured.len(),
        1,
        "the clock is a property of the format, so it is one finding for the dataset — not one per \
         episode: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
    assert!(
        unmeasured[0].message.contains("rlds-step-index"),
        "the finding must name the clock: {}",
        unmeasured[0].message
    );
    // And it is informational: the dataset is not defective for the format it was published in.
    assert_eq!(unmeasured[0].severity, veridex_core::check::Severity::Info);
    assert_eq!(out.verdict.status, veridex_core::engine::Status::Pass);
}

#[test]
fn an_unmeasured_timeline_is_never_graded_as_a_duration() {
    // `Episode::duration_ns` used to fall back to a step-index span, so a 500-step episode among
    // 20-step ones was reported as "lasts 0.0 ms — 26.3x longer than the dataset median of 0.0 ms":
    // a duration outlier measured in steps and printed in milliseconds.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 4, 20);
    let long = episode_record(500, &["pick up the block"], "/raw/long.h5", 7.0);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00001-of-00002"),
        shard(&[long]),
    )
    .unwrap();
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    assert!(
        !out.verdict
            .findings
            .iter()
            .any(|f| f.code == "TEMPORAL.EPISODE_DURATION_OUTLIER"),
        "a step count is not a duration, and must not be reported as one: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| f.message.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_shard_short_of_its_declared_episode_count_is_caught_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 10);
    // The manifest promises four episodes; the shard holds three — a half-finished download.
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![4]), "tfrecord"),
    )
    .unwrap();
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "the truncation must reach the verdict: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
}

// ---- Regressions from the multi-agent audit ----

/// A features.json holding exactly the given `steps` leaves, as raw JSON values.
fn features_with(steps: serde_json::Value) -> String {
    serde_json::json!({
        "featuresDict": {"features": {
            "steps": {"sequence": {"feature": {"featuresDict": {"features": steps}}}}
        }}
    })
    .to_string()
}

#[test]
fn a_shape_whose_product_overflows_is_disclosed_rather_than_wrapped() {
    // Two 2^32 dimensions overflow u64. Unchecked, this panics in a checked build and — far worse —
    // wraps silently in a release one, dividing the serialized list by an element size that was
    // never in the file and signing the fabricated step count into the content hash.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    std::fs::write(
        tmp.path().join("features.json"),
        features_with(serde_json::json!({
            "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
            "huge": {"tensor": {"shape": {"dimensions": ["4294967296", "4294967296"]},
                                "dtype": "float32"}}
        })),
    )
    .unwrap();
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 14])),
        ("steps/huge", Value::Floats(vec![0.0; 4])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    assert_eq!(
        ingested.dataset.episodes[0].streams.len(),
        1,
        "an unsizable leaf builds no stream"
    );
    assert!(
        ingested
            .report
            .unmapped_fields
            .iter()
            .any(|f| f.source_path.contains("huge")),
        "the overflow must be disclosed: {:?}",
        ingested.report.unmapped_fields
    );
}

#[test]
fn an_encoded_tensor_is_one_blob_per_step_whatever_its_declared_shape() {
    // TFDS replaces an encoded tensor's serialized spec with a single opaque blob per step, so its
    // declared shape says nothing about how many values a step holds. `kuka` — one of the largest
    // Open X-Embodiment datasets — does exactly this, and dividing by the declared product refuses
    // the whole dataset while blaming the data.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 9);
    std::fs::write(
        tmp.path().join("features.json"),
        features_with(serde_json::json!({
            "action": {"tensor": {"shape": {"dimensions": ["3"]}, "dtype": "float32",
                                  "encoding": "none"}},
            "base_pose_tool_reached": {"tensor": {"shape": {"dimensions": ["7"]},
                                                  "dtype": "float32", "encoding": "zlib"}}
        })),
    )
    .unwrap();
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 27])),
        // 9 steps, 9 blobs — not a multiple of the declared 7.
        (
            "steps/base_pose_tool_reached",
            Value::Bytes((0..9).map(|s| format!("zlib-{s}").into_bytes()).collect()),
        ),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).expect("an encoded tensor must not be refused");
    let ep = &ingested.dataset.episodes[0];
    assert_eq!(ep.streams.len(), 2);
    for stream in &ep.streams {
        assert_eq!(stream.frames.len(), 9, "stream `{}`", stream.name);
    }
}

#[test]
fn a_manifest_declaring_an_absurd_episode_total_is_refused_not_materialized() {
    // The episode set is built from a number in a few-hundred-byte manifest, before any ingest
    // budget exists. Unbounded, `u64::MAX` is a hang and a certain OOM.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 2);
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![18_446_744_073_709_551_615]), "tfrecord"),
    )
    .unwrap();
    match RldsAdapter.ingest(
        &Source::Local(tmp.path().to_path_buf()),
        &IngestOptions {
            sample: Sample::Fraction {
                fraction: 0.5,
                seed: 1,
            },
            ..IngestOptions::default()
        },
    ) {
        Err(IngestError::SamplingUnsupported { reason, .. }) => {
            assert!(reason.contains("ceiling"), "reason: {reason}");
        }
        other => panic!("an absurd declared total must be refused, got {other:?}"),
    }
}

#[test]
fn asking_for_more_episodes_than_exist_does_not_forge_a_manifest_claim() {
    // The sample count is the user's flag, not an assertion the manifest made. Recording it as the
    // declared total makes the declared-count check fail a sound dataset — and blame "the manifest"
    // for a number the manifest never stated.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 4);
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
    assert!(
        !out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
        "a sound dataset must not fail because the sample flag was larger than it: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
    assert_eq!(out.ingested.dataset.episodes.len(), 3);
}

#[test]
fn a_declared_feature_absent_from_a_record_is_bound_into_the_hash() {
    // A dataset that lost a camera during export must not hash and certify exactly like one that
    // never declared that camera. The absent feature becomes an empty stream, which is in the CDM,
    // in the hash, and gradeable by a check.
    let with_camera = tempfile::tempdir().unwrap();
    let without = tempfile::tempdir().unwrap();
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 14])),
        (
            "steps/observation/image",
            Value::Bytes(vec![b"a".to_vec(), b"b".to_vec()]),
        ),
    ]);
    for dir in [with_camera.path(), without.path()] {
        std::fs::write(
            dir.join("dataset_info.json"),
            dataset_info_json(Some(vec![1]), "tfrecord"),
        )
        .unwrap();
        std::fs::write(
            dir.join("demo_rlds-train.tfrecord-00000-of-00001"),
            shard(std::slice::from_ref(&record)),
        )
        .unwrap();
    }
    let leaves = serde_json::json!({
        "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
        "observation": {"featuresDict": {"features": {
            "image": {"image": {"shape": {"dimensions": ["8", "8", "3"]}, "dtype": "uint8"}}
        }}}
    });
    std::fs::write(
        without.path().join("features.json"),
        features_with(leaves.clone()),
    )
    .unwrap();
    let mut declared_more = leaves;
    declared_more["observation"]["featuresDict"]["features"]["wrist_image"] = serde_json::json!(
        {"image": {"shape": {"dimensions": ["8", "8", "3"]}, "dtype": "uint8"}}
    );
    std::fs::write(
        with_camera.path().join("features.json"),
        features_with(declared_more),
    )
    .unwrap();

    let mut lost = ingest(with_camera.path()).unwrap().dataset;
    let mut never_had = ingest(without.path()).unwrap().dataset;
    lost.canonicalize_order();
    never_had.canonicalize_order();
    assert_ne!(
        content_hash(&lost),
        content_hash(&never_had),
        "a dataset that lost a declared camera must not hash like one that never had it"
    );
    let empty: Vec<&str> = lost.episodes[0]
        .streams
        .iter()
        .filter(|s| s.frames.is_empty())
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(empty, vec!["observation/wrist_image"]);

    // And it reaches the verdict, not just the CDM.
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(with_camera.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EMPTY_STREAM"),
        "the lost camera must produce a finding: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_feature_carrying_two_value_lists_is_refused_as_ambiguous() {
    // `tf.train.Feature` is a oneof. Two lists have no single meaning, and letting the last win
    // lets the same bytes yield a different step count depending on which a reader honors — the
    // very ambiguity a duplicate map key is refused for.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let mut feature = Vec::new();
    let mut bytes_list = Vec::new();
    for entry in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
        delimited(1, entry, &mut bytes_list);
    }
    delimited(1, &bytes_list, &mut feature);
    let mut int_list = Vec::new();
    delimited(1, &[0u8, 1u8], &mut int_list);
    delimited(3, &int_list, &mut feature);

    let mut entry = Vec::new();
    delimited(1, b"steps/action", &mut entry);
    delimited(2, &feature, &mut entry);
    let mut map = Vec::new();
    delimited(1, &entry, &mut map);
    let mut record = Vec::new();
    delimited(1, &map, &mut record);

    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("decodable"), "{message}");
        }
        other => panic!("an ambiguous feature must be refused, got {other:?}"),
    }
}

#[test]
fn a_tfrecord_index_sibling_is_not_mistaken_for_a_shard() {
    // TFDS writes `<shard>_index.json` next to the shards it indexes. Those names contain
    // `.tfrecord`, and feeding one to the record reader reports a healthy dataset as corrupt.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 3);
    std::fs::write(
        tmp.path()
            .join("demo_rlds-train.tfrecord-00000-of-00001_index.json"),
        r#"{"index":[]}"#,
    )
    .unwrap();
    let ingested = ingest(tmp.path()).expect("an index sibling must not be read as a shard");
    assert_eq!(ingested.dataset.episodes.len(), 2);
}

#[test]
fn every_split_in_the_directory_is_read_and_each_episode_says_which() {
    // A TFDS directory holds every split side by side, and this adapter reads them into one
    // episode-index space. `test` sorts before `train`, so episode 0 is eval data — which a user
    // must be able to see.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 3);
    std::fs::write(
        tmp.path().join("demo_rlds-test.tfrecord-00000-of-00001"),
        shard(&[episode_record(3, &["evaluate"], "/raw/eval.h5", 900.0)]),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![3]), "tfrecord"),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    assert_eq!(ingested.dataset.episodes.len(), 3);
    let split_of = |index: u64| -> Option<String> {
        ingested
            .dataset
            .provenance
            .iter()
            .find(|p| p.scope == ProvenanceScope::Episode(index))?
            .elements
            .iter()
            .find(|e| e.key == "tfds_split")?
            .value
            .clone()
    };
    assert_eq!(
        split_of(0).as_deref(),
        Some("test"),
        "episode 0 is the test split, and the CDM has to say so"
    );
    assert_eq!(split_of(1).as_deref(), Some("train"));
    assert_eq!(
        ingested
            .dataset
            .metadata
            .iter()
            .find(|(k, _)| k == "tfds_splits_ingested")
            .map(|(_, v)| v.as_str()),
        Some("test,train")
    );
}

#[test]
fn one_split_without_shard_lengths_disables_the_count_check_and_says_which() {
    // Summing only the splits that declare lengths would compare a partial total against every
    // split's episodes, reporting a complete dataset as truncated.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 2);
    let mut info: serde_json::Value =
        serde_json::from_str(&dataset_info_json(Some(vec![2]), "tfrecord")).unwrap();
    info["splits"] = serde_json::json!([
        {"name": "train", "shardLengths": ["2"]},
        {"name": "test"},
    ]);
    std::fs::write(tmp.path().join("dataset_info.json"), info.to_string()).unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    assert!(
        !ingested
            .dataset
            .metadata
            .iter()
            .any(|(k, _)| k == veridex_core::cdm::META_DECLARED_EPISODES),
        "a partial total must not be recorded as the dataset's declared count"
    );
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("test")),
        "the report must name the split that omitted its lengths: {:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn an_instruction_that_is_not_decodable_text_is_disclosed() {
    // Dropping it silently leaves an episode with no task and nothing saying why.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 14])),
        (
            "steps/language_instruction",
            Value::Bytes(vec![vec![0xff, 0xfe], vec![0xff, 0xfe]]),
        ),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let ingested = ingest(tmp.path()).unwrap();
    assert_eq!(ingested.dataset.episodes[0].task, None);
    assert!(
        ingested
            .report
            .omitted_fields
            .iter()
            .any(|f| f.contains("not decodable UTF-8")),
        "unreadable instruction bytes must be disclosed: {:?}",
        ingested.report.omitted_fields
    );
}

#[test]
fn a_float_payload_is_fingerprinted_from_its_wire_bits() {
    // Two distinct NaN bit patterns are different bytes on disk. Round-tripping through f32 can
    // quiet a signaling NaN on some targets, which would make the same dataset hash differently
    // there — so the fingerprint is taken from the wire bits, never from a float.
    let quiet = f32::from_bits(0x7FC0_0001);
    let signaling = f32::from_bits(0x7F80_0001);
    let dirs: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
    for (dir, value) in dirs.iter().zip([quiet, signaling]) {
        write_dataset(dir.path(), 1, 1);
        let record = example(&[("steps/action", Value::Floats(vec![value; 7]))]);
        std::fs::write(
            dir.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
            shard(&[record]),
        )
        .unwrap();
    }
    let mut a = ingest(dirs[0].path()).unwrap().dataset;
    let mut b = ingest(dirs[1].path()).unwrap().dataset;
    a.canonicalize_order();
    b.canonicalize_order();
    assert_ne!(
        content_hash(&a),
        content_hash(&b),
        "two different NaN payloads are two different files"
    );
}

#[test]
fn a_shard_whose_records_carry_no_declared_feature_is_not_a_clean_pass() {
    // Every declared feature missing from every record used to yield episodes with zero streams and
    // a clean verdict over a dataset holding no data.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let record = example(&[("steps/unrelated", Value::Floats(vec![0.0, 1.0]))]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .unwrap();
    assert!(
        out.verdict
            .findings
            .iter()
            .any(|f| f.code == "STRUCTURAL.EMPTY_STREAM"),
        "an episode holding none of its declared data must not read as clean: {:?}",
        out.verdict
            .findings
            .iter()
            .map(|f| &f.code)
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_unselected_records_payload_is_stepped_over_rather_than_read() {
    // Sampling is meant to make checking a huge dataset cheap, and real Open X-Embodiment shards run
    // to hundreds of megabytes. An unselected record is framed and its length prefix verified, then
    // seeked past — so its payload is never read. The observable consequence, and the honest limit:
    // a corrupt payload in a record the sample skipped does not fail the run, because nothing looked
    // at it.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 4);
    let path = tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001");
    let mut bytes = std::fs::read(&path).unwrap();
    // Corrupt deep inside the *last* record, which only a full run reaches.
    let at = bytes.len() - 40;
    bytes[at] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let sampled = RldsAdapter
        .ingest(
            &Source::Local(tmp.path().to_path_buf()),
            &IngestOptions {
                sample: Sample::FirstEpisodes(1),
                ..IngestOptions::default()
            },
        )
        .expect("a record the sample never selected must not be read, so it cannot fail the run");
    assert_eq!(sampled.dataset.episodes.len(), 1);

    // A full run does read it, and does catch it.
    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("checksum"), "{message}");
        }
        other => panic!("a full run must still verify every payload, got {other:?}"),
    }
}

#[test]
fn a_shard_that_ends_mid_payload_is_refused() {
    // The streaming reader must not treat a short read as a clean end of file.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 6);
    let path = tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001");
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(
                message.contains("past the end") || message.contains("mid-record"),
                "{message}"
            );
        }
        other => panic!("a shard cut mid-payload must be refused, got {other:?}"),
    }
}

/// A `float_list` written **unpacked** — one wire-type-5 field per value. Real writers emit packed,
/// but the wire format permits this and a reader that only handled packed would silently see zero
/// values, which divides into a zero-step episode.
fn unpacked_floats(values: &[f32]) -> Vec<u8> {
    let mut list = Vec::new();
    for v in values {
        tag(1, 5, &mut list);
        list.extend_from_slice(&v.to_le_bytes());
    }
    let mut feature = Vec::new();
    delimited(2, &list, &mut feature);
    feature
}

/// An `int64_list` written **unpacked** — one wire-type-0 varint per value.
fn unpacked_ints(values: &[i64]) -> Vec<u8> {
    let mut list = Vec::new();
    for v in values {
        tag(1, 0, &mut list);
        varint(*v as u64, &mut list);
    }
    let mut feature = Vec::new();
    delimited(3, &list, &mut feature);
    feature
}

/// A record built from pre-encoded `tf.train.Feature` bodies, for the wire shapes the normal
/// fixture writer cannot produce.
fn record_from_features(features: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut map = Vec::new();
    for (key, feature) in features {
        let mut entry = Vec::new();
        delimited(1, key.as_bytes(), &mut entry);
        delimited(2, feature, &mut entry);
        delimited(1, &entry, &mut map);
    }
    let mut out = Vec::new();
    delimited(1, &map, &mut out);
    out
}

#[test]
fn unpacked_value_lists_are_read_the_same_as_packed_ones() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    std::fs::write(
        tmp.path().join("features.json"),
        features_with(serde_json::json!({
            "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
            "is_first": {"tensor": {"shape": {}, "dtype": "bool"}}
        })),
    )
    .unwrap();
    let record = record_from_features(&[
        ("steps/action", unpacked_floats(&[0.5f32; 21])),
        ("steps/is_first", unpacked_ints(&[1, 0, 0])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    let ep = &ingested.dataset.episodes[0];
    assert_eq!(ep.streams.len(), 2);
    for stream in &ep.streams {
        assert_eq!(
            stream.frames.len(),
            3,
            "unpacked `{}` must yield the same step count as packed would",
            stream.name
        );
    }
}

#[test]
fn a_record_repeating_a_feature_key_is_refused_as_ambiguous() {
    // Two entries for one key have no single meaning; picking a winner would let the same bytes
    // produce two different step counts depending on which a reader honors.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    let mut a = Vec::new();
    delimited(1, b"steps/action", &mut a);
    delimited(2, &Value::Floats(vec![0.0; 14]).encode(), &mut a);
    let mut b = Vec::new();
    delimited(1, b"steps/action", &mut b);
    delimited(2, &Value::Floats(vec![0.0; 7]).encode(), &mut b);
    let mut map = Vec::new();
    delimited(1, &a, &mut map);
    delimited(1, &b, &mut map);
    let mut record = Vec::new();
    delimited(1, &map, &mut record);

    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();
    match ingest(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("decodable"), "{message}");
        }
        other => panic!("a repeated feature key must be refused, got {other:?}"),
    }
}

#[test]
fn a_class_label_leaf_becomes_an_int64_stream() {
    // `ClassLabel` carries only `num_classes` in the proto — no dtype — so the adapter must supply
    // the int64 TFDS actually serializes rather than reading a field that is not there.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 2);
    std::fs::write(
        tmp.path().join("features.json"),
        features_with(serde_json::json!({
            "action": {"tensor": {"shape": {"dimensions": ["7"]}, "dtype": "float32"}},
            "outcome": {"classLabel": {"numClasses": "3"}}
        })),
    )
    .unwrap();
    let record = example(&[
        ("steps/action", Value::Floats(vec![0.0; 21])),
        ("steps/outcome", Value::Ints(vec![0, 1, 2])),
    ]);
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[record]),
    )
    .unwrap();

    let ingested = ingest(tmp.path()).unwrap();
    let outcome = ingested.dataset.episodes[0]
        .streams
        .iter()
        .find(|s| s.name == "outcome")
        .expect("the class-label leaf becomes a stream");
    assert_eq!(outcome.dtype.as_deref(), Some("int64"));
    assert_eq!(outcome.frames.len(), 3);
}

#[test]
fn a_sampled_run_reads_only_the_records_it_selected() {
    // Not just "returns 2 episodes" — the earlier version of this assertion would have passed if the
    // adapter parsed all five records and threw three away. What proves it read only two is that a
    // record it did not select can be unreadable without affecting the run.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 5, 3);
    let path = tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001");
    let mut bytes = std::fs::read(&path).unwrap();
    let at = bytes.len() - 30;
    bytes[at] ^= 0x01;
    std::fs::write(&path, &bytes).unwrap();

    let ingested = RldsAdapter
        .ingest(
            &Source::Local(tmp.path().to_path_buf()),
            &IngestOptions {
                sample: Sample::FirstEpisodes(2),
                ..IngestOptions::default()
            },
        )
        .expect("records past the sample must never be read");
    assert_eq!(ingested.dataset.episodes.len(), 2);
    assert_eq!(
        ingested.report.coverage,
        Coverage::Sample {
            sample: Sample::FirstEpisodes(2),
            episodes_ingested: 2,
        }
    );
}

/// The same rule as `asking_for_more_episodes_than_exist_does_not_forge_a_manifest_claim`, on the
/// path that test could not reach: its fixture always writes `shardLengths`, so it only exercised
/// the *clamped* branch. With no declared total there is nothing to clamp against, `want` stayed at
/// the operator's `n`, and a manifest that never stated a number was reported as declaring 10.
#[test]
fn a_sample_does_not_forge_a_count_when_the_manifest_declares_none() {
    for info in [
        // No `splits` key at all.
        dataset_info_json(None, "tfrecord"),
        // Two splits, one without `shardLengths` — a shape the adapter supports by name.
        serde_json::json!({
            "name": "demo_rlds",
            "version": "0.1.0",
            "fileFormat": "tfrecord",
            "moduleName": "tensorflow_datasets.robotics.demo_rlds",
            "citation": "@article{demo}",
            "redistributionInfo": {"license": "Apache-2.0"},
            "splits": [{"name": "train", "shardLengths": ["3"]}, {"name": "test"}],
        })
        .to_string(),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        write_dataset(tmp.path(), 3, 4);
        std::fs::write(tmp.path().join("dataset_info.json"), &info).unwrap();

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

        assert!(
            !out.verdict
                .findings
                .iter()
                .any(|f| f.code == "STRUCTURAL.EPISODE_COUNT_MISMATCH"),
            "the manifest declared no total, so nothing may be compared against one: {:?}",
            out.verdict
                .findings
                .iter()
                .map(|f| &f.code)
                .collect::<Vec<_>>()
        );
        assert_eq!(out.ingested.dataset.episodes.len(), 3);
    }
}

/// Protobuf merges repeated instances of an embedded message field, so TensorFlow concatenates the
/// two `float_list`s in a map entry that carries `value` twice. Letting the last one win made the
/// same shard bytes yield a different step count depending on who read them — and Veridex signed
/// its own answer. Refused, so the two can never disagree.
#[test]
fn a_map_entry_carrying_its_value_twice_is_refused() {
    // key = "steps/action", then field 2 (value) twice: 7 floats, then 14 floats.
    let mut entry: Vec<u8> = Vec::new();
    entry.push(0x0a); // field 1, wire 2
    let key = b"steps/action";
    entry.push(key.len() as u8);
    entry.extend_from_slice(key);
    for count in [7usize, 14usize] {
        let mut float_list = Vec::new();
        float_list.push(0x0a); // FloatList.value, packed
        float_list.push((count * 4) as u8);
        for i in 0..count {
            float_list.extend_from_slice(&(i as f32).to_le_bytes());
        }
        let mut feature = Vec::new();
        feature.push(0x12); // Feature.float_list = field 2
        feature.push(float_list.len() as u8);
        feature.extend_from_slice(&float_list);

        entry.push(0x12); // map entry field 2 (value)
        entry.push(feature.len() as u8);
        entry.extend_from_slice(&feature);
    }

    let mut features = Vec::new();
    features.push(0x0a); // Features.feature map entry
    features.push(entry.len() as u8);
    features.extend_from_slice(&entry);

    let mut example = Vec::new();
    example.push(0x0a); // Example.features
    example.push(features.len() as u8);
    example.extend_from_slice(&features);

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("features.json"), features_json()).unwrap();
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![1]), "tfrecord"),
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001"),
        shard(&[example]),
    )
    .unwrap();

    let err = default_registry().ingest(
        &Source::Local(tmp.path().to_path_buf()),
        &IngestOptions::default(),
    );
    assert!(
        err.is_err(),
        "an ambiguous record must be refused, not resolved by last-wins"
    );
}

#[test]
fn shards_are_read_in_numeric_order_not_lexicographic() {
    // A shard set written `-0-of-12` … `-11-of-12` rather than TFDS's zero-padded form. Episodes are
    // numbered by a counter running over the shards in read order, so reading `-10-of-12` third
    // renumbers every episode after it — the same recording described with different episode indices
    // depending on how its shards happened to be named, and a different content hash with it.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("features.json"), features_json()).unwrap();
    std::fs::write(
        dir.join("dataset_info.json"),
        dataset_info_json(Some(vec![12]), "tfrecord"),
    )
    .unwrap();
    // Shard k holds one episode whose recorded source file names k, so where each episode landed is
    // recoverable from the CDM rather than assumed.
    for k in 0..12usize {
        let record = episode_record(
            3,
            &["pick up the block"],
            &format!("/raw/ep{k}.h5"),
            k as f32 * 100.0,
        );
        std::fs::write(
            dir.join(format!("demo_rlds-train.tfrecord-{k}-of-12")),
            shard(&[record]),
        )
        .unwrap();
    }

    let d = ingest(dir).expect("ingest").dataset;
    assert_eq!(d.episodes.len(), 12);
    // Episode k must be the one from shard k.
    for (k, ep) in d.episodes.iter().enumerate() {
        assert_eq!(ep.index, k as u64);
        let uri = &ep.streams[0].frames[0].value_ref.uri;
        assert!(
            uri.starts_with(&format!("demo_rlds-train.tfrecord-{k}-of-12")),
            "episode {k} came from the wrong shard: {uri}"
        );
    }
}

// ---- Reading the manifest alone ----

fn metadata_only(dir: &Path) -> Result<veridex_core::adapter::Ingested, IngestError> {
    RldsAdapter.ingest(
        &Source::Local(dir.to_path_buf()),
        &IngestOptions {
            metadata_only: true,
            ..IngestOptions::default()
        },
    )
}

#[test]
fn the_manifest_alone_yields_the_declared_episodes_and_features() {
    // The point of the mode: an Open X-Embodiment directory is hundreds of gigabytes and its two
    // manifest files are a few kilobytes. What they answer — how many episodes, which features,
    // with what dtypes and shapes, under what licence — is answered without opening a shard.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 4);
    // Deleting the shard proves the claim rather than asserting it: a run that opened one now fails.
    std::fs::remove_file(tmp.path().join("demo_rlds-train.tfrecord-00000-of-00001")).unwrap();

    let out = metadata_only(tmp.path()).expect("a manifest-only ingest");
    assert_eq!(
        out.report.coverage,
        Coverage::MetadataOnly {
            episodes_declared: 3
        }
    );
    assert_eq!(out.dataset.episodes.len(), 3);
    // Every declared per-step feature is a stream, and every stream is empty by request.
    let full = ingest(tmp.path());
    assert!(full.is_err(), "the shard really is gone: {full:?}");
    let names: Vec<&str> = out.dataset.episodes[0]
        .streams
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains(&"action"), "{names:?}");
    assert!(names.contains(&"observation/image"), "{names:?}");
    assert!(out
        .dataset
        .episodes
        .iter()
        .all(|e| e.streams.iter().all(|s| s.frames.is_empty())));
    // RLDS declares episodes per shard, never steps per episode. Nothing may invent one.
    assert!(out
        .dataset
        .episodes
        .iter()
        .all(|e| e.declared_frame_count.is_none()));
    // The licence off `redistributionInfo` is real provenance and survives.
    assert!(out.dataset.provenance.iter().any(|p| p
        .elements
        .iter()
        .any(|e| e.key == "license" && e.value.as_deref() == Some("Apache-2.0"))));
    assert_eq!(out.dataset.id, "demo_rlds");
}

#[test]
fn a_manifest_with_no_shard_lengths_is_refused_not_read_as_an_empty_dataset() {
    // Without shard lengths there is no episode set, and this mode cannot go and find one. An empty
    // dataset would score a clean 100 on a catalog that measured nothing.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("features.json"), features_json()).unwrap();
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(None, "tfrecord"),
    )
    .unwrap();

    match metadata_only(tmp.path()) {
        Err(IngestError::Parse { message, .. }) => {
            assert!(message.contains("shard lengths"), "{message}");
            assert!(message.contains("full check"), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn the_declared_episode_count_is_not_reported_as_a_check_that_passed() {
    // The episode set is derived from the declared total, so comparing the two is `n == n`. Left out
    // of the CDM entirely, and said out loud, rather than passing a check that could not fail.
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 3, 4);
    let out = metadata_only(tmp.path()).unwrap();
    assert!(out
        .dataset
        .metadata
        .iter()
        .all(|(k, _)| k != veridex_core::cdm::META_DECLARED_EPISODES));
    assert!(
        out.report
            .omitted_fields
            .iter()
            .any(|f| f.contains("declared episode-count check")),
        "{:?}",
        out.report.omitted_fields
    );
}

#[test]
fn a_manifest_declaring_an_absurd_episode_total_is_refused_before_it_is_allocated() {
    // `"shardLengths": ["100000000"]` is a hundred-byte file. Against the declared features it asks
    // for hundreds of millions of streams, and no frame budget fires on a run that reads no frame —
    // so the product is charged before the first `Stream` is built.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("features.json"), features_json()).unwrap();
    std::fs::write(
        tmp.path().join("dataset_info.json"),
        dataset_info_json(Some(vec![100_000_000]), "tfrecord"),
    )
    .unwrap();

    match metadata_only(tmp.path()) {
        Err(IngestError::FrameBudgetExceeded { .. }) => {}
        other => panic!("expected the budget to refuse it, got {other:?}"),
    }
}

/// The values in a TFRecord are already decoded — parsing the `tf.train.Example` into typed lists is
/// what produces the per-step fingerprints — so the statistical family can grade them. Throwing the
/// numbers away after hashing them left it abstaining on the largest public robot corpus there is.
#[test]
fn numeric_step_values_are_measured_and_byte_payloads_are_not() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 1, 20);
    let d = default_registry()
        .ingest(
            &Source::Local(tmp.path().to_path_buf()),
            &IngestOptions::default(),
        )
        .expect("ingest")
        .dataset;
    let stream = |name: &str| {
        d.episodes[0]
            .streams
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("stream {name}"))
    };

    let action = stream("action");
    let stats = action
        .observed_stats
        .expect("a float_list leaf is measured");
    assert!(stats.min.is_finite() && stats.max >= stats.min);
    assert_eq!(
        action.observed_non_finite,
        Some(0),
        "read and finite, which is a different answer from never read"
    );
    assert!(
        action.observed_dim_stats.is_some(),
        "a 7-DoF action is measured per dimension, so a spike in joint 6 is not hidden behind joint 0"
    );

    // A `bytes_list` leaf is an image or a string: fingerprinted, never interpreted, and it must say
    // so rather than reporting statistics of bytes.
    let image = stream("observation/image");
    assert!(image.observed_stats.is_none() && image.observed_non_finite.is_none());
}

/// `is_first` and `is_last` are on every step of every RLDS episode — 1 once and 0 for the rest — so
/// grading a two-state channel as a saturating actuator accused every well-formed dataset in the
/// corpus of two defects it does not have.
#[test]
fn the_boolean_step_flags_are_not_reported_as_saturated() {
    let tmp = tempfile::tempdir().unwrap();
    write_dataset(tmp.path(), 2, 20);
    let out = veridex_core::pipeline::run_check(
        &default_registry(),
        &Source::Local(tmp.path().to_path_buf()),
        None,
        &IngestOptions::default(),
    )
    .expect("the run completes");
    let saturated: Vec<&str> = out
        .verdict
        .findings
        .iter()
        .filter(|f| f.code == "STATISTICAL.SATURATED")
        .map(|f| f.message.as_str())
        .collect();
    assert!(saturated.is_empty(), "{saturated:?}");
}
