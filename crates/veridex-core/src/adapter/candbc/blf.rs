//! Reading a Vector **BLF** CAN log — the binary format CANoe, CANalyzer and every Vector interface
//! write, and the one most vehicle traffic is actually recorded in.
//!
//! A BLF is a file header followed by a stream of length-prefixed objects. The objects that matter
//! here are `CAN_MESSAGE` / `CAN_MESSAGE2` and their CAN-FD counterparts `CAN_FD_MESSAGE` /
//! `CAN_FD_MESSAGE_64` — one bus frame each — and `LOG_CONTAINER`, which holds a zlib-compressed run
//! of other objects. An object may straddle two containers, so the container
//! payloads are reassembled into one stream and parsed from there rather than each on its own.
//!
//! What comes out is the same [`CanFrame`] the candump reader produces, so both kinds of log merge
//! into one recording and every existing signal decode, statistic, range check and provenance
//! extraction applies to a BLF unchanged. This module decides nothing about meaning; it reads bytes.
//!
//! The file is untrusted, so every length it declares is validated against the bytes that actually
//! remain, the walk is iterative rather than recursive, each container is charged to the run's
//! decompression budget *before* it is unpacked, and the reassembly buffer is bounded — a container
//! declaring more than the budget allows is disclosed as unread rather than allocated for.
//!
//! Field offsets follow the format as implemented by the reference reader (`python-can`'s
//! `can/io/blf.py`), against which the fixtures in the tests are shaped: a fixture written by this
//! module's own conventions would agree with itself whatever those conventions were.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::CanFrame;
use crate::adapter::{DecompressionBudget, IngestOptions, UnmappedField};

/// The adapter this reader belongs to, for the budget errors it can raise.
const FORMAT_ID: &str = "candbc";

/// File header signature.
const FILE_MAGIC: &[u8; 4] = b"LOGG";
/// Object header signature.
const OBJ_MAGIC: &[u8; 4] = b"LOBJ";

/// Bytes of the object header every object begins with: signature, header size, header version,
/// object size, object type.
const OBJ_HEADER_BASE: usize = 16;

/// Object types this reader knows by name. Everything else is counted and disclosed.
const CAN_MESSAGE: u32 = 1;
const LOG_CONTAINER: u32 = 10;
const CAN_MESSAGE2: u32 = 86;
const CAN_FD_MESSAGE: u32 = 100;
const CAN_FD_MESSAGE_64: u32 = 101;

/// Container compression methods.
const NO_COMPRESSION: u16 = 0;
const ZLIB_DEFLATE: u16 = 2;

/// The `LogContainer` payload's own header: compression method, six reserved bytes, the uncompressed
/// size, four more reserved bytes.
const CONTAINER_HEADER: usize = 16;

/// Object-header flag selecting the timestamp unit: set means ten-microsecond ticks, clear means
/// nanoseconds.
const TIME_TEN_MICS: u32 = 1;

/// Bytes of the `CanFdMessage` payload before its inline data: channel, flags, DLC, id, frame
/// length, bit count, FD flags, valid data bytes, five reserved.
const CAN_FD_PREFIX: usize = 20;

/// Bytes of the `CanFdMessage64` payload before its data, which follows rather than sitting inline.
const CAN_FD_64_PREFIX: usize = 40;

/// The largest payload a CAN-FD frame carries.
const MAX_FD_BYTES: usize = 64;

/// The remote-transmission bit of a CAN message's own flags byte. A remote frame *requests* data and
/// carries none, so its eight payload bytes are not a payload.
const REMOTE_FLAG: u8 = 0x80;

/// The extended-identifier bit of the arbitration id.
const CAN_ID_EXTENDED: u32 = 0x8000_0000;

/// The largest non-container object read into memory. A CAN object is 48 bytes; a file declaring
/// megabytes for one is not describing a frame this reader decodes, so it is skipped and disclosed
/// rather than allocated for.
const MAX_OBJECT_BYTES: u64 = 1024 * 1024;

/// Ceiling on the reassembly buffer holding container payloads. One container is a few hundred
/// kilobytes; this is far above any of them and far below a memory hazard, and reaching it means the
/// stream no longer parses as objects at all.
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

