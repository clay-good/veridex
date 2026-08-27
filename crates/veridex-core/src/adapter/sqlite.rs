//! A minimal, read-only, bounds-checked reader for the SQLite file format — enough to scan the two
//! tables a ROS 2 rosbag2 `.db3` keeps its recording in, and nothing else.
//!
//! **Why hand-written.** Every other binary format in this crate that Veridex must survive being
//! lied to by (HDF5, Zarr, MDF4, TFRecord) is parsed here rather than handed to a library, for one
//! reason: a `.db3` is an untrusted file, and this crate's ingest budgets have to bound what it can
//! make the process allocate. A general-purpose SQLite engine reads a file to serve queries, not to
//! defend against it — it will happily follow a page chain a corrupt header points into, and its
//! allocations are not ours to charge. So this reader does three things a general engine does not:
//! it refuses a page number outside the file, it refuses a b-tree or overflow chain that revisits a
//! page (a cycle a fuzzer writes in one byte), and it lets the caller cap the payload it will
//! assemble before the bytes are copied.
//!
//! **Scope.** Table b-trees only, by rowid, read in one forward pass: `open`, find a table's root
//! page from the schema table, walk it, hand each row's decoded column values to a visitor. No
//! queries, no indexes, no writes, no journal or WAL recovery — a bag being written to is read as
//! the committed pages say, and anything this reader does not understand is refused by name rather
//! than guessed at.
//!
//! **Reference.** <https://www.sqlite.org/fileformat2.html>. The constants below cite the section
//! they come from; they are not tunable.

use std::collections::BTreeSet;

/// The 16-byte magic every SQLite database begins with.
const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// The smallest and largest page sizes the format allows (§1.3).
const MIN_PAGE_SIZE: usize = 512;
const MAX_PAGE_SIZE: usize = 65536;

/// B-tree page types (§1.6).
const PAGE_INTERIOR_TABLE: u8 = 0x05;
const PAGE_LEAF_TABLE: u8 = 0x0d;
const PAGE_INTERIOR_INDEX: u8 = 0x02;
const PAGE_LEAF_INDEX: u8 = 0x0a;

/// The root page of the schema table, which lists every other table and its root page (§1.2).
const SCHEMA_ROOT_PAGE: u32 = 1;

/// What went wrong reading the file. The adapter turns this into a parse error naming the format;
/// no variant is recoverable, because a database that disagrees with its own header is not a
/// database Veridex will read half of.
#[derive(Debug)]
pub(crate) struct SqliteError(pub String);

impl std::fmt::Display for SqliteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err<T>(msg: impl Into<String>) -> Result<T, SqliteError> {
    Err(SqliteError(msg.into()))
}

/// One column value, in the subset of SQLite's storage classes a rosbag2 row uses.
///
/// `Real` and `Null` are carried rather than dropped so a column whose type is not what the caller
/// expected is visibly the wrong type instead of silently missing.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    /// The value as an integer, or `None` if it is not stored as one.
    pub(crate) fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The value as text, or `None` if it is not stored as text.
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// The value's bytes if it is a blob, or `None`.
    pub(crate) fn as_blob(&self) -> Option<&[u8]> {
        match self {
            Value::Blob(b) => Some(b.as_slice()),
            _ => None,
        }
    }
}

/// A read-only view over a SQLite database held in memory.
pub(crate) struct SqliteDb<'a> {
    bytes: &'a [u8],
    page_size: usize,
    /// Page size minus the reserved trailer each page ends with (§1.3). Every payload length in the
    /// format is measured against this, not the page size.
    usable: usize,
    /// Pages actually present in the byte slice. The header also states a page count; the smaller of
    /// the two governs, so a header claiming a larger file cannot walk off the end.
    page_count: u32,
}

/// What a table scan hands each row to: the cell's rowid and its decoded column values.
///
/// Returning an error stops the scan and propagates out of [`SqliteDb::scan_table`], which is how a
/// caller enforces its own budgets partway through a file rather than after it is fully read.
pub(crate) type RowVisitor<'v> = dyn FnMut(i64, &[Value]) -> Result<(), SqliteError> + 'v;

