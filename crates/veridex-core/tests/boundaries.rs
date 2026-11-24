//! The boundary pins: for each threshold a check promises, the value **exactly at** it and the
//! first value past it.
//!
//! Every test here was written against a surviving mutant. A mutation sweep over the catalog flipped
//! each comparison in turn (`>` ↔ `>=`, `<` ↔ `<=`) and ran the suite: the comparisons below changed
//! meaning without a single test noticing. A check that says "more than 50 ms" and fires at exactly
//! 50 ms is wrong in the direction that matters — it accuses honest data — and nothing in the suite
//! could tell the two apart.
//!
//! The shape of each test is the same: build the measurement at the threshold, assert silence;
//! move it by the smallest step the check can see, assert the finding. A test that only checked the
//! far side would pass under either comparison, which is how these got this far.

use veridex_core::cdm::{
    ClockKind, Dataset, Episode, Frame, Media, MediaParams, MediaStatus, Modality, Stream,
    StreamStats, ValueRef,
};
use veridex_core::check::Check;
use veridex_core::checks::{statistical, structural, temporal, video};

fn vref() -> ValueRef {
    ValueRef {
        uri: "x".into(),
        byte_offset: None,
        byte_len: None,
        content_hash: None,
    }
}

fn stream(name: &str, clock: &str, rate: Option<f64>, ts: &[i64]) -> Stream {
    Stream {
        name: name.into(),
        modality: Modality::ScalarState,
        declared_rate_hz: rate,
        clock_id: clock.into(),
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
        observed_diagnostics: None,
        observed_point_counts: None,
        observed_image_dims: None,
        observed_body_decodes: None,
        observed_header_stamps: None,
        observed_sequence: None,
        observed_fix_availability: None,
        media: None,
        frame_id: None,
        frames: ts
            .iter()
            .map(|t| Frame {
                ts: *t,
                value_ref: vref(),
            })
            .collect(),
    }
}

fn episode(index: u64, streams: Vec<Stream>) -> Episode {
    Episode {
        index,
        start_ts: None,
        end_ts: None,
        streams,
        task: None,
        labels: vec![],
        ego_poses: None,
        ego_frame: None,
        declared_frame_count: None,
    }
}

fn dataset(episodes: Vec<Episode>) -> Dataset {
    Dataset {
        id: "t".into(),
        calibration: None,
        metadata: vec![],
        provenance: vec![],
        episodes,
    }
}

fn codes(findings: Vec<veridex_core::check::Finding>) -> Vec<String> {
    findings.into_iter().map(|f| f.code).collect()
}

// ---------------------------------------------------------------------------
// temporal.rate-conformance — "observed rate differs from declared by more than the tolerance"
// ---------------------------------------------------------------------------

/// A stream of `n` frames whose observed rate deviates from `declared` by exactly `deviation`.
///
/// Built from the span rather than from per-frame intervals, because that is what the check
/// measures: `(frames - 1) / seconds`.
fn stream_deviating(declared: f64, deviation: f64, n: usize) -> Stream {
    let observed = declared * (1.0 + deviation);
    let span_ns = (n as f64 - 1.0) / observed * 1e9;
    let step = span_ns / (n as f64 - 1.0);
    let ts: Vec<i64> = (0..n).map(|i| (i as f64 * step).round() as i64).collect();
    stream("s", "c", Some(declared), &ts)
}

#[test]
fn a_rate_exactly_at_the_conformance_tolerance_is_not_reported() {
    // A quarter off a rate of 8 Hz, sampled at exactly 10 Hz: every quantity here is exact in
    // binary, so "exactly at the tolerance" is a real measurement rather than one float rounding
    // nudges past it. That nudge is why the first version of this test passed under both
    // comparisons.
    let check = temporal::RateConformance {
        relative_tolerance: 0.25,
    };
    let at = dataset(vec![episode(0, vec![stream_deviating(8.0, 0.25, 101)])]);
    assert!(
        codes(check.run(&at)).is_empty(),
        "a deviation exactly at the tolerance must not be reported"
    );
    // Past it, it fires.
    let past = dataset(vec![episode(0, vec![stream_deviating(8.0, 1.0, 101)])]);
    assert_eq!(codes(check.run(&past)), ["TEMPORAL.RATE"]);
}

