//! Incremental temporal index loading via async ByteSource.
//!
//! Loads temporal index pages on demand, pruning subtrees by time range
//! before fetching child pages.

use std::collections::HashMap;
use std::io::Cursor;

use byteorder::{LittleEndian, ReadBytesExt};
use copc_streaming::{ByteSource, Chunk, CopcStreamingReader, Fields, VoxelKey};

use crate::TemporalError;
use crate::gps_time::GpsTime;
use crate::temporal_index::NodeTemporalEntry;

/// Header of the temporal index EVLR (32 bytes).
#[derive(Debug, Clone)]
pub struct TemporalHeader {
    /// Format version (must be 1).
    pub version: u32,
    /// Sampling stride — every N-th point is recorded.
    pub stride: u32,
    /// Total number of node entries across all pages.
    pub node_count: u32,
    /// Total number of pages.
    pub page_count: u32,
    /// Absolute file offset of the root page.
    pub root_page_offset: u64,
    /// Size of the root page in bytes.
    pub root_page_size: u32,
}

#[derive(Debug, Clone)]
struct PendingPage {
    offset: u64,
    size: u32,
    subtree_time_min: f64,
    subtree_time_max: f64,
}

/// Incrementally-loaded temporal index cache.
pub struct TemporalCache {
    header: Option<TemporalHeader>,
    entries: HashMap<VoxelKey, NodeTemporalEntry>,
    pending_pages: Vec<PendingPage>,
    stride: u32,
}

impl Default for TemporalCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TemporalCache {
    /// Create an empty temporal cache.
    pub fn new() -> Self {
        Self {
            header: None,
            entries: HashMap::new(),
            pending_pages: Vec::new(),
            stride: 0,
        }
    }

    /// Open the temporal index from a COPC reader.
    ///
    /// Loads the temporal header and root page. Returns `Ok(None)` if
    /// no temporal EVLR exists in the file.
    pub async fn from_reader<S: ByteSource>(
        reader: &CopcStreamingReader<S>,
    ) -> Result<Option<Self>, TemporalError> {
        let mut cache = Self::new();
        let found = cache
            .load_header(reader.source(), reader.evlr_offset(), reader.evlr_count())
            .await?;
        if !found {
            return Ok(None);
        }
        cache.load_root_page(reader.source()).await?;
        Ok(Some(cache))
    }

    /// Scan EVLRs to find the temporal EVLR and read its header.
    /// Returns false if no temporal EVLR exists.
    pub async fn load_header(
        &mut self,
        source: &impl ByteSource,
        evlr_offset: u64,
        evlr_count: u32,
    ) -> Result<bool, TemporalError> {
        let mut pos = evlr_offset;

        for _ in 0..evlr_count {
            let hdr_data = source.read_range(pos, 60).await?;
            let mut r = Cursor::new(hdr_data.as_slice());

            // reserved (2)
            r.set_position(2);
            let mut user_id = [0u8; 16];
            std::io::Read::read_exact(&mut r, &mut user_id)?;
            let record_id = r.read_u16::<LittleEndian>()?;
            let data_length = r.read_u64::<LittleEndian>()?;

            let data_start = pos + 60;

            let uid_end = user_id.iter().position(|&b| b == 0).unwrap_or(16);
            let uid_str = std::str::from_utf8(&user_id[..uid_end]).unwrap_or("");

            if uid_str == "copc_temporal" && record_id == 1000 {
                let header_data = source.read_range(data_start, 32).await?;
                let header = parse_temporal_header(&header_data)?;
                self.stride = header.stride;
                self.header = Some(header);
                return Ok(true);
            }

            pos = data_start + data_length;
        }

        Ok(false)
    }

    /// Load the root temporal page.
    pub async fn load_root_page(&mut self, source: &impl ByteSource) -> Result<(), TemporalError> {
        let header = self.header.as_ref().ok_or(TemporalError::TruncatedHeader)?;

        let data = source
            .read_range(header.root_page_offset, header.root_page_size as u64)
            .await?;
        self.parse_page(&data)?;
        Ok(())
    }

