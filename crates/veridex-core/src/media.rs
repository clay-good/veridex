//! Reading a video container's headers — enough to say what it holds, never a decoded pixel.
//!
//! Veridex's promise is that it only reads and reports, and the `video.*` checks need exactly four
//! facts about a media file: how many frames it holds, at what resolution, in which codec, at what
//! rate. All four live in an MP4's `moov` box, which is metadata — so this module walks the ISO
//! base media file format (ISO/IEC 14496-12) box tree and stops there. No decoder, no `mdat`.
//!
//! The file is untrusted, so the walk is bounded the same way ingestion is: box sizes are validated
//! against what is actually left in the file rather than believed (so every box advances the cursor
//! and no declared size reaches past the end), the walk is iterative rather than recursive, and only
//! the `moov` box is read into memory — under a ceiling, because its declared size is a number the
//! file chose.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::cdm::MediaParams;

/// Ceiling on the `moov` box Veridex will read into memory.
///
/// `moov` is metadata: even an hour-long recording's sample tables are a few megabytes. The declared
/// size is attacker-controlled, so a file claiming more than this is refused as unreadable rather
/// than allocated for.
const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;

/// What an MP4's headers say about its video track.
#[derive(Debug, Clone, PartialEq)]
pub struct Mp4Probe {
    /// The track's encoding parameters, as far as the container states them.
    pub params: MediaParams,
    /// The video track's sample count — the frames the file holds.
    pub frame_count: Option<u64>,
}

/// Read `path`'s headers and report what its video track holds.
///
/// Returns `Err` with a human-readable reason when the file is not a readable MP4: that reason is
/// what `VIDEO.MEDIA_UNREADABLE` reports, so it names the specific structure that was wrong rather
/// than "parse error".
///
/// Every reason is built from the container's own structure and, for I/O failures, from
/// [`std::io::ErrorKind`] rather than the operating system's error text — which is
/// platform- and locale-dependent, and would otherwise make the same dataset report differently on
/// two machines.
pub fn probe_mp4(path: &Path) -> Result<Mp4Probe, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open: {:?}", e.kind()))?;
    let file_len = file
        .metadata()
        .map_err(|e| format!("cannot stat: {:?}", e.kind()))?
        .len();

    let moov = read_moov(&mut file, file_len)?;
    let track = video_track(&moov).ok_or_else(|| {
        "no video track: the container's moov box declares no track with a 'vide' handler"
            .to_string()
    })?;
    Ok(track)
}

/// Locate the top-level `moov` box and read it into memory.
///
/// Top level only — `moov` is always a top-level box — so this never descends into `mdat`.
fn read_moov(file: &mut File, file_len: u64) -> Result<Vec<u8>, String> {
    let mut offset = 0u64;
    let mut saw_any_box = false;
    // A box may declare size 0, meaning "I extend to the end of the file". Anything written after it
    // is unreachable by definition, so if the walk ends without a `moov`, the reason is that box —
    // not an absent one. Naming it is the difference between a user re-encoding and a user checking
    // whether their writer truncated the header.
    let mut open_ended: Option<String> = None;
    while offset < file_len {
        let header = read_box_header(file, offset, file_len)?;
        let (payload_offset, payload_len, kind) =
            (header.payload_offset, header.payload_len, header.kind);
        saw_any_box = true;
        if header.open_ended && &kind != b"moov" {
            open_ended = Some(fourcc(&kind));
        }
        if &kind == b"moov" {
            if payload_len > MAX_MOOV_BYTES {
                return Err(format!(
                    "moov box declares {payload_len} bytes, over the {MAX_MOOV_BYTES}-byte ceiling"
                ));
            }
            let mut buf = vec![0u8; payload_len as usize];
            file.seek(SeekFrom::Start(payload_offset))
                .map_err(|e| format!("cannot seek to moov: {:?}", e.kind()))?;
            file.read_exact(&mut buf)
                .map_err(|e| format!("moov box is truncated: {:?}", e.kind()))?;
            return Ok(buf);
        }
        offset = payload_offset + payload_len;
    }
    Err(match (saw_any_box, open_ended) {
        (_, Some(kind)) => format!(
            "no moov box: the '{kind}' box declares that it runs to the end of the file, so any \
             movie metadata written after it is unreachable"
        ),
        (true, None) => "no moov box: the file's top-level boxes carry no movie metadata".into(),
        (false, None) => "not an MP4 container: the file has no ISO base media boxes".into(),
    })
}