/// What one BLF yielded.
pub(super) struct BlfLog {
    /// The bus frames, in file order.
    pub frames: Vec<CanFrame>,
    /// What was in the file and is in no frame, ready to be disclosed as unread coverage.
    pub unread: Vec<UnmappedField>,
}

/// Whether `path` opens with the BLF file signature.
///
/// Read from the bytes rather than the extension, so a log named something else is still recognized
/// and a `.blf` that is not one is not claimed.
pub(super) fn is_blf(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && &magic == FILE_MAGIC
}

/// Read every CAN frame `path` holds.
///
/// `Err` names the structure that was wrong, in the same style as the other readers here: a reason a
/// person can act on rather than "parse error".
pub(super) fn read(path: &Path, options: &IngestOptions) -> Result<BlfLog, String> {
    let file = File::open(path).map_err(|e| format!("cannot open: {:?}", e.kind()))?;
    let len = file
        .metadata()
        .map_err(|e| format!("cannot stat: {:?}", e.kind()))?
        .len();
    let mut r = Reader {
        file: BufReader::new(file),
        len,
        pos: 0,
    };

    let header = r.read_bytes(r.len.min(72), "file header")?;
    if !header.starts_with(FILE_MAGIC) {
        return Err("not a BLF: the file does not open with the `LOGG` signature".into());
    }
    let header_size = u32::from_le_bytes(
        header
            .get(4..8)
            .and_then(|b| b.try_into().ok())
            .ok_or("not a BLF: the file is too short to hold its own header")?,
    ) as u64;
    if header_size < 8 || header_size > len {
        return Err(format!(
            "the file header declares {header_size} bytes, which the {len}-byte file cannot hold"
        ));
    }
    // The measurement's start time, so a BLF's frames land on the same wall clock a candump's do and
    // the two can be read as one recording. A header that states no valid date leaves the frames on
    // the measurement-relative clock the objects themselves carry: internally consistent, which is
    // what every temporal check measures, and better than a fabricated epoch.
    let start_ns = header.get(40..56).and_then(systemtime_ns).unwrap_or(0);

    let mut walk = Walk {
        frames: Vec::new(),
        start_ns,
        budget: DecompressionBudget::new(options, len),
        undecoded: std::collections::BTreeMap::new(),
        remote_frames: 0,
        short_payloads: 0,
        unread: Vec::new(),
    };
    r.seek_to(header_size)?;

    let mut pending: Vec<u8> = Vec::new();
    while r.pos < r.len {
        let Some(head) = r.object_header()? else {
            break;
        };
        let body_len = head.size - OBJ_HEADER_BASE as u64;
        if head.kind == LOG_CONTAINER {
            match walk.unpack(&mut r, body_len) {
                Ok(Some(bytes)) => {
                    if pending.len().saturating_add(bytes.len()) > MAX_PENDING_BYTES {
                        walk.unread.push(UnmappedField {
                            source_path: name(path),
                            note: "the log's object stream stopped parsing as objects and its \
                                   reassembly buffer reached this run's ceiling; the rest of the \
                                   file contributed no frames"
                                .into(),
                        });
                        break;
                    }
                    pending.extend_from_slice(&bytes);
                    walk.drain(&mut pending);
                }
                Ok(None) => {}
                Err(e) => return Err(e),
            }
        } else if body_len > MAX_OBJECT_BYTES {
            walk.count_undecoded(head.kind);
        } else {
            let body = r.read_bytes(body_len, "object body")?;
            walk.object(head.kind, head.header_size, head.version, &body);
        }
        let next = r.pos.max(head.offset + head.size);
        r.seek_to(align4(next))?;
        // A file whose objects are not four-byte aligned is read the other way rather than
        // abandoned: both conventions are in the wild, and the signature says which one this file
        // used. Neither is a guess — the next object either begins here or it does not.
        if r.pos < r.len && !r.peek_is_object()? {
            r.seek_to(next)?;
            if r.pos < r.len && !r.peek_is_object()? {
                walk.unread.push(UnmappedField {
                    source_path: name(path),
                    note: format!(
                        "the object at offset {} is not followed by another object; the remaining \
                         {} byte(s) of the file were not read",
                        head.offset,
                        r.len - r.pos
                    ),
                });
                break;
            }
        }
    }
    // Bytes left in the reassembly buffer are a container's worth of object that never completed.
    if !pending.is_empty() {
        walk.unread.push(UnmappedField {
            source_path: name(path),
            note: format!(
                "{} byte(s) of container payload end mid-object; that object contributed no frames",
                pending.len()
            ),
        });
    }
    Ok(walk.finish(path))
}

