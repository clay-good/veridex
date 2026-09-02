//! A pinned content hash for a fixed dataset — the one test that can catch an accidental change to
//! the canonical encoding.
//!
//! Every other hash test in this repo is *relative*: it asserts that two datasets hash the same, or
//! that they differ. Those hold under any encoding, so an edit to `encode` is self-consistent and
//! invisible to all of them. That is not hypothetical — `MediaStatus::Unreadable`'s encoding changed
//! from `u8(2) + str(reason)` to a bare `u8(2)` while `CANONICAL_VERSION` stayed at `6`, and the
//! commit that changed it also updated the one test that would have noticed. Two builds both
//! declaring encoding v6 produced different hashes for any dataset with an unreadable container, so
//! a certificate issued by the earlier build failed against byte-identical data on the later one —
//! and failed as `ContentHashMismatch`, which is to say, reported as tampering.
//!
//! `CANONICAL_VERSION`'s own contract is that hashes from different encoding versions never collide.
//! Nothing enforced it. This test does: change what `encode` writes and it fails, and the only way
//! to make it pass is to bump `CANONICAL_VERSION` and re-pin the vector — deliberately, in the same
//! commit, where a reviewer can see both.

use veridex_core::cdm::Dataset;
use veridex_core::{canonical::CANONICAL_VERSION, content_hash};

/// A dataset exercising the encoder arms that carry real content: metadata, provenance at two
/// scopes and three classes, calibration (transforms + intrinsics), ego poses, labels, per-stream
/// and per-dimension statistics, saturation, media with observed/declared parameters, and frames
/// with and without content fingerprints.
const GOLDEN: &str = include_str!("fixtures/canonical_golden.json");

#[test]
fn the_canonical_encoding_has_not_changed_without_a_version_bump() {
    let d: Dataset = serde_json::from_str(GOLDEN).expect("the golden fixture parses");

    assert_eq!(
        CANONICAL_VERSION, 16,
        "the encoding version changed; re-pin the hash below in the same commit"
    );
    assert_eq!(
        content_hash(&d).to_hex(),
        "38f3c8d7f1420d08b86c35ffdf06c90c738c86cb5d508df53573266678ed5d5d",
        "the canonical encoding changed. If that was deliberate, bump CANONICAL_VERSION and \
         re-pin this vector in the same commit — a hash change without a version bump means two \
         builds disagree about byte-identical data while both claiming the same encoding, and \
         `verify` reports that disagreement as tampering."
    );
}

/// The vector is only meaningful if the fixture actually reaches the interesting arms — a fixture
/// that quietly lost its calibration or its media would still pin *a* hash, just a uselessly
/// shallow one.
#[test]
fn the_golden_fixture_still_covers_what_it_claims_to() {
    let d: Dataset = serde_json::from_str(GOLDEN).expect("the golden fixture parses");

    assert!(d.calibration.is_some(), "calibration arm");
    assert!(
        d.calibration
            .as_ref()
            .unwrap()
            .intrinsics
            .iter()
            .any(|c| c.width.is_some() && c.height.is_some()),
        "declared image dimensions — the vector must reach the encoding with a value, not the \
         absent marker"
    );
    assert!(
        d.calibration
            .as_ref()
            .unwrap()
            .intrinsics
            .iter()
            .any(|c| c.distortion_model.is_some()),
        "declared distortion model — the vector must reach the encoding with a value, not the \
         absent marker"
    );
    assert!(!d.provenance.is_empty(), "provenance arm");
    assert!(!d.metadata.is_empty(), "metadata arm");
    assert!(d.episodes.len() >= 2, "multi-episode ordering");
    let streams: Vec<_> = d.episodes.iter().flat_map(|e| e.streams.iter()).collect();
    assert!(streams.iter().any(|s| s.media.is_some()), "media arm");
    assert!(
        streams.iter().any(|s| s.declared_range.is_some()),
        "declared-range arm — the vector must reach the encoding with a value, not the absent marker"
    );
    assert!(
        streams.iter().any(|s| s.latched == Some(true)),
        "latched arm — the vector must reach the encoding with a value, not just the absent marker"
    );
    assert!(
        streams
            .iter()
            .any(|s| s.point_fields.as_ref().is_some_and(|f| !f.is_empty())),
        "point-cloud layout arm — the vector reached it only as the absent marker until a cloud \
         stream was added"
    );
    assert!(
        streams.iter().any(|s| s.observed_point_counts.is_some()),
        "observed point counts — the vector must reach the encoding with a value, not the absent \
         marker"
    );
    assert!(
        streams.iter().any(
            |s| s.observed_header_stamps.is_some_and(|h| h.regressions > 0
                && h.unset > 0
                && h.min_offset_ns != 0
                && h.max_offset_ns != 0)
        ),
        "capture-stamp summary — the vector must reach the encoding with a non-zero value in \
         every field, not the `None` arm and not a summary of zeros"
    );
    assert!(
        streams.iter().any(|s| s
            .observed_sequence
            .is_some_and(|q| q.message_count > 0 && q.missing > 0 && q.non_increasing > 0)),
        "publisher sequence summary — the vector must reach the encoding with a non-zero value in \
         every field, not the `None` arm and not a summary of zeros"
    );
    assert!(streams.iter().any(|s| s.stats.is_some()), "stats arm");
    assert!(streams.iter().any(|s| s.dim_stats.is_some()), "dim arm");
    assert!(
        streams.iter().any(|s| s.observed_saturation.is_some()),
        "saturation arm"
    );
    assert!(
        d.episodes.iter().any(|e| e.ego_poses.is_some()),
        "ego-pose arm"
    );
    assert!(
        d.episodes.iter().any(|e| e.ego_frame.is_some()),
        "ego body frame — the vector must reach the encoding with a value, not the absent marker"
    );
    assert!(d.episodes.iter().any(|e| !e.labels.is_empty()), "label arm");
    assert!(
        streams
            .iter()
            .any(|s| s.frames.iter().any(|f| f.value_ref.content_hash.is_some())),
        "fingerprinted frame arm"
    );
}
