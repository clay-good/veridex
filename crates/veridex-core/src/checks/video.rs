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
//! [`Media`](crate::cdm::Media) when the adapter resolved a media file for it, so a scalar feature,
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
        &["VIDEO.MEDIA_MISSING", "VIDEO.MEDIA_UNREADABLE"]
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
        let mut findings = Vec::new();
        for (episode, stream, media) in media_streams(dataset) {
            let location = Location::Stream {
                episode,
                stream: stream.to_string(),
            };
            match &media.status {
                MediaStatus::Read => {}
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
    episodes: u64,
    detail: String,
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
        // Frame count is a per-episode fact — one bad video is one bad episode — so it is reported
        // per episode. Resolution, codec, and rate are properties of how the dataset was *exported*:
        // when they are wrong they are wrong for every episode, and a thousand identical findings
        // buries the one defect they describe. Those are charged once per stream, naming the first
        // episode and how many share it.
        let mut findings = Vec::new();
        let mut export_defects: BTreeMap<(&str, &'static str), Occurrence> = BTreeMap::new();

        for ep in &dataset.episodes {
            for s in &ep.streams {
                let Some(media) = &s.media else { continue };
                if media.status != MediaStatus::Read {
                    // An unreadable container has nothing to compare; `video.media-readable` owns it.
                    continue;
                }

                if let Some(container_frames) = media.frame_count {
                    let data_frames = s.frames.len() as u64;
                    if container_frames != data_frames {
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

                let mut record = |code: &'static str, detail: String| {
                    export_defects
                        .entry((s.name.as_str(), code))
                        .and_modify(|o| o.episodes += 1)
                        .or_insert(Occurrence {
                            first_episode: ep.index,
                            episodes: 1,
                            detail,
                        });
                };

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
                    if canonical_codec(dc) != canonical_codec(oc) {
                        record(
                            "VIDEO.CODEC_MISMATCH",
                            format!("declared `{dc}`, container holds `{oc}`"),
                        );
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

        for ((stream, code), occ) in export_defects {
            let (message_head, risk, remedy) = match code {
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
                    Severity::Warning,
                    Location::Stream {
                        episode: occ.first_episode,
                        stream: stream.to_string(),
                    },
                    code,
                    format!(
                        "stream `{stream}`: {message_head} disagrees with the manifest — {} ({scope})",
                        occ.detail
                    ),
                )
                .with_risk(risk)
                .with_remedy(remedy),
            );
        }
        findings
    }
}