/// A ceiling on how deep a table b-tree may be walked.
///
/// A real b-tree's depth is logarithmic in its row count: SQLite's own maximum, at the largest
/// database the format allows, is under 20. This bounds the *recursion*, which the visited-page set
/// does not — an acyclic chain of interior pages each pointing at one more is a legal shape for this
/// walk and an unbounded stack for the process.
const MAX_BTREE_DEPTH: u32 = 64;

/// A ceiling on the columns one record may declare.
///
/// SQLite's own compile-time maximum is 2000. Without this, the record header's length alone bounds
/// the count — and a 64 MiB row could declare 64 million one-byte serial types, which is half a
/// gigabyte of `i64` before a single column body is read.
const MAX_RECORD_COLUMNS: usize = 2000;

/// A ceiling on the bytes one row's payload may assemble to, so an overflow chain cannot be walked
/// into unbounded memory before the caller ever sees the row.
///
/// A rosbag2 message is a serialized ROS message: a camera frame is single-digit megabytes, a dense
/// LiDAR sweep a few more. 64 MiB is two orders of magnitude above that, and still a bound.
const MAX_ROW_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

impl<'a> SqliteDb<'a> {
    /// Open a database from its bytes, validating the header.
    ///
    /// Rejects a file that is not SQLite, a page size the format does not allow, a reserved-space
    /// byte that leaves no usable page, and a text encoding other than UTF-8 — the last by name,
    /// because reading UTF-16 text as UTF-8 would put mojibake into stream names and provenance
    /// rather than admitting the file was not understood.
    pub(crate) fn open(bytes: &'a [u8]) -> Result<Self, SqliteError> {
        if bytes.len() < 100 {
            return err("file is shorter than a SQLite header");
        }
        if &bytes[0..16] != MAGIC {
            return err("not a SQLite database (bad magic)");
        }
        let page_size = match u16::from_be_bytes([bytes[16], bytes[17]]) {
            // §1.3: the value 1 encodes a 65536-byte page, which does not fit in the u16.
            1 => MAX_PAGE_SIZE,
            n => n as usize,
        };
        if page_size < MIN_PAGE_SIZE || !page_size.is_power_of_two() {
            return err(format!("invalid page size {page_size}"));
        }
        let reserved = bytes[20] as usize;
        if reserved >= page_size || page_size - reserved < MIN_PAGE_SIZE {
            return err(format!(
                "page size {page_size} with {reserved} reserved bytes leaves no usable page"
            ));
        }
        match u32::from_be_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]) {
            // 0 means "not yet determined", which SQLite treats as UTF-8.
            0 | 1 => {}
            other => {
                return err(format!(
                    "text encoding {other} is not UTF-8; Veridex will not transcode a database it \
                     may misread"
                ))
            }
        }
        let pages_on_disk = (bytes.len() / page_size) as u32;
        let declared = u32::from_be_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);
        // The smaller wins. A truncated file whose header still claims the original page count is
        // the ordinary shape of an interrupted recording, and it is read as far as it goes.
        let page_count = if declared == 0 {
            pages_on_disk
        } else {
            declared.min(pages_on_disk)
        };
        if page_count == 0 {
            return err("database contains no complete pages");
        }
        Ok(SqliteDb {
            bytes,
            page_size,
            usable: page_size - reserved,
            page_count,
        })
    }

    /// The bytes of page `n` (1-indexed), or an error naming the page that is out of range.
    fn page(&self, n: u32) -> Result<&'a [u8], SqliteError> {
        if n == 0 || n > self.page_count {
            return err(format!(
                "page {n} is outside the database (it has {} pages)",
                self.page_count
            ));
        }
        let start = (n as usize - 1) * self.page_size;
        let end = start + self.page_size;
        self.bytes
            .get(start..end)
            .ok_or_else(|| SqliteError(format!("page {n} is truncated")))
    }

    /// The root page and `CREATE TABLE` statement of the table named `name`, or `None` if the schema
    /// has no such table.
    ///
    /// Reads the schema table (§2.6), whose rows are `(type, name, tbl_name, rootpage, sql)`. The
    /// statement is returned alongside the root page because a caller that binds columns by position
    /// silently reads the wrong column when the writer adds one; the statement is what lets it bind
    /// by name instead.
    pub(crate) fn table_def(&self, name: &str) -> Result<Option<(u32, String)>, SqliteError> {
        let mut found = None;
        self.scan_table(SCHEMA_ROOT_PAGE, &mut |_rowid, cols| {
            let is_table = cols.first().and_then(Value::as_text) == Some("table");
            let matches = cols.get(1).and_then(Value::as_text) == Some(name);
            if is_table && matches {
                // A rootpage of 0 marks a view or virtual table, which has no b-tree to walk.
                let root = cols
                    .get(3)
                    .and_then(Value::as_int)
                    .filter(|r| *r > 0)
                    .and_then(|r| u32::try_from(r).ok());
                let sql = cols.get(4).and_then(Value::as_text).unwrap_or("");
                if let Some(root) = root {
                    found = Some((root, sql.to_string()));
                }
            }
            Ok(())
        })?;
        Ok(found)
    }

    /// Walk the table b-tree rooted at `root` in rowid order, handing each row to `visit`.
    ///
    /// The visitor owns the flow: returning an error stops the scan and propagates, which is how the
    /// adapter charges its ingest budgets per row and refuses a bag partway rather than after it has
    /// been fully materialized.
    ///
    /// Interior pages are descended depth-first left to right, which for a table b-tree is rowid
    /// order. A page reached twice ends the scan with an error: a cycle is one edited byte away in
    /// any of these pointers, and following it is an infinite loop, not a partial read. So does a
    /// tree deeper than [`MAX_BTREE_DEPTH`], because the visited-page set alone does not bound this
    /// walk's *stack*: a file whose every interior page points at one more interior page is acyclic
    /// and recurses once per page, which a 10 MB file turns into twenty thousand frames.
    pub(crate) fn scan_table(
        &self,
        root: u32,
        visit: &mut RowVisitor<'_>,
    ) -> Result<(), SqliteError> {
        let mut seen = BTreeSet::new();
        self.walk(root, 0, &mut seen, visit)
    }

    fn walk(
        &self,
        page_no: u32,
        depth: u32,
        seen: &mut BTreeSet<u32>,
        visit: &mut RowVisitor<'_>,
    ) -> Result<(), SqliteError> {
        if depth > MAX_BTREE_DEPTH {
            return err(format!(
                "the b-tree is deeper than {MAX_BTREE_DEPTH} levels, which no SQLite database is — \
                 refusing to descend further"
            ));
        }
        if !seen.insert(page_no) {
            return err(format!(
                "page {page_no} is reachable twice — the b-tree has a cycle"
            ));
        }
        let page = self.page(page_no)?;
        // Page 1 carries the 100-byte file header before its b-tree header (§1.2).
        let header_at = if page_no == SCHEMA_ROOT_PAGE { 100 } else { 0 };
        let kind = *page
            .get(header_at)
            .ok_or_else(|| SqliteError(format!("page {page_no} has no b-tree header")))?;
        match kind {
            PAGE_LEAF_TABLE | PAGE_INTERIOR_TABLE => {}
            PAGE_INTERIOR_INDEX | PAGE_LEAF_INDEX => {
                return err(format!(
                    "page {page_no} is an index b-tree; this reader scans tables only"
                ))
            }
            other => {
                return err(format!(
                    "page {page_no} has b-tree type 0x{other:02x}, which is not a b-tree page"
                ))
            }
        }
        let interior = kind == PAGE_INTERIOR_TABLE;
        let ncells = u16::from_be_bytes([page[header_at + 3], page[header_at + 4]]) as usize;
        let cell_ptr_at = header_at + if interior { 12 } else { 8 };
        // Each cell pointer is 2 bytes and must lie inside the page.
        cell_ptr_at
            .checked_add(ncells * 2)
            .filter(|end| *end <= self.usable)
            .ok_or_else(|| {
                SqliteError(format!(
                    "page {page_no} declares {ncells} cells, more than the page can hold"
                ))
            })?;
        let mut offsets = Vec::with_capacity(ncells);
        for i in 0..ncells {
            let at = cell_ptr_at + i * 2;
            offsets.push(u16::from_be_bytes([page[at], page[at + 1]]) as usize);
        }

        if interior {
            for off in offsets {
                let cell = page.get(off..self.usable).ok_or_else(|| {
                    SqliteError(format!("cell at {off} is outside page {page_no}"))
                })?;
                let child = u32::from_be_bytes(
                    cell.get(0..4)
                        .ok_or_else(|| {
                            SqliteError(format!("truncated interior cell in page {page_no}"))
                        })?
                        .try_into()
                        .expect("a 4-byte slice"),
                );
                self.walk(child, depth + 1, seen, visit)?;
            }
            // The right-most pointer holds the keys past the last cell (§1.6).
            let right = u32::from_be_bytes([
                page[header_at + 8],
                page[header_at + 9],
                page[header_at + 10],
                page[header_at + 11],
            ]);
            self.walk(right, depth + 1, seen, visit)?;
            return Ok(());
        }

        for off in offsets {
            let cell = page
                .get(off..self.usable)
                .ok_or_else(|| SqliteError(format!("cell at {off} is outside page {page_no}")))?;
            let (payload_len, n1) = varint(cell, 0)?;
            let (rowid, n2) = varint(cell, n1)?;
            let payload_len = usize::try_from(payload_len)
                .ok()
                .filter(|n| *n <= MAX_ROW_PAYLOAD_BYTES)
                .ok_or_else(|| {
                    SqliteError(format!(
                        "a row declares a {payload_len}-byte payload, over the \
                         {MAX_ROW_PAYLOAD_BYTES}-byte ceiling this reader will assemble"
                    ))
                })?;
            let payload = self.assemble_payload(&cell[n1 + n2..], payload_len, seen)?;
            let cols = decode_record(&payload)?;
            visit(rowid, &cols)?;
        }
        Ok(())
    }

    /// Gather a cell's payload, following the overflow chain when the row does not fit in its page.
    ///
    /// The split point is the format's, not a choice (§1.6, "Table B-Tree Leaf Cell"): a payload
    /// larger than `U-35` keeps a computed prefix on the page and puts the rest on a chain of
    /// overflow pages, each of which begins with the number of the next.
    fn assemble_payload(
        &self,
        after_header: &[u8],
        payload_len: usize,
        seen: &mut BTreeSet<u32>,
    ) -> Result<Vec<u8>, SqliteError> {
        let u = self.usable;
        let max_local = u - 35;
        if payload_len <= max_local {
            return after_header
                .get(..payload_len)
                .map(<[u8]>::to_vec)
                .ok_or_else(|| SqliteError("a row's payload runs past its page".into()));
        }
        let min_local = ((u - 12) * 32 / 255) - 23;
        let k = min_local + (payload_len - min_local) % (u - 4);
        let local = if k <= max_local { k } else { min_local };
        let mut out = Vec::with_capacity(payload_len.min(1 << 20));
        out.extend_from_slice(
            after_header
                .get(..local)
                .ok_or_else(|| SqliteError("a row's local payload runs past its page".into()))?,
        );
        let mut next = u32::from_be_bytes(
            after_header
                .get(local..local + 4)
                .ok_or_else(|| {
                    SqliteError("a row has no overflow pointer where one is required".into())
                })?
                .try_into()
                .expect("a 4-byte slice"),
        );
        // Overflow pages are tracked in the same visited set as the b-tree: a chain that re-enters
        // any page already read is a cycle, and `payload_len` alone would not stop it — the row
        // declares that length, and a hostile row declares the largest one the ceiling allows.
        while out.len() < payload_len {
            if next == 0 {
                return err(format!(
                    "an overflow chain ended after {} of {payload_len} bytes",
                    out.len()
                ));
            }
            if !seen.insert(next) {
                return err(format!(
                    "overflow page {next} is reachable twice — the chain has a cycle"
                ));
            }
            let page = self.page(next)?;
            next = u32::from_be_bytes([page[0], page[1], page[2], page[3]]);
            let want = (payload_len - out.len()).min(u - 4);
            out.extend_from_slice(
                page.get(4..4 + want)
                    .ok_or_else(|| SqliteError("an overflow page is truncated".into()))?,
            );
        }
        Ok(out)
    }
}

