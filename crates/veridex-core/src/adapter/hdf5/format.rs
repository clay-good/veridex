//! A read-only HDF5 reader: exactly the slice of the format the CDM needs, and nothing else.
//!
//! There is no pure-Rust HDF5 library in the dependency set (the `hdf5` crate binds the C library,
//! which would make Veridex's build depend on a system package), so this module walks the on-disk
//! format directly — the same choice the MF4 and TFRecord adapters make.
//!
//! What it reads, and why that is enough for a robot dataset written by `h5py`:
//!
//! - **Superblock** v0/v1 (symbol-table root) and v2/v3 (object-header root), to find the root group
//!   and the file's offset/length field widths.
//! - **Object headers** v1 (`h5py`'s default) and v2 (`OHDR`, written with `libver='latest'`),
//!   including continuation chunks.
//! - **Groups** stored the old way (a v1 B-tree over symbol-table nodes plus a local heap) and the
//!   new way (compact link messages). *Dense* link storage — links moved into a fractal heap — is
//!   refused by name rather than read as an empty group, which would turn a large dataset into a
//!   clean verdict over nothing.
//! - **Datasets**: dataspace (dimensions), datatype (fixed-point, floating-point, fixed strings, and
//!   opaque/compound sizes), and layout classes compact, contiguous, and chunked (v1 B-tree index),
//!   with the `deflate`, `shuffle`, and `fletcher32` filters.
//!
//! Everything outside that slice is an explicit error naming what was found. A reader that guesses
//! past a structure it does not understand hands back a dataset that is missing whatever lived
//! there, and Veridex's whole job is to not do that.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::adapter::{DecompressionBudget, IngestError};

use super::FORMAT_ID;

pub(crate) type H5Result<T> = Result<T, IngestError>;

/// The 8-byte signature every HDF5 file starts with.
pub(crate) const SIGNATURE: [u8; 8] = [0x89, b'H', b'D', b'F', b'\r', b'\n', 0x1a, b'\n'];

/// Superblock addresses the format allows: byte 0, then 512, 1024, … (a file may carry a user block
/// ahead of its superblock).
const SUPERBLOCK_ADDRESSES: &[u64] = &[0, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

/// The ceiling on messages one object header may hold, and on nodes one B-tree walk may visit.
///
/// Both structures are address-linked graphs read from the file, so a crafted (or truncated-and-
/// patched) header can point back at itself. Visited-address tracking catches an exact cycle; these
/// bound the rest.
const MAX_HEADER_MESSAGES: usize = 65_536;
const MAX_BTREE_NODES: usize = 100_000;

/// The ceiling on chunks a single dataset's index may enumerate. The index is walked in full before
/// any row is read, so this bounds that walk against a file declaring an enormous one.
const MAX_CHUNKS_PER_DATASET: usize = 4_000_000;

pub(crate) fn parse_error(message: impl Into<String>) -> IngestError {
    IngestError::Parse {
        format_id: FORMAT_ID,
        message: message.into(),
    }
}

/// An address the format spells "undefined" (all bits set, at the file's offset width).
fn is_undefined(addr: u64, offset_size: usize) -> bool {
    if offset_size >= 8 {
        addr == u64::MAX
    } else {
        addr == (1u64 << (offset_size * 8)) - 1
    }
}

/// A little-endian integer of 1–8 bytes.
fn le_uint(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}

/// A cursor over a message body, which refuses to read past the end rather than panicking on a
/// truncated or hostile message.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], what: &'static str) -> Self {
        Cursor { data, pos: 0, what }
    }

    fn take(&mut self, n: usize) -> H5Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| parse_error(format!("{}: read length {n} overflows", self.what)))?;
        if end > self.data.len() {
            return Err(parse_error(format!(
                "{}: truncated — wanted {n} bytes at offset {}, only {} remain",
                self.what,
                self.pos,
                self.data.len().saturating_sub(self.pos)
            )));
        }
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn u8(&mut self) -> H5Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> H5Result<u16> {
        Ok(le_uint(self.take(2)?) as u16)
    }

    fn u32(&mut self) -> H5Result<u32> {
        Ok(le_uint(self.take(4)?) as u32)
    }

    fn u64(&mut self) -> H5Result<u64> {
        Ok(le_uint(self.take(8)?))
    }

    /// A variable-width field (the file's offset or length size).
    fn sized(&mut self, size: usize) -> H5Result<u64> {
        Ok(le_uint(self.take(size)?))
    }

    fn skip(&mut self, n: usize) -> H5Result<()> {
        self.take(n).map(|_| ())
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

/// One message of an object header.
#[derive(Debug, Clone)]
pub(crate) struct Message {
    pub kind: u16,
    pub data: Vec<u8>,
}

// Message type codes this reader acts on.
const MSG_DATASPACE: u16 = 0x0001;
const MSG_LINK_INFO: u16 = 0x0002;
const MSG_DATATYPE: u16 = 0x0003;
const MSG_LINK: u16 = 0x0006;
const MSG_LAYOUT: u16 = 0x0008;
const MSG_FILTER: u16 = 0x000B;
const MSG_ATTRIBUTE: u16 = 0x000C;
const MSG_ATTRIBUTE_INFO: u16 = 0x0015;
const MSG_CONTINUATION: u16 = 0x0010;
const MSG_SYMBOL_TABLE: u16 = 0x0011;

/// What a datatype is, to the extent the CDM cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatatypeClass {
    /// An integer. `signed` distinguishes `int32` from `uint32`.
    FixedPoint { signed: bool },
    /// An IEEE float.
    Float,
    /// A fixed-length string.
    FixedString,
    /// A variable-length type, whose stored bytes are a heap reference rather than the value.
    VariableLength,
    /// Anything else, recorded by size only (compound, opaque, array, enum, reference).
    Other(u8),
}

/// A dataset's or attribute's element type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Datatype {
    pub class: DatatypeClass,
    /// Bytes per element.
    pub size: u32,
    /// Whether elements are stored little-endian (false = big-endian).
    pub little_endian: bool,
}

impl Datatype {
    /// The canonical name this type carries into the CDM (`float32`, `uint8`, `string[16]`, …).
    ///
    /// Named for what the file says the bytes are, never for what a name suggests they mean.
    pub fn name(&self) -> String {
        match self.class {
            DatatypeClass::FixedPoint { signed } => {
                let prefix = if signed { "int" } else { "uint" };
                format!("{prefix}{}", self.size.saturating_mul(8))
            }
            DatatypeClass::Float => format!("float{}", self.size.saturating_mul(8)),
            DatatypeClass::FixedString => format!("string[{}]", self.size),
            DatatypeClass::VariableLength => "vlen".to_string(),
            DatatypeClass::Other(class) => format!("opaque[class {class}, {} bytes]", self.size),
        }
    }

    /// Whether this type's bytes are a number this reader can decode.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self.class,
            DatatypeClass::FixedPoint { .. } | DatatypeClass::Float
        )
    }

    /// Decode one element as an `f64`, or `None` for a class or width this reader does not decode.
    pub fn decode(&self, bytes: &[u8]) -> Option<f64> {
        let size = self.size as usize;
        if size > 8 || bytes.len() < size {
            return None;
        }
        // Normalize to little-endian so both byte orders decode through one path.
        let mut buf = [0u8; 8];
        if self.little_endian {
            buf[..size].copy_from_slice(&bytes[..size]);
        } else {
            for (i, b) in bytes[..size].iter().rev().enumerate() {
                buf[i] = *b;
            }
        }
        match (&self.class, size) {
            (DatatypeClass::Float, 4) => Some(f32::from_le_bytes(buf[..4].try_into().ok()?) as f64),
            (DatatypeClass::Float, 8) => Some(f64::from_le_bytes(buf)),
            (DatatypeClass::FixedPoint { signed: true }, n) => {
                // Sign-extend the stored width into an i64.
                let raw = u64::from_le_bytes(buf);
                let shift = 64 - n * 8;
                Some(((raw << shift) as i64 >> shift) as f64)
            }
            (DatatypeClass::FixedPoint { signed: false }, _) => {
                Some(u64::from_le_bytes(buf) as f64)
            }
            _ => None,
        }
    }
}

