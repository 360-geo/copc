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

    /// Load hierarchy pages intersecting `bounds`, down to `max_level`.
    ///
    /// Pages deeper than `max_level` are left pending even if they overlap
    /// the bounds. Combine with [`CopcInfo::level_for_resolution`] to load
    /// only the detail you need:
    ///
    /// ```rust,ignore
    /// let level = reader.copc_info().level_for_resolution(0.5);
    /// reader.load_hierarchy_for_bounds_to_level(&camera_box, level).await?;
    /// ```
    pub async fn load_hierarchy_for_bounds_to_level(
        &mut self,
        bounds: &crate::types::Aabb,
        max_level: i32,
    ) -> Result<(), CopcError> {
        let root_bounds = self.header.copc_info.root_bounds();
        self.hierarchy
            .load_pages_for_bounds_to_level(&self.source, bounds, &root_bounds, max_level)
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

    /// Parse all points from a chunk, keeping only those inside `bounds`.
    pub fn read_points_in_bounds(
        &self,
        chunk: &DecompressedChunk,
        bounds: &crate::types::Aabb,
    ) -> Result<Vec<las::Point>, CopcError> {
        let points = chunk::read_points(chunk, &self.header.las_header)?;
        Ok(filter_points_by_bounds(points, bounds))
    }

    /// Parse a sub-range of points, keeping only those inside `bounds`.
    ///
    /// Combines temporal range estimation with spatial filtering: first only
    /// the points in `range` are decompressed, then points outside `bounds`
    /// are discarded.
    pub fn read_points_range_in_bounds(
        &self,
        chunk: &DecompressedChunk,
        range: std::ops::Range<u32>,
        bounds: &crate::types::Aabb,
    ) -> Result<Vec<las::Point>, CopcError> {
        let points = chunk::read_points_range(chunk, &self.header.las_header, range)?;
        Ok(filter_points_by_bounds(points, bounds))
    }

    // --- High-level queries ---

    /// Load hierarchy and return all points inside `bounds`.
    ///
    /// This is the simplest way to query a spatial region. It loads the
    /// hierarchy pages that overlap `bounds`, fetches and decompresses
    /// matching chunks, and returns only the points inside the bounding box.
    ///
    /// ```rust,ignore
    /// let points = reader.query_points(&my_query_box).await?;
    /// ```
    pub async fn query_points(
        &mut self,
        bounds: &crate::types::Aabb,
    ) -> Result<Vec<las::Point>, CopcError> {
        self.load_hierarchy_for_bounds(bounds).await?;
        let root_bounds = self.header.copc_info.root_bounds();

        let keys: Vec<VoxelKey> = self
            .hierarchy
            .iter()
            .filter(|(k, e)| e.point_count > 0 && k.bounds(&root_bounds).intersects(bounds))
            .map(|(k, _)| *k)
            .collect();

        let mut all_points = Vec::new();
        for key in keys {
            let chunk = self.fetch_chunk(&key).await?;
            let points = self.read_points_in_bounds(&chunk, bounds)?;
            all_points.extend(points);
        }
        Ok(all_points)
    }

    /// Load hierarchy to `max_level` and return all points inside `bounds`.
    ///
    /// Like [`query_points`](Self::query_points) but limits the octree depth.
    /// Use with [`CopcInfo::level_for_resolution`] for LOD control:
    ///
    /// ```rust,ignore
    /// let level = reader.copc_info().level_for_resolution(0.5);
    /// let points = reader.query_points_to_level(&visible_box, level).await?;
    /// ```
    pub async fn query_points_to_level(
        &mut self,
        bounds: &crate::types::Aabb,
        max_level: i32,
    ) -> Result<Vec<las::Point>, CopcError> {
        self.load_hierarchy_for_bounds_to_level(bounds, max_level)
            .await?;
        let root_bounds = self.header.copc_info.root_bounds();

        let keys: Vec<VoxelKey> = self
            .hierarchy
            .iter()
            .filter(|(k, e)| {
                e.point_count > 0
                    && k.level <= max_level
                    && k.bounds(&root_bounds).intersects(bounds)
            })
            .map(|(k, _)| *k)
            .collect();

        let mut all_points = Vec::new();
        for key in keys {
            let chunk = self.fetch_chunk(&key).await?;
            let points = self.read_points_in_bounds(&chunk, bounds)?;
            all_points.extend(points);
        }
        Ok(all_points)
    }
}

/// Filter points to only those inside an axis-aligned bounding box.
pub fn filter_points_by_bounds(
    points: Vec<las::Point>,
    bounds: &crate::types::Aabb,
) -> Vec<las::Point> {
    points
        .into_iter()
        .filter(|p| {
            p.x >= bounds.min[0]
                && p.x <= bounds.max[0]
                && p.y >= bounds.min[1]
                && p.y <= bounds.max[1]
                && p.z >= bounds.min[2]
                && p.z <= bounds.max[2]
        })
        .collect()
}
