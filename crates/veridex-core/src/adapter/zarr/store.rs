//! Reading a Zarr v2 store: the array metadata, the chunk files, and the codecs that wrap them.
//!
//! Zarr keeps everything in plain files, which makes this far less work than HDF5: a group is a
//! directory holding `.zgroup`, an array is a directory holding a `.zarray` JSON descriptor plus one
//! file per chunk, named by its chunk coordinates (`0.0`, `1.0`, …). Attributes live in `.zattrs`.
//! There is no index to walk and no address to follow — a chunk's name *is* its position.
//!
//! What this module does not do is guess. A `.zarray` describing an order, dtype, filter, or
//! compressor it cannot decode is an error naming what it found, because a Zarr array read through
//! the wrong codec is not an error anyone downstream can detect: it is plausible-looking numbers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::adapter::chunks::{copy_chunk_into_row, RowCopy};
use crate::adapter::{DecompressionBudget, IngestError};

use super::FORMAT_ID;

pub(crate) fn parse_error(message: impl Into<String>) -> IngestError {
    IngestError::Parse {
        format_id: FORMAT_ID,
        message: message.into(),
    }
}

/// The file that marks a directory as a Zarr group.
pub(crate) const GROUP_FILE: &str = ".zgroup";
/// The file that marks a directory as a Zarr array, and describes it.
pub(crate) const ARRAY_FILE: &str = ".zarray";
/// The file holding a group's or an array's attributes.
pub(crate) const ATTRS_FILE: &str = ".zattrs";
/// The metadata file a Zarr **v3** store writes instead of `.zgroup` / `.zarray`.
pub(crate) const V3_FILE: &str = "zarr.json";

/// The `.zarray` descriptor, as written.
#[derive(Debug, Deserialize)]
struct ZarrayJson {
    zarr_format: Option<u32>,
    shape: Vec<u64>,
    chunks: Vec<u64>,
    dtype: serde_json::Value,
    #[serde(default)]
    order: Option<String>,
    #[serde(default)]
    compressor: Option<Codec>,
    #[serde(default)]
    filters: Option<Vec<Codec>>,
    #[serde(default)]
    dimension_separator: Option<String>,
    #[serde(default)]
    fill_value: Option<serde_json::Value>,
}

/// A codec entry from `.zarray` (`compressor` or one of `filters`).
#[derive(Debug, Clone, Deserialize)]
struct Codec {
    id: String,
    #[serde(default)]
    cname: Option<String>,
    #[serde(default)]
    shuffle: Option<i64>,
}

/// An element type Veridex can size and, where numeric, decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dtype {
    /// The canonical name carried into the CDM (`float32`, `uint8`, `int64`, …).
    pub name: String,
    /// Bytes per element.
    pub size: u32,
    pub little_endian: bool,
    pub numeric: bool,
    pub signed: bool,
    pub float: bool,
}

impl Dtype {
    /// Decode one element as an `f64`, or `None` for a type this reader does not decode.
    pub fn decode(&self, bytes: &[u8]) -> Option<f64> {
        let size = self.size as usize;
        if !self.numeric || size > 8 || bytes.len() < size {
            return None;
        }
        let mut buf = [0u8; 8];
        if self.little_endian {
            buf[..size].copy_from_slice(&bytes[..size]);
        } else {
            for (i, b) in bytes[..size].iter().rev().enumerate() {
                buf[i] = *b;
            }
        }
        match (self.float, size) {
            (true, 4) => Some(f32::from_le_bytes(buf[..4].try_into().ok()?) as f64),
            (true, 8) => Some(f64::from_le_bytes(buf)),
            (true, _) => None,
            (false, n) if self.signed => {
                let raw = u64::from_le_bytes(buf);
                let shift = 64 - n * 8;
                Some(((raw << shift) as i64 >> shift) as f64)
            }
            (false, _) => Some(u64::from_le_bytes(buf) as f64),
        }
    }
}

