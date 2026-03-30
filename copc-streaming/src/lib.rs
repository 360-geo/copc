//! Async streaming reader for [COPC](https://copc.io/) (Cloud-Optimized Point Cloud) files.
//!
//! COPC organises LAS/LAZ point cloud data in a spatial octree so that clients can
//! fetch only the regions they need. This crate reads COPC files incrementally through
//! the [`ByteSource`] trait — any random-access backend (local files, HTTP range
//! requests, in-memory buffers) works out of the box.
//!
//! # Quick start
//!
//! ```rust,ignore
//! use copc_streaming::{CopcStreamingReader, FileSource};
//!
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("points.copc.laz")?,
//! ).await?;
//!
//! // At this point the LAS header, COPC info and the root hierarchy page
//! // are loaded. Top-level octree nodes are already available.
//!
//! // Load the remaining hierarchy pages so every node is known.
//! reader.load_all_hierarchy().await?;
//!
//! // Iterate nodes and fetch point data for the ones you need.
//! for (key, entry) in reader.entries() {
//!     if entry.point_count <= 0 { continue; }
//!     let bounds = key.bounds(&reader.copc_info().root_bounds());
//!     if !bounds.intersects(&my_query_box) { continue; }
//!
//!     let chunk = reader.fetch_chunk(key).await?;
//!     let points = reader.read_points(&chunk)?;
//!     // each `point` has .x, .y, .z, .gps_time, .color, etc.
//! }
//! ```
//!
//! # Incremental hierarchy loading
//!
//! [`CopcStreamingReader::open`] fetches the root hierarchy page which lists the
//! coarsest octree nodes plus pointers to deeper pages. You can either:
//!
//! - call [`CopcStreamingReader::load_all_hierarchy`] to pull every page at once, or
//! - call [`CopcStreamingReader::load_pending_pages`] in a loop, checking
//!   [`CopcStreamingReader::has_pending_pages`] between rounds, to load one level
//!   at a time.
//!
//! # Custom byte sources
//!
//! Implement [`ByteSource`] to read from any backend. The trait requires only
//! `read_range(offset, length)` and `size()`. A default `read_ranges` implementation
//! issues sequential reads — override it for backends that support parallel fetches
//! (e.g. HTTP/2 multiplexing).
//!
//! Built-in implementations: [`FileSource`] (local files), `Vec<u8>` and `&[u8]`
//! (in-memory).
//!
//! Futures returned by `ByteSource` are *not* required to be `Send`, so the crate
//! works in single-threaded runtimes and WASM environments.

mod byte_source;
mod chunk;
mod error;
mod file_source;
mod header;
mod hierarchy;
mod reader;
mod types;

pub use byte_source::ByteSource;
pub use chunk::DecompressedChunk;
pub use error::CopcError;
pub use file_source::FileSource;
pub use header::{CopcHeader, CopcInfo};
pub use hierarchy::{HierarchyCache, HierarchyEntry};
pub use reader::CopcStreamingReader;
pub use types::{Aabb, VoxelKey};