/// One box header, as read from the file.
struct BoxHeader {
    /// Offset of the box's payload within the file.
    payload_offset: u64,
    /// Length of that payload.
    payload_len: u64,
    /// The box's four-character type.
    kind: [u8; 4],
    /// The box declared size 0 — "I run to the end of the file" — so nothing after it is reachable.
    open_ended: bool,
}

/// Read one box header at `offset`.
///
/// Every size is validated against the bytes actually remaining, so a declared size never sends the
/// walk past the end of the file or backwards into a loop.
fn read_box_header(file: &mut File, offset: u64, file_len: u64) -> Result<BoxHeader, String> {
    if file_len - offset < 8 {
        return Err(format!(
            "truncated box header at offset {offset}: {} bytes left, 8 needed",
            file_len - offset
        ));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("cannot seek to offset {offset}: {:?}", e.kind()))?;
    let mut header = [0u8; 8];
    file.read_exact(&mut header)
        .map_err(|e| format!("cannot read box header at offset {offset}: {:?}", e.kind()))?;
    let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as u64;
    let kind: [u8; 4] = [header[4], header[5], header[6], header[7]];

    let (header_len, total_len) = match size32 {
        // 0 means "this box extends to the end of the file".
        0 => (8u64, file_len - offset),
        // 1 means a 64-bit size follows the type.
        1 => {
            if file_len - offset < 16 {
                return Err(format!(
                    "truncated 64-bit box header at offset {offset}: {} bytes left, 16 needed",
                    file_len - offset
                ));
            }
            let mut ext = [0u8; 8];
            file.read_exact(&mut ext).map_err(|e| {
                format!(
                    "cannot read 64-bit box size at offset {offset}: {:?}",
                    e.kind()
                )
            })?;
            (16u64, u64::from_be_bytes(ext))
        }
        n => (8u64, n),
    };

    if total_len < header_len {
        return Err(format!(
            "box '{}' at offset {offset} declares {total_len} bytes, smaller than its own header",
            fourcc(&kind)
        ));
    }
    if total_len > file_len - offset {
        return Err(format!(
            "box '{}' at offset {offset} declares {total_len} bytes but only {} remain",
            fourcc(&kind),
            file_len - offset
        ));
    }
    Ok(BoxHeader {
        payload_offset: offset + header_len,
        payload_len: total_len - header_len,
        kind,
        open_ended: size32 == 0,
    })
}

/// A box within an in-memory buffer: its type and its payload slice.
struct InnerBox<'a> {
    kind: [u8; 4],
    payload: &'a [u8],
}

/// Split a buffer into the sequence of boxes it holds. A malformed size ends the walk rather than
/// failing the whole probe: a container can carry a box Veridex does not model, and what it *has*
/// already read is still true.
fn boxes(buf: &[u8]) -> Vec<InnerBox<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let size32 =
            u32::from_be_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]) as u64;
        let kind: [u8; 4] = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
        let remaining = (buf.len() - pos) as u64;
        let (header_len, total_len) = match size32 {
            0 => (8u64, remaining),
            1 => {
                if pos + 16 > buf.len() {
                    break;
                }
                let mut ext = [0u8; 8];
                ext.copy_from_slice(&buf[pos + 8..pos + 16]);
                (16u64, u64::from_be_bytes(ext))
            }
            n => (8u64, n),
        };
        if total_len < header_len || total_len > remaining {
            break;
        }
        let start = pos + header_len as usize;
        let end = pos + total_len as usize;
        out.push(InnerBox {
            kind,
            payload: &buf[start..end],
        });
        pos = end;
    }
    out
}