/// A Zarr dtype string: a byte-order character, a kind character, and a size in bytes.
///
/// `<f4` is little-endian float32, `|u1` is a single byte (order-agnostic), `>i8` is big-endian
/// int64. A structured dtype (a list of fields) is not one value per element and is refused.
fn parse_dtype(value: &serde_json::Value) -> Result<Dtype, IngestError> {
    let text = value.as_str().ok_or_else(|| {
        parse_error(
            "this array declares a structured dtype (a list of fields), which is not one value per \
             element; no stream is built from it",
        )
    })?;
    let mut chars = text.chars();
    let order = chars.next().ok_or_else(|| parse_error("empty dtype"))?;
    let little_endian = match order {
        '<' | '|' => true,
        '>' => false,
        other => {
            return Err(parse_error(format!(
                "dtype `{text}` starts with byte-order character `{other}`, which is not one of \
                 `<`, `>`, `|`"
            )))
        }
    };
    let kind = chars
        .next()
        .ok_or_else(|| parse_error(format!("dtype `{text}` declares no kind")))?;
    let size: u32 = chars
        .as_str()
        .parse()
        .map_err(|_| parse_error(format!("dtype `{text}` declares no element size")))?;
    if size == 0 {
        return Err(parse_error(format!("dtype `{text}` declares a zero size")));
    }
    let (name, numeric, signed, float) = match kind {
        'f' => (format!("float{}", size * 8), true, true, true),
        'i' => (format!("int{}", size * 8), true, true, false),
        'u' => (format!("uint{}", size * 8), true, false, false),
        'b' => ("bool".to_string(), true, false, false),
        'S' => (format!("bytes[{size}]"), false, false, false),
        'U' => (format!("string[{}]", size / 4), false, false, false),
        other => (format!("opaque[{other}{size}]"), false, false, false),
    };
    Ok(Dtype {
        name,
        size,
        little_endian,
        numeric,
        signed,
        float,
    })
}

/// One element of an array's fill value, as the bytes a chunk would have held.
///
/// `null` (and an absent field) means the store declares no fill value, which reads as zero bytes.
/// A float fill may be the JSON strings `"NaN"`, `"Infinity"`, or `"-Infinity"`, which is how Zarr
/// writes values JSON has no literal for — and `"NaN"` is what an unwritten float array gets by
/// default, so it is the case that matters most.
fn fill_element(dtype: &Dtype, value: Option<&serde_json::Value>) -> Result<Vec<u8>, IngestError> {
    let size = dtype.size as usize;
    let zeros = vec![0u8; size];
    let Some(value) = value else {
        return Ok(zeros);
    };
    let number = match value {
        serde_json::Value::Null => return Ok(zeros),
        serde_json::Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => match s.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            // A base64 fill value belongs to a dtype this reader does not decode anyway; the bytes
            // it would produce are not something to guess at.
            _ => None,
        },
        _ => None,
    };
    let Some(number) = number.filter(|_| dtype.numeric) else {
        // Not a numeric fill this reader can encode. Zero bytes are what an unwritten chunk of a
        // non-numeric array reads as here, and the array's values are not decoded either way.
        return Ok(zeros);
    };
    let bytes = match (dtype.float, size) {
        (true, 4) => (number as f32).to_le_bytes().to_vec(),
        (true, 8) => number.to_le_bytes().to_vec(),
        (true, _) => return Ok(zeros),
        (false, _) if dtype.signed => (number as i64).to_le_bytes()[..size.min(8)].to_vec(),
        (false, _) => (number as u64).to_le_bytes()[..size.min(8)].to_vec(),
    };
    let mut bytes = bytes;
    bytes.resize(size, 0);
    if !dtype.little_endian {
        bytes.reverse();
    }
    Ok(bytes)
}

/// Compressors this reader applies. Anything else is refused by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Compressor {
    None,
    Zlib,
    Gzip,
    Zstd,
    Lz4,
    /// Blosc, a container: it holds one of several codecs plus an optional byte shuffle.
    Blosc,
}

