use std::collections::HashMap;
use std::io::Cursor;
use std::ops::Range;

use byteorder::{LittleEndian, ReadBytesExt};

use copc_streaming::VoxelKey;

use crate::error::TemporalError;
use crate::gps_time::GpsTime;
use crate::vlr::VlrData;

/// Per-node temporal data: a set of sampled GPS timestamps.
#[derive(Debug, Clone)]
pub struct NodeTemporalEntry {
    /// The octree node this entry describes.
    pub key: VoxelKey,
    samples: Vec<GpsTime>,
}

impl NodeTemporalEntry {
    /// Create a new entry with the given key and samples.
    ///
    /// # Panics
    ///
    /// Panics if `samples` is empty — every node must have at least one sample.
    pub fn new(key: VoxelKey, samples: Vec<GpsTime>) -> Self {
        assert!(
            !samples.is_empty(),
            "NodeTemporalEntry requires at least one sample"
        );
        Self { key, samples }
    }

    /// The sampled GPS time values for this node.
    pub fn samples(&self) -> &[GpsTime] {
        &self.samples
    }

    /// Returns (min_time, max_time) for this node.
    pub fn time_range(&self) -> (GpsTime, GpsTime) {
        (self.samples[0], self.samples[self.samples.len() - 1])
    }

    /// Returns true if this node may contain points in [start, end].
    pub fn overlaps(&self, start: GpsTime, end: GpsTime) -> bool {
        let (min, max) = self.time_range();
        // Node overlaps if its max >= start AND its min <= end
        max >= start && min <= end
    }

    /// Estimate the point index range within the decompressed chunk for a time range.
    ///
    /// Implements the binary search logic from spec section 8.2:
    /// 1. Find first sample index `i` where `samples[i] >= t_start`
    /// 2. Find last sample index `j` where `samples[j] <= t_end`
    /// 3. Start point = i * stride
    /// 4. End point = min(j * stride + stride - 1, point_count - 1)
    pub fn estimate_point_range(
        &self,
        start: GpsTime,
        end: GpsTime,
        stride: u32,
        point_count: u32,
    ) -> Range<u32> {
        if point_count == 0 {
            return 0..0;
        }

        // First index i where samples[i] >= start
        let i = self.samples.partition_point(|s| *s < start);

        // Last index j where samples[j] <= end
        // partition_point finds first index where samples[j] > end, subtract 1
        let past_end = self.samples.partition_point(|s| *s <= end);

        if i >= past_end {
            // No samples in range
            return 0..0;
        }
        let j = past_end - 1;

        let start_point = (i as u64 * stride as u64).min(point_count as u64) as u32;
        let end_point =
            ((j as u64 * stride as u64 + stride as u64 - 1).min(point_count as u64 - 1)) as u32;

        start_point..(end_point + 1)
    }
}