// ---------------------------------------------------------------------------
// temporal.jitter — "the coefficient of variation exceeds max_cv", over at least MIN_INTERVALS
// ---------------------------------------------------------------------------

/// Intervals whose coefficient of variation is exactly `cv`: half short, half long, around a mean of
/// one second. For an even split the CV is the half-spread over the mean, exactly.
fn stream_with_cv(cv: f64, intervals: usize) -> Stream {
    let mean = 1e9;
    let half = cv * mean;
    let mut ts = vec![0i64];
    for i in 0..intervals {
        let step = if i % 2 == 0 { mean - half } else { mean + half };
        ts.push(ts[ts.len() - 1] + step.round() as i64);
    }
    stream("s", "c", None, &ts)
}

#[test]
fn a_jitter_coefficient_exactly_at_the_limit_is_not_reported() {
    let check = temporal::Jitter { max_cv: 0.5 };
    let at = dataset(vec![episode(0, vec![stream_with_cv(0.5, 8)])]);
    assert!(
        codes(check.run(&at)).is_empty(),
        "a CV exactly at max_cv is not 'exceeds max_cv'"
    );
    let past = dataset(vec![episode(0, vec![stream_with_cv(0.75, 8)])]);
    assert_eq!(codes(check.run(&past)), ["TEMPORAL.JITTER"]);
}

#[test]
fn jitter_measures_a_stream_with_exactly_the_minimum_intervals() {
    // The guard is "fewer than MIN_INTERVALS gives an unreliable CV". A stream with exactly that
    // many is measured — and under the flipped comparison it was skipped, so every finding this
    // check could make about a short stream silently disappeared.
    let check = temporal::Jitter { max_cv: 0.5 };
    let n = temporal::Jitter::MIN_INTERVALS;
    let at = dataset(vec![episode(0, vec![stream_with_cv(0.9, n)])]);
    assert_eq!(
        codes(check.run(&at)),
        ["TEMPORAL.JITTER"],
        "exactly MIN_INTERVALS intervals is enough evidence to measure"
    );
    let under = dataset(vec![episode(0, vec![stream_with_cv(0.9, n - 1)])]);
    assert!(
        codes(check.run(&under)).is_empty(),
        "one interval short of the minimum is not measured"
    );
}

// ---------------------------------------------------------------------------
// temporal.start-offset / end-offset — "offset exceeds the tolerance plus the sampling quantum"
// ---------------------------------------------------------------------------

/// Two streams on one clock, the second starting `offset_ns` after the first, both at `rate`.
fn offset_pair(rate: f64, offset_ns: i64, at_end: bool) -> Dataset {
    let period = (1e9 / rate) as i64;
    let a: Vec<i64> = (0..6).map(|i| i * period).collect();
    let b: Vec<i64> = if at_end {
        // Same start, later end: the end-offset case.
        (0..6).map(|i| i * period + offset_ns).collect()
    } else {
        (0..6).map(|i| i * period + offset_ns).collect()
    };
    let mut sa = stream("a", "c", Some(rate), &a);
    let sb = stream("b", "c", Some(rate), &b);
    if at_end {
        // Align the starts so only the ends differ.
        sa.frames[0].ts = sb.frames[0].ts;
    }
    dataset(vec![episode(0, vec![sa, sb])])
}

