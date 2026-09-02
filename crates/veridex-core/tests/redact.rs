//! Redaction: what a report loses when it is prepared for sharing, and what it must keep.
//!
//! Both halves matter. A redacted report that still names the customer's robot is a leak; one that
//! drops the 210 ms drift is not a report.

use veridex_core::cdm::*;
use veridex_core::check::{Category, Finding, Location, Severity};
use veridex_core::{Redactor, RunConfig};

fn stream(name: &str, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: Some(10.0),
        clock_id: "clock".into(),
        clock_kind: ClockKind::Measured,
        dtype: None,
        shape: None,
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
        observed_header_stamps: None,
        observed_sequence: None,
        media: None,
        frame_id: None,
        frames: ts
            .iter()
            .map(|t| Frame {
                ts: *t,
                value_ref: ValueRef {
                    uri: "x".into(),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: None,
                },
            })
            .collect(),
    }
}

/// A dataset whose every identifier is something a team would not want to publish.
fn sensitive_dataset() -> Dataset {
    Dataset {
        id: "acme-robotics/warehouse-pilot".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![Provenance {
            scope: ProvenanceScope::Dataset,
            elements: vec![
                ProvenanceElement {
                    key: "annotator".into(),
                    value: Some("dana.quinn@acme-robotics.example".into()),
                    class: ProvenanceClass::Known,
                },
                ProvenanceElement {
                    key: "license".into(),
                    value: Some("acme-internal-only".into()),
                    class: ProvenanceClass::Known,
                },
            ],
        }],
        episodes: vec![Episode {
            index: 0,
            start_ts: Some(0),
            end_ts: Some(900_000_000),
            streams: vec![
                stream("observation.images.warehouse_aisle_7", &[0, 100_000_000]),
                stream("observation.state", &[0, 100_000_000]),
            ],
            task: Some("pick the returned order from bin 4102".into()),
            labels: vec![Label {
                key: "language".into(),
                value: "pick the returned order from bin 4102".into(),
                ts: None,
            }],
            ego_poses: None,
            ego_frame: None,
            declared_frame_count: None,
        }],
    }
}

/// A finding quoting one identifier of each kind, as the real checks do.
fn finding_quoting_everything() -> Finding {
    Finding::new(
        "temporal.clock-skew",
        Category::Temporal,
        Severity::Error,
        Location::Stream {
            episode: 0,
            stream: "observation.images.warehouse_aisle_7".into(),
        },
        "TEMPORAL.CLOCK_SKEW",
        "episode 0 of acme-robotics/warehouse-pilot: streams \
         `observation.images.warehouse_aisle_7` and `observation.state` drift by 210.0 ms during \
         `pick the returned order from bin 4102` (annotated by dana.quinn@acme-robotics.example)",
    )
}

fn verdict_with(finding: Finding) -> veridex_core::Verdict {
    let dataset = Dataset {
        episodes: vec![],
        ..sensitive_dataset()
    };
    let engine = veridex_core::Engine::builder().build();
    let mut verdict = engine.run(
        &dataset,
        veridex_core::content_hash(&dataset),
        &RunConfig::default(),
    );
    verdict.findings.push(finding);
    verdict.counts.error += 1;
    verdict
}

#[test]
fn every_identifier_a_finding_quotes_is_replaced() {
    let mut redactor = Redactor::for_dataset(&sensitive_dataset());
    let verdict = verdict_with(finding_quoting_everything());
    let redacted = redactor.redact_verdict(&verdict);

    let text = serde_json::to_string(&redacted).expect("verdict serializes");
    for leaked in [
        "acme-robotics/warehouse-pilot",
        "warehouse_aisle_7",
        "observation.state",
        "bin 4102",
        "dana.quinn@acme-robotics.example",
        "acme-internal-only",
    ] {
        assert!(
            !text.contains(leaked),
            "`{leaked}` survived redaction: {text}"
        );
    }
    // Including in the location, not only in the prose.
    let quoted = redacted
        .findings
        .iter()
        .find(|f| f.code == "TEMPORAL.CLOCK_SKEW")
        .expect("the finding is still there");
    assert!(
        matches!(&quoted.location, Location::Stream { stream, .. } if stream.starts_with("stream#")),
        "unexpected location: {:?}",
        quoted.location
    );
}