    /// Load child pages that overlap a time range, pruning by subtree bounds.
    pub async fn load_pages_for_time_range(
        &mut self,
        source: &impl ByteSource,
        start: GpsTime,
        end: GpsTime,
    ) -> Result<(), TemporalError> {
        loop {
            let matching: Vec<PendingPage> = self
                .pending_pages
                .iter()
                .filter(|p| p.subtree_time_max >= start.0 && p.subtree_time_min <= end.0)
                .cloned()
                .collect();

            if matching.is_empty() {
                break;
            }

            self.pending_pages
                .retain(|p| !(p.subtree_time_max >= start.0 && p.subtree_time_min <= end.0));

            let ranges: Vec<_> = matching.iter().map(|p| (p.offset, p.size as u64)).collect();
            let results = source.read_ranges(&ranges).await?;

            for data in &results {
                self.parse_page(data)?;
            }
        }

        Ok(())
    }

    /// Load ALL pending pages.
    pub async fn load_all_pages(&mut self, source: &impl ByteSource) -> Result<(), TemporalError> {
        while !self.pending_pages.is_empty() {
            let pages: Vec<PendingPage> = self.pending_pages.drain(..).collect();
            let ranges: Vec<_> = pages.iter().map(|p| (p.offset, p.size as u64)).collect();
            let results = source.read_ranges(&ranges).await?;

            for data in &results {
                self.parse_page(data)?;
            }
        }
        Ok(())
    }

    /// Load relevant pages and return all nodes that overlap a time range.
    ///
    /// This is the primary query method — it ensures the right pages are loaded
    /// before returning results. Equivalent to calling `load_pages_for_time_range`
    /// followed by `nodes_in_range`, but cannot return incomplete results.
    pub async fn query(
        &mut self,
        source: &impl ByteSource,
        start: GpsTime,
        end: GpsTime,
    ) -> Result<Vec<&NodeTemporalEntry>, TemporalError> {
        self.load_pages_for_time_range(source, start, end).await?;
        Ok(self.nodes_in_range(start, end))
    }

    /// Look up the temporal entry for a node.
    pub fn get(&self, key: &VoxelKey) -> Option<&NodeTemporalEntry> {
        self.entries.get(key)
    }

    /// Return all loaded nodes whose time range overlaps `[start, end]`.
    pub fn nodes_in_range(&self, start: GpsTime, end: GpsTime) -> Vec<&NodeTemporalEntry> {
        self.entries
            .values()
            .filter(|e| e.overlaps(start, end))
            .collect()
    }

    /// The sampling stride (every N-th point is recorded in the index).
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// The parsed temporal index header, if loaded.
    pub fn header(&self) -> Option<&TemporalHeader> {
        self.header.as_ref()
    }

    /// Number of loaded node entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no node entries have been loaded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate all loaded entries.
    pub fn iter(&self) -> impl Iterator<Item = (&VoxelKey, &NodeTemporalEntry)> {
        self.entries.iter()
    }

    // ==================== Fast API ====================
    //
    // Column-oriented queries that return `Chunk`s with a caller-chosen
    // `Fields` mask, alongside an estimated candidate index range derived
    // from the temporal index stride samples. Callers walk columns
    // directly and decide for themselves whether to materialize
    // `las::Point` values.

    /// Query chunks by time range and spatial bounds.
    ///
    /// Loads the relevant hierarchy and temporal pages, fetches every
    /// matching chunk with the caller's `fields` mask, and returns
    /// `(chunk, candidate_range)` pairs. `candidate_range` is the
    /// sub-range of point indices in `chunk` whose GPS times could
    /// possibly fall within `[start, end]` based on the temporal index
    /// stride samples — points outside this range are guaranteed not to
    /// match and can be skipped.
    ///
    /// The caller is responsible for the exact per-point intersection.
    /// Combine with [`Chunk::indices_in_bounds`] and
    /// [`indices_in_time_range`](crate::indices_in_time_range) to compute
    /// the precise matching set, then walk column accessors or call
    /// [`Chunk::points_at`] to materialize owned values.
    ///
    /// ```rust,ignore
    /// use copc_streaming::Fields;
    ///
    /// let chunks = temporal
    ///     .query_chunks(
    ///         &mut reader,
    ///         &bbox,
    ///         start,
    ///         end,
    ///         Fields::Z | Fields::GPS_TIME,
    ///     )
    ///     .await?;
    ///
    /// for (chunk, range) in &chunks {
    ///     let times = chunk.gps_time().unwrap();
    ///     for (i, t) in times.enumerate().skip(range.start as usize)
    ///                                    .take(range.len()) {
    ///         if t >= start.0 && t <= end.0 {
    ///             // ...
    ///         }
    ///     }
    /// }
    /// ```
    pub async fn query_chunks<S: ByteSource>(
        &mut self,
        reader: &mut CopcStreamingReader<S>,
        bounds: &copc_streaming::Aabb,
        start: GpsTime,
        end: GpsTime,
        fields: Fields,
    ) -> Result<Vec<(Chunk, std::ops::Range<u32>)>, TemporalError> {
        reader
            .load_hierarchy_for_bounds(bounds)
            .await
            .map_err(TemporalError::Copc)?;

        self.load_pages_for_time_range(reader.source(), start, end)
            .await?;

        let root_bounds = reader.copc_info().root_bounds();
        let stride = self.stride;

        let matches: Vec<_> = self
            .nodes_in_range(start, end)
            .into_iter()
            .filter(|e| e.key.bounds(&root_bounds).intersects(bounds))
            .filter_map(|e| {
                let hier = reader.get(&e.key)?;
                let range = e.estimate_point_range(start, end, stride, hier.point_count);
                if range.is_empty() {
                    return None;
                }
                Some((e.key, range))
            })
            .collect();

        let mut out = Vec::with_capacity(matches.len());
        for (key, range) in matches {
            let chunk = reader
                .fetch_chunk(&key, fields)
                .await
                .map_err(TemporalError::Copc)?;
            out.push((chunk, range));
        }
        Ok(out)
    }

