//! LAS header + COPC info parsing.
//!
//! Uses the `las` crate for standard LAS/VLR parsing.
//! Only the COPC info VLR (160 bytes) is parsed by us — it's COPC-specific.

use std::io::Cursor;

use byteorder::{LittleEndian, ReadBytesExt};
use las::raw;
use laz::LazVlr;

use crate::error::CopcError;
use crate::types::Aabb;

/// Parsed COPC file header.
///
/// LAS-standard fields come from `las::Header`.
/// COPC-specific fields are in `copc_info`.
pub struct CopcHeader {
    pub(crate) las_header: las::Header,
    pub(crate) copc_info: CopcInfo,
    pub(crate) laz_vlr: LazVlr,
    pub(crate) evlr_offset: u64,
    pub(crate) evlr_count: u32,
}

impl CopcHeader {
    /// Full LAS header with transforms, bounds, point format, etc.
    pub fn las_header(&self) -> &las::Header {
        &self.las_header
    }

    /// COPC-specific info (octree center, halfsize, hierarchy location).
    pub fn copc_info(&self) -> &CopcInfo {
        &self.copc_info
    }

    /// LAZ decompression parameters.
    pub fn laz_vlr(&self) -> &LazVlr {
        &self.laz_vlr
    }

    /// File offset where EVLRs start.
    pub fn evlr_offset(&self) -> u64 {
        self.evlr_offset
    }

    /// Number of EVLRs.
    pub fn evlr_count(&self) -> u32 {
        self.evlr_count
    }
}

/// COPC info VLR payload (160 bytes). This is COPC-specific — not part of the LAS standard.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CopcInfo {
    /// Centre of the root octree cube `[x, y, z]`.
    pub center: [f64; 3],
    /// Half the side length of the root octree cube.
    pub halfsize: f64,
    /// Spacing at the finest octree level.
    pub spacing: f64,
    /// File offset of the root hierarchy page.
    pub root_hier_offset: u64,
    /// Size of the root hierarchy page in bytes.
    pub root_hier_size: u64,
    /// Minimum GPS time across all points.
    pub gpstime_minimum: f64,
    /// Maximum GPS time across all points.
    pub gpstime_maximum: f64,
}

impl CopcInfo {
    /// Compute the root octree bounding box from center + halfsize.
    pub fn root_bounds(&self) -> Aabb {
        Aabb {
            min: [
                self.center[0] - self.halfsize,
                self.center[1] - self.halfsize,
                self.center[2] - self.halfsize,
            ],
            max: [
                self.center[0] + self.halfsize,
                self.center[1] + self.halfsize,
                self.center[2] + self.halfsize,
            ],
        }
    }

    /// Compute the octree level needed for a given point spacing (in the
    /// same units as the point coordinates, typically meters).
    ///
    /// At level 0 the average distance between points equals
    /// [`CopcInfo::spacing`]. Each deeper level halves the distance. This
    /// returns the shallowest level where the point spacing is ≤ `resolution`.
    ///
    /// For example, if the file's base spacing is 10 m and you request 0.5 m,
    /// you get level 5 (10 → 5 → 2.5 → 1.25 → 0.625 → 0.3125).
    ///
    /// Use the returned level as `max_level` in
    /// [`CopcStreamingReader::query_points_to_level`](crate::CopcStreamingReader::query_points_to_level)
    /// or
    /// [`CopcStreamingReader::load_hierarchy_for_bounds_to_level`](crate::CopcStreamingReader::load_hierarchy_for_bounds_to_level).
    pub fn level_for_resolution(&self, resolution: f64) -> i32 {
        if resolution <= 0.0 || self.spacing <= 0.0 {
            return 0;
        }
        (self.spacing / resolution).log2().ceil().max(0.0) as i32
    }