/// The file name alone, for a disclosure that names the log rather than the caller's directory
/// layout.
fn name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("blf log")
        .to_string()
}

/// The next four-byte boundary at or after `n`.
fn align4(n: u64) -> u64 {
    n.saturating_add(3) & !3
}

/// What the walk has read so far.
struct Walk {
    frames: Vec<CanFrame>,
    /// Nanoseconds from the epoch to the measurement's start, added to every object's timestamp.
    start_ns: i64,
    budget: DecompressionBudget,
    /// Object types this reader does not decode, and how many of each the file held.
    undecoded: std::collections::BTreeMap<u32, u64>,
    /// Remote-transmission frames: a request for data, carrying none.
    remote_frames: u64,
    /// CAN objects whose declared length exceeds what a classic CAN frame can hold.
    short_payloads: u64,
    unread: Vec<UnmappedField>,
}

impl Walk {
    /// Read one `LOG_CONTAINER`'s payload, decompressing it when it is compressed.
    ///
    /// `Ok(None)` means the container was disclosed rather than read — its declared expansion is
    /// past this run's budget, its compression is one this reader does not know, or its stream is
    /// corrupt. Only a genuine I/O failure is an `Err`.
    fn unpack(&mut self, r: &mut Reader, body_len: u64) -> Result<Option<Vec<u8>>, String> {
        if body_len < CONTAINER_HEADER as u64 {
            self.unread.push(UnmappedField {
                source_path: "log container".into(),
                note: "a container too short to hold its own header contributed no frames".into(),
            });
            return Ok(None);
        }
        let container = r.read_bytes(CONTAINER_HEADER as u64, "container header")?;
        let method = u16::from_le_bytes([container[0], container[1]]);
        let declared =
            u32::from_le_bytes([container[8], container[9], container[10], container[11]]) as u64;
        let payload_len = body_len - CONTAINER_HEADER as u64;
        if method == NO_COMPRESSION {
            // Not charged to the decompression budget: nothing expands, and the bytes are already
            // bounded by the file's own length.
            return Ok(Some(r.read_bytes(payload_len, "container payload")?));
        }
        if method != ZLIB_DEFLATE {
            self.unread.push(UnmappedField {
                source_path: "log container".into(),
                note: format!(
                    "a container compressed by method {method}, which this reader does not \
                     decompress; its objects contributed no frames"
                ),
            });
            return Ok(None);
        }
        if self.budget.take(FORMAT_ID, declared).is_err() {
            self.unread.push(UnmappedField {
                source_path: "log container".into(),
                note: format!(
                    "a container declaring {declared} uncompressed byte(s) is past this run's \
                     decompression budget; its objects were not read"
                ),
            });
            return Ok(None);
        }
        let compressed = r.read_bytes(payload_len, "container payload")?;
        // Capped one byte past what the container declared, so a stream that expands further is
        // caught as corrupt instead of being unpacked in full first.
        let cap = declared.saturating_add(1).min(
            self.budget
                .remaining()
                .map_or(u64::MAX, |left| left.saturating_add(declared + 1)),
        );
        let mut out = Vec::new();
        let mut decoder = flate2::read::ZlibDecoder::new(std::io::Read::take(&compressed[..], cap));
        match decoder.read_to_end(&mut out) {
            Ok(n) if n as u64 <= declared => Ok(Some(out)),
            Ok(_) => {
                self.unread.push(UnmappedField {
                    source_path: "log container".into(),
                    note: format!(
                        "a container declares {declared} uncompressed byte(s) but its stream \
                         produces more; the container is corrupt and its objects were not read"
                    ),
                });
                Ok(None)
            }
            Err(e) => {
                self.unread.push(UnmappedField {
                    source_path: "log container".into(),
                    note: format!(
                        "a container declaring {declared} byte(s) did not decompress ({:?}); its \
                         objects contributed no frames",
                        e.kind()
                    ),
                });
                Ok(None)
            }
        }
    }

