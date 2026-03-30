//! Incremental COPC hierarchy loading.
//!
//! Loads hierarchy pages on demand rather than all at once.

use std::collections::HashMap;
use std::io::Cursor;

use crate::types::VoxelKey;
use byteorder::{LittleEndian, ReadBytesExt};

use crate::byte_source::ByteSource;
use crate::error::CopcError;
use crate::header::CopcInfo;

/// A single hierarchy entry: metadata for one octree node.
#[derive(Debug, Clone)]
pub struct HierarchyEntry {
    pub key: VoxelKey,
    /// Absolute file offset to the compressed point data.
    pub offset: u64,
    /// Size of compressed data in bytes.
    pub byte_size: i32,
    /// Number of points, or -1 if this is a page pointer.
    pub point_count: i32,
}

/// Reference to a hierarchy page that hasn't been loaded yet.
#[derive(Debug, Clone)]
struct PendingPage {
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
                    offset,
                    size: byte_size as u64,
                });
            } else {
                self.entries.insert(
                    key,
                    HierarchyEntry {
                        key,
                        offset,
                        byte_size,
                        point_count,
                    },
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

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
