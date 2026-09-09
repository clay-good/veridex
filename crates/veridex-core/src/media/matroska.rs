//! Reading a Matroska (`.mkv`) or WebM container's headers — the same four facts, none of the pixels.
//!
//! Matroska keeps what the `video.*` checks need in three places: the `Tracks` element states the
//! codec and the encoded resolution, `DefaultDuration` states the nominal frame interval, and the
//! frames themselves are `SimpleBlock`/`Block` elements inside the `Cluster`s. Unlike an MP4, a
//! Matroska file carries **no sample table** — nothing anywhere states how many frames it holds — so
//! the count is obtained the only way the format allows: by walking the cluster tree and reading
//! each block's *header*. The block payloads are seeked over, never read, so this stays a metadata
//! walk: no decoder, no compressed frame in memory.
//!
//! The file is untrusted, so every declared size is validated against the bytes that actually remain
//! rather than believed, the walk is iterative rather than recursive, and the two elements read into
//! memory (the EBML header and a `Tracks` element) are read under ceilings, because their sizes are
//! numbers the file chose.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use super::MediaProbe;
use crate::cdm::MediaParams;

// ---- element ids -------------------------------------------------------------------------------
//
// Ids are compared as their encoded bytes, marker bit included, which is how the Matroska
// specification writes them.

const ID_EBML: u64 = 0x1A45_DFA3;
const ID_DOC_TYPE: u64 = 0x4282;
const ID_SEGMENT: u64 = 0x1853_8067;
const ID_TRACKS: u64 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u64 = 0xAE;
const ID_TRACK_NUMBER: u64 = 0xD7;
const ID_TRACK_TYPE: u64 = 0x83;
const ID_CODEC_ID: u64 = 0x86;
const ID_DEFAULT_DURATION: u64 = 0x0023_E383;
const ID_VIDEO: u64 = 0xE0;
const ID_PIXEL_WIDTH: u64 = 0xB0;
const ID_PIXEL_HEIGHT: u64 = 0xBA;
const ID_CLUSTER: u64 = 0x1F43_B675;
const ID_SIMPLE_BLOCK: u64 = 0xA3;
const ID_BLOCK_GROUP: u64 = 0xA0;
const ID_BLOCK: u64 = 0xA1;

/// `TrackType` for a video track.
const TRACK_TYPE_VIDEO: u64 = 1;

/// Ceiling on the EBML header and on a `Tracks` element read into memory. Both are metadata a
/// writer emits once; a file declaring more than this is refused rather than allocated for.
const MAX_ELEMENT_BYTES: u64 = 16 * 1024 * 1024;

/// Ceiling on the blocks counted before the walk gives up and reports no count.
///
/// Twenty million frames is over a week of 30 fps video — far past any episode a dataset pairs with
/// a row count, so a file that reaches it is not a recording whose frame count this check was going
/// to compare. Reaching it yields *no* count rather than the partial one, because a count that
/// stopped early is a mismatch against every honest episode.
const MAX_BLOCKS: u64 = 20_000_000;

/// Distinct track numbers whose blocks are counted. A recording carries a handful; a file naming
/// more than this is not counted at all rather than allocated against.
const MAX_TRACKS: usize = 256;

