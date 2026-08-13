//! # veridex-core
//!
//! The Rust core of [Veridex](https://github.com/veridex/veridex) — the open, cross-format trust
//! layer for physical-AI data.
//!
//! This crate owns the [Canonical Dataset Model](cdm) (the neutrality substrate every adapter
//! populates and every check reads) and its [deterministic content hashing](canonical). Adapters,
//! the validation engine, and the trust certificate build on top of these.
//!
//! Determinism is a first-class contract: the same dataset bytes and the same Veridex version
//! always canonicalize to the same [`ContentHash`](canonical::ContentHash).

#![forbid(unsafe_code)]

pub mod adapter;
pub mod canonical;
pub mod cdm;
pub mod certificate;
pub mod check;
pub mod checks;
pub mod config;
pub mod diff;
pub mod emit;
pub mod engine;
pub mod pipeline;
pub mod report;

pub use adapter::lerobot::LeRobotAdapter;
pub use adapter::mcap::McapAdapter;
pub use adapter::{
    default_registry, Adapter, AdapterRegistry, Coverage, Detection, IngestError, IngestOptions,
    IngestReport, Ingested, Sample, Source, UnmappedField,
};
pub use canonical::{content_hash, ContentHash, CANONICAL_VERSION};
pub use certificate::{
    score, sign, verify, CertError, Certificate, Grade, Issuance, ProvenanceCoverage,
    SignedCertificate, SigningKeypair, TrustScore, Verified, RUBRIC_VERSION,
};
pub use check::{Category, Check, Finding, Location, Scope, Severity};
pub use config::{CheckConfig, ConfigError, FailOn};
pub use diff::{diff_reports, render_diff, render_diff_json, ReportDiff};
pub use emit::{render_provenance, to_croissant, to_prov};
pub use engine::{CheckInfo, Engine, RegistryError, RunConfig, Status, Tolerances, Verdict};
pub use pipeline::{run_check, run_check_with, CheckOutput};
pub use report::{
    render_catalog_json, render_html, render_json, render_sarif, render_terminal,
    REPORT_SCHEMA_VERSION,
};

/// The version of `veridex-core`, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdm::*;

    fn sample_dataset() -> Dataset {
        Dataset {
            id: "acme/pick-place".into(),
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
                        dtype: Some("video".into()),
                        shape: Some(vec![3, 480, 640]),
                        stats: None,
                        observed_stats: None,
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
                        dtype: Some("float32".into()),
                        shape: Some(vec![6]),
                        stats: None,
                        observed_stats: None,
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
                }],
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
    fn changing_a_timestamp_changes_the_hash() {
        let d = sample_dataset();
        let base = content_hash(&d);
        let mut d2 = d.clone();
        d2.episodes[0].streams[0].frames[0].ts += 1;
        assert_ne!(content_hash(&d2), base);
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
                dtype: None,
                shape: None,
                frames,
                stats: None,
                observed_stats: None,
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
