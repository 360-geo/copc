//! Incremental COPC hierarchy loading.
//!
//! Loads hierarchy pages on demand rather than all at once.

use std::collections::HashMap;
use std::io::Cursor;

use crate::types::{Aabb, VoxelKey};
use byteorder::{LittleEndian, ReadBytesExt};

use crate::byte_source::ByteSource;
use crate::error::CopcError;
use crate::header::CopcInfo;

/// A single hierarchy entry: metadata for one octree node.
///
/// The `key` is duplicated from the hierarchy map for convenience when
/// passing entries around without the map key.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HierarchyEntry {
    /// The octree node this entry describes.
    pub key: VoxelKey,
    /// Absolute file offset to the compressed point data.
    pub offset: u64,
    /// Size of compressed data in bytes.
    pub byte_size: u32,
    /// Number of points in this node.
    pub point_count: u32,
}

/// Reference to a hierarchy page that hasn't been loaded yet.
#[derive(Debug, Clone)]
struct PendingPage {
    /// The voxel key of the node that points to this page.
    key: VoxelKey,
    offset: u64,
    size: u64,
}

/// Incrementally-loaded hierarchy cache.
pub struct HierarchyCache {
    /// All loaded node entries (point_count >= 0).
    entries: HashMap<VoxelKey, HierarchyEntry>,
    /// Pages we know about but haven't fetched yet.
    pending_pages: Vec<PendingPage>,
    /// Whether the root page has been loaded.
    root_loaded: bool,
}

impl Default for HierarchyCache {
    fn default() -> Self {
        Self::new()
    }
}