/// The first child of `buf` with type `kind`.
fn find<'a>(buf: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(buf)
        .into_iter()
        .find(|b| &b.kind == kind)
        .map(|b| b.payload)
}

/// Follow a path of box types down from `buf` (e.g. `mdia/minf/stbl`).
fn descend<'a>(buf: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
    let mut cur = buf;
    for kind in path {
        cur = find(cur, kind)?;
    }
    Some(cur)
}

/// The video track's parameters, from the first `trak` whose handler is `vide`.
///
/// The *first* such track deliberately: a LeRobot video file holds one video track, and picking by
/// position rather than by "the largest" or "the longest" keeps the probe a pure function of the
/// bytes, so the same file always probes identically.
fn video_track(moov: &[u8]) -> Option<Mp4Probe> {
    // A **fragmented** file (`ffmpeg -movflags frag_keyframe+empty_moov`, DASH/CMAF, most hardware
    // recorders) declares `mvex` in its `moov` and carries every sample in `moof` fragments, leaving
    // the sample table in `moov` empty. Its `stsz` therefore says zero — which is "the table is not
    // here", not "this file holds no frames". Reading it as a count would report a hard
    // frame-count mismatch against every episode of a perfectly valid dataset.
    let fragmented = find(moov, b"mvex").is_some();
    for b in boxes(moov) {
        if &b.kind != b"trak" {
            continue;
        }
        // A `trak` this parser cannot walk is skipped, not fatal: a file can carry a track with no
        // `mdia` (or one whose children this parser stops short of) *and* a perfectly good video
        // track after it. Abandoning the scan here would report "no video track" about a file that
        // has one.
        let Some(mdia) = find(b.payload, b"mdia") else {
            continue;
        };
        let handler = find(mdia, b"hdlr").and_then(handler_type);
        if handler.as_deref() != Some("vide") {
            continue;
        }
        let media_time = find(mdia, b"mdhd").and_then(media_header);
        let stbl = descend(mdia, &[b"minf", b"stbl"]);
        // `stz2` is the compact sample-size table: a different encoding of the same field, at the
        // same offset. An encoder's choice between the two must not decide whether the check runs.
        let frame_count = if fragmented {
            None
        } else {
            stbl.and_then(|s| find(s, b"stsz").or_else(|| find(s, b"stz2")))
                .and_then(sample_count)
        };
        let (codec, width, height) =
            match stbl.and_then(|s| find(s, b"stsd")).and_then(sample_entry) {
                Some((c, w, h)) => (Some(c), Some(w), Some(h)),
                None => (None, None, None),
            };
        // fps is frames over elapsed media time. Both factors come from the file, so a zero or
        // absent timescale/duration yields no rate rather than a division that fabricates one.
        let fps = match (frame_count, media_time) {
            (Some(n), Some((ts, d))) if ts > 0 && d > 0 => Some(n as f64 * ts as f64 / d as f64),
            _ => None,
        };
        return Some(Mp4Probe {
            params: MediaParams {
                codec,
                width,
                height,
                fps,
            },
            frame_count,
        });
    }
    None
}

/// The `hdlr` box's handler type (`vide`, `soun`, …).
fn handler_type(hdlr: &[u8]) -> Option<String> {
    // version+flags (4), pre_defined (4), handler_type (4).
    let bytes = hdlr.get(8..12)?;
    Some(fourcc(bytes))
}

