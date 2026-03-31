//! Point chunk fetching and LAZ decompression.

use std::io::Cursor;

use laz::LazVlr;
use laz::record::{LayeredPointRecordDecompressor, RecordDecompressor};

use crate::byte_source::ByteSource;
use crate::error::CopcError;
use crate::hierarchy::HierarchyEntry;
use crate::types::VoxelKey;

/// A decompressed point data chunk.
#[non_exhaustive]
pub struct DecompressedChunk {
    /// The octree node this chunk belongs to.
    pub key: VoxelKey,
    /// Raw decompressed point record bytes.
    pub data: Vec<u8>,
    /// Number of points in this chunk.
    pub point_count: u32,
    /// Size of a single point record in bytes.
    pub point_record_length: u16,
}

/// Fetch and decompress a single chunk.
pub async fn fetch_and_decompress(
    source: &impl ByteSource,
    entry: &HierarchyEntry,
    laz_vlr: &LazVlr,
    point_record_length: u16,
) -> Result<DecompressedChunk, CopcError> {
    let compressed = source
        .read_range(entry.offset, entry.byte_size as u64)
        .await?;

    let decompressed_size = entry.point_count as usize * point_record_length as usize;
    let mut decompressed = vec![0u8; decompressed_size];

    decompress_copc_chunk(&compressed, &mut decompressed, laz_vlr)?;

    Ok(DecompressedChunk {
        key: entry.key,
        data: decompressed,
        point_count: entry.point_count,
        point_record_length,
    })
}

/// Decompress a single COPC chunk.
///
/// COPC chunks are independently compressed and do NOT start with the 8-byte
/// chunk table offset that standard LAZ files have. We use
/// `LayeredPointRecordDecompressor` directly (the same approach as copc-rs)
/// to bypass `LasZipDecompressor`'s chunk table handling.
fn decompress_copc_chunk(
    compressed: &[u8],
    decompressed: &mut [u8],
    laz_vlr: &LazVlr,
) -> Result<(), CopcError> {
    let src = Cursor::new(compressed);
    let mut decompressor = LayeredPointRecordDecompressor::new(src);
    decompressor.set_fields_from(laz_vlr.items())?;
    decompressor.decompress_many(decompressed)?;
    Ok(())
}

/// Parse all points from a decompressed chunk into `las::Point` values.
pub fn read_points(
    chunk: &DecompressedChunk,
    header: &las::Header,
) -> Result<Vec<las::Point>, CopcError> {
    read_points_range(chunk, header, 0..chunk.point_count)
}

/// Parse a sub-range of points from a decompressed chunk.
///
/// Only the points in `range` are parsed — bytes outside the range are skipped.
/// Returns an error if the range extends beyond the chunk's point count.
pub fn read_points_range(
    chunk: &DecompressedChunk,
    header: &las::Header,
    range: std::ops::Range<u32>,
) -> Result<Vec<las::Point>, CopcError> {
    if range.end > chunk.point_count {
        return Err(CopcError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "point range {}..{} exceeds chunk point count {}",
                range.start, range.end, chunk.point_count
            ),
        )));
    }

    let format = header.point_format();
    let transforms = header.transforms();
    let record_len = chunk.point_record_length as u64;

    let start = (range.start as u64 * record_len) as usize;
    let count = range.end.saturating_sub(range.start) as usize;

    let mut cursor = Cursor::new(&chunk.data[start..]);
    let mut points = Vec::with_capacity(count);

    for _ in 0..count {
        let raw = las::raw::Point::read_from(&mut cursor, format)?;
        points.push(las::Point::new(raw, transforms));
    }

    Ok(points)
}
