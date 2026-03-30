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
//! use copc_streaming::{CopcStreamingReader, FileSource};
//! use copc_temporal::{GpsTime, TemporalCache};
//!
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("survey.copc.laz")?,
//! ).await?;
//! reader.load_all_hierarchy().await?;
//!
//! // Load the temporal index (returns None if the file has no temporal EVLR).
//! // from_reader loads only the header and root page — not the entire index.
//! let mut temporal = match TemporalCache::from_reader(&reader).await? {
//!     Some(t) => t,
//!     None => return Ok(()), // no temporal index in this file
//! };
//!
//! let start = GpsTime(1_000_000.0);
//! let end   = GpsTime(1_000_010.0);
//! let root_bounds = reader.copc_info().root_bounds();
//!
//! // Query loads only the temporal pages that overlap the time range,
//! // then returns matching nodes. Pages outside the range are never fetched.
//! for entry in temporal.query(reader.source(), start, end).await? {
//!     let hier = reader.get(&entry.key).unwrap();
//!     if !entry.key.bounds(&root_bounds).intersects(&my_query_box) { continue; }
//!
//!     // Estimate which points fall in the time window, then read only those.
//!     let range = entry.estimate_point_range(
//!         start, end, temporal.stride(), hier.point_count,
//!     );
//!     let chunk = reader.fetch_chunk(&entry.key).await?;
//!     let points = reader.read_points_range(&chunk, range)?;
//! }
//! ```
//!
//! # Incremental page loading
//!
//! # How it works
//!
//! [`TemporalCache::from_reader`] loads the header and root page.
//! [`TemporalCache::query`] then loads only the pages whose subtree time bounds
//! overlap the requested range and returns matching nodes — pages outside the
//! range are never fetched.
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
