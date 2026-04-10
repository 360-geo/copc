//! Point chunk fetching and LAZ decompression.

use std::io::Cursor;

use las::PointCloud;
use laz::LazVlr;
use laz::record::{LayeredPointRecordDecompressor, RecordDecompressor};

use crate::byte_source::ByteSource;
use crate::error::CopcError;
use crate::fields::Fields;
use crate::hierarchy::HierarchyEntry;
use crate::types::{Aabb, VoxelKey};

/// A decompressed point data chunk.
///
/// A `Chunk` wraps a [`las::PointCloud`] along with the [`VoxelKey`] of the
/// octree node it came from and the [`Fields`] mask that was used to decode
/// it. Column accessors (`intensity`, `gps_time`, `rgb`, …) are guarded by
/// the mask and return `None` for fields that were not decoded.
///
/// Construct via [`CopcStreamingReader::fetch_chunk`](crate::CopcStreamingReader::fetch_chunk)
/// or [`CopcStreamingReader::query_chunks`](crate::CopcStreamingReader::query_chunks).
#[non_exhaustive]
pub struct Chunk {
    /// The octree node this chunk belongs to.
    pub key: VoxelKey,
    /// Which fields were actually decompressed into this chunk.
    pub fields: Fields,
    cloud: PointCloud,
}

impl Chunk {
    /// Construct a `Chunk` from a [`VoxelKey`], a [`Fields`] mask, and a
    /// [`las::PointCloud`].
    ///
    /// Normally you'll get chunks from
    /// [`CopcStreamingReader::fetch_chunk`](crate::CopcStreamingReader::fetch_chunk);
    /// this constructor exists for callers that drive their own
    /// decompression pipeline (and for tests). The caller is responsible
    /// for ensuring that `fields` accurately describes which LAZ layers
    /// were decoded into `cloud` — otherwise the chunk's field guards will
    /// hide columns that are actually valid, or (worse) expose columns
    /// that contain zero'd bytes for skipped layers.
    pub fn new(key: VoxelKey, fields: Fields, cloud: PointCloud) -> Self {
        Self { key, fields, cloud }
    }

    /// Borrow the underlying [`las::PointCloud`] — an **unchecked** escape
    /// hatch to the raw byte-level accessors in `las`.
    ///
    /// Use this when [`Chunk`]'s higher-level methods don't expose what
    /// you need: `cloud.x_raw()`, `cloud.record_len()`, `cloud.raw_bytes()`,
    /// `cloud.iter()` for zero-copy `PointRef` walks, etc.
    ///
    /// # ⚠ Field guards are bypassed
    ///
    /// Calling `cloud.rgb()`, `cloud.gps_time()`, `cloud.intensity()`, or
    /// any `PointRef` accessor on bytes from this cloud does **not** check
    /// whether the underlying layer was actually decoded. If you call one
    /// of those on a chunk that was fetched without the corresponding
    /// [`Fields`] flag, you get a valid-looking iterator of zeros.
    ///
    /// Prefer the [`Chunk`] methods ([`rgb`](Self::rgb),
    /// [`gps_time`](Self::gps_time), etc.) — they return `None` when the
    /// field is absent, making the hazard impossible to hit accidentally.
    pub fn cloud(&self) -> &PointCloud {
        &self.cloud
    }

    /// Number of points in this chunk.
    pub fn point_count(&self) -> usize {
        self.cloud.len()
    }

    /// Whether this chunk contains zero points.
    pub fn is_empty(&self) -> bool {
        self.cloud.is_empty()
    }

    /// Materialize every point in this chunk as an owned [`las::Point`].
    ///
    /// Returns [`CopcError::PartialDecode`] if this chunk was decoded with a
    /// partial field mask — otherwise the resulting `las::Point`s would have
    /// silently-zero values for skipped fields.
    ///
    /// This is the bridge from the column-oriented [`Chunk`] API back to
    /// the simple `Vec<las::Point>` API. Prefer the column accessors
    /// ([`intensity`](Self::intensity), [`gps_time`](Self::gps_time), …)
    /// or `chunk.cloud().iter()` when you don't need owned values.
    pub fn to_points(&self) -> Result<Vec<las::Point>, CopcError> {
        if self.fields != Fields::ALL {
            return Err(CopcError::PartialDecode(self.fields));
        }
        self.cloud.to_points().map_err(CopcError::Las)
    }