fn parse_compressor(codec: Option<&Codec>) -> Result<Compressor, IngestError> {
    let Some(codec) = codec else {
        return Ok(Compressor::None);
    };
    match codec.id.as_str() {
        "zlib" => Ok(Compressor::Zlib),
        "gzip" => Ok(Compressor::Gzip),
        "zstd" => Ok(Compressor::Zstd),
        "lz4" => Ok(Compressor::Lz4),
        "blosc" => {
            // Blosc's own header states its codec and shuffle, so `cname` here is only checked for
            // the one case this reader cannot decode at all — refused now rather than after a chunk
            // has been read and mis-decoded.
            if let Some(cname) = codec.cname.as_deref() {
                if cname.eq_ignore_ascii_case("blosclz") || cname.eq_ignore_ascii_case("snappy") {
                    return Err(parse_error(format!(
                        "this array is blosc-compressed with `{cname}`, which this reader does not \
                         implement — re-save it with `cname='lz4'`, `'zstd'`, or `'zlib'`"
                    )));
                }
            }
            if codec.shuffle == Some(2) {
                return Err(parse_error(
                    "this array is blosc-compressed with the bit shuffle, which this reader does \
                     not implement — re-save it with the byte shuffle (`shuffle=1`) or none",
                ));
            }
            Ok(Compressor::Blosc)
        }
        other => Err(parse_error(format!(
            "compressor `{other}` is not one this reader implements (none, zlib, gzip, zstd, lz4, \
             blosc)"
        ))),
    }
}

/// One Zarr array: its shape, its type, and where its chunks live.
#[derive(Debug, Clone)]
pub(crate) struct ZarrArray {
    /// The array's directory.
    pub dir: PathBuf,
    pub shape: Vec<u64>,
    pub chunks: Vec<u64>,
    pub dtype: Dtype,
    pub compressor: Compressor,
    /// What separates chunk coordinates in a chunk's filename (`.` by default, `/` for a nested
    /// store).
    pub separator: String,
    /// One element's worth of the array's fill value, as stored bytes. A chunk that was never written
    /// is not in the store at all and reads as this repeated — which is emphatically not zero: Zarr
    /// writes `"NaN"` for an unwritten float array by default, and reading those rows as zeros would
    /// turn missing data into plausible data.
    pub fill_element: Vec<u8>,
}

impl ZarrArray {
    /// Read and validate an array's `.zarray`.
    pub fn open(dir: &Path) -> Result<ZarrArray, IngestError> {
        let bytes =
            std::fs::read(dir.join(ARRAY_FILE)).map_err(|e| IngestError::Io(e.to_string()))?;
        let meta: ZarrayJson = serde_json::from_slice(&bytes)
            .map_err(|e| parse_error(format!("{}: {e}", dir.join(ARRAY_FILE).display())))?;
        if let Some(format) = meta.zarr_format {
            if format != 2 {
                return Err(IngestError::UnsupportedVersion {
                    format_id: FORMAT_ID,
                    version: Some(format.to_string()),
                    supported: super::SUPPORTED_VERSIONS,
                });
            }
        }
        if meta.shape.len() != meta.chunks.len() {
            return Err(parse_error(format!(
                "an array declares a {}-dimensional shape and a {}-dimensional chunk grid",
                meta.shape.len(),
                meta.chunks.len()
            )));
        }
        if meta.shape.is_empty() {
            return Err(parse_error(
                "a zero-dimensional array has no first dimension, so it carries no timeline",
            ));
        }
        if meta.chunks.contains(&0) {
            return Err(parse_error(
                "an array declares a zero-length chunk dimension",
            ));
        }
        // Fortran order would need every row transposed on the way out. Refused rather than read as
        // if it were C-order, which would silently permute every value.
        match meta.order.as_deref() {
            None | Some("C") => {}
            Some(other) => {
                return Err(parse_error(format!(
                    "this array is stored in `{other}` order; this reader reads C order only"
                )))
            }
        }
        if let Some(filters) = &meta.filters {
            if let Some(first) = filters.first() {
                return Err(parse_error(format!(
                    "this array declares the filter `{}`, which this reader does not apply; its \
                     values cannot be read",
                    first.id
                )));
            }
        }
        let separator = match meta.dimension_separator.as_deref() {
            None => ".".to_string(),
            Some(sep @ ("." | "/")) => sep.to_string(),
            Some(other) => {
                return Err(parse_error(format!(
                    "this array names its chunks with the separator `{other}`, which is not `.` or \
                     `/`"
                )))
            }
        };
        let dtype = parse_dtype(&meta.dtype)?;
        let fill_element = fill_element(&dtype, meta.fill_value.as_ref())?;
        Ok(ZarrArray {
            dir: dir.to_path_buf(),
            fill_element,
            dtype,
            compressor: parse_compressor(meta.compressor.as_ref())?,
            shape: meta.shape,
            chunks: meta.chunks,
            separator,
        })
    }

