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
//! use copc_streaming::{Aabb, CopcStreamingReader, FileSource};
//!
//! let mut reader = CopcStreamingReader::open(
//!     FileSource::open("points.copc.laz")?,
//! ).await?;
//!
//! // One call: loads hierarchy, fetches chunks, filters points by bounds.
//! let points = reader.query_points(&my_query_box).await?;
//! // each `las::Point` has .x, .y, .z, .gps_time, .color, etc.
//! ```
//!
//! # With LOD control
//!
//! ```rust,ignore
//! // Load points with at most 0.5 m between samples.
//! let level = reader.copc_info().level_for_resolution(0.5);
//! let points = reader.query_points_to_level(&my_query_box, level).await?;
//! ```
//!
//! # Fast path: select only the fields you need
//!
//! The [`query_points`](CopcStreamingReader::query_points) family decodes
//! every field and materializes `las::Point` values. For hot paths,
//! [`query_chunks`](CopcStreamingReader::query_chunks) returns [`Chunk`]s
//! with zero-copy column access and lets you pick which LAZ layers to
//! decode at all. Omitted layers skip arithmetic decoding entirely.
//!
//! ```rust,ignore
//! use copc_streaming::{CopcStreamingReader, FileSource, Fields};
//!
//! let level = reader.copc_info().level_for_resolution(0.5);
//! let chunks = reader
//!     .query_chunks_to_level(&my_query_box, level, Fields::Z | Fields::RGB)
//!     .await?;
//!
//! for chunk in &chunks {
//!     // `unwrap` is safe — we asked for Z and RGB so the chunk has them.
//!     let indices = chunk.indices_in_bounds(&my_query_box).unwrap();
//!     let rgb = chunk.rgb().unwrap();
//!     let positions = chunk.positions().unwrap();
//!     for (pos, rgb) in positions.zip(rgb) {
//!         // ...
//!     }
//! }
//! ```
//!
//! # Low-level access
//!
//! For full control over hierarchy loading and fetch parallelism, combine
//! [`load_hierarchy_for_bounds_to_level`](CopcStreamingReader::load_hierarchy_for_bounds_to_level),
//! [`visible_keys`](CopcStreamingReader::visible_keys), and
//! [`fetch_chunk`](CopcStreamingReader::fetch_chunk):
//!
//! ```rust,ignore
//! reader.load_hierarchy_for_bounds_to_level(&my_query_box, level).await?;
//! for key in reader.visible_keys(&my_query_box, Some(level)) {
//!     let chunk = reader.fetch_chunk(&key, Fields::Z | Fields::GPS_TIME).await?;
//!     let times = chunk.gps_time().unwrap();
//!     // ... drive your own parallelism / cancellation / prioritization
//! }
//! ```
//!
//! # Hierarchy loading
//!
//! The COPC hierarchy is stored as a tree of *pages*. Each page contains metadata
//! for a group of octree nodes (typically several levels deep) plus pointers to
//! child pages covering deeper subtrees.
//!
//! [`CopcStreamingReader::open`] reads the LAS header, COPC info, and the **root
//! hierarchy page**. This gives you the coarse octree nodes immediately (often
//! levels 0–3, depending on the file). Any subtrees stored in separate pages
//! are tracked as *pending pages* — they haven't been fetched yet.
//!
//! You then control when and which deeper pages are loaded:
//!
//! - [`load_hierarchy_for_bounds`](CopcStreamingReader::load_hierarchy_for_bounds) —
//!   load only pages whose subtree intersects a bounding box. Call this when the
//!   camera moves or a spatial query arrives.
//! - [`load_hierarchy_for_bounds_to_level`](CopcStreamingReader::load_hierarchy_for_bounds_to_level) —
//!   same, but stops at a maximum octree level. Use with
//!   [`CopcInfo::level_for_resolution`] for LOD control.
//! - [`load_pending_pages`](CopcStreamingReader::load_pending_pages) — fetch the
//!   next batch of pending pages (all of them). Useful when you don't need spatial
//!   filtering and just want to go one level deeper.
//! - [`load_all_hierarchy`](CopcStreamingReader::load_all_hierarchy) — convenience
//!   to pull every remaining page in one go.
//! - [`children`](CopcStreamingReader::children) — list loaded children of a node.
//!   Returns only children already in the cache; if deeper pages haven't been
//!   loaded yet this may return fewer than exist in the file.
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
mod fields;
mod file_source;
mod header;
mod hierarchy;
mod reader;
mod types;

pub use byte_source::ByteSource;
pub use chunk::Chunk;
pub use error::CopcError;
pub use fields::Fields;
pub use file_source::FileSource;
pub use header::{CopcHeader, CopcInfo};
pub use hierarchy::{HierarchyCache, HierarchyEntry};
pub use reader::CopcStreamingReader;
pub use types::{Aabb, VoxelKey};

// Re-exports of the handful of `las` types that appear in this crate's
// public API. Users who only consume the types we return don't need to
// add `las` as a direct dependency.
pub use las::{Point, PointCloud, PointRef};