#[test]
fn a_start_offset_exactly_at_the_allowance_is_not_reported() {
    // The allowance is the configured tolerance plus the slower stream's period, because a sensor
    // sampling every T cannot place its first sample nearer the true start than T. Exactly at that
    // allowance is honest data.
    let rate = 10.0;
    let period = (1e9 / rate) as i64;
    let check = temporal::StartOffset {
        tolerance_ns: 50_000_000,
    };
    let allowance = 50_000_000 + period;
    let at = offset_pair(rate, allowance, false);
    assert!(
        !codes(check.run(&at)).contains(&"TEMPORAL.START_OFFSET".to_string()),
        "an offset exactly at the allowance must not be reported"
    );
    let past = offset_pair(rate, allowance + 1_000_000, false);
    assert!(
        codes(check.run(&past)).contains(&"TEMPORAL.START_OFFSET".to_string()),
        "one millisecond past the allowance is a finding"
    );
}

#[test]
fn an_end_offset_exactly_at_the_allowance_is_not_reported() {
    let rate = 10.0;
    let period = (1e9 / rate) as i64;
    let check = temporal::EndOffset {
        tolerance_ns: 50_000_000,
    };
    let allowance = 50_000_000 + period;
    let at = offset_pair(rate, allowance, true);
    assert!(
        !codes(check.run(&at)).contains(&"TEMPORAL.END_OFFSET".to_string()),
        "an offset exactly at the allowance must not be reported"
    );
    let past = offset_pair(rate, allowance + 1_000_000, true);
    assert!(
        codes(check.run(&past)).contains(&"TEMPORAL.END_OFFSET".to_string()),
        "one millisecond past the allowance is a finding"
    );
}

// ---------------------------------------------------------------------------
// temporal.episode-duration — a factor of exactly 1.0 disables the check rather than flagging
// everything, and MIN_EPISODES is the smallest baseline it will judge from
// ---------------------------------------------------------------------------

/// `durations` as episodes of that many nanoseconds each.
fn dataset_of_durations(durations: &[i64]) -> Dataset {
    let mut d = dataset(
        durations
            .iter()
            .enumerate()
            .map(|(i, dur)| {
                let mut ep = episode(i as u64, vec![stream("s", "c", None, &[0, *dur])]);
                ep.start_ts = Some(0);
                ep.end_ts = Some(*dur);
                ep
            })
            .collect(),
    );
    d.canonicalize_order();
    d
}

#[test]
fn an_episode_duration_factor_of_exactly_one_disables_the_check() {
    // A factor of 1.0 makes `median / factor` and `median * factor` the median itself, so every
    // episode that is not exactly median-length would be flagged. The config layer rejects it; this
    // check treats it as disabled defensively, and the mutant that turned `<= 1.0` into `< 1.0`
    // armed it instead.
    let durations: Vec<i64> = (1..=10).map(|i| i * 1_000_000_000).collect();
    let d = dataset_of_durations(&durations);
    let disabled = temporal::EpisodeDuration { factor: 1.0 };
    assert!(
        codes(disabled.run(&d)).is_empty(),
        "factor 1.0 must disable the check, not flag every episode"
    );
    let armed = temporal::EpisodeDuration { factor: 2.0 };
    assert!(
        !codes(armed.run(&d)).is_empty(),
        "and the same data at a real factor does produce findings"
    );
}

#[test]
fn episode_duration_judges_from_exactly_the_minimum_number_of_episodes() {
    let n = temporal::EpisodeDuration::MIN_EPISODES;
    let check = temporal::EpisodeDuration { factor: 2.0 };
    // One clear outlier among otherwise equal episodes, at exactly the minimum count.
    let mut durations = vec![1_000_000_000i64; n - 1];
    durations.push(100_000_000_000);
    let at = dataset_of_durations(&durations);
    assert!(
        !codes(check.run(&at)).is_empty(),
        "exactly MIN_EPISODES is a baseline this check will judge from"
    );
    let under = dataset_of_durations(&durations[1..]);
    assert!(
        codes(check.run(&under)).is_empty(),
        "one episode short of the minimum, it says nothing"
    );
}

// ---------------------------------------------------------------------------
// statistical.stored-vs-observed — "observed exceeds the stored bound, beyond the rounding slack"
// ---------------------------------------------------------------------------

