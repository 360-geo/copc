//! Point chunk fetching and LAZ decompression.

use std::io::Cursor;

use laz::LazVlr;

use crate::byte_source::ByteSource;
use crate::error::CopcError;
use crate::hierarchy::HierarchyEntry;
use crate::types::VoxelKey;

/// A decompressed point data chunk.
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

    laz::decompress_buffer(&compressed, &mut decompressed, laz_vlr.clone())?;

    Ok(DecompressedChunk {
        key: entry.key,
        data: decompressed,
        point_count: entry.point_count as u32,
        point_record_length,
    })
}

/// Parse points from a decompressed chunk into `las::Point` values.
pub fn read_points(
    chunk: &DecompressedChunk,
    header: &las::Header,
) -> Result<Vec<las::Point>, CopcError> {
    let format = header.point_format();
    let transforms = header.transforms();
    let mut cursor = Cursor::new(chunk.data.as_slice());
    let mut points = Vec::with_capacity(chunk.point_count as usize);

    for _ in 0..chunk.point_count {
        let raw = las::raw::Point::read_from(&mut cursor, format)?;
        points.push(las::Point::new(raw, transforms));
    }

    Ok(points)
}
