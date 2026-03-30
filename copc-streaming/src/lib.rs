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
//! use copc_streaming::{CopcStreamingReader, FileSource, VoxelKey};
//!
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("points.copc.laz")?,
//! ).await?;
//!
//! // Coarse octree nodes are available immediately.
//! // Load deeper hierarchy pages as you need them.
//! let root_bounds = reader.copc_info().root_bounds();
//!
//! while reader.has_pending_pages() {
//!     reader.load_pending_pages().await?;
//! }
//!
//! // Walk the octree — check which nodes intersect your region.
//! for (key, entry) in reader.entries() {
//!     if entry.point_count == 0 { continue; }
//!     if !key.bounds(&root_bounds).intersects(&my_query_box) { continue; }
//!
//!     // Drill into finer nodes when available.
//!     let finer = reader.children(key);
//!     if !finer.is_empty() { continue; } // render children instead
//!
//!     let chunk = reader.fetch_chunk(key).await?;
//!     let points = reader.read_points(&chunk)?;
//!     // each `point` has .x, .y, .z, .gps_time, .color, etc.
//! }
//! ```
//!
//! # Load everything at once
//!
//! If you don't need progressive loading, pull the full hierarchy in one call:
//!
//! ```rust,ignore
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("points.copc.laz")?,
//! ).await?;
//! reader.load_all_hierarchy().await?;
//!
//! // Every node is now available via reader.entries() / reader.get().
//! ```
//!
//! # Hierarchy loading
//!
//! [`CopcStreamingReader::open`] reads the LAS header, COPC info and root hierarchy
//! page. Coarse octree nodes are available right away. Deeper pages are loaded
//! on demand:
//!
//! - [`load_hierarchy_for_bounds`](CopcStreamingReader::load_hierarchy_for_bounds) —
//!   load only pages whose subtree intersects a bounding box. Ideal for spatial
//!   queries over a small region of a large file.
//! - [`load_pending_pages`](CopcStreamingReader::load_pending_pages) — fetch the
//!   next level of pages. Call repeatedly, or only when you need finer detail.
//! - [`load_all_hierarchy`](CopcStreamingReader::load_all_hierarchy) — convenience
//!   to pull every remaining page in one go.
//! - [`children`](CopcStreamingReader::children) — list loaded children of a node.
//! - [`has_pending_pages`](CopcStreamingReader::has_pending_pages) — check if there
//!   are still unloaded pages.
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