    /// Parse as many whole objects as `pending` holds, leaving any partial one for the next
    /// container to complete.
    fn drain(&mut self, pending: &mut Vec<u8>) {
        let mut pos = 0usize;
        while pos + OBJ_HEADER_BASE <= pending.len() {
            let head = &pending[pos..pos + OBJ_HEADER_BASE];
            if &head[0..4] != OBJ_MAGIC {
                // Not an object boundary: the remainder is not a stream this reader can follow, and
                // guessing at where the next object starts would decode frames out of whatever the
                // bytes happen to be. Drop what is left and let the caller disclose it.
                break;
            }
            let header_size = u16::from_le_bytes([head[4], head[5]]) as usize;
            let version = u16::from_le_bytes([head[6], head[7]]);
            let size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as usize;
            let kind = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
            if size < OBJ_HEADER_BASE.max(header_size) {
                break;
            }
            if pos + size > pending.len() {
                break; // the object continues in the next container
            }
            self.object(
                kind,
                header_size,
                version,
                &pending[pos + OBJ_HEADER_BASE..pos + size],
            );
            let next = pos + size;
            let aligned = (next + 3) & !3;
            // Same two-way boundary the top-level walk uses, in memory.
            pos = if pending.len() >= aligned + 4 && &pending[aligned..aligned + 4] == OBJ_MAGIC {
                aligned
            } else if pending.len() >= next + 4 && &pending[next..next + 4] == OBJ_MAGIC {
                next
            } else {
                aligned.min(pending.len())
            };
        }
        pending.drain(..pos.min(pending.len()));
    }

    /// Read one object's body — everything after the sixteen-byte base header.
    fn object(&mut self, kind: u32, header_size: usize, version: u16, body: &[u8]) {
        match kind {
            CAN_MESSAGE | CAN_MESSAGE2 | CAN_FD_MESSAGE | CAN_FD_MESSAGE_64 => {}
            other => {
                self.count_undecoded(other);
                return;
            }
        }
        // Both header versions put the flags at the same offset and the timestamp eight bytes after
        // it, so one read serves both. A header this reader cannot locate the timestamp in yields no
        // frame rather than a frame at time zero.
        let Some(ts_ns) = self.timestamp(header_size, version, body) else {
            self.count_undecoded(kind);
            return;
        };
        // The type-specific payload begins where the object header ends.
        let Some(msg) = body.get(header_size.saturating_sub(OBJ_HEADER_BASE)..) else {
            self.short_payloads += 1;
            return;
        };
        let Some((channel, id, data)) = (match kind {
            CAN_FD_MESSAGE_64 => self.fd64_frame(msg),
            CAN_FD_MESSAGE => self.fd_frame(msg),
            _ => self.classic_frame(msg),
        }) else {
            return;
        };
        self.frames.push(CanFrame {
            ts_ns,
            // The channel is the bus, one-based as the file writes it. Named verbatim rather than
            // translated to a `canN` interface: which Linux interface a Vector channel corresponds
            // to is not something the file says, and inventing the correspondence would merge two
            // buses that a mixed directory keeps apart.
            iface: format!("channel{channel}"),
            id: id & !CAN_ID_EXTENDED & 0x1FFF_FFFF,
            data,
        });
    }

