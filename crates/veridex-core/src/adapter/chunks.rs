//! Assembling one row out of the chunks that hold it — shared by every chunked format.
//!
//! HDF5 and Zarr both store an N-dimensional array as a grid of fixed-size chunks, and both need the
//! same thing from it: the bytes of one index along the first dimension, gathered from however many
//! chunks overlap it. The two formats find their chunks differently (a v1 B-tree versus a filename),
//! but the copy is identical, and it is the subtle part: C-order strides, chunks that split the inner
//! dimensions, and edge chunks the array's shape cuts short. Getting it wrong in one format and right
//! in the other would mean the same logical array hashing differently depending on the container it
//! arrived in.

use crate::adapter::IngestError;

/// The geometry of one chunk-to-row copy.
pub(crate) struct RowCopy<'a> {
    /// The format asking, so a geometry fault names it.
    pub format_id: &'static str,
    /// The chunk's origin, in the array's element coordinates.
    pub origin: &'a [u64],
    /// The chunk's shape (only the first `dims.len()` entries are read, so HDF5 may pass its
    /// element-size entry along).
    pub chunk_dims: &'a [u64],
    /// The array's shape.
    pub dims: &'a [u64],
    /// Which row is being assembled.
    pub index: u64,
    /// Bytes per element.
    pub elem: u64,
}

/// Copy the part of `chunk` that belongs to row `geometry.index` into `row`.
///
/// Both buffers are C-ordered (the last dimension varies fastest), so the copy walks the dimensions
/// between the first and the last with an odometer, moving one contiguous run per position.
pub(crate) fn copy_chunk_into_row(
    geometry: &RowCopy<'_>,
    chunk: &[u8],
    row: &mut [u8],
) -> Result<(), IngestError> {
    let RowCopy {
        format_id,
        origin,
        chunk_dims,
        dims,
        index,
        elem,
    } = *geometry;

    let rank = dims.len();
    let within = index
        .checked_sub(origin[0])
        .ok_or_else(|| chunk_error(format_id, "a chunk origin sits after the row it indexes"))?;
    if rank == 1 {
        // A 1-D dataset's "row" is a single element.
        let src = (within * elem) as usize;
        if src + elem as usize > chunk.len() || elem as usize > row.len() {
            return Err(chunk_error(
                format_id,
                "a chunk is too small for the row it indexes",
            ));
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
                .ok_or_else(|| chunk_error(format_id, "a chunk offset overflows"))?;
            if d > 0 {
                dst = dst
                    .checked_mul(dims[d])
                    .and_then(|s| s.checked_add(origin[d] + counters[d]))
                    .ok_or_else(|| chunk_error(format_id, "a row offset overflows"))?;
            }
        }
        let src_bytes = (src * elem) as usize;
        let dst_bytes = (dst * elem) as usize;
        let run_bytes = (run * elem) as usize;
        if src_bytes + run_bytes > chunk.len() {
            return Err(chunk_error(
                format_id,
                "a chunk is too small for the row it indexes",
            ));
        }
        if dst_bytes + run_bytes > row.len() {
            return Err(chunk_error(
                format_id,
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

/// A chunk-geometry fault, reported against the format that hit it: "a chunk is too small for the
/// row it indexes" means something different in an HDF5 B-tree than in a Zarr chunk file.
fn chunk_error(format_id: &'static str, message: impl Into<String>) -> IngestError {
    IngestError::Parse {
        format_id,
        message: message.into(),
    }
}
