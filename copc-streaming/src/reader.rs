//! High-level streaming COPC reader.

use crate::byte_source::ByteSource;
use crate::chunk::{self, Chunk};
use crate::error::CopcError;
use crate::fields::Fields;
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

    // ==================== Fast API (tier 2) ====================
    //
    // Returns `Chunk`s with zero-copy column access. Caller picks which
    // fields to decode, walks columns directly, and decides when to
    // materialize `las::Point` values (if ever).

    /// Fetch and decompress a single chunk, decoding only the fields in
    /// `fields`.
    ///
    /// On LAS 1.4 layered formats (6/7/8 — the formats COPC mandates) this
    /// is a real CPU saving proportional to the number of skipped layers.
    /// Fields listed in [`Fields`] map directly to LAZ layer-level
    /// `DecompressionSelection` and the omitted layers are not arithmetically
    /// decoded at all.
    pub async fn fetch_chunk(&self, key: &VoxelKey, fields: Fields) -> Result<Chunk, CopcError> {
        self.fetch_chunk_with_source(&self.source, key, fields)
            .await
    }

    /// Fetch and decompress a point chunk using an external byte source.
    ///
    /// This is useful when the reader is behind a lock and you want to
    /// extract the metadata under the lock, then do the async fetch
    /// without holding it.
    pub async fn fetch_chunk_with_source(
        &self,
        source: &impl ByteSource,
        key: &VoxelKey,
        fields: Fields,
    ) -> Result<Chunk, CopcError> {
        let entry = self
            .hierarchy
            .get(key)
            .ok_or(CopcError::NodeNotFound(*key))?;
        chunk::fetch_and_decompress(
            source,
            entry,
            &self.header.laz_vlr,
            &self.header.las_header,
            fields,
        )
        .await
    }

    /// Fetch and decompress multiple chunks in one batched I/O call.
    ///
    /// Uses [`ByteSource::read_ranges`] to coalesce the fetches — HTTP
    /// sources that override `read_ranges` with parallel requests or
    /// range merging will issue far fewer round-trips than calling
    /// [`fetch_chunk`](Self::fetch_chunk) in a loop.
    ///
    /// All keys must already be present in the loaded hierarchy; call
    /// one of the `load_hierarchy_*` methods first.
    pub async fn fetch_chunks(
        &self,
        keys: &[VoxelKey],
        fields: Fields,
    ) -> Result<Vec<Chunk>, CopcError> {
        let entries: Vec<&HierarchyEntry> = keys
            .iter()
            .map(|k| self.hierarchy.get(k).ok_or(CopcError::NodeNotFound(*k)))
            .collect::<Result<_, _>>()?;

        let ranges: Vec<(u64, u64)> = entries
            .iter()
            .map(|e| (e.offset, e.byte_size as u64))
            .collect();

        let compressed_blobs = self.source.read_ranges(&ranges).await?;

        compressed_blobs
            .iter()
            .zip(entries.iter())
            .map(|(data, entry)| {
                chunk::decompress_chunk(
                    data,
                    entry,
                    &self.header.laz_vlr,
                    &self.header.las_header,
                    fields,
                )
            })
            .collect()
    }

    /// Keys in the currently-loaded hierarchy whose subtree intersects
    /// `bounds` and whose level is at most `max_level` (if provided).
    ///
    /// Use this to drive your own fetch loop — for parallelism, cancellation,
    /// prioritization, or streaming:
    ///
    /// ```rust,ignore
    /// reader.load_hierarchy_for_bounds_to_level(&bbox, lod).await?;
    /// for key in reader.visible_keys(&bbox, Some(lod)) {
    ///     let chunk = reader.fetch_chunk(&key, Fields::Z | Fields::RGB).await?;
    ///     // ...
    /// }
    /// ```
    pub fn visible_keys(
        &self,
        bounds: &crate::types::Aabb,
        max_level: Option<i32>,
    ) -> Vec<VoxelKey> {
        let root_bounds = self.header.copc_info.root_bounds();
        self.hierarchy
            .iter()
            .filter(|(k, e)| {
                e.point_count > 0
                    && max_level.is_none_or(|lvl| k.level <= lvl)
                    && k.bounds(&root_bounds).intersects(bounds)
            })
            .map(|(k, _)| *k)
            .collect()
    }

    /// Load hierarchy for `bounds`, fetch every intersecting chunk, and
    /// return them.
    ///
    /// Uses [`fetch_chunks`](Self::fetch_chunks) to batch all chunk
    /// fetches into a single [`ByteSource::read_ranges`] call.
    pub async fn query_chunks(
        &mut self,
        bounds: &crate::types::Aabb,
        fields: Fields,
    ) -> Result<Vec<Chunk>, CopcError> {
        self.load_hierarchy_for_bounds(bounds).await?;
        let keys = self.visible_keys(bounds, None);
        self.fetch_chunks(&keys, fields).await
    }

    /// Same as [`query_chunks`](Self::query_chunks) but limits the octree
    /// depth to `max_level`.
    pub async fn query_chunks_to_level(
        &mut self,
        bounds: &crate::types::Aabb,
        max_level: i32,
        fields: Fields,
    ) -> Result<Vec<Chunk>, CopcError> {
        self.load_hierarchy_for_bounds_to_level(bounds, max_level)
            .await?;
        let keys = self.visible_keys(bounds, Some(max_level));
        self.fetch_chunks(&keys, fields).await
    }

    // ==================== Simple API (tier 1) ====================
    //
    // One-line entry points for scripts, tests, and prototypes. Always
    // decodes every field and materializes `las::Point` values. Thin
    // wrappers over the fast API — no duplicate read paths.

    /// Fetch, decompress, and materialize every point in one chunk as
    /// owned [`las::Point`] values.
    ///
    /// Equivalent to `fetch_chunk(key, Fields::ALL).await?.to_points()?`.
    /// Prefer [`fetch_chunk`](Self::fetch_chunk) for performance-sensitive
    /// code paths.
    pub async fn fetch_points(&self, key: &VoxelKey) -> Result<Vec<las::Point>, CopcError> {
        self.fetch_chunk(key, Fields::ALL).await?.to_points()
    }

    /// Load hierarchy for `bounds`, fetch all intersecting chunks, and
    /// return the points inside `bounds`.
    ///
    /// Thin wrapper over [`query_chunks`](Self::query_chunks) with
    /// `Fields::ALL`. Decodes everything and materializes; prefer
    /// `query_chunks` when you only need a subset of fields or want to
    /// walk points column-by-column.
    pub async fn query_points(
        &mut self,
        bounds: &crate::types::Aabb,
    ) -> Result<Vec<las::Point>, CopcError> {
        let chunks = self.query_chunks(bounds, Fields::ALL).await?;
        let mut out = Vec::new();
        for chunk in &chunks {
            // `indices_in_bounds` on an `ALL`-decoded chunk always returns Some.
            let indices = chunk
                .indices_in_bounds(bounds)
                .expect("Fields::ALL includes Fields::Z");
            if indices.is_empty() {
                continue;
            }
            out.extend(chunk.points_at(&indices)?);
        }
        Ok(out)
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
        let chunks = self
            .query_chunks_to_level(bounds, max_level, Fields::ALL)
            .await?;
        let mut out = Vec::new();
        for chunk in &chunks {
            let indices = chunk
                .indices_in_bounds(bounds)
                .expect("Fields::ALL includes Fields::Z");
            if indices.is_empty() {
                continue;
            }
            out.extend(chunk.points_at(&indices)?);
        }
        Ok(out)
    }
}
