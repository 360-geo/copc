//! Reader for the [COPC Temporal Index Extension](https://github.com/360-geo/copc/blob/master/copc-temporal/docs/temporal-index-spec.md).
//!
//! When a COPC file contains data from multiple survey passes over the same area,
//! a spatial query alone returns points from *every* pass that touched that region.
//! The temporal index extension adds per-node GPS time metadata so that clients can
//! filter by time **before** decompressing any point data.
//!
//! This crate reads the temporal index incrementally via [`ByteSource`], matching
//! the streaming design of [`copc_streaming`].
//!
//! # Quick start
//!
//! ```rust,ignore
//! use copc_streaming::{Aabb, CopcStreamingReader, FileSource};
//! use copc_temporal::{GpsTime, TemporalCache};
//!
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("survey.copc.laz")?,
//! ).await?;
//!
//! let mut temporal = match TemporalCache::from_reader(&reader).await? {
//!     Some(t) => t,
//!     None => return Ok(()), // no temporal index in this file
//! };
//!
//! let start = GpsTime(1_000_000.0);
//! let end   = GpsTime(1_000_010.0);
//!
//! // One call: loads hierarchy + temporal pages, fetches chunks,
//! // filters by both bounds and time.
//! let points = temporal.query_points(
//!     &mut reader, &my_query_box, start, end,
//! ).await?;
//! ```
//!
//! # Fast path: select only the fields you need
//!
//! [`TemporalCache::query_points`] decodes every field and materializes
//! `las::Point` values. For hot paths,
//! [`TemporalCache::query_chunks`] returns `(Chunk, candidate_range)`
//! pairs with a caller-chosen [`Fields`](copc_streaming::Fields) mask and
//! lets you walk columns directly. `candidate_range` is the sub-range of
//! point indices within each chunk whose GPS times could possibly match
//! `[start, end]` based on the temporal index stride samples — use it to
//! skip points that are guaranteed not to match.
//!
//! ```rust,ignore
//! use copc_streaming::Fields;
//! use copc_temporal::indices_in_time_range;
//!
//! let chunks = temporal
//!     .query_chunks(
//!         &mut reader,
//!         &my_query_box,
//!         start,
//!         end,
//!         Fields::Z | Fields::GPS_TIME,
//!     )
//!     .await?;
//!
//! for (chunk, range) in &chunks {
//!     // Narrow to the candidate range first, then filter exactly.
//!     let precise: Vec<u32> = indices_in_time_range(chunk, start, end)
//!         .unwrap()
//!         .into_iter()
//!         .filter(|&i| (range.start..range.end).contains(&i))
//!         .collect();
//!     // ... walk chunk.positions() / chunk.gps_time() at these indices
//! }
//! ```
//!
//! # Low-level access
//!
//! For full control over page loading, use the building blocks directly:
//!
//! ```rust,ignore
//! // Load only temporal pages that overlap the time range.
//! temporal.load_pages_for_time_range(reader.source(), start, end).await?;
//!
//! // Find matching nodes, estimate point ranges, fetch chunks yourself.
//! for entry in temporal.nodes_in_range(start, end) {
//!     let range = entry.estimate_point_range(
//!         start, end, temporal.stride(), hier.point_count,
//!     );
//!     // ...
//! }
//! ```
//!
//! # How it works
//!
//! [`TemporalCache::from_reader`] loads the header and root page.
//! [`TemporalCache::query_chunks`] / [`TemporalCache::query_points`] then
//! load the relevant hierarchy and temporal pages, fetch matching chunks,
//! and return them (with candidate ranges) or the points inside both the
//! bounding box and time window.
//!
//! For time-only queries (no spatial filter), use
//! [`TemporalCache::query_chunks_by_time`] /
//! [`TemporalCache::query_points_by_time`].
//!
//! For advanced use cases you can call [`TemporalCache::load_pages_for_time_range`]
//! and [`TemporalCache::nodes_in_range`] separately, or
//! [`TemporalCache::load_all_pages`] to fetch the entire index at once.