    /// Bytes in one row (one index along the first dimension). `None` on overflow.
    pub fn row_bytes(&self) -> Option<u64> {
        let mut n = self.dtype.size as u64;
        for dim in self.shape.iter().skip(1) {
            n = n.checked_mul(*dim)?;
        }
        Some(n)
    }

    /// Values in one row: the product of every dimension after the first.
    pub fn row_elements(&self) -> u64 {
        self.shape
            .iter()
            .skip(1)
            .try_fold(1u64, |acc, dim| acc.checked_mul(*dim))
            .unwrap_or(u64::MAX)
    }

    /// The path of the chunk at `coords`.
    fn chunk_path(&self, coords: &[u64]) -> PathBuf {
        let name = coords
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(&self.separator);
        self.dir.join(name)
    }
}

/// Reads an array's rows, decoding chunks on demand.
pub(crate) struct RowReader<'a> {
    array: &'a ZarrArray,
    budget: &'a mut DecompressionBudget,
    /// The most recently decoded chunk, so a sequential row walk decodes each chunk once.
    cached: Option<(Vec<u64>, Vec<u8>)>,
    row_bytes: u64,
    /// Bytes one whole chunk holds when decoded.
    chunk_bytes: u64,
}

impl<'a> RowReader<'a> {
    pub fn new(
        array: &'a ZarrArray,
        budget: &'a mut DecompressionBudget,
    ) -> Result<RowReader<'a>, IngestError> {
        let row_bytes = array
            .row_bytes()
            .ok_or_else(|| parse_error("an array's declared shape overflows a byte count"))?;
        let mut chunk_bytes = array.dtype.size as u64;
        for dim in &array.chunks {
            chunk_bytes = chunk_bytes
                .checked_mul(*dim)
                .ok_or_else(|| parse_error("an array's chunk shape overflows a byte count"))?;
        }
        Ok(RowReader {
            array,
            budget,
            cached: None,
            row_bytes,
            chunk_bytes,
        })
    }

    pub fn row_bytes(&self) -> u64 {
        self.row_bytes
    }

    /// Row `index`, assembled from every chunk that overlaps it.
    pub fn row(&mut self, index: u64) -> Result<Vec<u8>, IngestError> {
        // Charged before the buffer exists: a row is assembled from chunks and fill value, so its
        // size comes from the metadata rather than from any file's length.
        self.budget.take(FORMAT_ID, self.row_bytes)?;
        let rank = self.array.shape.len();
        let mut row = vec![0u8; self.row_bytes as usize];
        let elem = self.array.dtype.size as u64;

        // The chunk grid position along the first dimension is fixed by the row; every other
        // dimension is visited across its whole extent.
        let first = index / self.array.chunks[0];
        let mut grid = vec![0u64; rank];
        grid[0] = first;
        loop {
            let origin: Vec<u64> = (0..rank).map(|d| grid[d] * self.array.chunks[d]).collect();
            self.load_chunk(&grid)?;
            let (_, chunk) = self.cached.as_ref().expect("just loaded");
            copy_chunk_into_row(
                &RowCopy {
                    format_id: FORMAT_ID,
                    origin: &origin,
                    chunk_dims: &self.array.chunks,
                    dims: &self.array.shape,
                    index,
                    elem,
                },
                chunk,
                &mut row,
            )?;
            // Advance over the chunk grid in every dimension but the first.
            let mut d = rank;
            let mut carried = true;
            while d > 1 {
                d -= 1;
                grid[d] += 1;
                if grid[d] * self.array.chunks[d] < self.array.shape[d] {
                    carried = false;
                    break;
                }
                grid[d] = 0;
            }
            if carried {
                break;
            }
        }
        Ok(row)
    }

    /// A whole chunk of the array's fill value — what an unwritten chunk reads as.
    fn fill_chunk(&self) -> Vec<u8> {
        let element = &self.array.fill_element;
        if element.iter().all(|b| *b == 0) {
            return vec![0u8; self.chunk_bytes as usize];
        }
        let mut out = Vec::with_capacity(self.chunk_bytes as usize);
        while out.len() + element.len() <= self.chunk_bytes as usize {
            out.extend_from_slice(element);
        }
        out.resize(self.chunk_bytes as usize, 0);
        out
    }

    /// Make `self.cached` hold the decoded bytes of the chunk at grid position `grid`.
    fn load_chunk(&mut self, grid: &[u64]) -> Result<(), IngestError> {
        if self
            .cached
            .as_ref()
            .is_some_and(|(cached, _)| cached.as_slice() == grid)
        {
            return Ok(());
        }
        let path = self.array.chunk_path(grid);
        let decoded = match std::fs::read(&path) {
            // A chunk that was never written is not in the store at all, and reads as the fill
            // value. That is Zarr's own semantics, not a guess: the array's shape says the row
            // exists, and a sparse write is ordinary.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.fill_chunk(),
            Err(e) => return Err(IngestError::Io(e.to_string())),
            Ok(raw) => {
                self.budget.take(FORMAT_ID, self.chunk_bytes)?;
                let decoded = decode_chunk(
                    &raw,
                    &self.array.compressor,
                    self.chunk_bytes,
                    self.array.dtype.size as usize,
                )?;
                if decoded.len() as u64 != self.chunk_bytes {
                    return Err(parse_error(format!(
                        "chunk {} decoded to {} bytes where its shape needs {}",
                        path.display(),
                        decoded.len(),
                        self.chunk_bytes
                    )));
                }
                decoded
            }
        };
        self.cached = Some((grid.to_vec(), decoded));
        Ok(())
    }
}