/// The `mdhd` box's `(timescale, duration)` — the track's time base and its length in that base.
///
/// ISO/IEC 14496-12 reserves an all-ones `duration` to mean **unknown**, which a still-being-written
/// or fragmented file legitimately carries. Taken at face value it is a duration of 49 days, and the
/// rate derived from it is a fabrication that reports every such file as playing at the wrong speed —
/// so it is returned as absent, and no rate is derived at all.
fn media_header(mdhd: &[u8]) -> Option<(u32, u64)> {
    let version = *mdhd.first()?;
    match version {
        0 => {
            // version+flags (4), creation (4), modification (4), timescale (4), duration (4).
            let timescale = u32::from_be_bytes(mdhd.get(12..16)?.try_into().ok()?);
            let duration = u32::from_be_bytes(mdhd.get(16..20)?.try_into().ok()?);
            if duration == u32::MAX {
                return None;
            }
            Some((timescale, duration as u64))
        }
        1 => {
            // version+flags (4), creation (8), modification (8), timescale (4), duration (8).
            let timescale = u32::from_be_bytes(mdhd.get(20..24)?.try_into().ok()?);
            let duration = u64::from_be_bytes(mdhd.get(24..32)?.try_into().ok()?);
            if duration == u64::MAX {
                return None;
            }
            Some((timescale, duration))
        }
        _ => None,
    }
}

/// A sample-size box's sample count — the frames the track holds. `stsz` (§8.7.3.2) and `stz2`
/// (§8.7.3.3) carry it at the same offset.
fn sample_count(stsz: &[u8]) -> Option<u64> {
    // version+flags (4), sample_size / field_size (4), sample_count (4).
    let count = u32::from_be_bytes(stsz.get(8..12)?.try_into().ok()?);
    Some(count as u64)
}

/// The first `stsd` entry's `(codec fourcc, width, height)`.
fn sample_entry(stsd: &[u8]) -> Option<(String, u64, u64)> {
    // version+flags (4), entry_count (4), then the entries as boxes.
    let entries = stsd.get(8..)?;
    let entry = boxes(entries).into_iter().next()?;
    let codec = fourcc(&entry.kind);
    // VisualSampleEntry payload: reserved (6), data_reference_index (2), pre_defined (2),
    // reserved (2), pre_defined (12), width (2), height (2).
    let width = u16::from_be_bytes(entry.payload.get(24..26)?.try_into().ok()?) as u64;
    let height = u16::from_be_bytes(entry.payload.get(26..28)?.try_into().ok()?) as u64;
    Some((codec, width, height))
}

/// A four-character box/handler type as a string, with any non-printable byte escaped so a corrupt
/// container's type never lands raw in a report.
fn fourcc(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                (b as char).to_string()
            } else {
                format!("\\x{b:02x}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_container_is_rejected_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not.mp4");
        // Eight bytes that parse as a box header declaring far more than the file holds.
        std::fs::write(&path, b"\xff\xff\xff\xffftyp").unwrap();
        let err = probe_mp4(&path).unwrap_err();
        assert!(err.contains("only"), "{err}");
    }

    #[test]
    fn an_empty_file_is_rejected_as_not_a_container() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.mp4");
        std::fs::write(&path, b"").unwrap();
        let err = probe_mp4(&path).unwrap_err();
        assert!(err.contains("no ISO base media boxes"), "{err}");
    }

    #[test]
    fn a_box_smaller_than_its_header_does_not_loop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.mp4");
        // A declared size of 4 is smaller than the 8-byte header: believing it would leave the walk
        // at the same offset forever.
        std::fs::write(&path, b"\x00\x00\x00\x04ftyp____").unwrap();
        let err = probe_mp4(&path).unwrap_err();
        assert!(err.contains("smaller than its own header"), "{err}");
    }

    #[test]
    fn an_oversized_moov_is_refused_rather_than_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.mp4");
        // A 64-bit moov declaring 4 GiB, in a file that really is that long only on paper: the size
        // check against the ceiling has to fire before any allocation is attempted.
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(&1u32.to_be_bytes());
        bytes[4..8].copy_from_slice(b"moov");
        bytes[8..16].copy_from_slice(&(4u64 * 1024 * 1024 * 1024).to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let err = probe_mp4(&path).unwrap_err();
        // Bounded either by the ceiling or by the bytes that actually remain — never allocated.
        assert!(err.contains("ceiling") || err.contains("remain"), "{err}");
    }
}