fn stats(min: f64, max: f64, mean: f64, std: f64) -> StreamStats {
    StreamStats {
        min,
        max,
        mean,
        std,
    }
}

/// A stream whose stored stats say one thing and whose observed stats say another.
fn stream_with_stored_and_observed(stored: StreamStats, observed: StreamStats) -> Stream {
    let mut s = stream("s", "c", None, &[0, 1_000_000_000]);
    s.stats = Some(stored);
    s.observed_stats = Some(observed);
    s
}

/// The slack `statistical.stored-vs-observed` allows for a source's independently-rounded bound,
/// written exactly as the check computes it so the two are bit-identical.
fn stored_slack(bound: f64) -> f64 {
    1e-6 * bound.abs().max(1.0)
}

/// The slack `statistical.range-sanity` allows a mean, written exactly as the check computes it.
fn range_slack(min: f64, max: f64) -> f64 {
    let magnitude = 1e-9 + min.abs().max(max.abs()) * 1e-6;
    let range = (max - min).abs();
    if range.is_finite() && range > 0.0 {
        magnitude.min(range * 4.0)
    } else {
        magnitude
    }
}

#[test]
fn an_observed_value_exactly_at_the_stored_bound_is_not_a_contradiction() {
    // The check reports data that goes *outside* what the manifest promised. Exactly at the bound is
    // inside it, and the mutant that made `<` into `<=` reported every dataset whose minimum is its
    // declared minimum — which is every honestly written manifest.
    let check = statistical::StoredVsObserved;
    let at = dataset(vec![episode(
        0,
        vec![stream_with_stored_and_observed(
            stats(0.0, 1.0, 0.5, 0.1),
            stats(0.0, 1.0, 0.5, 0.1),
        )],
    )]);
    assert!(
        !codes(check.run(&at))
            .iter()
            .any(|c| c == "STATISTICAL.STATS_STALE"),
        "observed bounds equal to the stored ones are agreement, not disagreement"
    );
    // And the real boundary, which equality is not: the check allows a rounding slack either side of
    // the stored bound, so the first value it must stay silent about is the one *exactly at* that
    // slack. Equality passes under either comparison — which is why the sweep walked straight
    // through the assertion above.
    let edge = dataset(vec![episode(
        0,
        vec![stream_with_stored_and_observed(
            stats(0.0, 1.0, 0.5, 0.1),
            stats(0.0 - stored_slack(0.0), 1.0 + stored_slack(1.0), 0.5, 0.1),
        )],
    )]);
    assert!(
        !codes(check.run(&edge))
            .iter()
            .any(|c| c == "STATISTICAL.STATS_STALE"),
        "a value exactly at the rounding slack is inside the slack"
    );
    // Outside them, by more than the rounding slack, it fires.
    let past = dataset(vec![episode(
        0,
        vec![stream_with_stored_and_observed(
            stats(0.0, 1.0, 0.5, 0.1),
            stats(-5.0, 7.0, 0.5, 0.1),
        )],
    )]);
    assert!(
        codes(check.run(&past))
            .iter()
            .any(|c| c == "STATISTICAL.STATS_STALE"),
        "data outside the stored bounds is the finding this check exists for"
    );
}