/// Where a dataset's raw bytes live.
#[derive(Debug, Clone)]
pub(crate) enum Layout {
    /// Stored inside the object header itself.
    Compact(Vec<u8>),
    /// One contiguous run of bytes at `addr`.
    Contiguous { addr: u64, size: u64 },
    /// Fixed-size chunks indexed by a v1 B-tree at `addr`. `dims` is the chunk shape with the
    /// element size appended, exactly as the layout message stores it.
    Chunked { addr: u64, dims: Vec<u64> },
}

/// A storage filter declared in a dataset's filter pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Filter {
    pub id: u16,
    /// The filter's client data (for `shuffle`, its element size).
    pub client_data: Vec<u32>,
}

/// Filter ids this reader applies.
const FILTER_DEFLATE: u16 = 1;
const FILTER_SHUFFLE: u16 = 2;
const FILTER_FLETCHER32: u16 = 3;

/// One attribute of an object.
#[derive(Debug, Clone)]
pub(crate) struct Attribute {
    pub name: String,
    pub datatype: Datatype,
    pub dims: Vec<u64>,
    pub data: Vec<u8>,
}

/// An attribute's value, reduced to what metadata and provenance can carry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AttrValue {
    Text(String),
    Number(f64),
    /// A value this reader does not render (a compound, a multi-element array, a reference).
    Opaque,
}

/// A parsed object header: every message at one address.
#[derive(Debug, Clone)]
pub(crate) struct ObjectHeader {
    pub messages: Vec<Message>,
}

impl ObjectHeader {
    fn first(&self, kind: u16) -> Option<&Message> {
        self.messages.iter().find(|m| m.kind == kind)
    }

    /// A header is a dataset when it declares where its data lives.
    pub fn is_dataset(&self) -> bool {
        self.first(MSG_LAYOUT).is_some()
    }

    /// A header is a group when it declares links — in either storage form.
    pub fn is_group(&self) -> bool {
        self.first(MSG_SYMBOL_TABLE).is_some() || self.first(MSG_LINK_INFO).is_some()
    }
}

/// A dataset's shape, type, and storage.
#[derive(Debug, Clone)]
pub(crate) struct DatasetInfo {
    pub dims: Vec<u64>,
    pub datatype: Datatype,
    pub layout: Layout,
    pub filters: Vec<Filter>,
}

impl DatasetInfo {
    /// Bytes in one row (one element of the first dimension). `None` on overflow.
    pub fn row_bytes(&self) -> Option<u64> {
        let mut n = self.datatype.size as u64;
        for dim in self.dims.iter().skip(1) {
            n = n.checked_mul(*dim)?;
        }
        Some(n)
    }
}

/// One chunk of a chunked dataset, as its index records it.
#[derive(Debug, Clone)]
struct ChunkRecord {
    /// Byte size on disk (after filters).
    size: u64,
    /// Which filters were skipped for this chunk (bit *i* set = filter *i* not applied).
    filter_mask: u32,
    addr: u64,
}

/// A link a group holds that this reader did not follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkippedLink {
    pub name: String,
    pub reason: &'static str,
}

/// An open HDF5 file.
pub(crate) struct H5File {
    file: File,
    file_len: u64,
    offset_size: usize,
    length_size: usize,
    superblock_version: u8,
    /// The address the file's own addresses are relative to (non-zero only behind a user block).
    base_address: u64,
    root_addr: u64,
    /// Charged for every byte a filter expands, so a chunk that unpacks to a gigabyte is refused on
    /// what it declares rather than after the memory is gone.
    budget: DecompressionBudget,
}

impl H5File {
    /// Whether `path` begins with the HDF5 signature at one of the addresses the format allows.
    pub fn is_hdf5(path: &Path) -> bool {
        let Ok(mut file) = File::open(path) else {
            return false;
        };
        let Ok(len) = file.metadata().map(|m| m.len()) else {
            return false;
        };
        SUPERBLOCK_ADDRESSES.iter().any(|&addr| {
            let mut buf = [0u8; 8];
            addr + 8 <= len
                && file.seek(SeekFrom::Start(addr)).is_ok()
                && file.read_exact(&mut buf).is_ok()
                && buf == SIGNATURE
        })
    }

    /// Open a file and parse its superblock.
    pub fn open(path: &Path, budget: DecompressionBudget) -> H5Result<H5File> {
        let mut file = File::open(path).map_err(|e| IngestError::Io(e.to_string()))?;
        let file_len = file
            .metadata()
            .map_err(|e| IngestError::Io(e.to_string()))?
            .len();

        let mut superblock_addr = None;
        for &addr in SUPERBLOCK_ADDRESSES {
            if addr + 8 > file_len {
                break;
            }
            let mut buf = [0u8; 8];
            file.seek(SeekFrom::Start(addr))
                .map_err(|e| IngestError::Io(e.to_string()))?;
            if file.read_exact(&mut buf).is_ok() && buf == SIGNATURE {
                superblock_addr = Some(addr);
                break;
            }
        }
        let superblock_addr = superblock_addr
            .ok_or_else(|| parse_error("no HDF5 signature at any superblock address"))?;

        // Enough for the largest superblock this reader handles.
        let want = 128.min(file_len - superblock_addr) as usize;
        let mut head = vec![0u8; want];
        file.seek(SeekFrom::Start(superblock_addr))
            .map_err(|e| IngestError::Io(e.to_string()))?;
        file.read_exact(&mut head)
            .map_err(|e| IngestError::Io(e.to_string()))?;

        let mut cursor = Cursor::new(&head, "superblock");
        cursor.skip(8)?;
        let superblock_version = cursor.u8()?;
        let (offset_size, length_size, base_address, root_addr) = match superblock_version {
            0 | 1 => {
                cursor.skip(1)?; // free-space storage version
                cursor.skip(1)?; // root symbol-table entry version
                cursor.skip(1)?; // reserved
                cursor.skip(1)?; // shared-header message-format version
                let offset_size = cursor.u8()? as usize;
                let length_size = cursor.u8()? as usize;
                cursor.skip(1)?; // reserved
                cursor.skip(4)?; // group leaf and internal node K
                cursor.skip(4)?; // file consistency flags
                if superblock_version == 1 {
                    cursor.skip(4)?; // indexed storage internal node K + reserved
                }
                check_widths(offset_size, length_size)?;
                let base_address = cursor.sized(offset_size)?;
                cursor.sized(offset_size)?; // free-space info address
                cursor.sized(offset_size)?; // end-of-file address
                cursor.sized(offset_size)?; // driver information block address
                                            // The root group's symbol-table entry: its link name offset, then its object header.
                cursor.sized(offset_size)?;
                let root_addr = cursor.sized(offset_size)?;
                (offset_size, length_size, base_address, root_addr)
            }
            2 | 3 => {
                let offset_size = cursor.u8()? as usize;
                let length_size = cursor.u8()? as usize;
                check_widths(offset_size, length_size)?;
                cursor.skip(1)?; // file consistency flags
                let base_address = cursor.sized(offset_size)?;
                cursor.sized(offset_size)?; // superblock extension address
                cursor.sized(offset_size)?; // end-of-file address
                let root_addr = cursor.sized(offset_size)?;
                (offset_size, length_size, base_address, root_addr)
            }
            other => {
                return Err(parse_error(format!(
                    "superblock version {other} is not one this reader knows (0, 1, 2, 3)"
                )))
            }
        };

        Ok(H5File {
            file,
            file_len,
            offset_size,
            length_size,
            superblock_version,
            // Every address the file states is relative to the base address, which is non-zero only
            // when a user block precedes the superblock. Folding it in here keeps every later read
            // absolute.
            base_address,
            root_addr,
            budget,
        })
    }