/// Decode one chunk's bytes through its compressor.
fn decode_chunk(
    raw: &[u8],
    compressor: &Compressor,
    expected: u64,
    typesize: usize,
) -> Result<Vec<u8>, IngestError> {
    match compressor {
        Compressor::None => Ok(raw.to_vec()),
        Compressor::Zlib => inflate_zlib(raw, expected),
        Compressor::Gzip => inflate_gzip(raw, expected),
        Compressor::Zstd => {
            let mut out = Vec::with_capacity(expected.min(1 << 20) as usize);
            std::io::Read::read_to_end(
                &mut std::io::Read::take(
                    zstd::stream::read::Decoder::new(raw)
                        .map_err(|e| parse_error(format!("a zstd chunk could not be read: {e}")))?,
                    expected,
                ),
                &mut out,
            )
            .map_err(|e| parse_error(format!("a zstd chunk failed to decompress: {e}")))?;
            Ok(out)
        }
        Compressor::Lz4 => decode_lz4_framed(raw, expected),
        Compressor::Blosc => decode_blosc(raw, expected, typesize),
    }
}

fn inflate_zlib(raw: &[u8], expected: u64) -> Result<Vec<u8>, IngestError> {
    let mut out = Vec::with_capacity(expected.min(1 << 20) as usize);
    std::io::Read::read_to_end(
        &mut std::io::Read::take(flate2::read::ZlibDecoder::new(raw), expected),
        &mut out,
    )
    .map_err(|e| parse_error(format!("a zlib chunk failed to inflate: {e}")))?;
    Ok(out)
}

fn inflate_gzip(raw: &[u8], expected: u64) -> Result<Vec<u8>, IngestError> {
    let mut out = Vec::with_capacity(expected.min(1 << 20) as usize);
    std::io::Read::read_to_end(
        &mut std::io::Read::take(flate2::read::GzDecoder::new(raw), expected),
        &mut out,
    )
    .map_err(|e| parse_error(format!("a gzip chunk failed to inflate: {e}")))?;
    Ok(out)
}

/// numcodecs' `lz4` codec frames a raw LZ4 block with its 4-byte little-endian decoded length.
fn decode_lz4_framed(raw: &[u8], expected: u64) -> Result<Vec<u8>, IngestError> {
    if raw.len() < 4 {
        return Err(parse_error("an lz4 chunk is too short to hold its length"));
    }
    let declared = u32::from_le_bytes(raw[..4].try_into().expect("four bytes")) as u64;
    if declared != expected {
        return Err(parse_error(format!(
            "an lz4 chunk declares {declared} decoded bytes where its shape needs {expected}"
        )));
    }
    lz4_flex::block::decompress(&raw[4..], expected as usize)
        .map_err(|e| parse_error(format!("an lz4 chunk failed to decompress: {e}")))
}