    /// A classic `CanMessage` / `CanMessage2` payload: channel, flags, DLC, id, eight data bytes.
    fn classic_frame(&mut self, msg: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
        if msg.len() < 16 {
            self.short_payloads += 1;
            return None;
        }
        let flags = msg[2];
        let dlc = msg[3] as usize;
        if flags & REMOTE_FLAG != 0 {
            // A remote frame requests data and carries none. Its eight payload bytes are not a
            // payload, and decoding signals out of them would put fabricated samples — usually a
            // run of zeros — into the streams the checks then grade.
            self.remote_frames += 1;
            return None;
        }
        if dlc > 8 {
            // A classic CAN frame holds at most eight bytes. A larger declaration is a frame this
            // object type cannot represent, so nothing here is trustworthy as a payload.
            self.short_payloads += 1;
            return None;
        }
        Some((
            u16::from_le_bytes([msg[0], msg[1]]),
            u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]),
            msg[8..8 + dlc].to_vec(),
        ))
    }

    /// A `CanFdMessage` payload: the classic prefix, then a frame length, the FD flags, the count of
    /// payload bytes that are real, and sixty-four bytes of data inline.
    ///
    /// The count is what says how much of those sixty-four is payload — the DLC of an FD frame is a
    /// *code* (9 through 15 mean 12, 16, 20, 24, 32, 48 and 64 bytes), so reading the DLC as a
    /// length would take twelve bytes for a sixty-four-byte frame.
    fn fd_frame(&mut self, msg: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
        if msg.len() < CAN_FD_PREFIX {
            self.short_payloads += 1;
            return None;
        }
        // The count sits after the frame length, the bit count and the FD flags — five reserved
        // bytes ahead of the data, not adjacent to it.
        let valid = msg[14] as usize;
        let available = msg.len() - CAN_FD_PREFIX;
        let take = valid.min(MAX_FD_BYTES).min(available);
        if take < valid.min(MAX_FD_BYTES) {
            // Fewer bytes than the object says it carries. What is there is used — a signal that
            // fits still decodes, and one that does not is skipped by the decoder — but the shortfall
            // is disclosed rather than padded with zeros a check would read as measurements.
            self.short_payloads += 1;
        }
        Some((
            u16::from_le_bytes([msg[0], msg[1]]),
            u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]),
            msg[CAN_FD_PREFIX..CAN_FD_PREFIX + take].to_vec(),
        ))
    }

    /// A `CanFdMessage64` payload: a wider prefix whose data *follows* it rather than sitting inline,
    /// and whose channel is a single byte.
    fn fd64_frame(&mut self, msg: &[u8]) -> Option<(u16, u32, Vec<u8>)> {
        if msg.len() < CAN_FD_64_PREFIX {
            self.short_payloads += 1;
            return None;
        }
        let valid = msg[2] as usize;
        let available = msg.len() - CAN_FD_64_PREFIX;
        let take = valid.min(MAX_FD_BYTES).min(available);
        if take < valid.min(MAX_FD_BYTES) {
            self.short_payloads += 1;
        }
        Some((
            msg[0] as u16,
            u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]),
            msg[CAN_FD_64_PREFIX..CAN_FD_64_PREFIX + take].to_vec(),
        ))
    }

    /// The object's timestamp, in nanoseconds on the same clock the candump reader produces.
    fn timestamp(&self, header_size: usize, _version: u16, body: &[u8]) -> Option<i64> {
        if header_size < 32 {
            return None;
        }
        // `body` starts after the base header, so the object header's own fields are at the front of
        // it: flags at 0, timestamp at 8.
        let flags = u32::from_le_bytes(body.get(0..4)?.try_into().ok()?);
        let ticks = u64::from_le_bytes(body.get(8..16)?.try_into().ok()?);
        let ns = if flags & TIME_TEN_MICS != 0 {
            (ticks as i64).saturating_mul(10_000)
        } else {
            ticks as i64
        };
        Some(self.start_ns.saturating_add(ns))
    }

    fn count_undecoded(&mut self, kind: u32) {
        *self.undecoded.entry(kind).or_insert(0) += 1;
    }

    /// Turn the counters into the disclosures the report carries.
    fn finish(mut self, path: &Path) -> BlfLog {
        let log = name(path);
        for (kind, count) in std::mem::take(&mut self.undecoded) {
            let what = format!("object(s) of type {kind}");
            self.unread.push(UnmappedField {
                source_path: log.clone(),
                note: format!(
                    "{count} {what}, which this reader does not decode; they contributed no frames"
                ),
            });
        }
        if self.remote_frames > 0 {
            self.unread.push(UnmappedField {
                source_path: log.clone(),
                note: format!(
                    "{} remote-transmission frame(s), which request data and carry none; no signal \
                     was decoded from them",
                    self.remote_frames
                ),
            });
        }
        if self.short_payloads > 0 {
            self.unread.push(UnmappedField {
                source_path: log,
                note: format!(
                    "{} CAN object(s) whose declared length a classic CAN frame cannot hold; they \
                     contributed no frames",
                    self.short_payloads
                ),
            });
        }
        BlfLog {
            frames: self.frames,
            unread: self.unread,
        }
    }
}

