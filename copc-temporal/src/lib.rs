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
//! # Low-level access
//!
//! For full control over page loading and chunk processing, use the building
//! blocks directly:
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
//! [`TemporalCache::query_points`] then loads the relevant hierarchy and
//! temporal pages, fetches matching chunks, and returns only the points
//! that fall inside both the bounding box and time window.
//!
//! For time-only queries (no spatial filter), use
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

/// Filter points to only those whose GPS time falls within `[start, end]`.
///
/// Points without a GPS time are excluded. Use after
/// [`CopcStreamingReader::read_points_range`](copc_streaming::CopcStreamingReader::read_points_range)
/// to trim points at the edges of an estimated temporal range.
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