    /// Materialize `las::Point` values only at the given indices.
    ///
    /// Pair with [`indices_in_bounds`](Self::indices_in_bounds) or any
    /// other index-producing filter to skip the materialization cost for
    /// rejected points entirely:
    ///
    /// ```rust,ignore
    /// let chunk = reader.fetch_chunk(&key, Fields::ALL).await?;
    /// let indices = chunk.indices_in_bounds(&bounds).unwrap();
    /// let points = chunk.points_at(&indices)?;
    /// ```
    ///
    /// Returns [`CopcError::PartialDecode`] if the chunk was decoded with
    /// a partial field mask, same as [`to_points`](Self::to_points).
    pub fn points_at(&self, indices: &[u32]) -> Result<Vec<las::Point>, CopcError> {
        if self.fields != Fields::ALL {
            return Err(CopcError::PartialDecode(self.fields));
        }
        let record_len = self.cloud.record_len();
        let format = self.cloud.format();
        let transforms = self.cloud.transforms();
        let bytes = self.cloud.raw_bytes();
        indices
            .iter()
            .map(|&i| {
                let start = i as usize * record_len;
                let end = start + record_len;
                let mut cursor = Cursor::new(&bytes[start..end]);
                let raw = las::raw::Point::read_from(&mut cursor, format)?;
                Ok(las::Point::new(raw, transforms))
            })
            .collect()
    }

    /// Iterate `(x, y, z)` world coordinates as `[f64; 3]`, or `None` if
    /// [`Fields::Z`] was not set.
    ///
    /// `x` and `y` are always decoded on LAS 1.4 layered formats (they
    /// share the always-on base layer with `return_number`,
    /// `number_of_returns` and `scanner_channel`), but `z` is its own
    /// skippable layer. A chunk without `Fields::Z` has zero bytes in the
    /// `z` slots; returning `None` here keeps the footgun out of reach.
    pub fn positions(&self) -> Option<impl Iterator<Item = [f64; 3]> + '_> {
        if !self.fields.contains(Fields::Z) {
            return None;
        }
        Some(self.cloud.iter().map(|p| [p.x(), p.y(), p.z()]))
    }

    /// Intensity column, or `None` if [`Fields::INTENSITY`] was not set
    /// or the format does not include intensity.
    pub fn intensity(&self) -> Option<impl Iterator<Item = u16> + '_> {
        if !self.fields.contains(Fields::INTENSITY) {
            return None;
        }
        Some(self.cloud.intensity())
    }

    /// Classification byte column, or `None` if [`Fields::CLASSIFICATION`]
    /// was not set.
    pub fn classification(&self) -> Option<impl Iterator<Item = u8> + '_> {
        if !self.fields.contains(Fields::CLASSIFICATION) {
            return None;
        }
        Some(self.cloud.classification())
    }

    /// Scan angle column in degrees, or `None` if [`Fields::SCAN_ANGLE`]
    /// was not set.
    pub fn scan_angle(&self) -> Option<impl Iterator<Item = f32> + '_> {
        if !self.fields.contains(Fields::SCAN_ANGLE) {
            return None;
        }
        Some(self.cloud.scan_angle_degrees())
    }

    /// User data byte column, or `None` if [`Fields::USER_DATA`] was not set.
    pub fn user_data(&self) -> Option<impl Iterator<Item = u8> + '_> {
        if !self.fields.contains(Fields::USER_DATA) {
            return None;
        }
        Some(self.cloud.user_data())
    }

    /// Point source ID column, or `None` if [`Fields::POINT_SOURCE_ID`]
    /// was not set.
    pub fn point_source_id(&self) -> Option<impl Iterator<Item = u16> + '_> {
        if !self.fields.contains(Fields::POINT_SOURCE_ID) {
            return None;
        }
        Some(self.cloud.point_source_id())
    }

    /// GPS time column, or `None` if [`Fields::GPS_TIME`] was not set or
    /// the format does not include GPS time.
    pub fn gps_time(&self) -> Option<impl Iterator<Item = f64> + '_> {
        if !self.fields.contains(Fields::GPS_TIME) {
            return None;
        }
        self.cloud.gps_time()
    }

    /// RGB column, or `None` if [`Fields::RGB`] was not set or the format
    /// does not include color.
    pub fn rgb(&self) -> Option<impl Iterator<Item = (u16, u16, u16)> + '_> {
        if !self.fields.contains(Fields::RGB) {
            return None;
        }
        self.cloud.rgb()
    }

    /// NIR column, or `None` if [`Fields::NIR`] was not set or the format
    /// does not include NIR.
    pub fn nir(&self) -> Option<impl Iterator<Item = u16> + '_> {
        if !self.fields.contains(Fields::NIR) {
            return None;
        }
        self.cloud.nir()
    }

    /// Indices of points inside `bounds`, or `None` if [`Fields::Z`] was
    /// not set.
    ///
    /// A 3D bounding-box intersection requires z, so a chunk without
    /// `Fields::Z` can't produce meaningful results — `None` makes that
    /// impossible to misuse. Pair the returned indices with any column
    /// iterator on the chunk to produce a filtered view without
    /// materializing `las::Point` values.
    pub fn indices_in_bounds(&self, bounds: &Aabb) -> Option<Vec<u32>> {
        let positions = self.positions()?;
        Some(
            positions
                .enumerate()
                .filter_map(|(i, [x, y, z])| {
                    let inside = x >= bounds.min[0]
                        && x <= bounds.max[0]
                        && y >= bounds.min[1]
                        && y <= bounds.max[1]
                        && z >= bounds.min[2]
                        && z <= bounds.max[2];
                    inside.then_some(i as u32)
                })
                .collect(),
        )
    }
}

