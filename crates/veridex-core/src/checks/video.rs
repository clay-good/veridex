//! Video and media checks: does the media file match the data it is paired with?
//!
//! A video dataset is two artifacts that nothing reconciles. The manifest declares an encoding and
//! the data table carries one row per frame; the pixels live in a separate `.mp4`. Re-encode the
//! video, resume an interrupted upload, or export the two halves from different runs, and the
//! dataset still loads — the loader indexes the video by frame number and hands back whatever sits
//! at that index. Every observation after the divergence is paired with the wrong action.
//!
//! These checks compare the two halves. They read the container's **headers** only (see
//! [`crate::media`]) — Veridex never decodes a pixel, so "is this video decodable" is answered as
//! "does this container parse and describe a video track", which is what catches the truncated and
//! half-written files, not a per-frame decode.
//!
//! Both checks are silent on a dataset with no separate media: a stream only carries
//! [`Media`] when the adapter resolved a media file for it, so a scalar feature,
//! an inline-image dataset, or a video layout Veridex cannot resolve per episode contributes
//! nothing rather than a false alarm.

use std::collections::BTreeMap;

use crate::cdm::{canonical_codec, Dataset, Media, MediaStatus};
use crate::check::{Category, Check, Finding, Location, Scope, Severity};

/// Walk every stream that has a media file, in canonical order, as `(episode, stream, media)`.
fn media_streams(dataset: &Dataset) -> impl Iterator<Item = (u64, &str, &Media)> {
    dataset.episodes.iter().flat_map(|ep| {
        ep.streams
            .iter()
            .filter_map(move |s| s.media.as_ref().map(|m| (ep.index, s.name.as_str(), m)))
    })
}

/// The media file the manifest implies is on disk and parses as a video container.
pub struct MediaReadable;

impl Check for MediaReadable {
    fn id(&self) -> &'static str {
        "video.media-readable"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "VIDEO.MEDIA_ABSENT",
            "VIDEO.MEDIA_MISSING",
            "VIDEO.MEDIA_UNREADABLE",
            "VIDEO.MEDIA_UNATTRIBUTED",
        ]
    }

    fn abstention_codes(&self) -> &'static [&'static str] {
        &["VIDEO.MEDIA_UNREADABLE"]
    }
    fn title(&self) -> &'static str {
        "Media file present and readable"
    }
    fn category(&self) -> Category {
        Category::Video
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // A stream whose media is missing for *every* episode is one gap, not one per episode: the
        // usual cause is a whole tree that never arrived — an un-pulled LFS pointer, an interrupted
        // download — and a thousand identical findings describe that no better than one does. A
        // stream missing media for only some episodes is the opposite: each of those episodes is
        // separately wrong, and naming them is the point.
        let mut per_stream: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new(); // name -> (episodes, missing, first_episode)
        for (episode, stream, media) in media_streams(dataset) {
            let entry = per_stream.entry(stream).or_insert((0, 0, episode));
            entry.0 += 1;
            if media.status == MediaStatus::Missing {
                entry.1 += 1;
            }
        }

        let mut findings = Vec::new();
        let mut absent_reported: BTreeMap<&str, ()> = BTreeMap::new();
        for (episode, stream, media) in media_streams(dataset) {
            let location = Location::Stream {
                episode,
                stream: stream.to_string(),
            };
            let wholly_absent = per_stream
                .get(stream)
                .is_some_and(|(episodes, missing, _)| episodes == missing && *episodes > 0);
            match &media.status {
                MediaStatus::Read => {}
                // The manifest says this stream's pixels are in video files, and they may well all
                // be there — just not laid out one file per episode, so no container can be paired
                // with the rows it belongs to. Reported once for the stream, informational: nothing
                // is wrong with the dataset, and nothing about its video was verified either. Before
                // this, such a stream carried no `media` at all, so the whole video family iterated
                // past it and emitted nothing — even for a file holding no container.
                MediaStatus::Unattributable { reason } => {
                    if absent_reported.insert(stream, ()).is_some() {
                        continue;
                    }
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Video,
                            Severity::Info,
                            location,
                            "VIDEO.MEDIA_UNATTRIBUTED",
                            format!(
                                "stream `{stream}`: {reason}, so no container could be paired with \
                                 the rows it belongs to and the video checks did not run on it"
                            ),
                        )
                        .with_risk(
                            "A desynced, re-encoded, or unreadable video in this stream is not \
                             absent from this report — it was never looked for. The pairing these \
                             checks verify is between a container and the episode whose actions it \
                             accompanies, and that pairing is what the layout does not express.",
                        )
                        .with_remedy(
                            "Re-export with one media file per episode (`episode_<n>.<ext>`) if you \
                             want the video checked against the data it is paired with.",
                        ),
                    );
                }
                MediaStatus::Missing if wholly_absent => {
                    if absent_reported.insert(stream, ()).is_some() {
                        continue;
                    }
                    let (episodes, ..) = per_stream[stream];
                    findings.push(
                        Finding::new(
                            self.id(),
                            Category::Video,
                            Severity::Error,
                            location,
                            "VIDEO.MEDIA_ABSENT",
                            format!(
                                "stream `{stream}`: the manifest declares this stream's pixels \
                                 live in video files, but none were found under `{}` — no episode \
                                 of {episodes} has any imagery",
                                media.uri
                            ),
                        )
                        .with_risk(
                            "Every episode's frames have timestamps and actions but no imagery at \
                             all. Nothing in the manifest or the data table records this, so the \
                             dataset reads as complete right up until a loader tries to open a \
                             video — the signature of an un-pulled LFS pointer or an interrupted \
                             download.",
                        )
                        .with_remedy(
                            "Fetch the video files (for a Hugging Face repo, `git lfs pull` or a \
                             full `snapshot_download`), or drop the video feature from \
                             `meta/info.json` if this copy is deliberately data-only.",
                        ),
                    );
                }
                MediaStatus::Missing => findings.push(
                    Finding::new(
                        self.id(),
                        Category::Video,
                        Severity::Error,
                        location,
                        "VIDEO.MEDIA_MISSING",
                        format!(
                            "episode {episode} stream `{stream}`: no media file at `{}`, though \
                             the dataset stores this stream's video per episode",
                            media.uri
                        ),
                    )
                    .with_risk(
                        "The episode's frames have timestamps and actions but no imagery. A loader \
                         either fails on this episode or, worse, silently trains on the remaining \
                         streams with the visual observation absent.",
                    )
                    .with_remedy(
                        "Re-export or re-upload the missing video file, or drop the episode from \
                         the manifest so the dataset stops claiming imagery it does not have.",
                    ),
                ),
                MediaStatus::Unreadable { reason } => findings.push(
                    Finding::new(
                        self.id(),
                        Category::Video,
                        Severity::Error,
                        location,
                        "VIDEO.MEDIA_UNREADABLE",
                        format!(
                            "episode {episode} stream `{stream}`: `{}` is not a readable video \
                             container ({reason})",
                            media.uri
                        ),
                    )
                    .with_risk(
                        "A container that will not parse will not decode. Training fails at this \
                         episode — usually hours in, once the loader reaches it.",
                    )
                    .with_remedy(
                        "Re-encode or re-upload the file; a truncated size against the original is \
                         the usual cause of an interrupted transfer.",
                    ),
                ),
            }
        }
        findings
    }
}