/// True when `bytes` opens with the EBML magic — how this container is told apart from an MP4
/// before either parser is asked to explain why it is not the other one.
pub fn is_ebml(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// Read `path`'s headers and report what its video track holds.
///
/// Returns `Err` with a human-readable reason when the file is not a readable Matroska: as with the
/// MP4 probe, the reason names the structure that was wrong, and every I/O failure is described by
/// its [`std::io::ErrorKind`] rather than the operating system's platform-dependent error text.
pub fn probe(path: &Path) -> Result<MediaProbe, String> {
    let file = File::open(path).map_err(|e| format!("cannot open: {:?}", e.kind()))?;
    let len = file
        .metadata()
        .map_err(|e| format!("cannot stat: {:?}", e.kind()))?
        .len();
    let mut r = Ebml {
        file: BufReader::new(file),
        len,
        pos: 0,
    };

    let header = r.header()?;
    if header.id != ID_EBML {
        return Err("not a Matroska container: the file does not open with an EBML header".into());
    }
    let doc_type = doc_type(&r.payload(&header, "EBML header")?);
    match doc_type.as_deref() {
        Some("matroska") | Some("webm") => {}
        Some(other) => {
            return Err(format!(
                "not a Matroska container: its EBML header declares doc type `{}`",
                printable(other)
            ))
        }
        // A `DocType` is mandatory, but its absence says nothing about the video track, and refusing
        // here would report a file whose tracks are perfectly readable as unreadable.
        None => {}
    }

    let segment = loop {
        if r.pos >= r.len {
            return Err("no segment: the file's EBML tree carries no Segment element".into());
        }
        let element = r.header()?;
        if element.id == ID_SEGMENT {
            break element;
        }
        r.skip(&element)?;
    };

    let mut walk = Walk::default();
    let end = segment.end(r.len);
    while r.pos < end {
        let element = r.header()?;
        match element.id {
            ID_TRACKS => {
                let bytes = r.payload(&element, "Tracks")?;
                walk.read_tracks(&bytes);
            }
            ID_CLUSTER => walk.read_cluster(&mut r, &element)?,
            _ => r.skip(&element)?,
        }
    }

    let track = walk.video.ok_or_else(|| {
        "no video track: the container's Tracks element declares no track of type video".to_string()
    })?;
    // Counted per track number and resolved afterwards, so a file whose clusters precede its
    // `Tracks` element — legal, and what a recording finalized in one pass writes — is counted the
    // same as one that states its tracks first.
    let frame_count = walk
        .counted
        .then(|| walk.blocks.get(&track.number).copied().unwrap_or(0));
    // fps is what the file *declares* a frame lasts, and nothing else: deriving it from the segment
    // duration would divide a value the container rounds by a count this walk may have abstained
    // from, and report a rate no encoder was asked for.
    let fps = track
        .default_duration_ns
        .filter(|ns| *ns > 0)
        .map(|ns| 1e9 / ns as f64);
    Ok(MediaProbe {
        params: MediaParams {
            codec: track.codec,
            width: track.width,
            height: track.height,
            fps,
        },
        frame_count,
    })
}

/// What the walk has learned so far.
struct Walk {
    /// The first video track the `Tracks` element declares.
    video: Option<VideoTrack>,
    /// Blocks seen per track number.
    blocks: BTreeMap<u64, u64>,
    /// The block count is trustworthy — no cluster was skipped, no block header was malformed, and
    /// no ceiling was reached. False means this walk reports no count at all.
    counted: bool,
}

impl Default for Walk {
    fn default() -> Self {
        // A file with no clusters at all has counted zero frames, truthfully; the flag only falls to
        // false where something the walk met made the count untrustworthy.
        Self {
            video: None,
            blocks: BTreeMap::new(),
            counted: true,
        }
    }
}

/// The facts a video track states about itself.
struct VideoTrack {
    number: u64,
    codec: Option<String>,
    width: Option<u64>,
    height: Option<u64>,
    default_duration_ns: Option<u64>,
}

impl Walk {
    fn read_tracks(&mut self, bytes: &[u8]) {
        if self.video.is_some() {
            return;
        }
        for entry in children(bytes) {
            if entry.id != ID_TRACK_ENTRY {
                continue;
            }
            let mut number = None;
            let mut kind = None;
            let mut track = VideoTrack {
                number: 0,
                codec: None,
                width: None,
                height: None,
                default_duration_ns: None,
            };
            for field in children(entry.payload) {
                match field.id {
                    ID_TRACK_NUMBER => number = uint(field.payload),
                    ID_TRACK_TYPE => kind = uint(field.payload),
                    ID_CODEC_ID => track.codec = text(field.payload),
                    ID_DEFAULT_DURATION => track.default_duration_ns = uint(field.payload),
                    ID_VIDEO => {
                        for v in children(field.payload) {
                            match v.id {
                                ID_PIXEL_WIDTH => track.width = uint(v.payload).filter(|n| *n > 0),
                                ID_PIXEL_HEIGHT => {
                                    track.height = uint(v.payload).filter(|n| *n > 0)
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            if kind == Some(TRACK_TYPE_VIDEO) {
                if let Some(number) = number {
                    track.number = number;
                    self.video = Some(track);
                    return;
                }
            }
        }
    }

    /// Count the blocks in one cluster, reading each block's header and seeking over its payload.
    fn read_cluster(&mut self, r: &mut Ebml, element: &Header) -> Result<(), String> {
        if element.unknown_size {
            // A cluster of unknown size ends wherever the next top-level element begins, which
            // cannot be found without guessing at ids inside compressed payloads. A live-muxed file
            // legitimately carries these, so this is an abstention, not an error: everything the
            // Tracks element stated stays reported, and no frame count is.
            self.counted = false;
            r.seek_to(r.len)?;
            return Ok(());
        }
        let end = element.end(r.len);
        while r.pos < end {
            let child = r.header()?;
            match child.id {
                ID_SIMPLE_BLOCK => self.count_block(r, &child)?,
                ID_BLOCK_GROUP => {
                    let group_end = child.end(r.len);
                    while r.pos < group_end {
                        let inner = r.header()?;
                        if inner.id == ID_BLOCK {
                            self.count_block(r, &inner)?;
                        } else {
                            r.skip(&inner)?;
                        }
                    }
                }
                _ => r.skip(&child)?,
            }
        }
        Ok(())
    }

    /// Read one block's header — track number, then the flags that say how many frames it laces
    /// together — and seek over its payload.
    fn count_block(&mut self, r: &mut Ebml, element: &Header) -> Result<(), String> {
        // Track vint (up to 8) + relative timestamp (2) + flags (1) + lace count (1).
        let want = element.size.min(12);
        let head = r.read(want, "block header")?;
        r.seek_to(element.end(r.len))?;
        if !self.counted {
            return Ok(());
        }
        let Some((track, used)) = vint(&head, false) else {
            self.counted = false;
            return Ok(());
        };
        // Two bytes of relative timestamp, then the flags byte whose bits 1-2 are the lacing mode.
        let Some(&flags) = head.get(used + 2) else {
            self.counted = false;
            return Ok(());
        };
        let frames = if (flags >> 1) & 0x03 == 0 {
            1
        } else {
            // A laced block states its frame count, minus one, in the byte after the flags.
            match head.get(used + 3) {
                Some(&n) => n as u64 + 1,
                None => {
                    self.counted = false;
                    return Ok(());
                }
            }
        };
        if !self.blocks.contains_key(&track) && self.blocks.len() >= MAX_TRACKS {
            self.counted = false;
            return Ok(());
        }
        let counter = self.blocks.entry(track).or_insert(0);
        *counter += frames;
        if *counter > MAX_BLOCKS {
            self.counted = false;
        }
        Ok(())
    }
}

// ---- the reader --------------------------------------------------------------------------------

/// One element header, as read from the file.
struct Header {
    id: u64,
    /// The payload's length. For an unknown-size element this is what remains of the file, so every
    /// bound derived from it still holds.
    size: u64,
    /// Offset of the payload within the file.
    offset: u64,
    /// The element declared the all-ones size the format reserves for "unknown".
    unknown_size: bool,
}

impl Header {
    /// Where this element's payload ends, clamped to the file.
    fn end(&self, file_len: u64) -> u64 {
        self.offset.saturating_add(self.size).min(file_len)
    }
}

/// A bounded, seeking reader over the EBML element tree.
struct Ebml {
    file: BufReader<File>,
    len: u64,
    pos: u64,
}

impl Ebml {
    /// Read the element header at the current position.
    fn header(&mut self) -> Result<Header, String> {
        let offset = self.pos;
        let head = self.read((self.len - self.pos).min(16), "element header")?;
        let (id, id_len) =
            vint(&head, true).ok_or_else(|| format!("malformed element id at offset {offset}"))?;
        let (size, size_len) = vint(&head[id_len..], false)
            .ok_or_else(|| format!("malformed element size at offset {offset}"))?;
        let unknown_size = all_ones(&head[id_len..id_len + size_len]);
        let payload_offset = offset + (id_len + size_len) as u64;
        if payload_offset > self.len {
            return Err(format!("truncated element header at offset {offset}"));
        }
        let remaining = self.len - payload_offset;
        if !unknown_size && size > remaining {
            return Err(format!(
                "element {id:#x} at offset {offset} declares {size} bytes but only {remaining} remain"
            ));
        }
        self.seek_to(payload_offset)?;
        Ok(Header {
            id,
            size: if unknown_size { remaining } else { size },
            offset: payload_offset,
            unknown_size,
        })
    }

    /// Read an element's payload into memory, under [`MAX_ELEMENT_BYTES`].
    fn payload(&mut self, element: &Header, name: &str) -> Result<Vec<u8>, String> {
        if element.size > MAX_ELEMENT_BYTES {
            return Err(format!(
                "{name} declares {} bytes, over the {MAX_ELEMENT_BYTES}-byte ceiling",
                element.size
            ));
        }
        self.seek_to(element.offset)?;
        let bytes = self.read(element.size, name)?;
        self.seek_to(element.end(self.len))?;
        Ok(bytes)
    }

    /// Read exactly `n` bytes from the current position. `n` is always derived from what remains in
    /// the file, so this never allocates against a number the file chose alone.
    fn read(&mut self, n: u64, what: &str) -> Result<Vec<u8>, String> {
        if n > self.len - self.pos {
            return Err(format!(
                "truncated {what} at offset {}: {} bytes left, {n} needed",
                self.pos,
                self.len - self.pos
            ));
        }
        let mut buf = vec![0u8; n as usize];
        self.file
            .read_exact(&mut buf)
            .map_err(|e| format!("cannot read {what} at offset {}: {:?}", self.pos, e.kind()))?;
        self.pos += n;
        Ok(buf)
    }

    /// Seek over an element's payload.
    fn skip(&mut self, element: &Header) -> Result<(), String> {
        self.seek_to(element.end(self.len))
    }

    fn seek_to(&mut self, offset: u64) -> Result<(), String> {
        let offset = offset.min(self.len);
        if offset == self.pos {
            return Ok(());
        }
        // Counting a cluster's blocks means two short hops per block — the header, then over the
        // payload — and the payloads of a small recording are often already in the buffer. Seeking
        // *relatively* keeps that buffer where the hop lands inside it, which is the difference
        // between one read syscall per cluster and three per frame.
        let delta = offset as i64 - self.pos as i64;
        self.file
            .seek_relative(delta)
            .map_err(|e| format!("cannot seek to offset {offset}: {:?}", e.kind()))?;
        self.pos = offset;
        Ok(())
    }
}

// ---- primitives --------------------------------------------------------------------------------

/// One child element within an in-memory payload.
struct Child<'a> {
    id: u64,
    payload: &'a [u8],
}

/// Split a payload into the child elements it holds. A malformed child ends the walk rather than
/// failing the probe, exactly as the MP4 box walk does: what has already been read is still true.
fn children(buf: &[u8]) -> Vec<Child<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let Some((id, id_len)) = vint(&buf[pos..], true) else {
            break;
        };
        let Some((size, size_len)) = vint(&buf[pos + id_len..], false) else {
            break;
        };
        let start = pos + id_len + size_len;
        let Some(end) = start.checked_add(size as usize).filter(|e| *e <= buf.len()) else {
            break;
        };
        out.push(Child {
            id,
            payload: &buf[start..end],
        });
        pos = end;
    }
    out
}

/// Read a variable-length integer: `(value, bytes consumed)`.
///
/// With `keep_marker` the length marker stays in the value, which is how an element **id** is
/// written and compared; without it the marker is masked off, which is how a **size** and a block's
/// track number are read.
fn vint(buf: &[u8], keep_marker: bool) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    if first == 0 {
        // Eight leading zero bits would mean a length of nine or more, which the format does not
        // encode; believing it would read past whatever follows.
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = buf.get(..len)?;
    let mut value = if keep_marker {
        first as u64
    } else {
        // An eight-byte vint spends its whole first byte on the marker: it has no data bits there.
        (first & 0xFFu8.checked_shr(len as u32).unwrap_or(0)) as u64
    };
    for &b in &bytes[1..] {
        value = (value << 8) | b as u64;
    }
    Some((value, len))
}

/// True when every data bit of an encoded vint is set — the size the format reserves for "unknown".
fn all_ones(bytes: &[u8]) -> bool {
    let Some((&first, rest)) = bytes.split_first() else {
        return false;
    };
    // An eight-byte vint spends its whole first byte on the marker, so it has no data bits there.
    let data_bits = 0xFFu8.checked_shr(bytes.len() as u32).unwrap_or(0);
    first & data_bits == data_bits && rest.iter().all(|&b| b == 0xFF)
}

/// An EBML unsigned integer: big-endian, of any length up to eight bytes.
fn uint(buf: &[u8]) -> Option<u64> {
    if buf.is_empty() || buf.len() > 8 {
        return None;
    }
    Some(buf.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64))
}

/// An EBML string, with the trailing NUL padding a writer may add removed.
fn text(buf: &[u8]) -> Option<String> {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let s = std::str::from_utf8(&buf[..end]).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// A string from the file with every non-printable byte escaped, so a corrupt container's text never
/// lands raw in a report.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c.to_string()
            } else {
                format!("\\x{:02x}", c as u32 & 0xff)
            }
        })
        .collect()
}

/// The EBML header's `DocType`.
fn doc_type(header: &[u8]) -> Option<String> {
    children(header)
        .into_iter()
        .find(|c| c.id == ID_DOC_TYPE)
        .and_then(|c| text(c.payload))
}
