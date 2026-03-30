//! High-level streaming COPC reader.

use crate::byte_source::ByteSource;
use crate::chunk::{self, DecompressedChunk};
use crate::error::CopcError;
use crate::header::{self, CopcHeader, CopcInfo};
use crate::hierarchy::{HierarchyCache, HierarchyEntry};
use crate::types::VoxelKey;

/// Async streaming COPC reader.
///
/// `open()` reads the LAS header, VLRs, and root hierarchy page.
/// Deeper hierarchy pages and point chunks are loaded on demand.
pub struct CopcStreamingReader<S: ByteSource> {
    source: S,
    header: CopcHeader,
    hierarchy: HierarchyCache,
}

impl<S: ByteSource> CopcStreamingReader<S> {
    /// Open a COPC file.
    pub async fn open(source: S) -> Result<Self, CopcError> {
        let size = source.size().await?.unwrap_or(65536);
        let read_size = size.min(65536);
        let data = source.read_range(0, read_size).await?;
        let header = header::parse_header(&data)?;

        let mut hierarchy = HierarchyCache::new();
        hierarchy.load_root(&source, &header.copc_info).await?;

        Ok(Self {
            source,
            header,
            hierarchy,
        })
    }

    // --- Header accessors ---

    /// The parsed COPC file header.
    pub fn header(&self) -> &CopcHeader {
        &self.header
    }

    /// Shortcut for `header().copc_info()`.
    pub fn copc_info(&self) -> &CopcInfo {
        &self.header.copc_info
    }

    /// File offset where EVLRs start.
    pub fn evlr_offset(&self) -> u64 {
        self.header.evlr_offset
    }

    /// Number of EVLRs in the file.
    pub fn evlr_count(&self) -> u32 {
        self.header.evlr_count
    }

    /// The underlying byte source.
    pub fn source(&self) -> &S {
        &self.source
    }

    // --- Hierarchy queries ---

    /// Look up a hierarchy entry by voxel key.
    pub fn get(&self, key: &VoxelKey) -> Option<&HierarchyEntry> {
        self.hierarchy.get(key)
    }

    /// Iterate all loaded hierarchy entries.
    pub fn entries(&self) -> impl Iterator<Item = (&VoxelKey, &HierarchyEntry)> {
        self.hierarchy.iter()
    }

    /// Return loaded child entries for a given node.
    ///
    /// Only returns children that are already in the hierarchy cache.
    /// If deeper hierarchy pages haven't been loaded yet, this may
    /// return fewer children than actually exist in the file.
    pub fn children(&self, key: &VoxelKey) -> Vec<&HierarchyEntry> {
        key.children()
            .iter()
            .filter_map(|child| self.hierarchy.get(child))
            .collect()
    }

    /// Number of loaded hierarchy entries.
    pub fn node_count(&self) -> usize {
        self.hierarchy.len()
    }

    /// Whether there are hierarchy pages that haven't been loaded yet.
    pub fn has_pending_pages(&self) -> bool {
        self.hierarchy.has_pending_pages()
    }

    // --- Hierarchy loading ---

    /// Load the next batch of pending hierarchy pages.
    pub async fn load_pending_pages(&mut self) -> Result<(), CopcError> {
        self.hierarchy.load_pending_pages(&self.source).await
    }

    /// Load only hierarchy pages whose subtree intersects `bounds`.
    ///
    /// Pages outside the region are left pending for future calls.
    /// Much cheaper than `load_all_hierarchy` when querying a small area.
    pub async fn load_hierarchy_for_bounds(
        &mut self,
        bounds: &crate::types::Aabb,
    ) -> Result<(), CopcError> {
        let root_bounds = self.header.copc_info.root_bounds();
        self.hierarchy
            .load_pages_for_bounds(&self.source, bounds, &root_bounds)
            .await
    }

    /// Load all remaining hierarchy pages.
    pub async fn load_all_hierarchy(&mut self) -> Result<(), CopcError> {
        self.hierarchy
            .load_all(&self.source, &self.header.copc_info)
            .await
    }

    // --- Point data ---

    /// Fetch and decompress a single point chunk.
    pub async fn fetch_chunk(&self, key: &VoxelKey) -> Result<DecompressedChunk, CopcError> {
        let entry = self
            .hierarchy
            .get(key)
            .ok_or(CopcError::NodeNotFound(*key))?;
        let point_record_length = self.header.las_header.point_format().len()
            + self.header.las_header.point_format().extra_bytes;
        chunk::fetch_and_decompress(
            &self.source,
            entry,
            &self.header.laz_vlr,
            point_record_length,
        )
        .await
    }

    /// Parse all points from a decompressed chunk.
    pub fn read_points(&self, chunk: &DecompressedChunk) -> Result<Vec<las::Point>, CopcError> {
        chunk::read_points(chunk, &self.header.las_header)
    }

    /// Parse a sub-range of points from a decompressed chunk.
    ///
    /// Only the points in `range` are parsed — bytes outside the range are skipped.
    /// Pair with `NodeTemporalEntry::estimate_point_range` from the `copc-temporal`
    /// crate to read only the points that fall within a time window.
    pub fn read_points_range(
        &self,
        chunk: &DecompressedChunk,
        range: std::ops::Range<u32>,
    ) -> Result<Vec<las::Point>, CopcError> {
        chunk::read_points_range(chunk, &self.header.las_header, range)
    }
}