/// Streams where every episode that could be *measured* misses its row count by the *same* signed
/// amount, and at least two could be.
///
/// That is the fingerprint of a systematic export defect — an encoder dropping a leading frame, a
/// converter counting from one — rather than N independently broken episodes, and it is charged once
/// like the other export-wide defects. A stream where only some episodes are wrong, or where they
/// are wrong by differing amounts, is not here: those episodes are each separately wrong, and naming
/// them is the point. Neither is a stream where only one episode yielded a length — an episode whose
/// file is missing, whose container would not parse, or whose container declares no sample count was
/// never weighed, and one measurement is not a pattern.
fn systematic_frame_delta(dataset: &Dataset) -> BTreeMap<&str, i64> {
    /// What one stream's episodes have shown so far.
    struct Pattern {
        /// The delta every measured episode has agreed on, or `None` once two disagreed or one
        /// came back clean.
        delta: Option<i64>,
        /// Distinct episodes that actually yielded a length. Counted by watching the episode index
        /// change: a stream name may legitimately appear twice in one episode.
        measured: u64,
        last_episode: Option<u64>,
    }

    let mut patterns: BTreeMap<&str, Pattern> = BTreeMap::new();
    for ep in &dataset.episodes {
        for s in &ep.streams {
            // An episode whose file is missing, whose container would not parse, or whose container
            // declares no sample count contributed no length. It is not evidence for a pattern and
            // it is not evidence against one — it simply was not measured, and the roll-up's claim
            // is about the episodes that were.
            let Some(media) = &s.media else { continue };
            if media.status != MediaStatus::Read {
                continue;
            }
            let Some(container_frames) = media.frame_count else {
                continue;
            };
            let delta = container_frames as i64 - s.frames.len() as i64;
            let pattern = patterns.entry(s.name.as_str()).or_insert(Pattern {
                delta: Some(delta),
                measured: 0,
                last_episode: None,
            });
            if pattern.last_episode != Some(ep.index) {
                pattern.measured += 1;
                pattern.last_episode = Some(ep.index);
            }
            // A zero delta means this episode is fine, so the stream is not systematically off.
            if pattern.delta != Some(delta) || delta == 0 {
                pattern.delta = None;
            }
        }
    }

    patterns
        .into_iter()
        // A stream measured in a single episode has no pattern to be systematic about; leave it to
        // the per-episode report, which names the file and both counts.
        .filter(|(_, p)| p.measured > 1)
        .filter_map(|(name, p)| p.delta.filter(|d| *d != 0).map(|d| (name, d)))
        .collect()
}