/// One object header, as read from the file.
struct ObjectHeader {
    /// Offset of the header itself.
    offset: u64,
    /// Bytes of header, base included.
    header_size: usize,
    /// Header layout version.
    version: u16,
    /// Bytes of the whole object, header included.
    size: u64,
    /// The object type.
    kind: u32,
}

/// A bounded, seeking reader over the object stream.
struct Reader {
    file: BufReader<File>,
    len: u64,
    pos: u64,
}

impl Reader {
    /// Read the object header at the current position, or `None` when what remains is not one.
    fn object_header(&mut self) -> Result<Option<ObjectHeader>, String> {
        let offset = self.pos;
        if self.len - self.pos < OBJ_HEADER_BASE as u64 {
            return Ok(None);
        }
        let head = self.read_bytes(OBJ_HEADER_BASE as u64, "object header")?;
        if &head[0..4] != OBJ_MAGIC {
            return Ok(None);
        }
        let header_size = u16::from_le_bytes([head[4], head[5]]) as usize;
        let version = u16::from_le_bytes([head[6], head[7]]);
        let size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]) as u64;
        let kind = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
        if size < OBJ_HEADER_BASE as u64 || size < header_size as u64 {
            return Err(format!(
                "the object at offset {offset} declares {size} bytes, less than its own header"
            ));
        }
        if size > self.len - offset {
            return Err(format!(
                "the object at offset {offset} declares {size} bytes but only {} remain",
                self.len - offset
            ));
        }
        Ok(Some(ObjectHeader {
            offset,
            header_size,
            version,
            size,
            kind,
        }))
    }

    /// Whether an object signature sits at the current position.
    fn peek_is_object(&mut self) -> Result<bool, String> {
        if self.len - self.pos < 4 {
            return Ok(false);
        }
        let here = self.pos;
        let magic = self.read_bytes(4, "object signature")?;
        self.seek_to(here)?;
        Ok(&magic[..] == OBJ_MAGIC)
    }

    /// Read exactly `n` bytes. `n` is always derived from what remains in the file, so this never
    /// allocates against a number the file chose alone.
    fn read_bytes(&mut self, n: u64, what: &str) -> Result<Vec<u8>, String> {
        if n > self.len - self.pos {
            return Err(format!(
                "truncated {what} at offset {}: {} byte(s) left, {n} needed",
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

    fn seek_to(&mut self, offset: u64) -> Result<(), String> {
        let offset = offset.min(self.len);
        if offset == self.pos {
            return Ok(());
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| format!("cannot seek to offset {offset}: {:?}", e.kind()))?;
        self.pos = offset;
        Ok(())
    }
}

/// A Windows `SYSTEMTIME` — year, month, day-of-week, day, hour, minute, second, millisecond, each a
/// little-endian `u16` — as nanoseconds from the Unix epoch, in UTC.
///
/// `None` for anything that is not a real date: a header full of zeros is what a writer that did not
/// record a start time leaves, and reading it as the year 0 would put the whole recording an epoch
/// away from any other log it is read beside.
fn systemtime_ns(bytes: &[u8]) -> Option<i64> {
    let f = |i: usize| -> Option<i64> {
        Some(u16::from_le_bytes(bytes.get(i * 2..i * 2 + 2)?.try_into().ok()?) as i64)
    };
    let (year, month, day) = (f(0)?, f(1)?, f(3)?);
    let (hour, minute, second, milli) = (f(4)?, f(5)?, f(6)?, f(7)?);
    if !(1601..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
        || milli > 999
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(secs.saturating_mul(1_000_000_000) + milli * 1_000_000)
}

/// Days from the Unix epoch to `y-m-d` in the proleptic Gregorian calendar.
///
/// The civil-from-days algorithm, written out rather than pulled from a date crate: it is a dozen
/// lines of integer arithmetic, it is exact for every year this reader accepts, and a timestamp that
/// reaches the CDM content hash must not depend on a dependency's rounding.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
