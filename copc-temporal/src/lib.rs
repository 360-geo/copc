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
//! if let Some(mut temporal) = TemporalCache::from_reader(&reader).await? {
//!     temporal.load_all_pages(reader.source()).await?;
//!
//!     // Find octree nodes that overlap a 10-second GPS time window.
//!     let start = GpsTime(1_000_000.0);
//!     let end   = GpsTime(1_000_010.0);
//!     let nodes = temporal.nodes_in_range(start, end);
//!
//!     for entry in &nodes {
//!         // Estimate the point sub-range within the node.
//!         let hier = reader.get(&entry.key).unwrap();
//!         let range = entry.estimate_point_range(
//!             start, end, temporal.stride(), hier.point_count as u32,
//!         );
//!         println!("{:?}: points {}..{}", entry.key, range.start, range.end);
//!     }
//! }
//! ```
//!
//! # Incremental page loading
//!
//! Like the spatial hierarchy, the temporal index is organised in pages.
//! [`TemporalCache::from_reader`] loads the header and root page. You can then:
//!
//! - call [`TemporalCache::load_all_pages`] to fetch everything, or
//! - call [`TemporalCache::load_pages_for_time_range`] to fetch only the pages
//!   whose subtree time bounds overlap your query — skipping irrelevant subtrees
//!   entirely.

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