/// The media file holds what the dataset says it holds: as many frames as the data stream, at the
/// declared resolution, codec, and rate.
pub struct MediaConformance {
    /// Relative tolerance on the frames-per-second comparison (shares `rate_deviation` with
    /// `temporal.rate-conformance`: same question, one knob).
    pub fps_tolerance: f64,
}

/// One deduplicated conformance defect: the first episode that showed it, and how many did.
struct Occurrence {
    first_episode: u64,
    /// Distinct episodes that showed it. Counted by watching the episode index change rather than
    /// by call count: a stream name may legitimately appear twice in one episode (a duplicate key is
    /// a condition Veridex reports rather than assumes away), and counting those as two episodes
    /// would overstate the spread of the defect.
    last_episode: u64,
    episodes: u64,
}

impl Check for MediaConformance {
    fn id(&self) -> &'static str {
        "video.media-conformance"
    }
    fn finding_codes(&self) -> &'static [&'static str] {
        &[
            "VIDEO.FRAME_COUNT_MISMATCH",
            "VIDEO.RESOLUTION_MISMATCH",
            "VIDEO.CODEC_MISMATCH",
            "VIDEO.FPS_MISMATCH",
        ]
    }
    fn title(&self) -> &'static str {
        "Media matches its declared encoding and paired data"
    }
    fn category(&self) -> Category {
        Category::Video
    }
    fn default_severity(&self) -> Severity {
        Severity::Error
    }
    fn scope(&self) -> Scope {
        Scope::Stream
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn run(&self, dataset: &Dataset) -> Vec<Finding> {
        // Frame count is normally a per-episode fact — one bad video is one bad episode — so it is
        // reported per episode. The exception is an export where *every* episode's video is off by
        // the *same* amount: that is one systematic defect (an encoder dropping a leading frame, a
        // converter counting from one), and a thousand identical findings describe it no better than
        // one does. Resolution, codec, and rate are always properties of the export, so they are
        // charged once per stream per distinct value.
        let systematic_delta = systematic_frame_delta(dataset);
        let mut findings = Vec::new();
        let mut export_defects: BTreeMap<(&str, &'static str, String), Occurrence> =
            BTreeMap::new();

        for ep in &dataset.episodes {
            for s in &ep.streams {
                let Some(media) = &s.media else { continue };
                if media.status != MediaStatus::Read {
                    // An unreadable container has nothing to compare; `video.media-readable` owns it.
                    continue;
                }

                // Keyed by the *value* as well as the code: two episodes encoded at different wrong
                // resolutions are two defects, and collapsing them under whichever came first would
                // report the second episode as holding a resolution it does not hold — and hide the
                // more serious condition, that the episodes disagree with each other.
                let mut record = |code: &'static str, detail: String| {
                    export_defects
                        .entry((s.name.as_str(), code, detail))
                        .and_modify(|o| {
                            if o.last_episode != ep.index {
                                o.episodes += 1;
                                o.last_episode = ep.index;
                            }
                        })
                        .or_insert(Occurrence {
                            first_episode: ep.index,
                            last_episode: ep.index,
                            episodes: 1,
                        });
                };

                if let Some(container_frames) = media.frame_count {
                    let data_frames = s.frames.len() as u64;
                    if container_frames != data_frames {
                        if let Some(&delta) = systematic_delta.get(s.name.as_str()) {
                            record(
                                "VIDEO.FRAME_COUNT_MISMATCH",
                                format!(
                                    "every episode with a readable video is {} frame(s) {} than \
                                     its rows",
                                    delta.unsigned_abs(),
                                    if delta < 0 { "shorter" } else { "longer" }
                                ),
                            );
                        } else {
                            findings.push(
                                Finding::new(
                                    self.id(),
                                    Category::Video,
                                    Severity::Error,
                                    Location::Stream {
                                        episode: ep.index,
                                        stream: s.name.clone(),
                                    },
                                    "VIDEO.FRAME_COUNT_MISMATCH",
                                    format!(
                                        "episode {} stream `{}`: `{}` holds {container_frames} frames \
                                         but the episode records {data_frames}",
                                        ep.index, s.name, media.uri
                                    ),
                                )
                                .with_risk(
                                    "A loader pairs video frame i with data row i. When the two lengths \
                                     differ, every pair past the shorter one is wrong or absent — the \
                                     policy learns actions against images from a different moment.",
                                )
                                .with_remedy(
                                    "Re-export the episode so the video and the data table are written \
                                     from the same run; a video shorter than the table is usually an \
                                     encode that stopped early.",
                                ),
                            );
                        }
                    }
                }

                if let (Some(dw), Some(dh), Some(ow), Some(oh)) = (
                    media.declared.width,
                    media.declared.height,
                    media.observed.width,
                    media.observed.height,
                ) {
                    if (dw, dh) != (ow, oh) {
                        record(
                            "VIDEO.RESOLUTION_MISMATCH",
                            format!("declared {dw}x{dh}, container holds {ow}x{oh}"),
                        );
                    }
                }

                if let (Some(dc), Some(oc)) = (&media.declared.codec, &media.observed.codec) {
                    // Both names must resolve to a codec Veridex recognizes. Encoder names are an
                    // open namespace — a manifest may record `libopenh264` or `h264_videotoolbox`
                    // for the same encoding a container stamps `avc1` — so an unrecognized spelling
                    // means "cannot tell", which is not the same as "differ".
                    if let (Some(d), Some(o)) = (canonical_codec(dc), canonical_codec(oc)) {
                        if d != o {
                            record(
                                "VIDEO.CODEC_MISMATCH",
                                format!("declared `{dc}`, container holds `{oc}`"),
                            );
                        }
                    }
                }

                if let (Some(df), Some(of)) = (media.declared.fps, media.observed.fps) {
                    // A declared rate that is not a positive finite number is corrupt metadata,
                    // which `temporal.rate-validity` owns; comparing against it would only produce
                    // a second finding for one defect.
                    if df.is_finite() && df > 0.0 && of.is_finite() && of > 0.0 {
                        let deviation = (of - df).abs() / df;
                        if deviation > self.fps_tolerance {
                            record(
                                "VIDEO.FPS_MISMATCH",
                                format!(
                                    "declared {df:.3} fps, container plays at {of:.3} fps \
                                     ({:.1}% off)",
                                    deviation * 100.0
                                ),
                            );
                        }
                    }
                }
            }
        }

        for ((stream, code, detail), occ) in export_defects {
            let (message_head, risk, remedy) = match code {
                "VIDEO.FRAME_COUNT_MISMATCH" => (
                    "video length",
                    "A loader pairs video frame i with data row i. When the two lengths differ, \
                     every pair past the shorter one is wrong or absent — the policy learns actions \
                     against images from a different moment. That every episode is off by the same \
                     amount points at the export step rather than at any one recording.",
                    "Re-export so the video and the data table are written from the same run; a \
                     constant offset across every episode is usually an encoder dropping a leading \
                     frame or a converter counting from one.",
                ),
                "VIDEO.RESOLUTION_MISMATCH" => (
                    "video resolution",
                    "The manifest's resolution is what preprocessing pipelines size their tensors \
                     from. A container at a different resolution is silently rescaled or crops the \
                     wrong region, so the policy sees a different image than the dataset promises.",
                    "Re-encode the video at the declared resolution, or correct the declaration in \
                     the manifest — whichever of the two is the one that is wrong.",
                ),
                "VIDEO.CODEC_MISMATCH" => (
                    "video codec",
                    "Consumers select a decoder from the declared codec. A file in a different one \
                     either fails to open or takes a fallback path with different colour handling, \
                     so the pixels differ from the ones the dataset was validated on.",
                    "Correct the declared codec in the manifest, or re-encode to it.",
                ),
                "VIDEO.FPS_MISMATCH" => (
                    "video frame rate",
                    "Frame rate is how video time is converted to data time. A container playing at \
                     a different rate drifts against the action timeline it is paired with, growing \
                     the further into the episode it goes.",
                    "Re-export the video at the declared rate, or correct the declared rate — the \
                     data table's own timestamps say which one is right.",
                ),
                _ => continue,
            };
            let scope = if occ.episodes == 1 {
                format!("episode {}", occ.first_episode)
            } else {
                format!(
                    "{} episodes, first at episode {}",
                    occ.episodes, occ.first_episode
                )
            };
            findings.push(
                Finding::new(
                    self.id(),
                    Category::Video,
                    // Rolling a defect up changes how often it is reported, never how serious it is.
                    if code == "VIDEO.FRAME_COUNT_MISMATCH" {
                        Severity::Error
                    } else {
                        Severity::Warning
                    },
                    Location::Stream {
                        episode: occ.first_episode,
                        stream: stream.to_string(),
                    },
                    code,
                    format!(
                        "stream `{stream}`: {message_head} disagrees with the manifest — {detail} ({scope})"
                    ),
                )
                .with_risk(risk)
                .with_remedy(remedy),
            );
        }
        findings
    }
}