// ---- blosc ----
//
// Blosc is a container, not a codec: a 16-byte header, then per-block offsets, then blocks that were
// each compressed independently by one of several codecs and optionally byte-shuffled. Reading it is
// the price of reading real Zarr robot data, because it is what `numcodecs` reaches for by default.

const BLOSC_HEADER_LEN: usize = 16;
const BLOSC_SHUFFLE: u8 = 0x01;
const BLOSC_MEMCPYED: u8 = 0x02;
const BLOSC_BITSHUFFLE: u8 = 0x04;
const BLOSC_DONT_SPLIT: u8 = 0x10;

fn decode_blosc(raw: &[u8], expected: u64, _typesize: usize) -> Result<Vec<u8>, IngestError> {
    if raw.len() < BLOSC_HEADER_LEN {
        return Err(parse_error("a blosc chunk is shorter than its own header"));
    }
    let flags = raw[2];
    let typesize = raw[3] as usize;
    let nbytes = u32::from_le_bytes(raw[4..8].try_into().expect("four bytes")) as u64;
    let blocksize = u32::from_le_bytes(raw[8..12].try_into().expect("four bytes")) as u64;
    let cbytes = u32::from_le_bytes(raw[12..16].try_into().expect("four bytes")) as u64;
    if nbytes != expected {
        return Err(parse_error(format!(
            "a blosc chunk declares {nbytes} decoded bytes where its shape needs {expected}"
        )));
    }
    if cbytes as usize > raw.len() {
        return Err(parse_error(format!(
            "a blosc chunk declares {cbytes} compressed bytes but only {} are present",
            raw.len()
        )));
    }
    if flags & BLOSC_BITSHUFFLE != 0 {
        return Err(parse_error(
            "a blosc chunk uses the bit shuffle, which this reader does not implement — re-save the \
             array with the byte shuffle (`shuffle=1`) or none",
        ));
    }
    if flags & BLOSC_MEMCPYED != 0 {
        // Stored verbatim: blosc found it not worth compressing.
        let body = &raw[BLOSC_HEADER_LEN..];
        if (body.len() as u64) < nbytes {
            return Err(parse_error("an uncompressed blosc chunk is truncated"));
        }
        return Ok(body[..nbytes as usize].to_vec());
    }
    let codec = blosc_codec(flags >> 5)?;
    if blocksize == 0 {
        return Err(parse_error("a blosc chunk declares a zero block size"));
    }
    let nblocks = nbytes.div_ceil(blocksize) as usize;
    let offsets_end = BLOSC_HEADER_LEN + 4 * nblocks;
    if raw.len() < offsets_end {
        return Err(parse_error(
            "a blosc chunk is too short to hold its block offsets",
        ));
    }
    let mut out = Vec::with_capacity(nbytes as usize);
    for block in 0..nblocks {
        let at = BLOSC_HEADER_LEN + 4 * block;
        let offset = u32::from_le_bytes(raw[at..at + 4].try_into().expect("four bytes")) as usize;
        if offset >= raw.len() {
            return Err(parse_error(
                "a blosc block offset points past the end of the chunk",
            ));
        }
        // The last block is short where the chunk's size is not a whole number of blocks.
        let remaining = nbytes - out.len() as u64;
        let block_size = blocksize.min(remaining) as usize;
        // Blosc splits a block into one stream per byte position of the element, unless the codec or
        // the flags say not to, and a short trailing block is never split. The decoder has to make
        // exactly the same choice the compressor did, or the streams are read at the wrong offsets.
        let leftover = block_size < blocksize as usize;
        let splits = if flags & BLOSC_DONT_SPLIT == 0 && !leftover && typesize > 0 {
            typesize
        } else {
            1
        };
        let stream_size = block_size / splits;
        let mut cursor = offset;
        for split in 0..splits {
            // Every stream is prefixed with its own compressed length; a length equal to the stream
            // size means it was stored verbatim.
            if cursor + 4 > raw.len() {
                return Err(parse_error("a blosc block is truncated before its length"));
            }
            let clen = u32::from_le_bytes(raw[cursor..cursor + 4].try_into().expect("four bytes"))
                as usize;
            cursor += 4;
            let end = cursor
                .checked_add(clen)
                .filter(|e| *e <= raw.len())
                .ok_or_else(|| parse_error("a blosc stream runs past the end of the chunk"))?;
            // The final stream of a split block carries whatever the division left over.
            let want = if split + 1 == splits {
                block_size - stream_size * (splits - 1)
            } else {
                stream_size
            };
            if clen == want {
                out.extend_from_slice(&raw[cursor..end]);
            } else {
                out.extend_from_slice(&decode_blosc_stream(codec, &raw[cursor..end], want)?);
            }
            cursor = end;
        }
    }
    if out.len() as u64 != nbytes {
        return Err(parse_error(format!(
            "a blosc chunk decoded to {} bytes where its header declares {nbytes}",
            out.len()
        )));
    }
    if flags & BLOSC_SHUFFLE != 0 && typesize > 1 {
        out = unshuffle(&out, typesize);
    }
    Ok(out)
}