    pub fn root_addr(&self) -> u64 {
        self.root_addr
    }

    /// The superblock version, as the ingest report states it.
    pub fn superblock_version(&self) -> u8 {
        self.superblock_version
    }

    /// Read `len` bytes at the file address `addr` states, refusing a read past the end of the file.
    fn read_at(&mut self, addr: u64, len: u64) -> H5Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let addr = addr.checked_add(self.base_address).ok_or_else(|| {
            parse_error(format!(
                "address {addr} plus the file's base address overflows the address space"
            ))
        })?;
        let end = addr.checked_add(len).ok_or_else(|| {
            parse_error(format!(
                "a read of {len} bytes at {addr} overflows the address space"
            ))
        })?;
        if end > self.file_len {
            return Err(parse_error(format!(
                "a read of {len} bytes at {addr} runs past the end of the file ({} bytes) — the \
                 file is truncated or its addresses are corrupt",
                self.file_len
            )));
        }
        let mut buf = vec![0u8; len as usize];
        self.file
            .seek(SeekFrom::Start(addr))
            .map_err(|e| IngestError::Io(e.to_string()))?;
        self.file
            .read_exact(&mut buf)
            .map_err(|e| IngestError::Io(e.to_string()))?;
        Ok(buf)
    }

    // ---- object headers ----

    /// Parse the object header at `addr`, following its continuation chunks.
    pub fn object_header(&mut self, addr: u64) -> H5Result<ObjectHeader> {
        let head = self.read_at(addr, 4.min(self.file_len.saturating_sub(addr)))?;
        if head.len() < 4 {
            return Err(parse_error(format!(
                "the object header at {addr} is truncated"
            )));
        }
        if &head[..4] == b"OHDR" {
            self.object_header_v2(addr)
        } else if head[0] == 1 {
            self.object_header_v1(addr)
        } else {
            Err(parse_error(format!(
                "the object header at {addr} is neither version 1 nor version 2 (`OHDR`)"
            )))
        }
    }

    fn object_header_v1(&mut self, addr: u64) -> H5Result<ObjectHeader> {
        let head = self.read_at(addr, 16)?;
        let header_size = le_uint(&head[8..12]);
        let mut messages = Vec::new();
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        let mut pending: Vec<(u64, u64)> = vec![(addr + 16, header_size)];
        while let Some((chunk_addr, chunk_len)) = pending.pop() {
            if !visited.insert(chunk_addr) {
                return Err(parse_error(format!(
                    "the object header at {addr} has a continuation chunk that points back at \
                     {chunk_addr}"
                )));
            }
            let data = self.read_at(chunk_addr, chunk_len)?;
            let mut cursor = Cursor::new(&data, "object header message");
            // Trailing bytes too short for a message header are padding, not a message.
            while cursor.remaining() >= 8 {
                let kind = cursor.u16()?;
                let size = cursor.u16()? as usize;
                cursor.skip(1)?; // flags
                cursor.skip(3)?; // reserved
                let body = cursor.take(size)?.to_vec();
                if messages.len() >= MAX_HEADER_MESSAGES {
                    return Err(parse_error(format!(
                        "the object header at {addr} declares more than {MAX_HEADER_MESSAGES} \
                         messages"
                    )));
                }
                if kind == MSG_CONTINUATION {
                    let mut c = Cursor::new(&body, "continuation message");
                    let next_addr = c.sized(self.offset_size)?;
                    let next_len = c.sized(self.length_size)?;
                    if !is_undefined(next_addr, self.offset_size) && next_len > 0 {
                        pending.push((next_addr, next_len));
                    }
                    continue;
                }
                messages.push(Message { kind, data: body });
            }
        }
        Ok(ObjectHeader { messages })
    }

    fn object_header_v2(&mut self, addr: u64) -> H5Result<ObjectHeader> {
        let head = self.read_at(addr, 6.min(self.file_len.saturating_sub(addr)))?;
        let mut cursor = Cursor::new(&head, "object header v2");
        cursor.skip(4)?; // OHDR
        let version = cursor.u8()?;
        if version != 2 {
            return Err(parse_error(format!(
                "the object header at {addr} carries the `OHDR` signature but version {version}"
            )));
        }
        let flags = cursor.u8()?;
        let track_order = flags & 0x04 != 0;
        // After the flags: optional timestamps, optional phase-change values, then chunk 0's size.
        let mut prefix = 6u64;
        if flags & 0x20 != 0 {
            prefix += 16;
        }
        if flags & 0x10 != 0 {
            prefix += 4;
        }
        let size_width = 1usize << (flags & 0x03);
        let size_field = self.read_at(addr + prefix, size_width as u64)?;
        let chunk0_size = le_uint(&size_field);
        let first_chunk = addr + prefix + size_width as u64;

        let mut messages = Vec::new();
        let mut visited: BTreeSet<u64> = BTreeSet::new();
        let mut pending: Vec<(u64, u64, bool)> = vec![(first_chunk, chunk0_size, false)];
        while let Some((chunk_addr, chunk_len, is_continuation)) = pending.pop() {
            if !visited.insert(chunk_addr) {
                return Err(parse_error(format!(
                    "the object header at {addr} has a continuation chunk that points back at \
                     {chunk_addr}"
                )));
            }
            let data = self.read_at(chunk_addr, chunk_len)?;
            let body: &[u8] = if is_continuation {
                // A continuation chunk is `OCHK`, then messages, then a 4-byte checksum.
                if data.len() < 8 || &data[..4] != b"OCHK" {
                    return Err(parse_error(format!(
                        "the object header continuation at {chunk_addr} lacks the `OCHK` signature"
                    )));
                }
                &data[4..data.len() - 4]
            } else {
                &data
            };
            let mut cursor = Cursor::new(body, "object header v2 message");
            let msg_prefix = if track_order { 6 } else { 4 };
            // A chunk's last bytes may be a gap too small to hold a message header.
            while cursor.remaining() >= msg_prefix {
                let kind = cursor.u8()? as u16;
                let size = cursor.u16()? as usize;
                cursor.skip(1)?; // flags
                if track_order {
                    cursor.skip(2)?; // creation order
                }
                let body = cursor.take(size)?.to_vec();
                if messages.len() >= MAX_HEADER_MESSAGES {
                    return Err(parse_error(format!(
                        "the object header at {addr} declares more than {MAX_HEADER_MESSAGES} \
                         messages"
                    )));
                }
                if kind == MSG_CONTINUATION {
                    let mut c = Cursor::new(&body, "continuation message");
                    let next_addr = c.sized(self.offset_size)?;
                    let next_len = c.sized(self.length_size)?;
                    if !is_undefined(next_addr, self.offset_size) && next_len > 0 {
                        pending.push((next_addr, next_len, true));
                    }
                    continue;
                }
                messages.push(Message { kind, data: body });
            }
        }
        Ok(ObjectHeader { messages })
    }

    // ---- groups ----

    /// The links of the group whose header is `header`, in the order the file stores them.
    ///
    /// Returns `(name, object header address)` for hard links. A soft or external link points
    /// outside this file's object graph; it is reported through `skipped` rather than followed, so a
    /// group is never quietly reported as smaller than it is.
    pub fn group_links(
        &mut self,
        header: &ObjectHeader,
        skipped: &mut Vec<SkippedLink>,
    ) -> H5Result<Vec<(String, u64)>> {
        // New-style groups keep their links as messages in the header itself…
        if let Some(info) = header.first(MSG_LINK_INFO) {
            let mut cursor = Cursor::new(&info.data, "link info message");
            cursor.u8()?; // version
            let flags = cursor.u8()?;
            if flags & 0x01 != 0 {
                cursor.skip(8)?; // maximum creation index
            }
            let fractal_heap = cursor.sized(self.offset_size)?;
            if !is_undefined(fractal_heap, self.offset_size) {
                return Err(parse_error(
                    "a group stores its links in a fractal heap (dense storage), which this reader \
                     does not read — every object under it would be invisible. Re-save the file \
                     with h5py's default settings, or with fewer links per group",
                ));
            }
            let mut links = Vec::new();
            for msg in header.messages.iter().filter(|m| m.kind == MSG_LINK) {
                let (name, kind, mut body) = self.parse_link_prefix(&msg.data)?;
                match kind {
                    0 => links.push((name, body.sized(self.offset_size)?)),
                    _ => skipped.push(SkippedLink {
                        name,
                        reason: "a soft or external link points outside this file's object graph \
                                 and is not followed",
                    }),
                }
            }
            return Ok(links);
        }

        // …old-style groups keep them in a B-tree of symbol-table nodes plus a local heap.
        let Some(symtab) = header.first(MSG_SYMBOL_TABLE) else {
            return Err(parse_error(
                "an object declares neither link messages nor a symbol table, so its children \
                 cannot be listed",
            ));
        };
        let mut cursor = Cursor::new(&symtab.data, "symbol table message");
        let btree_addr = cursor.sized(self.offset_size)?;
        let heap_addr = cursor.sized(self.offset_size)?;
        if is_undefined(btree_addr, self.offset_size) {
            return Ok(Vec::new());
        }
        let heap = self.read_local_heap(heap_addr)?;
        let mut entries = Vec::new();
        self.walk_symbol_btree(btree_addr, &heap, &mut entries, skipped, &mut 0)?;
        Ok(entries)
    }

    /// The head of a link message: its name and link type, leaving the cursor at the body.
    fn parse_link_prefix<'a>(&self, data: &'a [u8]) -> H5Result<(String, u8, Cursor<'a>)> {
        let mut cursor = Cursor::new(data, "link message");
        let version = cursor.u8()?;
        if version != 1 {
            return Err(parse_error(format!(
                "link message version {version} is not supported (only 1)"
            )));
        }
        let flags = cursor.u8()?;
        let len_width = 1usize << (flags & 0x03);
        let kind = if flags & 0x08 != 0 { cursor.u8()? } else { 0 };
        if flags & 0x04 != 0 {
            cursor.skip(8)?; // creation order
        }
        if flags & 0x10 != 0 {
            cursor.skip(1)?; // name character set
        }
        let name_len = cursor.sized(len_width)?;
        let name_bytes = cursor.take(name_len as usize)?;
        let name = String::from_utf8(name_bytes.to_vec())
            .map_err(|_| parse_error("a link name is not valid UTF-8"))?;
        Ok((name, kind, cursor))
    }

    /// The data segment of a local heap (where old-style group link names live).
    fn read_local_heap(&mut self, addr: u64) -> H5Result<Vec<u8>> {
        let head = self.read_at(
            addr,
            8 + 2 * self.length_size as u64 + self.offset_size as u64,
        )?;
        let mut cursor = Cursor::new(&head, "local heap");
        if cursor.take(4)? != b"HEAP" {
            return Err(parse_error(format!(
                "the local heap at {addr} lacks the `HEAP` signature"
            )));
        }
        cursor.skip(4)?; // version + reserved
        let data_size = cursor.sized(self.length_size)?;
        cursor.sized(self.length_size)?; // free-list head
        let data_addr = cursor.sized(self.offset_size)?;
        self.read_at(data_addr, data_size)
    }

    /// Walk a v1 B-tree of group nodes, collecting `(name, object header address)`.
    fn walk_symbol_btree(
        &mut self,
        addr: u64,
        heap: &[u8],
        out: &mut Vec<(String, u64)>,
        skipped: &mut Vec<SkippedLink>,
        visited: &mut usize,
    ) -> H5Result<()> {
        *visited += 1;
        if *visited > MAX_BTREE_NODES {
            return Err(parse_error(
                "a group B-tree walk visited more nodes than a real file holds; the index is \
                 cyclic or corrupt",
            ));
        }
        let head = self.read_at(addr, 8 + 2 * self.offset_size as u64)?;
        let mut cursor = Cursor::new(&head, "group B-tree node");
        if cursor.take(4)? != b"TREE" {
            return Err(parse_error(format!(
                "the group B-tree node at {addr} lacks the `TREE` signature"
            )));
        }
        let node_type = cursor.u8()?;
        if node_type != 0 {
            return Err(parse_error(format!(
                "the B-tree node at {addr} has type {node_type}, not a group node"
            )));
        }
        let level = cursor.u8()?;
        let entries = cursor.u16()? as u64;
        // Each entry is a key (an offset into the local heap) followed by a child address, and one
        // trailing key closes the node.
        let body_addr = addr + 8 + 2 * self.offset_size as u64;
        let entry_size = self.length_size as u64 + self.offset_size as u64;
        let body = self.read_at(body_addr, entries * entry_size + self.length_size as u64)?;
        let mut cursor = Cursor::new(&body, "group B-tree entries");
        for _ in 0..entries {
            cursor.sized(self.length_size)?; // key
            let child = cursor.sized(self.offset_size)?;
            if level > 0 {
                self.walk_symbol_btree(child, heap, out, skipped, visited)?;
            } else {
                self.read_symbol_table_node(child, heap, out, skipped)?;
            }
        }
        Ok(())
    }

    fn read_symbol_table_node(
        &mut self,
        addr: u64,
        heap: &[u8],
        out: &mut Vec<(String, u64)>,
        skipped: &mut Vec<SkippedLink>,
    ) -> H5Result<()> {
        let head = self.read_at(addr, 8)?;
        if &head[..4] != b"SNOD" {
            return Err(parse_error(format!(
                "the symbol table node at {addr} lacks the `SNOD` signature"
            )));
        }
        let count = le_uint(&head[6..8]);
        // An entry is: name offset, object header address, cache type, reserved, scratch pad.
        let entry_size = 2 * self.offset_size as u64 + 8 + 16;
        let body = self.read_at(addr + 8, count * entry_size)?;
        let mut cursor = Cursor::new(&body, "symbol table entries");
        for _ in 0..count {
            let name_offset = cursor.sized(self.offset_size)?;
            let obj_addr = cursor.sized(self.offset_size)?;
            let cache_type = cursor.u32()?;
            cursor.skip(4 + 16)?;
            let name = heap_string(heap, name_offset)?;
            // Cache type 2 is a *symbolic* (soft) link: its target is a path string in the local
            // heap, and its object header address field is undefined. Following it would read at
            // 0xFFFF… and blame the file for a truncated header.
            if cache_type == 2 || is_undefined(obj_addr, self.offset_size) {
                skipped.push(SkippedLink {
                    name,
                    reason: "a soft or external link points outside this file's object graph and \
                             is not followed",
                });
                continue;
            }
            out.push((name, obj_addr));
        }
        Ok(())
    }

    // ---- datasets ----

    /// The shape, type, storage, and filters of the dataset whose header is `header`.
    pub fn dataset_info(&self, header: &ObjectHeader) -> H5Result<DatasetInfo> {
        let dims = match header.first(MSG_DATASPACE) {
            Some(msg) => self.parse_dataspace(&msg.data)?,
            None => {
                return Err(parse_error(
                    "a dataset declares no dataspace, so its shape is unknown",
                ))
            }
        };
        let datatype = match header.first(MSG_DATATYPE) {
            Some(msg) => parse_datatype(&msg.data)?,
            None => {
                return Err(parse_error(
                    "a dataset declares no datatype, so its elements cannot be sized",
                ))
            }
        };
        let layout = self.parse_layout(
            &header
                .first(MSG_LAYOUT)
                .ok_or_else(|| parse_error("a dataset declares no data layout"))?
                .data,
        )?;
        let filters = match header.first(MSG_FILTER) {
            Some(msg) => parse_filter_pipeline(&msg.data)?,
            None => Vec::new(),
        };
        Ok(DatasetInfo {
            dims,
            datatype,
            layout,
            filters,
        })
    }

    fn parse_dataspace(&self, data: &[u8]) -> H5Result<Vec<u64>> {
        let mut cursor = Cursor::new(data, "dataspace message");
        let version = cursor.u8()?;
        let rank = cursor.u8()? as usize;
        cursor.u8()?; // flags
        match version {
            1 => cursor.skip(5)?, // reserved
            2 => {
                // 0 = scalar (rank 0), 1 = simple, 2 = null (no elements at all).
                if cursor.u8()? == 2 {
                    return Ok(vec![0]);
                }
            }
            other => {
                return Err(parse_error(format!(
                    "dataspace message version {other} is not supported (1, 2)"
                )))
            }
        }
        let mut dims = Vec::with_capacity(rank.min(64));
        for _ in 0..rank {
            dims.push(cursor.sized(self.length_size)?);
        }
        Ok(dims)
    }

    fn parse_layout(&self, data: &[u8]) -> H5Result<Layout> {
        let mut cursor = Cursor::new(data, "data layout message");
        let version = cursor.u8()?;
        match version {
            3 => {
                let class = cursor.u8()?;
                match class {
                    0 => {
                        let size = cursor.u16()? as usize;
                        Ok(Layout::Compact(cursor.take(size)?.to_vec()))
                    }
                    1 => {
                        let addr = cursor.sized(self.offset_size)?;
                        let size = cursor.sized(self.length_size)?;
                        Ok(Layout::Contiguous { addr, size })
                    }
                    2 => {
                        // `rank` counts the element-size dimension the format appends.
                        let rank = cursor.u8()? as usize;
                        let addr = cursor.sized(self.offset_size)?;
                        let mut dims = Vec::with_capacity(rank.min(64));
                        for _ in 0..rank {
                            dims.push(cursor.u32()? as u64);
                        }
                        Ok(Layout::Chunked { addr, dims })
                    }
                    other => Err(parse_error(format!(
                        "data layout class {other} is not one this reader knows (compact, \
                         contiguous, chunked)"
                    ))),
                }
            }
            4 => Err(parse_error(
                "data layout message version 4 (the HDF5 1.10 chunk indexes) is not supported; \
                 re-save the file with h5py's default settings",
            )),
            other => Err(parse_error(format!(
                "data layout message version {other} is not supported (3)"
            ))),
        }
    }

    /// A reader over one dataset's rows.
    pub fn rows<'a>(&'a mut self, info: &'a DatasetInfo) -> H5Result<RowReader<'a>> {
        RowReader::new(self, info)
    }

    // ---- attributes ----

    /// Every attribute attached to an object, in header order.
    ///
    /// An attribute this reader cannot parse is skipped rather than fatal — an unreadable
    /// *annotation* must not fail an otherwise sound dataset — but never silently: its name (or the
    /// reason it had none) lands in `skipped` for the ingest report.
    pub fn attributes(
        &mut self,
        header: &ObjectHeader,
        skipped: &mut Vec<String>,
    ) -> Vec<Attribute> {
        let messages: Vec<Vec<u8>> = header
            .messages
            .iter()
            .filter(|m| m.kind == MSG_ATTRIBUTE)
            .map(|m| m.data.clone())
            .collect();
        // An object with many attributes moves them out of the header into a fractal heap. Those are
        // not read — but an object whose attributes all live there would otherwise come back as
        // having none, and "no license, no task, no declared count" is a materially different dataset
        // from "the attributes were somewhere this reader does not look".
        if let Some(info) = header.first(MSG_ATTRIBUTE_INFO) {
            match self.dense_attribute_storage(&info.data) {
                Ok(true) => skipped.push(
                    "this object keeps its attributes in dense (fractal heap) storage, which this \
                     reader does not read, so none of them were mapped"
                        .into(),
                ),
                Ok(false) => {}
                Err(e) => skipped.push(e.to_string()),
            }
        }
        let mut out = Vec::new();
        for data in messages {
            match self.parse_attribute(&data) {
                Ok(attr) => out.push(attr),
                Err(e) => skipped.push(e.to_string()),
            }
        }
        out
    }

    /// Whether an attribute-info message says the object's attributes live in a fractal heap.
    fn dense_attribute_storage(&self, data: &[u8]) -> H5Result<bool> {
        let mut cursor = Cursor::new(data, "attribute info message");
        cursor.u8()?; // version
        let flags = cursor.u8()?;
        if flags & 0x01 != 0 {
            cursor.skip(2)?; // maximum creation index
        }
        let heap = cursor.sized(self.offset_size)?;
        Ok(!is_undefined(heap, self.offset_size))
    }

    fn parse_attribute(&mut self, data: &[u8]) -> H5Result<Attribute> {
        let mut cursor = Cursor::new(data, "attribute message");
        let version = cursor.u8()?;
        let flags = cursor.u8()?;
        let name_size = cursor.u16()? as usize;
        let datatype_size = cursor.u16()? as usize;
        let dataspace_size = cursor.u16()? as usize;
        match version {
            1 | 2 => {}
            3 => {
                cursor.u8()?; // name character-set encoding
            }
            other => {
                return Err(parse_error(format!(
                    "attribute message version {other} is not supported (1, 2, 3)"
                )))
            }
        }
        if version >= 2 && flags != 0 {
            return Err(parse_error(
                "an attribute uses a shared datatype or dataspace, which this reader does not \
                 resolve",
            ));
        }
        // Version 1 pads each variable-length part to an 8-byte boundary; later versions do not.
        let pad = |n: usize| if version == 1 { (8 - n % 8) % 8 } else { 0 };
        let name_bytes = cursor.take(name_size)?.to_vec();
        cursor.skip(pad(name_size))?;
        let datatype_bytes = cursor.take(datatype_size)?.to_vec();
        cursor.skip(pad(datatype_size))?;
        let dataspace_bytes = cursor.take(dataspace_size)?.to_vec();
        cursor.skip(pad(dataspace_size))?;

        let name = String::from_utf8(
            name_bytes
                .iter()
                .copied()
                .take_while(|b| *b != 0)
                .collect::<Vec<u8>>(),
        )
        .map_err(|_| parse_error("an attribute name is not valid UTF-8"))?;
        let datatype = parse_datatype(&datatype_bytes)?;
        let dims = self.parse_dataspace(&dataspace_bytes)?;
        let left = cursor.remaining();
        let data = cursor.take(left)?.to_vec();
        Ok(Attribute {
            name,
            datatype,
            dims,
            data,
        })
    }

    /// Render an attribute as text or a number, resolving a variable-length string through the
    /// global heap. Anything else is [`AttrValue::Opaque`] — recorded as present, not invented.
    pub fn attribute_value(&mut self, attr: &Attribute) -> AttrValue {
        let elements: u64 = attr.dims.iter().product::<u64>().max(1);
        if elements != 1 {
            return AttrValue::Opaque;
        }
        match attr.datatype.class {
            DatatypeClass::FixedString => AttrValue::Text(decode_text(&attr.data)),
            DatatypeClass::VariableLength => match self.read_vlen(&attr.data) {
                Ok(bytes) => AttrValue::Text(decode_text(&bytes)),
                Err(_) => AttrValue::Opaque,
            },
            _ => match attr.datatype.decode(&attr.data) {
                Some(n) => AttrValue::Number(n),
                None => AttrValue::Opaque,
            },
        }
    }

    /// Resolve a variable-length descriptor (a length, then a global-heap id) to its bytes.
    fn read_vlen(&mut self, descriptor: &[u8]) -> H5Result<Vec<u8>> {
        let mut cursor = Cursor::new(descriptor, "variable-length descriptor");
        let length = cursor.u32()? as u64;
        let collection = cursor.sized(self.offset_size)?;
        let index = cursor.u32()?;
        if is_undefined(collection, self.offset_size) || length == 0 {
            return Ok(Vec::new());
        }
        let head = self.read_at(collection, 8 + self.length_size as u64)?;
        if &head[..4] != b"GCOL" {
            return Err(parse_error(format!(
                "the global heap collection at {collection} lacks the `GCOL` signature"
            )));
        }
        let collection_size = le_uint(&head[8..8 + self.length_size]);
        let body = self.read_at(collection, collection_size)?;
        // Objects follow the collection header, each padded to an 8-byte boundary.
        let mut pos = 8 + self.length_size;
        while pos + 8 + self.length_size <= body.len() {
            let obj_index = le_uint(&body[pos..pos + 2]) as u32;
            let size = le_uint(&body[pos + 8..pos + 8 + self.length_size]);
            let data_start = pos + 8 + self.length_size;
            if obj_index == 0 {
                break; // the free-space object closes the collection
            }
            if obj_index == index {
                let end = data_start
                    .checked_add(size.min(length) as usize)
                    .filter(|e| *e <= body.len())
                    .ok_or_else(|| parse_error("a global heap object runs past its collection"))?;
                return Ok(body[data_start..end].to_vec());
            }
            pos = data_start
                .checked_add((size as usize).next_multiple_of(8))
                .ok_or_else(|| parse_error("a global heap object size overflows"))?;
        }
        Err(parse_error(format!(
            "global heap object {index} is not in the collection at {collection}"
        )))
    }

    /// Walk a v1 B-tree of chunk records, collecting every chunk's origin and location.
    fn walk_chunk_btree(
        &mut self,
        addr: u64,
        key_rank: usize,
        out: &mut BTreeMap<Vec<u64>, ChunkRecord>,
        visited: &mut usize,
    ) -> H5Result<()> {
        *visited += 1;
        if *visited > MAX_BTREE_NODES {
            return Err(parse_error(
                "a chunk B-tree walk visited more nodes than a real file holds; the index is \
                 cyclic or corrupt",
            ));
        }
        let head = self.read_at(addr, 8 + 2 * self.offset_size as u64)?;
        let mut cursor = Cursor::new(&head, "chunk B-tree node");
        if cursor.take(4)? != b"TREE" {
            return Err(parse_error(format!(
                "the chunk B-tree node at {addr} lacks the `TREE` signature"
            )));
        }
        let node_type = cursor.u8()?;
        if node_type != 1 {
            return Err(parse_error(format!(
                "the B-tree node at {addr} has type {node_type}, not a chunk node"
            )));
        }
        let level = cursor.u8()?;
        let entries = cursor.u16()? as u64;
        // A chunk key is a byte size, a filter mask, and one 64-bit offset per key dimension.
        let key_size = 8 + 8 * key_rank as u64;
        let body_addr = addr + 8 + 2 * self.offset_size as u64;
        let body = self.read_at(
            body_addr,
            entries * (key_size + self.offset_size as u64) + key_size,
        )?;
        let mut cursor = Cursor::new(&body, "chunk B-tree entries");
        for _ in 0..entries {
            let size = cursor.u32()? as u64;
            let filter_mask = cursor.u32()?;
            let mut origin = Vec::with_capacity(key_rank.min(64));
            for _ in 0..key_rank {
                origin.push(cursor.u64()?);
            }
            let child = cursor.sized(self.offset_size)?;
            if level > 0 {
                self.walk_chunk_btree(child, key_rank, out, visited)?;
            } else {
                if out.len() >= MAX_CHUNKS_PER_DATASET {
                    return Err(parse_error(format!(
                        "a dataset's chunk index holds more than {MAX_CHUNKS_PER_DATASET} chunks"
                    )));
                }
                out.insert(
                    origin,
                    ChunkRecord {
                        size,
                        filter_mask,
                        addr: child,
                    },
                );
            }
        }
        Ok(())
    }

    /// Charge the ingest's expansion budget for `n` bytes this reader is about to synthesize.
    fn charge(&mut self, n: u64) -> H5Result<()> {
        self.budget.take(FORMAT_ID, n)
    }

    /// Undo a chunk's filter pipeline, in reverse declaration order.
    fn apply_filters(
        &mut self,
        mut data: Vec<u8>,
        filters: &[Filter],
        filter_mask: u32,
        expected: u64,
    ) -> H5Result<Vec<u8>> {
        for (i, filter) in filters.iter().enumerate().rev() {
            // The chunk's own mask records the filters skipped when it was written.
            if filter_mask & (1u32 << i) != 0 {
                continue;
            }
            data = match filter.id {
                FILTER_DEFLATE => {
                    // Charged on what the chunk's shape says it must become, before inflating.
                    self.budget.take(FORMAT_ID, expected)?;
                    inflate(&data, expected)?
                }
                FILTER_SHUFFLE => {
                    let stride = filter.client_data.first().copied().unwrap_or(0) as usize;
                    if stride == 0 {
                        return Err(parse_error(
                            "a chunk is shuffle-filtered but declares no element size, so the \
                             permutation cannot be undone",
                        ));
                    }
                    unshuffle(&data, stride)
                }
                FILTER_FLETCHER32 => {
                    if data.len() < 4 {
                        return Err(parse_error("a fletcher32-protected chunk is too short"));
                    }
                    let split = data.len() - 4;
                    let stored = u32::from_le_bytes(data[split..].try_into().expect("four bytes"));
                    let body = data[..split].to_vec();
                    if stored != fletcher32(&body) {
                        return Err(parse_error(
                            "a chunk fails the fletcher32 checksum stored with it — the data is \
                             corrupt",
                        ));
                    }
                    body
                }
                other => {
                    return Err(parse_error(format!(
                        "chunk data is filtered with {}, which this reader does not implement (it \
                         applies deflate, shuffle, and fletcher32) — re-save the array with gzip \
                         compression, or without a filter",
                        filter_name(other)
                    )))
                }
            };
        }
        Ok(data)
    }
}