#[test]
fn every_measurement_the_finding_is_about_survives() {
    // A redacted report that dropped these would not be redacted, it would be empty.
    let mut redactor = Redactor::for_dataset(&sensitive_dataset());
    let redacted = redactor.redact_verdict(&verdict_with(finding_quoting_everything()));
    let quoted = redacted
        .findings
        .iter()
        .find(|f| f.code == "TEMPORAL.CLOCK_SKEW")
        .expect("the finding is still there");

    assert!(quoted.message.contains("210.0 ms"), "{}", quoted.message);
    assert!(quoted.message.contains("episode 0"), "{}", quoted.message);
    assert_eq!(quoted.severity, Severity::Error);
    assert_eq!(quoted.check_id, "temporal.clock-skew");
    // The check's own risk/remedy prose is static and carries no identifier, so it passes through.
    let original = finding_quoting_everything();
    assert_eq!(quoted.risk, original.risk);
    assert_eq!(quoted.remedy, original.remedy);
}

#[test]
fn the_report_says_it_was_redacted() {
    // The disclosure is a finding, not a header line, so it reaches SARIF, JSON, HTML, the terminal
    // and `diff` alike — a rendering-only banner would be invisible to the machine consumer most
    // likely to receive the shared document.
    let mut redactor = Redactor::for_dataset(&sensitive_dataset());
    let verdict = verdict_with(finding_quoting_everything());
    let redacted = redactor.redact_verdict(&verdict);

    let note = redacted
        .findings
        .iter()
        .find(|f| f.code == veridex_core::REDACTION_CODE)
        .expect("the redaction must disclose itself");
    assert_eq!(note.severity, Severity::Info);
    assert!(note.message.contains("redacted for sharing"));
    // And it says what was kept, so nobody reads "redacted" as "anonymous".
    assert!(note.risk.contains("best-effort"), "{}", note.risk);
    assert_eq!(redacted.counts.info, verdict.counts.info + 1);
    // An info finding costs the data score nothing, so a shared report grades identically.
    let coverage = veridex_core::ProvenanceCoverage::of(&sensitive_dataset());
    assert_eq!(
        veridex_core::score(&redacted, &coverage).score,
        veridex_core::score(&verdict, &coverage).score,
        "redaction must not move the score"
    );
}

#[test]
fn the_verdict_a_shared_report_describes_is_the_same_run() {
    let mut redactor = Redactor::for_dataset(&sensitive_dataset());
    let verdict = verdict_with(finding_quoting_everything());
    let redacted = redactor.redact_verdict(&verdict);

    // The hash is kept deliberately: it is what lets whoever holds the dataset match the report.
    assert_eq!(redacted.cdm_content_hash, verdict.cdm_content_hash);
    assert_eq!(redacted.status, verdict.status);
    assert_eq!(redacted.counts.error, verdict.counts.error);
    assert_eq!(redacted.coverage, verdict.coverage);
}