/// Fetch and decompress a single chunk, decoding only the layers requested
/// by `fields`.
pub(crate) async fn fetch_and_decompress(
    source: &impl ByteSource,
    entry: &HierarchyEntry,
    laz_vlr: &LazVlr,
    header: &las::Header,
    fields: Fields,
) -> Result<Chunk, CopcError> {
    let compressed = source
        .read_range(entry.offset, entry.byte_size as u64)
        .await?;

    let format = *header.point_format();
    let transforms = *header.transforms();

    let record_len = format.len() as usize + format.extra_bytes as usize;
    let decompressed_size = entry.point_count as usize * record_len;
    let mut decompressed = vec![0u8; decompressed_size];

    decompress_copc_chunk(&compressed, &mut decompressed, laz_vlr, fields)?;

    let cloud = PointCloud::from_raw_bytes(format, transforms, decompressed)?;

    Ok(Chunk {
        key: entry.key,
        fields,
        cloud,
    })
}

/// Decompress a single COPC chunk.
///
/// COPC chunks are independently compressed and do NOT start with the 8-byte
/// chunk table offset that standard LAZ files have. We use
/// `LayeredPointRecordDecompressor` directly (the same approach as copc-rs)
/// to bypass `LasZipDecompressor`'s chunk table handling.
///
/// `fields` is wired through `set_selection` so that LAZ skips arithmetic
/// decoding of omitted layers. On LAS 1.4 layered formats (6/7/8 — the
/// formats COPC mandates) this is a real CPU saving; on pre-1.4 formats
/// the layered decompressor ignores the selection and decodes everything,
/// but those aren't valid COPC point formats anyway.
fn decompress_copc_chunk(
    compressed: &[u8],
    decompressed: &mut [u8],
    laz_vlr: &LazVlr,
    fields: Fields,
) -> Result<(), CopcError> {
    let src = Cursor::new(compressed);
    let mut decompressor = LayeredPointRecordDecompressor::new(src);
    decompressor.set_fields_from(laz_vlr.items())?;
    decompressor.set_selection(fields.to_laz_selection());
    decompressor.decompress_many(decompressed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use las::point::Format;
    use las::raw::point::{Flags, ScanAngle};

    /// Build a `PointCloud` in format 7 (has gps_time + rgb) with `n` points
    /// whose fields are a function of their index: `x = i, y = i + 1,
    /// z = i + 2, intensity = 100 + i, gps_time = 1000 + i, rgb = (i, i, i)`.
    /// Unit transforms (scale=1, offset=0) so raw == world for easy asserts.
    fn build_test_cloud(n: i32) -> las::PointCloud {
        let format = Format::new(7).unwrap();
        let unit = las::Transform {
            scale: 1.0,
            offset: 0.0,
        };
        let transforms = las::Vector {
            x: unit,
            y: unit,
            z: unit,
        };
        let mut buf = Vec::new();
        for i in 0..n {
            let rp = las::raw::Point {
                x: i,
                y: i + 1,
                z: i + 2,
                intensity: 100 + i as u16,
                // Format 7 is extended -> ThreeByte flags.
                flags: Flags::ThreeByte(0, 0, 2),
                scan_angle: ScanAngle::Scaled(0),
                user_data: 0,
                point_source_id: 0,
                gps_time: Some(1000.0 + f64::from(i)),
                color: Some(las::Color {
                    red: i as u16,
                    green: i as u16,
                    blue: i as u16,
                }),
                waveform: None,
                nir: None,
                extra_bytes: Vec::new(),
            };
            rp.write_to(&mut buf, &format).unwrap();
        }
        las::PointCloud::from_raw_bytes(format, transforms, buf).unwrap()
    }

    fn make_chunk(cloud: las::PointCloud, fields: Fields) -> Chunk {
        Chunk {
            key: VoxelKey::ROOT,
            fields,
            cloud,
        }
    }

    #[test]
    fn point_count_and_empty() {
        let cloud = build_test_cloud(5);
        let chunk = make_chunk(cloud, Fields::ALL);
        assert_eq!(chunk.point_count(), 5);
        assert!(!chunk.is_empty());
    }

    #[test]
    fn positions_iterate_correctly() {
        let chunk = make_chunk(build_test_cloud(3), Fields::ALL);
        let positions: Vec<_> = chunk.positions().unwrap().collect();
        assert_eq!(positions.len(), 3);
        assert_eq!(positions[0], [0.0, 1.0, 2.0]);
        assert_eq!(positions[2], [2.0, 3.0, 4.0]);
    }

    #[test]
    fn positions_returns_none_without_z_field() {
        let chunk = make_chunk(build_test_cloud(3), Fields::empty());
        assert!(chunk.positions().is_none());
    }

    #[test]
    fn gps_time_column_guarded_by_fields() {
        let chunk = make_chunk(build_test_cloud(3), Fields::Z);
        assert!(
            chunk.gps_time().is_none(),
            "GPS_TIME not in mask -> column should be None"
        );
    }

    #[test]
    fn gps_time_column_present_when_fields_allow() {
        let chunk = make_chunk(build_test_cloud(3), Fields::ALL);
        let times: Vec<_> = chunk.gps_time().unwrap().collect();
        assert_eq!(times, vec![1000.0, 1001.0, 1002.0]);
    }

    #[test]
    fn rgb_column_guarded_by_fields() {
        let chunk = make_chunk(build_test_cloud(3), Fields::Z | Fields::GPS_TIME);
        assert!(chunk.rgb().is_none());
    }

    #[test]
    fn rgb_column_present_when_fields_allow() {
        let chunk = make_chunk(build_test_cloud(3), Fields::ALL);
        let rgb: Vec<_> = chunk.rgb().unwrap().collect();
        assert_eq!(rgb, vec![(0, 0, 0), (1, 1, 1), (2, 2, 2)]);
    }

    #[test]
    fn intensity_column_guarded() {
        let chunk = make_chunk(build_test_cloud(3), Fields::Z);
        assert!(chunk.intensity().is_none());
        let chunk = make_chunk(build_test_cloud(3), Fields::Z | Fields::INTENSITY);
        let intensities: Vec<_> = chunk.intensity().unwrap().collect();
        assert_eq!(intensities, vec![100, 101, 102]);
    }

    #[test]
    fn to_points_refuses_partial_fields() {
        let chunk = make_chunk(build_test_cloud(3), Fields::Z);
        let r = chunk.to_points();
        assert!(matches!(r, Err(CopcError::PartialDecode(_))));
    }

    #[test]
    fn to_points_succeeds_with_all_fields() {
        let chunk = make_chunk(build_test_cloud(3), Fields::ALL);
        let pts = chunk.to_points().unwrap();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0].intensity, 100);
        assert_eq!(pts[1].gps_time, Some(1001.0));
    }

    #[test]
    fn points_at_materializes_subset() {
        let chunk = make_chunk(build_test_cloud(5), Fields::ALL);
        let pts = chunk.points_at(&[0, 2, 4]).unwrap();
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0].x, 0.0);
        assert_eq!(pts[1].x, 2.0);
        assert_eq!(pts[2].x, 4.0);
        assert_eq!(pts[0].intensity, 100);
        assert_eq!(pts[2].intensity, 104);
    }

    #[test]
    fn points_at_refuses_partial_fields() {
        let chunk = make_chunk(build_test_cloud(3), Fields::Z);
        let r = chunk.points_at(&[0, 1]);
        assert!(matches!(r, Err(CopcError::PartialDecode(_))));
    }

    #[test]
    fn points_at_empty_slice_returns_empty_vec() {
        let chunk = make_chunk(build_test_cloud(3), Fields::ALL);
        let pts = chunk.points_at(&[]).unwrap();
        assert!(pts.is_empty());
    }

    #[test]
    fn indices_in_bounds_filters_correctly() {
        // Points: (0,1,2), (1,2,3), (2,3,4), (3,4,5), (4,5,6)
        let chunk = make_chunk(build_test_cloud(5), Fields::ALL);
        let bounds = Aabb {
            min: [1.0, 0.0, 0.0],
            max: [2.0, 10.0, 10.0],
        };
        // x in [1, 2]: indices 1 and 2.
        assert_eq!(chunk.indices_in_bounds(&bounds).unwrap(), vec![1, 2]);
    }

    #[test]
    fn indices_in_bounds_empty_when_outside() {
        let chunk = make_chunk(build_test_cloud(5), Fields::ALL);
        let bounds = Aabb {
            min: [100.0, 100.0, 100.0],
            max: [200.0, 200.0, 200.0],
        };
        assert!(chunk.indices_in_bounds(&bounds).unwrap().is_empty());
    }

    #[test]
    fn indices_in_bounds_returns_none_without_z_field() {
        let chunk = make_chunk(build_test_cloud(5), Fields::empty());
        let bounds = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [10.0, 10.0, 10.0],
        };
        assert!(chunk.indices_in_bounds(&bounds).is_none());
    }
}