impl HierarchyCache {
    /// Create an empty hierarchy cache.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            pending_pages: Vec::new(),
            root_loaded: false,
        }
    }

    /// Load the root hierarchy page.
    pub async fn load_root(
        &mut self,
        source: &impl ByteSource,
        info: &CopcInfo,
    ) -> Result<(), CopcError> {
        if self.root_loaded {
            return Ok(());
        }
        let data = source
            .read_range(info.root_hier_offset, info.root_hier_size)
            .await?;
        self.parse_page(&data, info.root_hier_offset)?;
        self.root_loaded = true;
        Ok(())
    }

    /// Load the next batch of pending hierarchy pages.
    pub async fn load_pending_pages(&mut self, source: &impl ByteSource) -> Result<(), CopcError> {
        if self.pending_pages.is_empty() {
            return Ok(());
        }
        let pages: Vec<_> = self.pending_pages.drain(..).collect();
        let ranges: Vec<_> = pages.iter().map(|p| (p.offset, p.size)).collect();
        let results = source.read_ranges(&ranges).await?;

        for (page, data) in pages.iter().zip(results) {
            self.parse_page(&data, page.offset)?;
        }

        Ok(())
    }

    /// Load all pending pages (breadth-first).
    ///
    /// Each depth level is fetched in a single [`ByteSource::read_ranges`]
    /// call, so HTTP backends that override `read_ranges` with parallel
    /// fetches will issue one round-trip per depth level.
    pub async fn load_all(
        &mut self,
        source: &impl ByteSource,
        info: &CopcInfo,
    ) -> Result<(), CopcError> {
        self.load_root(source, info).await?;

        while !self.pending_pages.is_empty() {
            self.load_pending_pages(source).await?;
        }

        Ok(())
    }

    /// Load only pending pages whose subtree intersects `bounds`.
    ///
    /// Pages whose voxel key falls outside the query region are left pending
    /// for future calls. New child pages discovered during loading are
    /// evaluated in subsequent iterations, so the full relevant subtree is
    /// loaded by the time this returns.
    pub async fn load_pages_for_bounds(
        &mut self,
        source: &impl ByteSource,
        bounds: &Aabb,
        root_bounds: &Aabb,
    ) -> Result<(), CopcError> {
        loop {
            let matching: Vec<PendingPage> = self
                .pending_pages
                .iter()
                .filter(|p| p.key.bounds(root_bounds).intersects(bounds))
                .cloned()
                .collect();

            if matching.is_empty() {
                break;
            }

            self.pending_pages
                .retain(|p| !p.key.bounds(root_bounds).intersects(bounds));

            let ranges: Vec<_> = matching.iter().map(|p| (p.offset, p.size)).collect();
            let results = source.read_ranges(&ranges).await?;

            for (page, data) in matching.iter().zip(results) {
                self.parse_page(&data, page.offset)?;
            }
        }

        Ok(())
    }

    /// Like [`load_pages_for_bounds`](Self::load_pages_for_bounds) but stops
    /// at `max_level` — pages whose key is deeper than `max_level` are left
    /// pending even if they intersect the bounds.
    pub async fn load_pages_for_bounds_to_level(
        &mut self,
        source: &impl ByteSource,
        bounds: &Aabb,
        root_bounds: &Aabb,
        max_level: i32,
    ) -> Result<(), CopcError> {
        loop {
            let matching: Vec<PendingPage> = self
                .pending_pages
                .iter()
                .filter(|p| {
                    p.key.level <= max_level && p.key.bounds(root_bounds).intersects(bounds)
                })
                .cloned()
                .collect();

            if matching.is_empty() {
                break;
            }

            self.pending_pages.retain(|p| {
                !(p.key.level <= max_level && p.key.bounds(root_bounds).intersects(bounds))
            });

            let ranges: Vec<_> = matching.iter().map(|p| (p.offset, p.size)).collect();
            let results = source.read_ranges(&ranges).await?;

            for (page, data) in matching.iter().zip(results) {
                self.parse_page(&data, page.offset)?;
            }
        }

        Ok(())
    }

    /// Whether there are unloaded hierarchy pages.
    pub fn has_pending_pages(&self) -> bool {
        !self.pending_pages.is_empty()
    }

    /// Look up a hierarchy entry by key.
    pub fn get(&self, key: &VoxelKey) -> Option<&HierarchyEntry> {
        self.entries.get(key)
    }

    /// Iterate all loaded entries.
    pub fn iter(&self) -> impl Iterator<Item = (&VoxelKey, &HierarchyEntry)> {
        self.entries.iter()
    }

    /// Number of loaded entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no entries have been loaded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Parse a hierarchy page and add entries / page pointers.
    fn parse_page(&mut self, data: &[u8], _base_offset: u64) -> Result<(), CopcError> {
        let entry_size = 32; // VoxelKey (16) + offset (8) + byte_size (4) + point_count (4)
        let mut r = Cursor::new(data);

        while (r.position() as usize + entry_size) <= data.len() {
            let level = r.read_i32::<LittleEndian>()?;
            let x = r.read_i32::<LittleEndian>()?;
            let y = r.read_i32::<LittleEndian>()?;
            let z = r.read_i32::<LittleEndian>()?;
            let offset = r.read_u64::<LittleEndian>()?;
            let byte_size = r.read_i32::<LittleEndian>()?;
            let point_count = r.read_i32::<LittleEndian>()?;

            let key = VoxelKey { level, x, y, z };

            if point_count == -1 {
                // Page pointer — register for later loading
                self.pending_pages.push(PendingPage {
                    key,
                    offset,
                    size: byte_size as u64,
                });
            } else if point_count >= 0 && byte_size >= 0 {
                self.entries.insert(
                    key,
                    HierarchyEntry {
                        key,
                        offset,
                        byte_size: byte_size as u32,
                        point_count: point_count as u32,
                    },
                );
            }
            // Silently skip entries with invalid negative values (corrupt file).
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    #[allow(clippy::too_many_arguments)]
    fn write_hierarchy_entry(
        buf: &mut Vec<u8>,
        level: i32,
        x: i32,
        y: i32,
        z: i32,
        offset: u64,
        byte_size: i32,
        point_count: i32,
    ) {
        buf.write_i32::<LittleEndian>(level).unwrap();
        buf.write_i32::<LittleEndian>(x).unwrap();
        buf.write_i32::<LittleEndian>(y).unwrap();
        buf.write_i32::<LittleEndian>(z).unwrap();
        buf.write_u64::<LittleEndian>(offset).unwrap();
        buf.write_i32::<LittleEndian>(byte_size).unwrap();
        buf.write_i32::<LittleEndian>(point_count).unwrap();
    }

    /// Build a Vec<u8> source containing:
    /// - Root page at offset 0: root node + two page pointers (left child, right child)
    /// - Left child page at some offset: a single node entry
    /// - Right child page at some offset: a single node entry
    ///
    /// Root bounds: center [50, 50, 50], halfsize 50 → [0..100] on each axis.
    /// Level-1 child (1,0,0,0) covers [0..50] on x → "left"
    /// Level-1 child (1,1,0,0) covers [50..100] on x → "right"
    fn build_two_child_source() -> (Vec<u8>, Aabb) {
        let root_bounds = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 100.0, 100.0],
        };

        // Build child pages first so we know their offsets
        let mut left_page = Vec::new();
        // Node in left subtree: level 2, (0,0,0)
        write_hierarchy_entry(&mut left_page, 2, 0, 0, 0, 9000, 64, 10);

        let mut right_page = Vec::new();
        // Node in right subtree: level 2, (2,0,0)
        write_hierarchy_entry(&mut right_page, 2, 2, 0, 0, 9500, 64, 20);

        // Root page: root node + two page pointers
        let mut root_page = Vec::new();
        write_hierarchy_entry(&mut root_page, 0, 0, 0, 0, 1000, 256, 100);

        // root_page will have 3 entries (root node + 2 page pointers) = 96 bytes
        let root_page_size = 3 * 32;
        let left_page_offset = root_page_size as u64;
        let right_page_offset = left_page_offset + left_page.len() as u64;

        // Page pointer for left child (1,0,0,0) → covers [0..50] on x
        write_hierarchy_entry(
            &mut root_page,
            1,
            0,
            0,
            0,
            left_page_offset,
            left_page.len() as i32,
            -1,
        );
        // Page pointer for right child (1,1,0,0) → covers [50..100] on x
        write_hierarchy_entry(
            &mut root_page,
            1,
            1,
            0,
            0,
            right_page_offset,
            right_page.len() as i32,
            -1,
        );

        let mut source = root_page;
        source.extend_from_slice(&left_page);
        source.extend_from_slice(&right_page);

        (source, root_bounds)
    }

    #[tokio::test]
    async fn test_load_pages_for_bounds_filters_spatially() {
        let (source, root_bounds) = build_two_child_source();

        let mut cache = HierarchyCache::new();
        // Parse root page (offset 0, 96 bytes = 3 entries)
        cache.parse_page(&source[..96], 0).unwrap();

        assert_eq!(cache.len(), 1); // root node
        assert_eq!(cache.pending_pages.len(), 2); // left + right page pointers

        // Query only the left side: x in [0..30]
        let left_query = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [30.0, 100.0, 100.0],
        };
        cache
            .load_pages_for_bounds(&source, &left_query, &root_bounds)
            .await
            .unwrap();

        // Should have loaded left child page (level 2, node (2,0,0,0))
        assert_eq!(cache.len(), 2); // root + left level-2 node
        assert!(
            cache
                .get(&VoxelKey {
                    level: 2,
                    x: 0,
                    y: 0,
                    z: 0,
                })
                .is_some()
        );

        // Right page should still be pending
        assert_eq!(cache.pending_pages.len(), 1);
        assert_eq!(
            cache.pending_pages[0].key,
            VoxelKey {
                level: 1,
                x: 1,
                y: 0,
                z: 0
            }
        );

        // Now load with a right-side query
        let right_query = Aabb {
            min: [60.0, 0.0, 0.0],
            max: [100.0, 100.0, 100.0],
        };
        cache
            .load_pages_for_bounds(&source, &right_query, &root_bounds)
            .await
            .unwrap();

        assert_eq!(cache.len(), 3); // root + left + right level-2 nodes
        assert!(
            cache
                .get(&VoxelKey {
                    level: 2,
                    x: 2,
                    y: 0,
                    z: 0,
                })
                .is_some()
        );
        assert!(cache.pending_pages.is_empty());
    }

    #[tokio::test]
    async fn test_load_pages_for_bounds_to_level_stops_at_max_level() {
        let (source, root_bounds) = build_two_child_source();

        let mut cache = HierarchyCache::new();
        cache.parse_page(&source[..96], 0).unwrap();

        assert_eq!(cache.len(), 1); // root node
        assert_eq!(cache.pending_pages.len(), 2); // level-1 page pointers

        // Query the entire bounds but limit to level 0 — no pages should load
        // because the pending pages are level-1 pointers.
        let full_query = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 100.0, 100.0],
        };
        cache
            .load_pages_for_bounds_to_level(&source, &full_query, &root_bounds, 0)
            .await
            .unwrap();

        assert_eq!(cache.len(), 1); // still just root
        assert_eq!(cache.pending_pages.len(), 2); // both pages still pending

        // Now allow level 1 — both pages should load
        cache
            .load_pages_for_bounds_to_level(&source, &full_query, &root_bounds, 1)
            .await
            .unwrap();

        assert_eq!(cache.len(), 3); // root + two level-2 nodes
        assert!(cache.pending_pages.is_empty());
    }

    #[tokio::test]
    async fn test_parse_hierarchy_page() {
        let mut page_data = Vec::new();
        write_hierarchy_entry(&mut page_data, 0, 0, 0, 0, 1000, 256, 100);
        write_hierarchy_entry(&mut page_data, 1, 0, 0, 0, 2000, 512, 50);
        write_hierarchy_entry(&mut page_data, 1, 1, 0, 0, 5000, 128, -1);

        let mut cache = HierarchyCache::new();
        cache.parse_page(&page_data, 0).unwrap();

        assert_eq!(cache.len(), 2); // 2 node entries
        assert_eq!(cache.pending_pages.len(), 1); // 1 page pointer

        let root = cache
            .get(&VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            })
            .unwrap();
        assert_eq!(root.point_count, 100);
        assert_eq!(root.offset, 1000);
    }
}