/// The top-level temporal index parsed from an EVLR.
/// Not part of the public API — used internally and in tests.
#[derive(Debug, Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TemporalIndex {
    version: u32,
    stride: u32,
    entries: HashMap<VoxelKey, NodeTemporalEntry>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl TemporalIndex {
    /// Parse the temporal index from a list of EVLRs.
    ///
    /// Returns `Ok(None)` if no temporal EVLR is present.
    /// Returns `Err` if the EVLR is present but malformed.
    pub fn from_evlrs(evlrs: &[VlrData]) -> Result<Option<Self>, TemporalError> {
        let vlr = evlrs
            .iter()
            .find(|v| v.user_id == "copc_temporal" && v.record_id == 1000);

        let vlr = match vlr {
            Some(v) => v,
            None => return Ok(None),
        };

        let data = &vlr.data;
        if data.len() < 32 {
            return Err(TemporalError::TruncatedHeader);
        }

        let mut cursor = Cursor::new(data);

        let version = cursor.read_u32::<LittleEndian>()?;
        if version != 1 {
            return Err(TemporalError::UnsupportedVersion(version));
        }

        let stride = cursor.read_u32::<LittleEndian>()?;
        if stride < 1 {
            return Err(TemporalError::InvalidStride(stride));
        }

        let node_count = cursor.read_u32::<LittleEndian>()?;
        let _page_count = cursor.read_u32::<LittleEndian>()?;
        let _root_page_offset = cursor.read_u64::<LittleEndian>()?;
        let _root_page_size = cursor.read_u32::<LittleEndian>()?;
        let _reserved = cursor.read_u32::<LittleEndian>()?;

        // For local file access, all pages are contiguous after the header.
        // Scan sequentially, skipping page pointers (sample_count == 0).
        let mut entries = HashMap::with_capacity(node_count as usize);

        while entries.len() < node_count as usize {
            let level = match cursor.read_i32::<LittleEndian>() {
                Ok(v) => v,
                Err(e) => return Err(TemporalError::Io(e)),
            };
            let x = cursor.read_i32::<LittleEndian>()?;
            let y = cursor.read_i32::<LittleEndian>()?;
            let z = cursor.read_i32::<LittleEndian>()?;
            let sample_count = cursor.read_u32::<LittleEndian>()?;

            if sample_count == 0 {
                // Page pointer: skip the remaining 28 bytes
                // (child_page_offset u64 + child_page_size u32 + subtree_time_min f64 + subtree_time_max f64)
                cursor.set_position(cursor.position() + 28);
                continue;
            }

            let mut samples = Vec::with_capacity(sample_count as usize);
            for _ in 0..sample_count {
                let t = cursor.read_f64::<LittleEndian>()?;
                samples.push(GpsTime(t));
            }

            let key = VoxelKey { level, x, y, z };

            entries.insert(key, NodeTemporalEntry { key, samples });
        }

        Ok(Some(TemporalIndex {
            version,
            stride,
            entries,
        }))
    }

    /// Look up the temporal entry for a given voxel key.
    pub fn get(&self, key: &VoxelKey) -> Option<&NodeTemporalEntry> {
        self.entries.get(key)
    }

    /// Return all node entries whose time range overlaps [start, end].
    pub fn nodes_in_range(&self, start: GpsTime, end: GpsTime) -> Vec<&NodeTemporalEntry> {
        self.entries
            .values()
            .filter(|e| e.overlaps(start, end))
            .collect()
    }

    /// The sampling stride.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// The format version.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The number of node entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::WriteBytesExt;

    /// Build a v2 EVLR payload: header (32 bytes) + single root page with all entries.
    fn build_evlr_payload(
        version: u32,
        stride: u32,
        nodes: &[(i32, i32, i32, i32, &[f64])],
    ) -> Vec<u8> {
        // Compute root page size
        let mut page_size: u32 = 0;
        for (_, _, _, _, samples) in nodes {
            page_size += 20 + samples.len() as u32 * 8;
        }

        let mut buf = Vec::new();
        // Header (32 bytes)
        buf.write_u32::<LittleEndian>(version).unwrap();
        buf.write_u32::<LittleEndian>(stride).unwrap();
        buf.write_u32::<LittleEndian>(nodes.len() as u32).unwrap();
        buf.write_u32::<LittleEndian>(1).unwrap(); // page_count
        buf.write_u64::<LittleEndian>(32).unwrap(); // root_page_offset (relative to EVLR data start, used as absolute in tests)
        buf.write_u32::<LittleEndian>(page_size).unwrap();
        buf.write_u32::<LittleEndian>(0).unwrap(); // reserved

        // Root page: all node entries
        for &(level, x, y, z, samples) in nodes {
            buf.write_i32::<LittleEndian>(level).unwrap();
            buf.write_i32::<LittleEndian>(x).unwrap();
            buf.write_i32::<LittleEndian>(y).unwrap();
            buf.write_i32::<LittleEndian>(z).unwrap();
            buf.write_u32::<LittleEndian>(samples.len() as u32).unwrap();
            for &s in samples.iter() {
                buf.write_f64::<LittleEndian>(s).unwrap();
            }
        }

        buf
    }

    fn make_vlr(user_id: &str, record_id: u16, data: Vec<u8>) -> VlrData {
        VlrData {
            user_id: user_id.to_string(),
            record_id,
            data,
        }
    }

    #[test]
    fn test_parse_roundtrip() {
        let samples_a: &[f64] = &[100.0, 200.0, 300.0];
        let samples_b: &[f64] = &[400.0, 500.0];
        let nodes = vec![(0, 0, 0, 0, samples_a), (1, 1, 0, 0, samples_b)];
        let data = build_evlr_payload(1, 10, &nodes);
        let vlr = make_vlr("copc_temporal", 1000, data);

        let index = TemporalIndex::from_evlrs(&[vlr]).unwrap().unwrap();
        assert_eq!(index.stride(), 10);
        assert_eq!(index.version(), 1);
        assert_eq!(index.len(), 2);

        let entry_a = index
            .get(&VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            })
            .unwrap();
        assert_eq!(entry_a.samples().len(), 3);
        assert_eq!(entry_a.samples()[0], GpsTime(100.0));
        assert_eq!(entry_a.samples()[2], GpsTime(300.0));

        let entry_b = index
            .get(&VoxelKey {
                level: 1,
                x: 1,
                y: 0,
                z: 0,
            })
            .unwrap();
        assert_eq!(entry_b.samples().len(), 2);
        assert_eq!(entry_b.time_range(), (GpsTime(400.0), GpsTime(500.0)));
    }

    #[test]
    fn test_no_temporal_evlr() {
        let vlr = make_vlr("copc", 1, vec![0; 160]);
        let result = TemporalIndex::from_evlrs(&[vlr]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_empty_evlr_list() {
        let result = TemporalIndex::from_evlrs(&[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_wrong_version() {
        let data = build_evlr_payload(99, 10, &[]);
        let vlr = make_vlr("copc_temporal", 1000, data);
        let result = TemporalIndex::from_evlrs(&[vlr]);
        assert!(matches!(result, Err(TemporalError::UnsupportedVersion(99))));
    }

    #[test]
    fn test_truncated_header() {
        let vlr = make_vlr("copc_temporal", 1000, vec![1, 0, 0, 0]); // only 4 bytes
        let result = TemporalIndex::from_evlrs(&[vlr]);
        assert!(matches!(result, Err(TemporalError::TruncatedHeader)));
    }

    #[test]
    fn test_truncated_node_data() {
        // Header says 1 node, but no node data follows
        let data = build_evlr_payload(1, 10, &[]);
        let mut modified = data.clone();
        // Patch node_count to 1
        modified[8] = 1;
        let vlr = make_vlr("copc_temporal", 1000, modified);
        let result = TemporalIndex::from_evlrs(&[vlr]);
        assert!(result.is_err());
    }

    #[test]
    fn test_overlaps_exact_boundaries() {
        let entry = NodeTemporalEntry {
            key: VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            },
            samples: vec![GpsTime(100.0), GpsTime(200.0), GpsTime(300.0)],
        };

        // Exact match on boundaries
        assert!(entry.overlaps(GpsTime(100.0), GpsTime(300.0)));
        // Query ends exactly at start
        assert!(entry.overlaps(GpsTime(50.0), GpsTime(100.0)));
        // Query starts exactly at end
        assert!(entry.overlaps(GpsTime(300.0), GpsTime(400.0)));
        // Query entirely before
        assert!(!entry.overlaps(GpsTime(0.0), GpsTime(99.9)));
        // Query entirely after
        assert!(!entry.overlaps(GpsTime(300.1), GpsTime(400.0)));
        // Query contains node
        assert!(entry.overlaps(GpsTime(0.0), GpsTime(1000.0)));
        // Query within node
        assert!(entry.overlaps(GpsTime(150.0), GpsTime(250.0)));
    }

    #[test]
    fn test_overlaps_single_sample() {
        let entry = NodeTemporalEntry {
            key: VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            },
            samples: vec![GpsTime(100.0)],
        };

        assert!(entry.overlaps(GpsTime(100.0), GpsTime(100.0)));
        assert!(entry.overlaps(GpsTime(50.0), GpsTime(150.0)));
        assert!(!entry.overlaps(GpsTime(50.0), GpsTime(99.9)));
        assert!(!entry.overlaps(GpsTime(100.1), GpsTime(200.0)));
    }

    #[test]
    fn test_nodes_in_range() {
        let samples_a: &[f64] = &[100.0, 200.0, 300.0];
        let samples_b: &[f64] = &[400.0, 500.0];
        let samples_c: &[f64] = &[600.0, 700.0, 800.0];
        let data = build_evlr_payload(
            1,
            10,
            &[
                (0, 0, 0, 0, samples_a),
                (1, 0, 0, 0, samples_b),
                (1, 1, 0, 0, samples_c),
            ],
        );
        let vlr = make_vlr("copc_temporal", 1000, data);
        let index = TemporalIndex::from_evlrs(&[vlr]).unwrap().unwrap();

        // Should match only node A and B
        let result = index.nodes_in_range(GpsTime(250.0), GpsTime(450.0));
        assert_eq!(result.len(), 2);

        // Should match only node C
        let result = index.nodes_in_range(GpsTime(650.0), GpsTime(750.0));
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0].key,
            VoxelKey {
                level: 1,
                x: 1,
                y: 0,
                z: 0
            }
        );
    }

    #[test]
    fn test_estimate_point_range_basic() {
        // Samples at indices 0, 10, 20, 30, 39 (stride=10, 40 points)
        let entry = NodeTemporalEntry {
            key: VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            },
            samples: vec![
                GpsTime(100.0),
                GpsTime(200.0),
                GpsTime(300.0),
                GpsTime(400.0),
                GpsTime(450.0),
            ],
        };

        // Query covers samples[1] through samples[3] (200..400)
        let range = entry.estimate_point_range(GpsTime(200.0), GpsTime(400.0), 10, 40);
        // i=1 (first sample >= 200), j=3 (last sample <= 400)
        // start = 1*10 = 10, end = min(3*10 + 9, 39) = 39
        assert_eq!(range, 10..40);

        // Query covers only sample[2] (300..300)
        let range = entry.estimate_point_range(GpsTime(300.0), GpsTime(300.0), 10, 40);
        // i=2, j=2
        // start = 20, end = min(29, 39) = 29
        assert_eq!(range, 20..30);
    }

    #[test]
    fn test_estimate_point_range_no_overlap() {
        let entry = NodeTemporalEntry {
            key: VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            },
            samples: vec![GpsTime(100.0), GpsTime(200.0), GpsTime(300.0)],
        };

        // Query entirely before
        let range = entry.estimate_point_range(GpsTime(0.0), GpsTime(50.0), 10, 30);
        assert_eq!(range, 0..0);

        // Query entirely after
        let range = entry.estimate_point_range(GpsTime(400.0), GpsTime(500.0), 10, 30);
        assert_eq!(range, 0..0);
    }

    #[test]
    fn test_estimate_point_range_stride_1() {
        let entry = NodeTemporalEntry {
            key: VoxelKey {
                level: 0,
                x: 0,
                y: 0,
                z: 0,
            },
            samples: vec![
                GpsTime(1.0),
                GpsTime(2.0),
                GpsTime(3.0),
                GpsTime(4.0),
                GpsTime(5.0),
            ],
        };

        // With stride 1, every point is sampled
        let range = entry.estimate_point_range(GpsTime(2.0), GpsTime(4.0), 1, 5);
        // i=1, j=3, start=1, end=min(3+0, 4)=3
        assert_eq!(range, 1..4);
    }
}