/// Read a big-endian base-128 varint at `at`, returning the value and how many bytes it used (§4).
///
/// A varint is one to nine bytes; the ninth contributes all eight of its bits. Values are read into
/// `i64` exactly as SQLite stores them (the ninth-byte form is a two's-complement 64-bit integer).
fn varint(buf: &[u8], at: usize) -> Result<(i64, usize), SqliteError> {
    let mut value: u64 = 0;
    for i in 0..8 {
        let byte = *buf
            .get(at + i)
            .ok_or_else(|| SqliteError("a varint runs past the end of its cell".into()))?;
        if byte < 0x80 {
            return Ok(((value << 7 | byte as u64) as i64, i + 1));
        }
        value = value << 7 | (byte & 0x7f) as u64;
    }
    let last = *buf
        .get(at + 8)
        .ok_or_else(|| SqliteError("a varint runs past the end of its cell".into()))?;
    Ok((((value << 8) | last as u64) as i64, 9))
}

/// Decode a record (§2.1): a varint header length, then one serial-type varint per column, then the
/// column bodies back to back.
fn decode_record(payload: &[u8]) -> Result<Vec<Value>, SqliteError> {
    let (header_len, n) = varint(payload, 0)?;
    let header_len = usize::try_from(header_len)
        .ok()
        .filter(|h| *h >= n && *h <= payload.len())
        .ok_or_else(|| {
            SqliteError(format!(
                "record header length {header_len} is not inside the record"
            ))
        })?;
    let mut types = Vec::new();
    let mut at = n;
    while at < header_len {
        let (t, used) = varint(payload, at)?;
        if used == 0 {
            return err("a serial type consumed no bytes");
        }
        if types.len() == MAX_RECORD_COLUMNS {
            return err(format!(
                "a record declares more than {MAX_RECORD_COLUMNS} columns, which no SQLite table has"
            ));
        }
        types.push(t);
        at += used;
    }
    let mut body = header_len;
    let mut out = Vec::with_capacity(types.len());
    for t in types {
        let (value, used) = read_column(payload, body, t)?;
        out.push(value);
        body += used;
    }
    Ok(out)
}