/// The name HDF5 registers for a filter id, so a refusal names what the file used rather than a
/// number the reader's user has to look up.
fn filter_name(id: u16) -> String {
    let name = match id {
        4 => "szip",
        5 => "nbit",
        6 => "scaleoffset",
        32_000 => "lzf",
        32_001 => "blosc",
        32_004 => "lz4",
        32_015 => "zstd",
        _ => return format!("filter {id}"),
    };
    format!("the `{name}` filter (id {id})")
}

/// Both field widths must be something this reader can hold in a `u64`.
fn check_widths(offset_size: usize, length_size: usize) -> H5Result<()> {
    for (what, size) in [("offset", offset_size), ("length", length_size)] {
        if size == 0 || size > 8 {
            return Err(parse_error(format!(
                "the file declares {size}-byte {what} fields; this reader handles 1 through 8"
            )));
        }
    }
    Ok(())
}

/// A null-terminated name at `offset` in a local heap's data segment.
fn heap_string(heap: &[u8], offset: u64) -> H5Result<String> {
    let start = offset as usize;
    if start >= heap.len() {
        return Err(parse_error(
            "a group link name points past the end of its local heap",
        ));
    }
    let bytes: Vec<u8> = heap[start..]
        .iter()
        .copied()
        .take_while(|b| *b != 0)
        .collect();
    String::from_utf8(bytes).map_err(|_| parse_error("a group link name is not valid UTF-8"))
}