#[test]
fn a_mean_exactly_at_the_rounding_slack_is_not_out_of_range() {
    // `range-sanity` allows a mean a rounding slack outside `[min, max]`, because a source that
    // computes its mean independently can land one ULP past a bound on a near-constant stream. The
    // boundary is that slack, so the two values this check must stay silent about are exactly
    // `min - slack` and `max + slack`.
    let check = statistical::RangeSanity;
    let slack = range_slack(0.0, 1.0);
    for mean in [0.0 - slack, 1.0 + slack] {
        let mut s = stream("s", "c", None, &[0, 1]);
        s.stats = Some(stats(0.0, 1.0, mean, 0.25));
        let d = dataset(vec![episode(0, vec![s])]);
        let found = codes(check.run(&d));
        assert!(
            !found.iter().any(|c| c == "STATISTICAL.MEAN_OUT_OF_RANGE"),
            "a mean exactly at the slack is inside it: {found:?}"
        );
    }
    // One slack further out, in either direction, and it is a finding.
    for mean in [0.0 - slack * 3.0, 1.0 + slack * 3.0] {
        let mut s = stream("s", "c", None, &[0, 1]);
        s.stats = Some(stats(0.0, 1.0, mean, 0.25));
        let d = dataset(vec![episode(0, vec![s])]);
        assert!(
            codes(check.run(&d))
                .iter()
                .any(|c| c == "STATISTICAL.MEAN_OUT_OF_RANGE"),
            "a mean three times the slack outside its own range is reported"
        );
    }
}

#[test]
fn a_mean_exactly_at_a_bound_is_not_impossible() {
    // `STATISTICAL.IMPOSSIBLE_STATS` is for a mean *outside* [min, max] — a mean equal to either
    // bound is what a constant stream has, and flagging it would accuse every latched signal.
    let check = statistical::RangeSanity;
    for mean in [0.0, 1.0] {
        let mut s = stream("s", "c", None, &[0, 1]);
        s.stats = Some(stats(0.0, 1.0, mean, 0.0));
        let d = dataset(vec![episode(0, vec![s])]);
        assert!(
            !codes(check.run(&d))
                .iter()
                .any(|c| c == "STATISTICAL.MEAN_OUT_OF_RANGE"),
            "a mean of {mean} sits inside [0, 1] and is not out of range"
        );
    }
    let mut s = stream("s", "c", None, &[0, 1]);
    s.stats = Some(stats(0.0, 1.0, 2.0, 0.0));
    let d = dataset(vec![episode(0, vec![s])]);
    assert!(
        codes(check.run(&d))
            .iter()
            .any(|c| c == "STATISTICAL.MEAN_OUT_OF_RANGE"),
        "a mean above the maximum is"
    );
}

#[test]
fn bounds_that_are_equal_are_not_inverted() {
    // `min > max` is the impossible case. `min == max` is a constant stream, which is a different
    // finding (`STATISTICAL.DEGENERATE`) and must not also be called inverted.
    let check = statistical::RangeSanity;
    let mut s = stream("s", "c", None, &[0, 1]);
    s.stats = Some(stats(1.0, 1.0, 1.0, 0.0));
    let d = dataset(vec![episode(0, vec![s])]);
    let found = codes(check.run(&d));
    assert!(
        !found.iter().any(|c| c == "STATISTICAL.RANGE_INVERTED"),
        "equal bounds are a constant, not an inversion: {found:?}"
    );
}

#[test]
fn an_outlier_at_a_tie_names_one_end_and_does_so_the_same_way_every_time() {
    // `extreme-outlier` reports the more extreme end of a stream's range. When the mean sits exactly
    // at the midpoint the two ends are equally extreme, and which one the message names is settled
    // by a comparison nothing tested: the tie goes to the maximum. It is a message, not a verdict —
    // but a finding that named a different end on different machines would be a report that does not
    // reproduce, which is the one thing this tool promises.
    let check = statistical::ExtremeOutlier { z_threshold: 10.0 };
    let mut s = stream("s", "c", None, &[0, 1]);
    // Mean at the midpoint, both ends 20 standard deviations out.
    s.stats = Some(stats(-20.0, 20.0, 0.0, 1.0));
    let d = dataset(vec![episode(0, vec![s])]);
    let findings = check.run(&d);
    let f = findings
        .iter()
        .find(|f| f.code == "STATISTICAL.OUTLIER")
        .expect("twenty standard deviations is an outlier");
    assert!(
        f.message.contains("maximum"),
        "a tie names the maximum, every time: {}",
        f.message
    );
}

// ---------------------------------------------------------------------------
// structural.near-duplicate-episode — evidence thresholds
// ---------------------------------------------------------------------------