    fn parse(data: &[u8]) -> Result<Self, CopcError> {
        if data.len() < 160 {
            return Err(CopcError::CopcInfoNotFound);
        }
        let mut r = Cursor::new(data);
        let center_x = r.read_f64::<LittleEndian>()?;
        let center_y = r.read_f64::<LittleEndian>()?;
        let center_z = r.read_f64::<LittleEndian>()?;
        let halfsize = r.read_f64::<LittleEndian>()?;
        let spacing = r.read_f64::<LittleEndian>()?;
        let root_hier_offset = r.read_u64::<LittleEndian>()?;
        let root_hier_size = r.read_u64::<LittleEndian>()?;
        let gpstime_minimum = r.read_f64::<LittleEndian>()?;
        let gpstime_maximum = r.read_f64::<LittleEndian>()?;
        Ok(CopcInfo {
            center: [center_x, center_y, center_z],
            halfsize,
            spacing,
            root_hier_offset,
            root_hier_size,
            gpstime_minimum,
            gpstime_maximum,
        })
    }
}

/// Parse COPC header from a byte buffer.
///
/// The buffer must contain the LAS header and all VLRs (typically first ~64KB).
pub(crate) fn parse_header(data: &[u8]) -> Result<CopcHeader, CopcError> {
    let mut cursor = Cursor::new(data);

    // Parse the raw LAS header
    let raw_header = raw::Header::read_from(&mut cursor)?;

    let evlr_offset = raw_header
        .evlr
        .as_ref()
        .map_or(0, |e| e.start_of_first_evlr);
    let evlr_count = raw_header.evlr.as_ref().map_or(0, |e| e.number_of_evlrs);
    let number_of_vlrs = raw_header.number_of_variable_length_records;
    let header_size = raw_header.header_size;

    // Build the high-level header (for transforms, point format, etc.)
    let mut builder = las::Builder::new(raw_header)?;

    // Seek to VLR start and parse all VLRs
    cursor.set_position(header_size as u64);

    let mut copc_info = None;
    let mut laz_vlr = None;

    for _ in 0..number_of_vlrs {
        let raw_vlr = raw::Vlr::read_from(&mut cursor, false)?;
        let vlr = las::Vlr::new(raw_vlr);

        match (vlr.user_id.as_str(), vlr.record_id) {
            ("copc", 1) => {
                copc_info = Some(CopcInfo::parse(&vlr.data)?);
            }
            ("laszip encoded", 22204) => {
                laz_vlr = Some(LazVlr::read_from(vlr.data.as_slice())?);
            }
            _ => {}
        }

        builder.vlrs.push(vlr);
    }

    let las_header = builder.into_header()?;
    let copc_info = copc_info.ok_or(CopcError::CopcInfoNotFound)?;
    let laz_vlr = laz_vlr.ok_or(CopcError::LazVlrNotFound)?;

    Ok(CopcHeader {
        las_header,
        copc_info,
        laz_vlr,
        evlr_offset,
        evlr_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copc_info_root_bounds() {
        let info = CopcInfo {
            center: [100.0, 200.0, 10.0],
            halfsize: 50.0,
            spacing: 1.0,
            root_hier_offset: 0,
            root_hier_size: 0,
            gpstime_minimum: 0.0,
            gpstime_maximum: 0.0,
        };
        let b = info.root_bounds();
        assert_eq!(b.min, [50.0, 150.0, -40.0]);
        assert_eq!(b.max, [150.0, 250.0, 60.0]);
    }

    #[test]
    fn test_level_for_resolution() {
        let info = CopcInfo {
            center: [0.0, 0.0, 0.0],
            halfsize: 500.0,
            spacing: 10.0,
            root_hier_offset: 0,
            root_hier_size: 0,
            gpstime_minimum: 0.0,
            gpstime_maximum: 0.0,
        };

        // spacing=10, resolution=10 → level 0 (no refinement needed)
        assert_eq!(info.level_for_resolution(10.0), 0);
        // spacing=10, resolution=5 → level 1 (one halving)
        assert_eq!(info.level_for_resolution(5.0), 1);
        // spacing=10, resolution=2.5 → level 2
        assert_eq!(info.level_for_resolution(2.5), 2);
        // spacing=10, resolution=1.0 → ceil(log2(10)) = 4
        assert_eq!(info.level_for_resolution(1.0), 4);
        // spacing=10, resolution=0.5 → ceil(log2(20)) = 5
        assert_eq!(info.level_for_resolution(0.5), 5);
        // resolution larger than spacing → level 0
        assert_eq!(info.level_for_resolution(20.0), 0);
        // edge case: zero/negative resolution → level 0
        assert_eq!(info.level_for_resolution(0.0), 0);
        assert_eq!(info.level_for_resolution(-1.0), 0);
    }
}