/// Text from a fixed-length or heap-resolved string, cut at its first NUL.
fn decode_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn parse_datatype(data: &[u8]) -> H5Result<Datatype> {
    let mut cursor = Cursor::new(data, "datatype message");
    let class_and_version = cursor.u8()?;
    let class = class_and_version & 0x0F;
    let bitfields = cursor.take(3)?;
    let size = cursor.u32()?;
    // Bit 0 of the class bit field is the byte order for the numeric classes.
    let little_endian = bitfields[0] & 0x01 == 0;
    let class = match class {
        0 => DatatypeClass::FixedPoint {
            // Bit 3 is a fixed-point type's signedness.
            signed: bitfields[0] & 0x08 != 0,
        },
        1 => DatatypeClass::Float,
        3 => DatatypeClass::FixedString,
        9 => DatatypeClass::VariableLength,
        other => DatatypeClass::Other(other),
    };
    Ok(Datatype {
        class,
        size,
        little_endian,
    })
}

fn parse_filter_pipeline(data: &[u8]) -> H5Result<Vec<Filter>> {
    let mut cursor = Cursor::new(data, "filter pipeline message");
    let version = cursor.u8()?;
    let count = cursor.u8()? as usize;
    match version {
        1 => cursor.skip(6)?, // reserved
        2 => {}
        other => {
            return Err(parse_error(format!(
                "filter pipeline message version {other} is not supported (1, 2)"
            )))
        }
    }
    let mut filters = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let id = cursor.u16()?;
        // Version 2 omits the name (and its length) for the filters HDF5 defines itself.
        let name_len = if version == 1 || id >= 256 {
            cursor.u16()? as usize
        } else {
            0
        };
        cursor.u16()?; // flags
        let client_count = cursor.u16()? as usize;
        cursor.skip(name_len)?;
        let mut client_data = Vec::with_capacity(client_count.min(64));
        for _ in 0..client_count {
            client_data.push(cursor.u32()?);
        }
        if version == 1 && client_count % 2 == 1 {
            cursor.skip(4)?; // padding to an 8-byte boundary
        }
        filters.push(Filter { id, client_data });
    }
    Ok(filters)
}