/// Read one column body of serial type `t` starting at `at`, returning it and its byte length (§2.1).
fn read_column(payload: &[u8], at: usize, t: i64) -> Result<(Value, usize), SqliteError> {
    let take = |n: usize| -> Result<&[u8], SqliteError> {
        // `n` comes from a serial type the file writes, so the end offset is computed, not assumed:
        // a length near `usize::MAX` would otherwise wrap into a range that looks in-bounds.
        at.checked_add(n)
            .and_then(|end| payload.get(at..end))
            .ok_or_else(|| SqliteError(format!("a {n}-byte column body runs past the record")))
    };
    let int_from = |b: &[u8]| -> i64 {
        // Sign-extend a big-endian two's-complement integer of 1..8 bytes.
        let mut v: i64 = if b[0] & 0x80 != 0 { -1 } else { 0 };
        for byte in b {
            v = (v << 8) | *byte as i64;
        }
        v
    };
    Ok(match t {
        0 => (Value::Null, 0),
        1 => (Value::Int(int_from(take(1)?)), 1),
        2 => (Value::Int(int_from(take(2)?)), 2),
        3 => (Value::Int(int_from(take(3)?)), 3),
        4 => (Value::Int(int_from(take(4)?)), 4),
        5 => (Value::Int(int_from(take(6)?)), 6),
        6 => (Value::Int(int_from(take(8)?)), 8),
        7 => (
            Value::Real(f64::from_be_bytes(
                take(8)?.try_into().expect("an 8-byte slice"),
            )),
            8,
        ),
        // 8 and 9 are the constants 0 and 1, stored entirely in the type (§2.1).
        8 => (Value::Int(0), 0),
        9 => (Value::Int(1), 0),
        10 | 11 => return err(format!("serial type {t} is reserved for internal use")),
        t if t >= 12 => {
            let len = usize::try_from((t - 12) / 2)
                .map_err(|_| SqliteError(format!("serial type {t} declares an unusable length")))?;
            let bytes = take(len)?;
            if t % 2 == 0 {
                (Value::Blob(bytes.to_vec()), len)
            } else {
                // Text is UTF-8 (the header's encoding was checked at open). A column that is not
                // valid UTF-8 is a fault in the file, not something to paper over with replacement
                // characters that would then be reported as the topic's name.
                let s = std::str::from_utf8(bytes)
                    .map_err(|_| SqliteError("a text column is not valid UTF-8".into()))?;
                (Value::Text(s.to_string()), len)
            }
        }
        t => return err(format!("serial type {t} is not a valid serial type")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_varint_reads_one_and_nine_byte_forms() {
        assert_eq!(varint(&[0x00], 0).unwrap(), (0, 1));
        assert_eq!(varint(&[0x7f], 0).unwrap(), (127, 1));
        assert_eq!(varint(&[0x81, 0x00], 0).unwrap(), (128, 2));
        // The nine-byte form carries all eight bits of its last byte.
        let all_ones = [0xff; 9];
        assert_eq!(varint(&all_ones, 0).unwrap(), (-1, 9));
    }

    #[test]
    fn a_truncated_varint_is_an_error_not_a_zero() {
        assert!(varint(&[0x81], 0).is_err());
    }

    #[test]
    fn a_short_or_unmagical_file_is_refused() {
        assert!(SqliteDb::open(&[]).is_err());
        assert!(SqliteDb::open(&[0u8; 200]).is_err());
    }

    #[test]
    fn integer_columns_sign_extend() {
        // -1 stored as a single byte.
        let (v, n) = read_column(&[0xff], 0, 1).unwrap();
        assert_eq!((v, n), (Value::Int(-1), 1));
        // The type-only constants consume no body.
        assert_eq!(read_column(&[], 0, 8).unwrap(), (Value::Int(0), 0));
        assert_eq!(read_column(&[], 0, 9).unwrap(), (Value::Int(1), 0));
    }

    /// A database of `pages` pages, where page 1 and every page after it is an empty interior table
    /// page whose right-most pointer is the page named by `right[i]`. Enough of a file for the
    /// b-tree walk to descend it, which is all these two tests are about.
    fn chain_of_interior_pages(right: &[u32]) -> Vec<u8> {
        const PAGE: usize = 512;
        let pages = right.len();
        let mut bytes = vec![0u8; PAGE * pages];
        bytes[0..16].copy_from_slice(MAGIC);
        bytes[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
        bytes[20] = 0; // no reserved space
        bytes[28..32].copy_from_slice(&(pages as u32).to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes()); // UTF-8
        for (i, next) in right.iter().enumerate() {
            // Page 1 keeps the 100-byte file header ahead of its b-tree header.
            let at = i * PAGE + if i == 0 { 100 } else { 0 };
            bytes[at] = PAGE_INTERIOR_TABLE;
            bytes[at + 3..at + 5].copy_from_slice(&0u16.to_be_bytes()); // no cells
            bytes[at + 8..at + 12].copy_from_slice(&next.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn a_btree_that_points_at_itself_is_refused_rather_than_followed_forever() {
        // Page 1 -> 2 -> 1. Acyclic-looking one edge at a time; an infinite descent in practice.
        let bytes = chain_of_interior_pages(&[2, 1]);
        let db = SqliteDb::open(&bytes).expect("a readable header");
        let err = db
            .scan_table(1, &mut |_, _| Ok(()))
            .expect_err("a cycle is refused");
        assert!(err.0.contains("cycle"), "{}", err.0);
    }

    #[test]
    fn a_btree_deeper_than_any_real_one_is_refused_before_the_stack_runs_out() {
        // A straight chain: 1 -> 2 -> 3 -> … Every page is visited once, so the cycle guard never
        // fires; only the depth bound stands between this and one stack frame per page.
        let right: Vec<u32> = (2..=(MAX_BTREE_DEPTH + 8)).collect();
        let mut right = right;
        right.push(1_000_000); // the last one points off the end, if we ever got that far
        let bytes = chain_of_interior_pages(&right);
        let db = SqliteDb::open(&bytes).expect("a readable header");
        let err = db
            .scan_table(1, &mut |_, _| Ok(()))
            .expect_err("an over-deep tree is refused");
        assert!(err.0.contains("deeper than"), "{}", err.0);
    }

    #[test]
    fn a_record_declaring_absurdly_many_columns_is_refused() {
        // A header claiming one NULL column per byte. Without the cap, the column-type vector alone
        // is eight bytes per declared column.
        let count = MAX_RECORD_COLUMNS + 10;
        let mut payload = vec![0u8; count + 2];
        // A two-byte varint header length covering itself plus `count` one-byte serial types.
        let header_len = count + 2;
        payload[0] = 0x80 | ((header_len >> 7) as u8);
        payload[1] = (header_len & 0x7f) as u8;
        let err = decode_record(&payload).expect_err("the column cap holds");
        assert!(err.0.contains("columns"), "{}", err.0);
    }

    #[test]
    fn a_reserved_serial_type_is_refused() {
        assert!(read_column(&[0u8; 8], 0, 10).is_err());
        assert!(read_column(&[0u8; 8], 0, 11).is_err());
    }
}
