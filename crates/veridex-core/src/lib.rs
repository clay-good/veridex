//! # veridex-core
//!
//! The Rust core of [Veridex](https://github.com/clay-good/veridex) — the open, cross-format trust
//! layer for physical-AI data.
//!
//! This crate owns the [Canonical Dataset Model](cdm) (the neutrality substrate every adapter
//! populates and every check reads) and its [deterministic content hashing](canonical). Adapters,
//! the validation engine, and the trust certificate build on top of these.
//!
//! Determinism is a first-class contract: the same dataset bytes and the same Veridex version
//! always canonicalize to the same [`ContentHash`].
//!
//! ```no_run
//! use veridex_core::{default_registry, run_check, status_label, IngestOptions, Source};
//!
//! let registry = default_registry();
//! let source = Source::Local("my-dataset".into());
//! let out = run_check(&registry, &source, None, &IngestOptions::default())?;
//! println!(
//!     "{} ({}) — {}",
//!     out.trust.score,
//!     out.trust.grade.letter(),
//!     status_label(out.verdict.status)
//! );
//! # Ok::<(), veridex_core::IngestError>(())
//! ```

#![forbid(unsafe_code)]

pub mod adapter;
pub mod canonical;
pub mod cdm;
pub mod certificate;
pub mod check;
pub mod checks;
pub mod config;
pub mod diff;
pub mod effective;
pub mod emit;
pub mod engine;
pub mod media;
pub mod pipeline;
pub mod profile;
pub mod redact;
pub mod remote;
pub mod report;
pub mod scenario;
pub mod simref;
pub mod watch;

pub use adapter::lerobot::LeRobotAdapter;
pub use adapter::mcap::McapAdapter;
pub use adapter::{
    default_registry, Adapter, AdapterRegistry, Coverage, Detection, IngestError, IngestOptions,
    IngestReport, Ingested, Sample, Source, UnmappedField,
};
pub use canonical::{content_hash, ContentHash, CANONICAL_VERSION};
pub use certificate::{
    conflicts, readiness_verdict, render_label, render_readiness, render_verified, score, sign,
    sign_attestation, status_label, verified_json, verify, verify_attestation, AttestError,
    Attestation, AttestationConflict, AttestedElement, CertError, Certificate, Grade, Issuance,
    ProvenanceCoverage, SignedAttestation, SignedCertificate, SigningKeypair, TrustScore, Verified,
    Zeroizing, ATTESTATION_SCHEMA_VERSION, RUBRIC_VERSION,
};
pub use check::{Category, Check, Finding, Location, Scope, Severity};
pub use config::{CheckConfig, ConfigError, FailOn};
pub use diff::{diff_reports, is_report_shaped, render_diff, render_diff_json, ReportDiff};
pub use effective::{
    render_effective_config, render_effective_config_json, EFFECTIVE_CONFIG_SCHEMA_VERSION,
};
pub use emit::{
    render_provenance, render_provenance_attested, to_croissant, to_croissant_attested, to_prov,
    to_prov_attested,
};
pub use engine::{
    AppliedAttestation, CheckInfo, CoverageNote, Engine, RegistryError, RunConfig, Status,
    Tolerances, Verdict,
};
pub use pipeline::{check_ingested, run_check, run_check_attested, run_check_with, CheckOutput};
pub use redact::{Redactor, REDACTION_CHECK_ID, REDACTION_CODE};
pub use report::{
    render_catalog_json, render_html, render_html_with_readiness, render_inspect_json, render_json,
    render_json_with_readiness, render_sarif, render_sarif_with_readiness, render_terminal,
    render_terminal_with, rollups, EpisodeTally, FindingDetail, Rollups, SeverityTally,
    StreamRollup, INSPECT_SCHEMA_VERSION, REPORT_SCHEMA_VERSION,
};
pub use watch::{fingerprint, fingerprint_within, MAX_WATCH_ENTRIES};