/// A distinct 32-byte frame hash from two small numbers.
fn hash_bytes(tag: u8, i: u8) -> [u8; 32] {
    let mut h = [0u8; 32];
    h[0] = tag;
    h[1] = i;
    h
}

/// Two episodes sharing `shared` of `total` frame hashes in one stream.
fn near_duplicate_pair(total: usize, shared: usize) -> Dataset {
    let hashed = |tag: &str, n: usize, offset: usize| -> Stream {
        let mut s = stream("s", "c", None, &[]);
        s.frames = (0..n)
            .map(|i| Frame {
                ts: i as i64 * 1_000_000,
                value_ref: ValueRef {
                    uri: format!("{tag}/{i}"),
                    byte_offset: None,
                    byte_len: None,
                    // Shared frames carry identical hashes; the rest are unique per episode.
                    content_hash: Some(if i < offset {
                        hash_bytes(0x10, i as u8)
                    } else {
                        hash_bytes(if tag == "b" { 0x90 } else { 0x80 }, i as u8)
                    }),
                },
            })
            .collect();
        s
    };
    let mut d = dataset(vec![
        episode(0, vec![hashed("a", total, shared)]),
        episode(1, vec![hashed("b", total, shared)]),
    ]);
    d.canonicalize_order();
    d
}

#[test]
fn near_duplicate_detection_uses_a_stream_of_exactly_the_minimum_length() {
    // "At least MIN_FRAMES long counts as evidence" — a stream of exactly that length is evidence,
    // and the flipped comparison discarded it, which silently narrowed the check to longer episodes.
    let check = structural::NearDuplicateEpisode { min_overlap: 0.8 };
    let n = structural::NearDuplicateEpisode::MIN_FRAMES;
    // Shared in all but one frame: sharing *every* frame makes them exact duplicates, which is a
    // different check's finding and suppressed here.
    let at = near_duplicate_pair(n, n - 1);
    assert_eq!(
        codes(check.run(&at)),
        ["STRUCTURAL.NEAR_DUPLICATE_EPISODE"],
        "a stream of exactly MIN_FRAMES is long enough to be evidence"
    );
    let under = near_duplicate_pair(n - 1, n - 2);
    assert!(
        codes(check.run(&under)).is_empty(),
        "one frame short, there is not enough evidence"
    );
}

// ---------------------------------------------------------------------------
// video.media-conformance — the frame-gap factor and the fps tolerance
// ---------------------------------------------------------------------------

fn media_with(fps: f64, gap_ns: Option<u64>, declared_fps: Option<f64>) -> Media {
    Media {
        uri: "ep.mp4".into(),
        declared: MediaParams {
            fps: declared_fps,
            ..MediaParams::default()
        },
        status: MediaStatus::Read,
        observed: MediaParams {
            fps: Some(fps),
            ..MediaParams::default()
        },
        frame_count: None,
        longest_frame_gap_ns: gap_ns,
    }
}

fn video_dataset(media: Media) -> Dataset {
    let mut s = stream("observation.images.cam", "c", Some(30.0), &[0, 33_333_333]);
    s.modality = Modality::Video;
    s.media = Some(media);
    dataset(vec![episode(0, s_vec(s))])
}

fn s_vec(s: Stream) -> Vec<Stream> {
    vec![s]
}

#[test]
fn a_frame_gap_of_exactly_the_factor_is_reported() {
    // This one is `>=`, deliberately: two frame intervals with nothing in them is a frame the camera
    // did not deliver, and the loader closes the gap up. The pin is here because the mutant that
    // made it `>` moved the threshold to "more than two intervals", which lets the single dropped
    // frame through — the commonest case there is.
    let check = video::MediaConformance { fps_tolerance: 0.1 };
    // 32 fps: the nominal interval is 31_250_000 ns exactly, so a gap of twice that is exactly two
    // intervals. At 30 fps it is 33_333_333.33…, and the rounding put the gap a hair over two —
    // which is how this comparison survived a sweep with a test already pointed at it.
    let nominal = 1e9 / 32.0;
    let at = video_dataset(media_with(32.0, Some((nominal * 2.0_f64) as u64), None));
    assert!(
        codes(check.run(&at)).contains(&"VIDEO.FRAME_GAP".to_string()),
        "a gap of exactly two frame intervals is a dropped frame"
    );
    let under = video_dataset(media_with(32.0, Some((nominal * 1.5_f64) as u64), None));
    assert!(
        !codes(check.run(&under)).contains(&"VIDEO.FRAME_GAP".to_string()),
        "a gap shorter than two intervals is jitter, not a missing frame"
    );
}