/// Reads a dataset's rows, decoding chunks on demand.
pub(crate) struct RowReader<'a> {
    file: &'a mut H5File,
    info: &'a DatasetInfo,
    /// The chunk index, when the dataset is chunked: chunk origin -> where its bytes are.
    chunks: BTreeMap<Vec<u64>, ChunkRecord>,
    /// The most recently decoded chunk, so a sequential row walk decodes each chunk once.
    cached: Option<(Vec<u64>, Vec<u8>)>,
    row_bytes: u64,
}

impl<'a> RowReader<'a> {
    fn new(file: &'a mut H5File, info: &'a DatasetInfo) -> H5Result<RowReader<'a>> {
        let row_bytes = info
            .row_bytes()
            .ok_or_else(|| parse_error("a dataset's declared shape overflows a byte count"))?;
        let mut chunks = BTreeMap::new();
        if let Layout::Chunked { addr, dims } = &info.layout {
            let (addr, dims) = (*addr, dims.clone());
            if !is_undefined(addr, file.offset_size) {
                let mut visited = 0usize;
                file.walk_chunk_btree(addr, dims.len(), &mut chunks, &mut visited)?;
            }
        }
        Ok(RowReader {
            file,
            info,
            chunks,
            cached: None,
            row_bytes,
        })
    }

    /// Bytes in one row.
    pub fn row_bytes(&self) -> u64 {
        self.row_bytes
    }

    /// The byte offset of row `index` within the file, when the dataset is stored contiguously —
    /// `None` for a layout where a row has no single address.
    pub fn row_offset(&self, index: u64) -> Option<u64> {
        match self.info.layout {
            Layout::Contiguous { addr, .. } => index
                .checked_mul(self.row_bytes)
                .and_then(|o| addr.checked_add(o)),
            _ => None,
        }
    }

    /// Row `index`, assembled from wherever its bytes live.
    pub fn row(&mut self, index: u64) -> H5Result<Vec<u8>> {
        match &self.info.layout {
            Layout::Compact(bytes) => {
                let start = index
                    .checked_mul(self.row_bytes)
                    .ok_or_else(|| parse_error("a compact dataset row offset overflows"))?
                    as usize;
                let end = start
                    .checked_add(self.row_bytes as usize)
                    .ok_or_else(|| parse_error("a compact dataset row overflows"))?;
                if end > bytes.len() {
                    return Err(parse_error(format!(
                        "compact row {index} runs past the {} bytes stored in the header",
                        bytes.len()
                    )));
                }
                Ok(bytes[start..end].to_vec())
            }
            Layout::Contiguous { addr, size } => {
                let (addr, size) = (*addr, *size);
                let offset = index
                    .checked_mul(self.row_bytes)
                    .ok_or_else(|| parse_error("a contiguous dataset row offset overflows"))?;
                if offset.saturating_add(self.row_bytes) > size {
                    return Err(parse_error(format!(
                        "row {index} runs past the {size} bytes the dataset declares"
                    )));
                }
                self.file.read_at(addr + offset, self.row_bytes)
            }
            Layout::Chunked { dims, .. } => {
                let dims = dims.clone();
                self.chunked_row(index, &dims)
            }
        }
    }

    /// Assemble one row out of every chunk that intersects it.
    fn chunked_row(&mut self, index: u64, chunk_dims: &[u64]) -> H5Result<Vec<u8>> {
        let rank = self.info.dims.len();
        if chunk_dims.len() != rank + 1 {
            return Err(parse_error(format!(
                "a chunked dataset of rank {rank} declares a {}-dimensional chunk shape",
                chunk_dims.len()
            )));
        }
        if rank == 0 {
            return Err(parse_error("a chunked dataset declares no dimensions"));
        }
        // Charged before the buffer exists. A chunked row is *synthesized* — assembled from chunks
        // and fill value — so unlike a contiguous read its size is not bounded by the file's own
        // size: one flipped byte in a dataspace dimension of this 23 KB fixture turned a 144-byte
        // row into a 141 MB one, and four of those took forty seconds to hash into frames the file
        // never held. The expansion budget is exactly the instrument for that (it scales with the
        // source and has a 64 MB floor, so a real dataset never notices).
        self.file.charge(self.row_bytes)?;
        let elem = self.info.datatype.size as u64;
        let mut row = vec![0u8; self.row_bytes as usize];
        // Every chunk holding part of this row: its first-dimension extent covers `index`, and the
        // other dimensions are visited in full.
        let origins: Vec<Vec<u64>> = self
            .chunks
            .keys()
            .filter(|origin| {
                origin.len() == rank + 1
                    && origin[0] <= index
                    && index < origin[0].saturating_add(chunk_dims[0])
            })
            .cloned()
            .collect();
        if origins.is_empty() {
            return Err(parse_error(format!(
                "no chunk in the index holds row {index}; the dataset's chunk index is incomplete"
            )));
        }
        for origin in origins {
            self.load_chunk(&origin, chunk_dims, elem)?;
            let (_, chunk) = self.cached.as_ref().expect("just loaded");
            copy_chunk_into_row(
                chunk,
                &origin,
                chunk_dims,
                &self.info.dims,
                index,
                elem,
                &mut row,
            )?;
        }
        Ok(row)
    }

    /// Make `self.cached` hold the decoded bytes of the chunk at `origin`.
    fn load_chunk(&mut self, origin: &[u64], chunk_dims: &[u64], elem: u64) -> H5Result<()> {
        if self
            .cached
            .as_ref()
            .is_some_and(|(cached, _)| cached.as_slice() == origin)
        {
            return Ok(());
        }
        let record = self
            .chunks
            .get(origin)
            .ok_or_else(|| parse_error("a chunk vanished from its own index"))?
            .clone();
        // A chunk always stores its full shape, even where the dataset's edge cuts it short.
        let mut expected = elem;
        for dim in &chunk_dims[..chunk_dims.len() - 1] {
            expected = expected
                .checked_mul(*dim)
                .ok_or_else(|| parse_error("a chunk's declared shape overflows a byte count"))?;
        }
        let raw = self.file.read_at(record.addr, record.size)?;
        let decoded =
            self.file
                .apply_filters(raw, &self.info.filters, record.filter_mask, expected)?;
        if decoded.len() as u64 != expected {
            return Err(parse_error(format!(
                "a chunk decoded to {} bytes where its shape needs {expected}",
                decoded.len()
            )));
        }
        self.cached = Some((origin.to_vec(), decoded));
        Ok(())
    }
}