mod error;
mod gps_time;
mod temporal_cache;
mod temporal_index;
mod vlr;

pub use error::TemporalError;
pub use gps_time::GpsTime;
pub use temporal_cache::{TemporalCache, TemporalHeader};
pub use temporal_index::NodeTemporalEntry;

// Re-export copc-streaming types that temporal consumers will need.
pub use copc_streaming::{Aabb, ByteSource, VoxelKey};

/// Indices of points whose GPS time falls within `[start, end]`.
///
/// Returns `None` if the chunk was not decoded with
/// [`copc_streaming::Fields::GPS_TIME`] or the underlying format does not
/// include GPS time. Pair the returned indices with any other column
/// iterator on the chunk to build a filtered view without materializing
/// `las::Point` values.
///
/// ```rust,ignore
/// use copc_streaming::Fields;
///
/// let chunk = reader.fetch_chunk(&key, Fields::Z | Fields::GPS_TIME).await?;
/// let indices = copc_temporal::indices_in_time_range(&chunk, start, end)
///     .expect("we asked for GPS_TIME");
/// for &i in &indices {
///     let p = chunk.point(i as usize);
///     // ...
/// }
/// ```
pub fn indices_in_time_range(
    chunk: &copc_streaming::Chunk,
    start: GpsTime,
    end: GpsTime,
) -> Option<Vec<u32>> {
    let times = chunk.gps_time()?;
    Some(
        times
            .enumerate()
            .filter_map(|(i, t)| (t >= start.0 && t <= end.0).then_some(i as u32))
            .collect(),
    )
}

/// Filter points to only those whose GPS time falls within `[start, end]`.
///
/// Points without a GPS time are excluded. Convenience for the simple
/// `Vec<las::Point>` API; prefer [`indices_in_time_range`] on a
/// [`copc_streaming::Chunk`] when you're already on the column-oriented path.
pub fn filter_points_by_time(
    points: Vec<las::Point>,
    start: GpsTime,
    end: GpsTime,
) -> Vec<las::Point> {
    points
        .into_iter()
        .filter(|p| p.gps_time.is_some_and(|t| t >= start.0 && t <= end.0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use copc_streaming::{Chunk, Fields, VoxelKey};
    use las::point::Format;
    use las::raw::point::{Flags, ScanAngle};

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
                y: i,
                z: i,
                intensity: 0,
                flags: Flags::ThreeByte(0, 0, 0),
                scan_angle: ScanAngle::Scaled(0),
                user_data: 0,
                point_source_id: 0,
                gps_time: Some(1000.0 + f64::from(i)),
                color: Some(las::Color {
                    red: 0,
                    green: 0,
                    blue: 0,
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
        Chunk::new(VoxelKey::ROOT, fields, cloud)
    }

    #[test]
    fn indices_in_time_range_returns_none_without_gps_time_field() {
        let chunk = make_chunk(build_test_cloud(5), Fields::Z);
        let r = indices_in_time_range(&chunk, GpsTime(1000.0), GpsTime(1003.0));
        assert!(r.is_none());
    }

    #[test]
    fn indices_in_time_range_filters_window() {
        let chunk = make_chunk(build_test_cloud(5), Fields::ALL);
        // gps times: 1000, 1001, 1002, 1003, 1004
        let inside = indices_in_time_range(&chunk, GpsTime(1001.0), GpsTime(1003.0)).unwrap();
        assert_eq!(inside, vec![1, 2, 3]);
    }

    #[test]
    fn indices_in_time_range_empty_when_outside() {
        let chunk = make_chunk(build_test_cloud(5), Fields::ALL);
        let inside = indices_in_time_range(&chunk, GpsTime(9000.0), GpsTime(10000.0)).unwrap();
        assert!(inside.is_empty());
    }

    #[test]
    fn filter_points_by_time_excludes_none_gps_time() {
        let p = las::Point {
            gps_time: None,
            ..Default::default()
        };
        let filtered = filter_points_by_time(vec![p], GpsTime(0.0), GpsTime(10.0));
        assert!(filtered.is_empty());
    }
}