#[test]
fn an_fps_deviation_exactly_at_the_tolerance_is_not_reported() {
    // A quarter, and rates that are powers of two: the arithmetic is exact in binary, so "exactly
    // at the tolerance" is a real value here rather than one float rounding puts just past it.
    let check = video::MediaConformance {
        fps_tolerance: 0.25,
    };
    let at = video_dataset(media_with(40.0, None, Some(32.0)));
    let found = codes(check.run(&at));
    assert!(
        !found.contains(&"VIDEO.FPS_MISMATCH".to_string()),
        "a deviation exactly at the tolerance must not be reported: {found:?}"
    );
    let past = video_dataset(media_with(48.0, None, Some(32.0)));
    assert!(
        codes(check.run(&past)).contains(&"VIDEO.FPS_MISMATCH".to_string()),
        "half again as fast is a finding"
    );
}

// ---------------------------------------------------------------------------
// structural.frozen-episode — the shortest stream it will judge, and the shortest it will not
// ---------------------------------------------------------------------------

/// `n` episodes of one action vector, the first `frozen` of them byte-identical throughout.
fn frozen_dataset(episodes: usize, frozen: usize, frames: usize) -> Dataset {
    let make = |ep: usize, still: bool| -> Stream {
        let mut s = stream("action", "c", None, &[]);
        s.modality = Modality::Action;
        s.shape = Some(vec![7]);
        s.frames = (0..frames)
            .map(|i| Frame {
                ts: i as i64 * 1_000_000,
                value_ref: ValueRef {
                    uri: format!("ep{ep}/{i}"),
                    byte_offset: None,
                    byte_len: None,
                    content_hash: Some(hash_bytes(ep as u8, if still { 0 } else { i as u8 })),
                },
            })
            .collect();
        s
    };
    let mut d = dataset(
        (0..episodes)
            .map(|ep| episode(ep as u64, vec![make(ep, ep < frozen)]))
            .collect(),
    );
    d.canonicalize_order();
    d
}

#[test]
fn a_frozen_episode_of_exactly_the_minimum_length_is_judged() {
    // "Below this many frames, every frame being identical is too easily true by chance." A stream
    // of exactly that many is not below it — and the mutant that made the guard `<=` discarded
    // exactly those episodes, which is the length a short dropped session has.
    let check = structural::FrozenEpisode;
    let at = frozen_dataset(4, 1, 8);
    assert_eq!(
        codes(check.run(&at)),
        ["STRUCTURAL.FROZEN_EPISODE"],
        "eight identical frames in one of four episodes is the finding"
    );
    let under = frozen_dataset(4, 1, 7);
    assert!(
        codes(check.run(&under)).is_empty(),
        "seven is below the evidence threshold, and says nothing either way"
    );
}

#[test]
fn a_stream_frozen_in_half_its_episodes_is_the_datasets_shape() {
    // The other side of the same check: frozen in a strict minority is a fault, frozen in half or
    // more is how the dataset was built.
    let check = structural::FrozenEpisode;
    assert!(
        codes(check.run(&frozen_dataset(4, 2, 8))).is_empty(),
        "two of four is not a minority"
    );
    assert_eq!(
        codes(check.run(&frozen_dataset(5, 2, 8))).len(),
        2,
        "two of five is, and each frozen episode is named"
    );
}