/// The version of `veridex-core`, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdm::*;

    fn sample_dataset() -> Dataset {
        Dataset {
            id: "acme/pick-place".into(),
            calibration: None,
            metadata: vec![
                ("robot".into(), "so100".into()),
                ("source".into(), "lerobot-v3".into()),
            ],
            provenance: vec![Provenance {
                scope: ProvenanceScope::Dataset,
                elements: vec![
                    ProvenanceElement {
                        key: "license".into(),
                        value: Some("apache-2.0".into()),
                        class: ProvenanceClass::Known,
                    },
                    ProvenanceElement {
                        key: "annotator".into(),
                        value: None,
                        class: ProvenanceClass::Unknown,
                    },
                ],
            }],
            episodes: vec![Episode {
                index: 0,
                start_ts: Some(1_000),
                end_ts: Some(2_000),
                streams: vec![
                    Stream {
                        name: "observation.images.top".into(),
                        modality: Modality::Video,
                        declared_rate_hz: Some(30.0),
                        clock_id: "camera".into(),
                        clock_kind: ClockKind::Measured,
                        dtype: Some("video".into()),
                        shape: Some(vec![3, 480, 640]),
                        dim_names: None,
                        stats: None,
                        dim_stats: None,
                        observed_stats: None,
                        observed_saturation: None,
                        observed_non_finite: None,
                        observed_dim_stats: None,
                        latched: None,
                        declared_range: None,
                        point_fields: None,
                        observed_point_counts: None,
                        media: None,
                        frame_id: None,
                        frames: vec![
                            Frame {
                                ts: 1_000,
                                value_ref: ValueRef {
                                    uri: "videos/ep0_top.mp4".into(),
                                    byte_offset: Some(0),
                                    byte_len: Some(1024),
                                    content_hash: None,
                                },
                            },
                            Frame {
                                ts: 1_033,
                                value_ref: ValueRef {
                                    uri: "videos/ep0_top.mp4".into(),
                                    byte_offset: Some(1024),
                                    byte_len: Some(1024),
                                    content_hash: None,
                                },
                            },
                        ],
                    },
                    Stream {
                        name: "observation.state".into(),
                        modality: Modality::ScalarState,
                        declared_rate_hz: Some(50.0),
                        clock_id: "robot".into(),
                        clock_kind: ClockKind::Measured,
                        dtype: Some("float32".into()),
                        shape: Some(vec![6]),
                        dim_names: None,
                        stats: None,
                        dim_stats: None,
                        observed_stats: None,
                        observed_saturation: None,
                        observed_non_finite: None,
                        observed_dim_stats: None,
                        latched: None,
                        declared_range: None,
                        point_fields: None,
                        observed_point_counts: None,
                        media: None,
                        frame_id: None,
                        frames: vec![Frame {
                            ts: 1_000,
                            value_ref: ValueRef {
                                uri: "data/ep0.parquet".into(),
                                byte_offset: None,
                                byte_len: None,
                                content_hash: Some([7u8; 32]),
                            },
                        }],
                    },
                ],
                task: Some("pick up the cube".into()),
                labels: vec![Label {
                    key: "language".into(),
                    value: "pick up the cube".into(),
                    ts: None,
                }],
                ego_poses: None,
                ego_frame: None,
                declared_frame_count: None,
            }],
        }
    }

    #[test]
    fn hashing_is_deterministic() {
        let d = sample_dataset();
        assert_eq!(content_hash(&d), content_hash(&d));
    }

    #[test]
    fn hash_is_stable_across_clones() {
        let d = sample_dataset();
        let d2 = d.clone();
        assert_eq!(content_hash(&d), content_hash(&d2));
    }

    #[test]
    fn reordering_order_insensitive_collections_does_not_change_hash() {
        let d = sample_dataset();
        let base = content_hash(&d);

        // reorder episodes' streams
        let mut d_streams = d.clone();
        d_streams.episodes[0].streams.reverse();
        assert_eq!(
            content_hash(&d_streams),
            base,
            "stream order must not matter"
        );

        // reorder metadata
        let mut d_meta = d.clone();
        d_meta.metadata.reverse();
        assert_eq!(
            content_hash(&d_meta),
            base,
            "metadata order must not matter"
        );

        // reorder provenance elements
        let mut d_prov = d.clone();
        d_prov.provenance[0].elements.reverse();
        assert_eq!(
            content_hash(&d_prov),
            base,
            "provenance element order must not matter"
        );
    }

    #[test]
    fn provenance_hash_is_permutation_insensitive_with_ties() {
        // Two records in the same scope, and two elements sharing a key — the cases where sorting by
        // scope-alone / key-alone would leave a non-total order and let a permutation change the hash.
        let el = |k: &str, v: &str, c: ProvenanceClass| ProvenanceElement {
            key: k.into(),
            value: Some(v.into()),
            class: c,
        };
        let mk = |order: bool| Dataset {
            id: "t".into(),
            calibration: None,
            metadata: vec![],
            provenance: {
                let a = Provenance {
                    scope: ProvenanceScope::Dataset,
                    elements: vec![
                        el("sensor", "cam-a", ProvenanceClass::Known),
                        el("sensor", "cam-b", ProvenanceClass::Asserted),
                    ],
                };
                let b = Provenance {
                    scope: ProvenanceScope::Dataset,
                    elements: vec![el("license", "mit", ProvenanceClass::Known)],
                };
                if order {
                    vec![a, b]
                } else {
                    vec![b, a]
                }
            },
            episodes: vec![],
        };
        // Also reverse the tied elements inside the first record.
        let mut swapped = mk(false);
        swapped.provenance[1].elements.reverse();
        assert_eq!(
            content_hash(&mk(true)),
            content_hash(&swapped),
            "provenance record and element order must not change the hash, even with ties"
        );
    }

    #[test]
    fn changing_a_timestamp_changes_the_hash() {
        let d = sample_dataset();
        let base = content_hash(&d);
        let mut d2 = d.clone();
        d2.episodes[0].streams[0].frames[0].ts += 1;
        assert_ne!(content_hash(&d2), base);
    }

    #[test]
    fn changing_a_label_timestamp_changes_the_hash() {
        // A timestamped annotation's time is data: retiming it must change the content hash, so a
        // certificate binds to the annotation timeline it was issued against.
        let d = sample_dataset();
        let base = content_hash(&d);
        let mut d2 = d.clone();
        d2.episodes[0].labels[0].ts = Some(42);
        assert_ne!(content_hash(&d2), base);
    }

    /// A populated [`Media`] to mutate one field of at a time in the hash-drift guard.
    fn base_media() -> Media {
        Media {
            uri: "videos/cam/episode_000000.mp4".into(),
            declared: MediaParams {
                codec: Some("h264".into()),
                width: Some(640),
                height: Some(480),
                fps: Some(30.0),
            },
            status: MediaStatus::Read,
            observed: MediaParams {
                codec: Some("avc1".into()),
                width: Some(640),
                height: Some(480),
                fps: Some(30.0),
            },
            frame_count: Some(100),
        }
    }

    #[test]
    fn the_reason_a_container_would_not_parse_is_deliberately_out_of_the_hash() {
        // The status *variant* binds (a readable stream and an unreadable one are different data);
        // the prose explaining why does not. It is built partly from the operating system's own
        // error text, which differs by platform, and it is reworded whenever a message is improved —
        // so hashing it would break "same bytes, same hash on any platform" and would turn every
        // wording fix into a silent hash change. This test is the record of that decision.
        let mut a = sample_dataset();
        let mut b = sample_dataset();
        let unreadable = |reason: &str| Media {
            status: MediaStatus::Unreadable {
                reason: reason.into(),
            },
            observed: MediaParams::default(),
            frame_count: None,
            ..base_media()
        };
        a.episodes[0].streams[0].media = Some(unreadable("no moov box"));
        b.episodes[0].streams[0].media = Some(unreadable("cannot open: PermissionDenied"));
        assert_eq!(content_hash(&a), content_hash(&b));

        // But the variant itself still separates them.
        let mut readable = sample_dataset();
        readable.episodes[0].streams[0].media = Some(base_media());
        assert_ne!(content_hash(&a), content_hash(&readable));
    }

    #[test]
    fn every_stream_field_binds_into_the_hash() {
        // Guards against the hand-written canonical encoder drifting from the `Stream` struct: each
        // content-bearing field, when changed, must change the content hash. `dim_stats` in
        // particular is stored source content (parallel to `stats`); omitting it once let two
        // datasets with different corrupted per-joint stats collide.
        //
        // The table covered the fields the encoder had *most recently* gained and none of the ones
        // it started with, so deleting `clock_kind` from the encoder — the change `CANONICAL_VERSION`
        // was last bumped *for* — passed the whole suite. A `Stream` is destructured below so a new
        // field is a compile error here rather than a silent omission.
        let stats = StreamStats {
            min: -1.0,
            max: 1.0,
            mean: 0.0,
            std: 0.5,
        };
        let dims = vec![DimStats { dim: 0, stats }];
        let sat = Saturation {
            sample_count: 10,
            at_min: 2,
            at_max: 3,
            min: -1.0,
            max: 1.0,
            dim: 0,
        };
        let base = content_hash(&sample_dataset());
        type Mutator = fn(&mut Stream);
        let mutate: [(&str, Mutator); 31] = [
            // The fields the encoder has carried from the beginning. Absent from this table until a
            // mutation audit deleted `clock_kind` from `encode` and watched 692 tests pass: a
            // stream's frames are a synchronized rig under one value and an unmeasurable timeline
            // under the other, so two datasets that grade differently hashed identically.
            ("name", |s| s.name = "observation.state.v2".into()),
            ("modality", |s| s.modality = Modality::Audio),
            ("declared_rate_hz", |s| s.declared_rate_hz = Some(19.5)),
            ("clock_id", |s| s.clock_id = "gps".into()),
            ("clock_kind", |s| s.clock_kind = ClockKind::StepIndex),
            ("dtype", |s| s.dtype = Some("int16".into())),
            ("shape", |s| s.shape = Some(vec![3, 4, 5])),
            ("stats", |s| {
                s.stats = Some(StreamStats {
                    min: -7.0,
                    max: 7.0,
                    mean: 0.7,
                    std: 1.7,
                })
            }),
            ("point_fields", |s| {
                s.point_fields = Some(vec![
                    crate::cdm::PointField {
                        name: "x".into(),
                        dtype: Some("float32".into()),
                    },
                    crate::cdm::PointField {
                        name: "intensity".into(),
                        dtype: Some("uint16".into()),
                    },
                ])
            }),
            // The sensor's coordinate frame: `autonomy.sensor-frame-resolution` fails a stream on it,
            // so a rig whose LiDAR names a frame the TF tree relates and one whose LiDAR does not
            // must not hash alike.
            ("frame_id", |s| s.frame_id = Some("lidar_top_v2".into())),
            ("dim_stats", |s| {
                s.dim_stats = Some(vec![DimStats {
                    dim: 0,
                    stats: StreamStats {
                        min: -2.0,
                        max: 2.0,
                        mean: 0.1,
                        std: 0.9,
                    },
                }])
            }),
            ("observed_stats", |s| {
                s.observed_stats = Some(StreamStats {
                    min: -3.0,
                    max: 3.0,
                    mean: 0.2,
                    std: 1.1,
                })
            }),
            ("observed_saturation", |s| {
                s.observed_saturation = Some(Saturation {
                    sample_count: 9,
                    at_min: 1,
                    at_max: 1,
                    min: -3.0,
                    max: 3.0,
                    dim: 1,
                })
            }),
            ("observed_non_finite", |s| s.observed_non_finite = Some(4)),
            ("observed_dim_stats", |s| {
                s.observed_dim_stats = Some(vec![DimStats {
                    dim: 1,
                    stats: StreamStats {
                        min: -4.0,
                        max: 4.0,
                        mean: 0.3,
                        std: 1.3,
                    },
                }])
            }),
            // The media file behind a video stream. Every field of it binds: the `video.*` checks
            // fail a stream on the disagreement between the declared and the observed halves, and a
            // re-encode changes nothing else in the CDM — so a sound export and a broken one must
            // not collide. The one deliberate exclusion is the `Unreadable` *reason* prose (see
            // `canonical.rs`); the status variant itself binds, and is covered below.
            ("media", |s| s.media = Some(base_media())),
            ("media.uri", |s| {
                s.media = Some(Media {
                    uri: "videos/cam/episode_000009.mp4".into(),
                    ..base_media()
                })
            }),
            ("media.declared.codec", |s| {
                let mut m = base_media();
                m.declared.codec = Some("hevc".into());
                s.media = Some(m);
            }),
            ("media.declared.width", |s| {
                let mut m = base_media();
                m.declared.width = Some(1280);
                s.media = Some(m);
            }),
            ("media.declared.height", |s| {
                let mut m = base_media();
                m.declared.height = Some(720);
                s.media = Some(m);
            }),
            ("media.declared.fps", |s| {
                let mut m = base_media();
                m.declared.fps = Some(60.0);
                s.media = Some(m);
            }),
            ("media.status", |s| {
                let mut m = base_media();
                m.status = MediaStatus::Missing;
                s.media = Some(m);
            }),
            ("media.observed.codec", |s| {
                let mut m = base_media();
                m.observed.codec = Some("vp09".into());
                s.media = Some(m);
            }),
            ("media.observed.width", |s| {
                let mut m = base_media();
                m.observed.width = Some(320);
                s.media = Some(m);
            }),
            ("media.observed.height", |s| {
                let mut m = base_media();
                m.observed.height = Some(240);
                s.media = Some(m);
            }),
            ("media.observed.fps", |s| {
                let mut m = base_media();
                m.observed.fps = Some(29.97);
                s.media = Some(m);
            }),
            ("media.frame_count", |s| {
                let mut m = base_media();
                m.frame_count = Some(97);
                s.media = Some(m);
            }),
            // Three checks abstain on a latched stream, so the same frames reach opposite verdicts
            // under `Some(true)` and `None` — and a certificate over the clean reading must not
            // verify the other.
            ("latched", |s| s.latched = Some(true)),
            // The source's name for each dimension. Source content, sitting in the same manifest
            // entry as `dtype` and `shape`, and what a statistical finding calls the joint it
            // reports — two datasets whose reports name different joints must not hash alike.
            ("dim_names", |s| {
                s.dim_names = Some(vec!["shoulder".into(), "elbow".into()])
            }),
            // The range the source declares its values fall in. A check compares the values against
            // it, so the same samples are in-spec under one declaration and out of it under another.
            ("declared_range", |s| {
                s.declared_range = Some(crate::cdm::DeclaredRange {
                    min: 0.0,
                    max: 100.0,
                })
            }),
            // How many points the stream's cloud messages carried. `AUTONOMY.POINT_CLOUD_EMPTY`
            // fails a stream on it, and a LiDAR that published only empty sweeps is otherwise
            // identical to one that published full ones — same schema, same timestamps, same rate,
            // same frame.
            ("observed_point_counts", |s| {
                s.observed_point_counts = Some(crate::cdm::PointCounts {
                    message_count: 10,
                    min: 0,
                    max: 0,
                    empty: 10,
                })
            }),
        ];

        // A compile-time census. Adding a field to `Stream` breaks this destructuring, which is the
        // point: the author is then forced to decide whether it belongs in the hash and to add a
        // row above. `frames` is the one field with no row — it is covered exhaustively by the
        // frame/`ValueRef` tests, and mutating it here would only restate them.
        {
            let s = sample_dataset().episodes[0].streams[0].clone();
            let Stream {
                name: _,
                modality: _,
                declared_rate_hz: _,
                clock_id: _,
                clock_kind: _,
                latched: _,
                declared_range: _,
                dtype: _,
                shape: _,
                dim_names: _,
                frames: _,
                stats: _,
                dim_stats: _,
                observed_stats: _,
                observed_saturation: _,
                observed_non_finite: _,
                observed_dim_stats: _,
                point_fields: _,
                observed_point_counts: _,
                media: _,
                frame_id: _,
            } = s;
        }

        for (field, apply) in mutate {
            let mut d = sample_dataset();
            // Seed baseline stats so the mutation is a change, not a None→Some appearance alone.
            d.episodes[0].streams[1].dim_stats = Some(dims.clone());
            d.episodes[0].streams[1].observed_stats = Some(stats);
            d.episodes[0].streams[1].observed_saturation = Some(sat);
            d.episodes[0].streams[1].observed_non_finite = Some(0);
            d.episodes[0].streams[1].observed_dim_stats = Some(dims.clone());
            d.episodes[0].streams[1].frame_id = Some("lidar_top".into());
            let seeded = content_hash(&d);
            assert_ne!(seeded, base, "seeding stats fields must change the hash");
            apply(&mut d.episodes[0].streams[1]);
            assert_ne!(
                content_hash(&d),
                seeded,
                "changing {field} must change the content hash"
            );
        }
    }

    #[test]
    fn frame_order_is_significant() {
        let d = sample_dataset();
        let base = content_hash(&d);
        let mut d2 = d.clone();
        d2.episodes[0].streams[0].frames.reverse();
        assert_ne!(
            content_hash(&d2),
            base,
            "frame timeline order is data-significant"
        );
    }

    #[test]
    fn signed_zero_rate_does_not_split_hash() {
        let mut a = sample_dataset();
        let mut b = sample_dataset();
        a.episodes[0].streams[0].declared_rate_hz = Some(0.0);
        b.episodes[0].streams[0].declared_rate_hz = Some(-0.0);
        assert_eq!(content_hash(&a), content_hash(&b));
    }

    #[test]
    fn content_hash_hex_is_64_chars() {
        let h = content_hash(&sample_dataset());
        let hex = h.to_hex();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

#[cfg(test)]
mod proptests {
    use crate::cdm::*;
    use crate::content_hash;
    use proptest::prelude::*;

    fn arb_modality() -> impl Strategy<Value = Modality> {
        prop_oneof![
            Just(Modality::Video),
            Just(Modality::ScalarState),
            Just(Modality::Action),
            Just(Modality::Audio),
            Just(Modality::TactileForceTorque),
        ]
    }

    fn arb_frame() -> impl Strategy<Value = Frame> {
        (any::<i64>(), "[a-z0-9/._]{0,12}").prop_map(|(ts, uri)| Frame {
            ts,
            value_ref: ValueRef {
                uri,
                byte_offset: None,
                byte_len: None,
                content_hash: None,
            },
        })
    }

    fn arb_stream() -> impl Strategy<Value = Stream> {
        (
            arb_modality(),
            prop::option::of(any::<u16>().prop_map(|r| r as f64)),
            "[a-z]{1,6}",
            prop::collection::vec(arb_frame(), 0..6),
        )
            .prop_map(|(modality, rate, clock_id, frames)| Stream {
                name: String::new(), // assigned uniquely by the parent episode
                modality,
                declared_rate_hz: rate,
                clock_id,
                clock_kind: ClockKind::Measured,
                dtype: None,
                shape: None,
                dim_names: None,
                frames,
                stats: None,
                dim_stats: None,
                observed_stats: None,
                observed_saturation: None,
                observed_non_finite: None,
                observed_dim_stats: None,
                latched: None,
                declared_range: None,
                point_fields: None,
                observed_point_counts: None,
                media: None,
                frame_id: None,
            })
    }

    fn arb_episode() -> impl Strategy<Value = Episode> {
        prop::collection::vec(arb_stream(), 0..4).prop_map(|mut streams| {
            // Unique stream names within the episode so the canonical sort key is total; the
            // permutation-invariance tests need names attached to streams, not positions.
            for (i, s) in streams.iter_mut().enumerate() {
                s.name = format!("stream{i}");
            }
            Episode {
                index: 0, // assigned uniquely by the parent dataset
                start_ts: None,
                end_ts: None,
                streams,
                task: None,
                labels: vec![],
                ego_poses: None,
                ego_frame: None,
                declared_frame_count: None,
            }
        })
    }

    fn arb_dataset() -> impl Strategy<Value = Dataset> {
        ("[a-z/]{1,10}", prop::collection::vec(arb_episode(), 0..4)).prop_map(
            |(id, mut episodes)| {
                // Unique episode indices so canonicalization by index is a total order.
                for (i, ep) in episodes.iter_mut().enumerate() {
                    ep.index = i as u64;
                }
                Dataset {
                    id,
                    metadata: vec![],
                    provenance: vec![],
                    episodes,
                    calibration: None,
                }
            },
        )
    }

    proptest! {
        /// Determinism (design D5): hashing the same value twice always agrees.
        #[test]
        fn hash_is_deterministic(d in arb_dataset()) {
            prop_assert_eq!(content_hash(&d), content_hash(&d));
        }

        /// Stream order within an episode must never affect the hash.
        #[test]
        fn stream_permutation_is_invisible(d in arb_dataset()) {
            let base = content_hash(&d);
            let mut d2 = d.clone();
            for ep in &mut d2.episodes {
                ep.streams.reverse();
            }
            prop_assert_eq!(content_hash(&d2), base);
        }

        /// Episode order must never affect the hash (episodes canonicalize by index).
        #[test]
        fn episode_permutation_is_invisible(d in arb_dataset()) {
            let base = content_hash(&d);
            let mut d2 = d.clone();
            d2.episodes.reverse();
            prop_assert_eq!(content_hash(&d2), base);
        }
    }
}