    /// Query chunks by time range only (no spatial filtering).
    ///
    /// Like [`query_chunks`](Self::query_chunks) but loads the full
    /// hierarchy and skips the bounding-box intersection test. Returns
    /// every chunk whose node temporal range overlaps `[start, end]`,
    /// each paired with its candidate index range.
    pub async fn query_chunks_by_time<S: ByteSource>(
        &mut self,
        reader: &mut CopcStreamingReader<S>,
        start: GpsTime,
        end: GpsTime,
        fields: Fields,
    ) -> Result<Vec<(Chunk, std::ops::Range<u32>)>, TemporalError> {
        reader
            .load_all_hierarchy()
            .await
            .map_err(TemporalError::Copc)?;

        self.load_pages_for_time_range(reader.source(), start, end)
            .await?;

        let stride = self.stride;

        let matches: Vec<_> = self
            .nodes_in_range(start, end)
            .into_iter()
            .filter_map(|e| {
                let hier = reader.get(&e.key)?;
                let range = e.estimate_point_range(start, end, stride, hier.point_count);
                if range.is_empty() {
                    return None;
                }
                Some((e.key, range))
            })
            .collect();

        let mut out = Vec::with_capacity(matches.len());
        for (key, range) in matches {
            let chunk = reader
                .fetch_chunk(&key, fields)
                .await
                .map_err(TemporalError::Copc)?;
            out.push((chunk, range));
        }
        Ok(out)
    }

    // ==================== Simple API ====================
    //
    // One-line entry points for the common case. Internally wrap the
    // fast API with `Fields::ALL` and materialize matching points.

    /// Query points by time range and spatial bounds.
    ///
    /// Loads the relevant hierarchy and temporal pages, fetches matching
    /// chunks, and returns only the points that fall inside both the time
    /// window and the bounding box.
    ///
    /// Thin wrapper over [`query_chunks`](Self::query_chunks) with
    /// `Fields::ALL`. Prefer `query_chunks` when you don't need every
    /// field materialized as `las::Point` values.
    ///
    /// ```rust,ignore
    /// let points = temporal.query_points(
    ///     &mut reader, &query_box, start, end,
    /// ).await?;
    /// ```
    pub async fn query_points<S: ByteSource>(
        &mut self,
        reader: &mut CopcStreamingReader<S>,
        bounds: &copc_streaming::Aabb,
        start: GpsTime,
        end: GpsTime,
    ) -> Result<Vec<las::Point>, TemporalError> {
        let chunks_with_ranges = self
            .query_chunks(reader, bounds, start, end, Fields::ALL)
            .await?;

        let mut all_points = Vec::new();
        for (chunk, range) in chunks_with_ranges {
            // Zero-copy filter walk: inspect PointRef-level accessors in
            // the candidate range and collect indices that match both the
            // time window and the bounding box.
            let start_idx = range.start as usize;
            let end_idx = (range.end as usize).min(chunk.point_count());
            let mut matching = Vec::with_capacity(end_idx - start_idx);
            for i in start_idx..end_idx {
                let p = chunk.cloud().point(i);
                let Some(t) = p.gps_time() else {
                    continue;
                };
                if t < start.0 || t > end.0 {
                    continue;
                }
                let (x, y, z) = (p.x(), p.y(), p.z());
                if x < bounds.min[0]
                    || x > bounds.max[0]
                    || y < bounds.min[1]
                    || y > bounds.max[1]
                    || z < bounds.min[2]
                    || z > bounds.max[2]
                {
                    continue;
                }
                matching.push(i as u32);
            }
            all_points.extend(chunk.points_at(&matching)?);
        }
        Ok(all_points)
    }