/// The codec a blosc header's compressor id names.
fn blosc_codec(id: u8) -> Result<BloscCodec, IngestError> {
    match id {
        1 | 2 => Ok(BloscCodec::Lz4),
        4 => Ok(BloscCodec::Zlib),
        5 => Ok(BloscCodec::Zstd),
        0 => Err(parse_error(
            "this chunk is blosc-compressed with `blosclz`, which this reader does not implement — \
             re-save the array with `cname='lz4'`, `'zstd'`, or `'zlib'`",
        )),
        3 => Err(parse_error(
            "this chunk is blosc-compressed with `snappy`, which this reader does not implement — \
             re-save the array with `cname='lz4'`, `'zstd'`, or `'zlib'`",
        )),
        other => Err(parse_error(format!(
            "this chunk names blosc compressor id {other}, which is not one this reader knows"
        ))),
    }
}

#[derive(Debug, Clone, Copy)]
enum BloscCodec {
    Lz4,
    Zlib,
    Zstd,
}

fn decode_blosc_stream(
    codec: BloscCodec,
    data: &[u8],
    want: usize,
) -> Result<Vec<u8>, IngestError> {
    match codec {
        BloscCodec::Lz4 => lz4_flex::block::decompress(data, want)
            .map_err(|e| parse_error(format!("a blosc lz4 stream failed to decompress: {e}"))),
        BloscCodec::Zlib => inflate_zlib(data, want as u64),
        BloscCodec::Zstd => {
            let mut out = Vec::with_capacity(want);
            std::io::Read::read_to_end(
                &mut std::io::Read::take(
                    zstd::stream::read::Decoder::new(data).map_err(|e| {
                        parse_error(format!("a blosc zstd stream could not be read: {e}"))
                    })?,
                    want as u64,
                ),
                &mut out,
            )
            .map_err(|e| parse_error(format!("a blosc zstd stream failed to decompress: {e}")))?;
            Ok(out)
        }
    }
}

/// Undo blosc's byte shuffle, which regroups bytes by their position within an element.
fn unshuffle(data: &[u8], typesize: usize) -> Vec<u8> {
    let elements = data.len() / typesize;
    let mut out = vec![0u8; data.len()];
    let mut src = 0usize;
    for byte in 0..typesize {
        for element in 0..elements {
            out[element * typesize + byte] = data[src];
            src += 1;
        }
    }
    // Blosc leaves a trailing partial element unshuffled.
    out[src..].copy_from_slice(&data[src..]);
    out
}

/// The attributes of a group or array (`.zattrs`), as flat key/value pairs. A nested or array value
/// is rendered as its compact JSON, which is what the source actually said.
pub(crate) fn read_attrs(dir: &Path) -> BTreeMap<String, serde_json::Value> {
    let path = dir.join(ATTRS_FILE);
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(serde_json::Value::Object(map)) => map.into_iter().collect(),
        _ => BTreeMap::new(),
    }
}
