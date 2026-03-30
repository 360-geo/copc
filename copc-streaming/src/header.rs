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
pub struct CopcInfo {
    pub center: [f64; 3],
    pub halfsize: f64,
    pub spacing: f64,
    pub root_hier_offset: u64,
    pub root_hier_size: u64,
    pub gpstime_minimum: f64,
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
}