/// Copy the part of `chunk` that belongs to row `index` into `row`.
///
/// Both buffers are C-ordered (the last dimension varies fastest), so the copy walks the dimensions
/// between the first and the last with an odometer, moving one contiguous run per position.
fn copy_chunk_into_row(
    chunk: &[u8],
    origin: &[u64],
    chunk_dims: &[u64],
    dims: &[u64],
    index: u64,
    elem: u64,
    row: &mut [u8],
) -> H5Result<()> {
    let rank = dims.len();
    let within = index
        .checked_sub(origin[0])
        .ok_or_else(|| parse_error("a chunk origin sits after the row it indexes"))?;
    if rank == 1 {
        // A 1-D dataset's "row" is a single element.
        let src = (within * elem) as usize;
        if src + elem as usize > chunk.len() || elem as usize > row.len() {
            return Err(parse_error("a chunk is too small for the row it indexes"));
        }
        row[..elem as usize].copy_from_slice(&chunk[src..src + elem as usize]);
        return Ok(());
    }
    let last = rank - 1;
    // How many elements of the last dimension this chunk contributes.
    let run = chunk_dims[last].min(dims[last].saturating_sub(origin[last]));
    if run == 0 {
        return Ok(());
    }
    // Per-dimension extents inside this chunk, clipped where the dataset's edge cuts it short.
    let extents: Vec<u64> = (0..rank)
        .map(|d| {
            if d == 0 {
                1
            } else {
                chunk_dims[d].min(dims[d].saturating_sub(origin[d]))
            }
        })
        .collect();
    if extents.contains(&0) {
        return Ok(());
    }
    let mut counters = vec![0u64; rank];
    counters[0] = within;
    loop {
        // The source offset inside the chunk, and the destination offset inside the row.
        let mut src = 0u64;
        let mut dst = 0u64;
        for d in 0..rank {
            src = src
                .checked_mul(chunk_dims[d])
                .and_then(|s| s.checked_add(counters[d]))
                .ok_or_else(|| parse_error("a chunk offset overflows"))?;
            if d > 0 {
                dst = dst
                    .checked_mul(dims[d])
                    .and_then(|s| s.checked_add(origin[d] + counters[d]))
                    .ok_or_else(|| parse_error("a row offset overflows"))?;
            }
        }
        let src_bytes = (src * elem) as usize;
        let dst_bytes = (dst * elem) as usize;
        let run_bytes = (run * elem) as usize;
        if src_bytes + run_bytes > chunk.len() {
            return Err(parse_error("a chunk is too small for the row it indexes"));
        }
        if dst_bytes + run_bytes > row.len() {
            return Err(parse_error(
                "a chunk claims more of a row than the dataset's shape holds",
            ));
        }
        row[dst_bytes..dst_bytes + run_bytes]
            .copy_from_slice(&chunk[src_bytes..src_bytes + run_bytes]);

        // Advance the odometer over the dimensions between the first and the last.
        let mut d = last;
        let mut carried = true;
        while d > 1 {
            d -= 1;
            counters[d] += 1;
            if counters[d] < extents[d] {
                carried = false;
                break;
            }
            counters[d] = 0;
        }
        if carried {
            return Ok(());
        }
    }
}