#[test]
fn the_same_dataset_always_redacts_to_the_same_report() {
    // A redacted report is still reproducible, and two runs of one dataset are still comparable.
    let a = Redactor::for_dataset(&sensitive_dataset())
        .redact_verdict(&verdict_with(finding_quoting_everything()));
    let b = Redactor::for_dataset(&sensitive_dataset())
        .redact_verdict(&verdict_with(finding_quoting_everything()));
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

#[test]
fn a_name_that_contains_another_name_is_not_left_in_pieces() {
    // `observation.state` is a substring of nothing here, but `arm` is a substring of `arm/gripper`:
    // substituting the short one first would leave `stream#1/gripper` — the long name, disclosed.
    let mut dataset = sensitive_dataset();
    dataset.episodes[0].streams = vec![stream("arm", &[0, 1]), stream("arm/gripper", &[0, 1])];
    let mut redactor = Redactor::for_dataset(&dataset);
    let redacted = redactor.redact_text("`arm/gripper` lags `arm` by 40 ms");
    assert!(
        !redacted.contains("gripper"),
        "the longer name survived in pieces: {redacted}"
    );
    assert!(redacted.contains("40 ms"), "{redacted}");
}

#[test]
fn a_path_a_finding_quotes_is_not_a_way_around_redaction() {
    // Two findings the catalog really emits quote *paths* rather than stream names: the video family
    // names the media file it could not pair (`videos/observation.images.wrist_cam/…`), and the
    // coverage disclosure names every source file the adapter declined to read. A redactor built
    // only from stream and task text lets both straight through — and a directory path is often the
    // most identifying string in a dataset.
    use veridex_core::check::{Category, Finding, Location, Severity};
    let mut dataset = sensitive_dataset();
    dataset.episodes[0].streams[0].media = Some(Media {
        uri: "videos/acme_warehouse_aisle_7/episode_000000.mp4".into(),
        declared: MediaParams::default(),
        status: MediaStatus::Missing,
        observed: MediaParams::default(),
        frame_count: None,
    });
    dataset.metadata = vec![("robot".into(), "acme-picker-mk3".into())];
    dataset.episodes[0].streams[0].frame_id = Some("acme_wrist_cam_link".into());

    let mut verdict = verdict_with(Finding::new(
        "video.media-readable",
        Category::Video,
        Severity::Error,
        Location::Dataset,
        "VIDEO.MEDIA_ABSENT",
        "stream `observation.images.warehouse_aisle_7`: none were found under \
         `videos/acme_warehouse_aisle_7/episode_000000.mp4`",
    ));
    verdict.findings.push(Finding::new(
        "veridex.coverage",
        Category::Structural,
        Severity::Warning,
        Location::Dataset,
        "COVERAGE.SOURCE_UNREAD",
        "1 source(s) the dataset declares were not read \
             (data/acme-warehouse-pilot/chunk-000/file-001.parquet)",
    ));
    verdict.findings.push(Finding::new(
        "autonomy.sensor-frame-resolution",
        Category::Autonomy,
        Severity::Warning,
        Location::Dataset,
        "AUTONOMY.SENSOR_FRAME_UNRELATED",
        "stream declares frame `acme_wrist_cam_link`, which the transform tree does not relate \
         to the camera (robot acme-picker-mk3)",
    ));

    let redacted = Redactor::for_dataset(&dataset).redact_verdict(&verdict);
    let text = serde_json::to_string(&redacted).expect("verdict serializes");
    for leaked in [
        "videos/acme_warehouse_aisle_7/episode_000000.mp4",
        "acme_warehouse_aisle_7",
        "data/acme-warehouse-pilot/chunk-000/file-001.parquet",
        "acme_wrist_cam_link",
        "acme-picker-mk3",
    ] {
        assert!(
            !text.contains(leaked),
            "`{leaked}` survived redaction: {text}"
        );
    }
}

#[test]
fn a_value_a_producer_attested_is_redacted_like_any_other() {
    // Attested values are not in the dataset, so a redactor built from the dataset alone does not
    // know them — and the conflict finding quotes them verbatim. A producer attesting an annotator's
    // address or an internal licence term, then sharing a redacted report, published exactly the
    // string redaction exists to remove.
    use veridex_core::check::{Category, Finding, Location, Severity};
    let dataset = sensitive_dataset();
    let attested = [
        "dana.quinn@acme-robotics.example",
        "acme-internal-only-terms",
    ];

    let mut redactor = Redactor::for_dataset(&dataset)
        .and_attested(attested.iter().map(|v| v.to_string()).collect::<Vec<_>>());
    let verdict = verdict_with(Finding::new(
        "veridex.attestation",
        Category::Provenance,
        Severity::Warning,
        Location::Dataset,
        "PROVENANCE.ATTESTATION_CONFLICT",
        "1 attested value(s) contradict what the dataset records (license: recorded \
         `acme-internal-only` → attested `acme-internal-only-terms`)",
    ));
    let redacted = redactor.redact_verdict(&verdict);
    let text = serde_json::to_string(&redacted).expect("serializes");
    for leaked in attested {
        assert!(
            !text.contains(leaked),
            "`{leaked}` survived redaction: {text}"
        );
    }
    // The finding still says a conflict happened — the measurement survives, the strings do not.
    assert!(
        text.contains("contradict what the dataset records"),
        "the finding must survive: {text}"
    );
}

#[test]
fn a_joint_name_a_finding_quotes_is_redacted_like_any_other_identifier() {
    // Every statistical finding on a multi-DoF stream now quotes the source's own name for the
    // dimension. A report meant to leave the building must not carry `acme_wrist_gripper_v2` out
    // with it just because the finding says which joint saturated.
    use veridex_core::check::{Category, Finding, Location, Severity};
    let mut dataset = sensitive_dataset();
    dataset.episodes[0].streams[0].dim_names = Some(vec![
        "acme_shoulder_pan".into(),
        "acme_wrist_gripper_v2".into(),
    ]);
    let verdict = verdict_with(Finding::new(
        "statistical.saturation",
        Category::Statistical,
        Severity::Warning,
        Location::Dataset,
        "STATISTICAL.SATURATED",
        "stream `observation.state` (dimension 1 `acme_wrist_gripper_v2`): 75% of values sit \
         exactly at its maximum (2)",
    ));

    let mut redactor = Redactor::for_dataset(&dataset);
    let redacted = redactor.redact_verdict(&verdict);
    let message = &redacted
        .findings
        .iter()
        .find(|f| f.code == "STATISTICAL.SATURATED")
        .expect("the finding is still there")
        .message;
    assert!(
        !message.contains("acme_wrist_gripper_v2") && !message.contains("acme_shoulder_pan"),
        "the joint name left the building: {message}"
    );
    // The measurement itself is exactly what a redacted report is for, and it survives.
    assert!(
        message.contains("75%") && message.contains("dimension 1"),
        "{message}"
    );
}

#[test]
fn the_redacted_dataset_id_does_not_depend_on_what_the_redactor_saw_first() {
    // Two front-ends now redact the dataset id for the report's `dataset.id` field, and they build
    // their redactors differently: the CLI reuses the one that already redacted the whole verdict,
    // the Python binding builds a fresh one. That is only safe because the id resolves through the
    // *deterministic* replacement table, which is fixed at construction — never through the
    // path-placeholder map, which is numbered in the order paths are met.
    //
    // If it ever fell through to the path map, the same dataset would redact to `path#1` from one
    // front-end and `path#7` from the other, and the CI parity job would catch it only for a
    // dataset whose id happens to contain a slash. This says so directly instead.
    use veridex_core::cdm::{Dataset, Episode};
    for id in [
        "acme/warehouse/pick",
        "a/b",
        "plain-name",
        "hf://org/name",
        "x",
    ] {
        let d = Dataset {
            id: id.into(),
            calibration: None,
            metadata: vec![],
            provenance: vec![],
            episodes: vec![Episode {
                index: 0,
                start_ts: None,
                end_ts: None,
                streams: vec![],
                task: None,
                labels: vec![],
                ego_poses: None,
                ego_frame: None,
                declared_frame_count: None,
            }],
        };
        let fresh = veridex_core::Redactor::for_dataset(&d).redact_text(&d.id);
        let mut used = veridex_core::Redactor::for_dataset(&d);
        // Anything path-shaped, so the stateful map is non-empty before the id is redacted.
        let _ = used.redact_text("read /var/lib/other/thing and /tmp/second");
        assert_eq!(
            fresh,
            used.redact_text(&d.id),
            "the redacted id for `{id}` changed with the redactor's state"
        );
        assert!(
            !fresh.contains(id) || id.chars().count() < 3,
            "and it is redacted at all: `{id}` -> `{fresh}`"
        );
    }
}