    /// Query points by time range only (no spatial filtering).
    ///
    /// Loads all hierarchy pages and temporal pages that overlap the time
    /// range, then returns points within the time window.
    ///
    /// Thin wrapper over [`query_chunks_by_time`](Self::query_chunks_by_time)
    /// with `Fields::ALL`.
    pub async fn query_points_by_time<S: ByteSource>(
        &mut self,
        reader: &mut CopcStreamingReader<S>,
        start: GpsTime,
        end: GpsTime,
    ) -> Result<Vec<las::Point>, TemporalError> {
        let chunks_with_ranges = self
            .query_chunks_by_time(reader, start, end, Fields::ALL)
            .await?;

        let mut all_points = Vec::new();
        for (chunk, range) in chunks_with_ranges {
            let start_idx = range.start as usize;
            let end_idx = (range.end as usize).min(chunk.point_count());
            let mut matching = Vec::with_capacity(end_idx - start_idx);
            for i in start_idx..end_idx {
                let p = chunk.cloud().point(i);
                let Some(t) = p.gps_time() else {
                    continue;
                };
                if t >= start.0 && t <= end.0 {
                    matching.push(i as u32);
                }
            }
            all_points.extend(chunk.points_at(&matching)?);
        }
        Ok(all_points)
    }

    fn parse_page(&mut self, data: &[u8]) -> Result<(), TemporalError> {
        let mut r = Cursor::new(data);

        while (r.position() as usize) < data.len() {
            if r.position() as usize + 20 > data.len() {
                break;
            }

            let level = r.read_i32::<LittleEndian>()?;
            let x = r.read_i32::<LittleEndian>()?;
            let y = r.read_i32::<LittleEndian>()?;
            let z = r.read_i32::<LittleEndian>()?;
            let sample_count = r.read_u32::<LittleEndian>()?;

            let key = VoxelKey { level, x, y, z };

            if sample_count == 0 {
                // Page pointer: 28 more bytes
                let child_offset = r.read_u64::<LittleEndian>()?;
                let child_size = r.read_u32::<LittleEndian>()?;
                let time_min = r.read_f64::<LittleEndian>()?;
                let time_max = r.read_f64::<LittleEndian>()?;

                self.pending_pages.push(PendingPage {
                    offset: child_offset,
                    size: child_size,
                    subtree_time_min: time_min,
                    subtree_time_max: time_max,
                });
            } else {
                let mut samples = Vec::with_capacity(sample_count as usize);
                for _ in 0..sample_count {
                    samples.push(GpsTime(r.read_f64::<LittleEndian>()?));
                }

                self.entries
                    .insert(key, NodeTemporalEntry::new(key, samples));
            }
        }

        Ok(())
    }
}

fn parse_temporal_header(data: &[u8]) -> Result<TemporalHeader, TemporalError> {
    if data.len() < 32 {
        return Err(TemporalError::TruncatedHeader);
    }
    let mut r = Cursor::new(data);
    let version = r.read_u32::<LittleEndian>()?;
    if version != 1 {
        return Err(TemporalError::UnsupportedVersion(version));
    }
    let stride = r.read_u32::<LittleEndian>()?;
    if stride < 1 {
        return Err(TemporalError::InvalidStride(stride));
    }
    let node_count = r.read_u32::<LittleEndian>()?;
    let page_count = r.read_u32::<LittleEndian>()?;
    let root_page_offset = r.read_u64::<LittleEndian>()?;
    let root_page_size = r.read_u32::<LittleEndian>()?;
    let _reserved = r.read_u32::<LittleEndian>()?;

    Ok(TemporalHeader {
        version,
        stride,
        node_count,
        page_count,
        root_page_offset,
        root_page_size,
    })
}