/// Inflate a zlib stream, refusing to produce more than `expected` bytes.
fn inflate(data: &[u8], expected: u64) -> H5Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.min(1 << 20) as usize);
    // Bounded by the chunk's own shape: it says exactly how many bytes must come out, so anything
    // past that is a claim the reader never has to allocate for.
    let read = flate2::read::ZlibDecoder::new(data)
        .take(expected)
        .read_to_end(&mut out)
        .map_err(|e| parse_error(format!("a deflate-compressed chunk failed to inflate: {e}")))?;
    if read as u64 != expected {
        return Err(parse_error(format!(
            "a deflate-compressed chunk inflated to {read} bytes where its shape needs {expected}"
        )));
    }
    Ok(out)
}

/// Undo the shuffle filter, which regroups a chunk's bytes by their position within an element.
fn unshuffle(data: &[u8], stride: usize) -> Vec<u8> {
    let elements = data.len() / stride;
    let mut out = vec![0u8; data.len()];
    let mut src = 0usize;
    for byte in 0..stride {
        for element in 0..elements {
            out[element * stride + byte] = data[src];
            src += 1;
        }
    }
    // The tail the shuffle filter leaves unpermuted (a chunk whose size is not a whole number of
    // elements) is copied through as it stands.
    out[src..].copy_from_slice(&data[src..]);
    out
}

/// The Fletcher-32 checksum HDF5 stores alongside a filtered chunk.
///
/// Written to match the library's own `H5_checksum_fletcher32` rather than the textbook algorithm:
/// it reads each pair of bytes as a **big-endian** 16-bit word, and reduces the running sums every
/// 360 words instead of taking a modulus per word. Either difference yields a different number, and
/// a checksum that disagrees with the writer's would report every sound chunk as corrupt.
fn fletcher32(data: &[u8]) -> u32 {
    let mut sum1: u32 = 0;
    let mut sum2: u32 = 0;
    let words = data.len() / 2;
    let mut i = 0usize;
    let mut left = words;
    while left > 0 {
        let batch = left.min(360);
        left -= batch;
        for _ in 0..batch {
            sum1 = sum1.wrapping_add(u16::from_be_bytes([data[i], data[i + 1]]) as u32);
            sum2 = sum2.wrapping_add(sum1);
            i += 2;
        }
        sum1 = (sum1 & 0xffff) + (sum1 >> 16);
        sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    }
    // An odd trailing byte is the high half of a word.
    if data.len() % 2 == 1 {
        sum1 = sum1.wrapping_add((data[data.len() - 1] as u32) << 8);
        sum2 = sum2.wrapping_add(sum1);
        sum1 = (sum1 & 0xffff) + (sum1 >> 16);
        sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    }
    sum1 = (sum1 & 0xffff) + (sum1 >> 16);
    sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    (sum2 << 16) | sum1
}
