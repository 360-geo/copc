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
//! let root_bounds = reader.copc_info().root_bounds();
//!
//! // Load only the hierarchy pages that cover the region you care about.
//! reader.load_hierarchy_for_bounds(&my_query_box).await?;
//!
//! // Walk loaded nodes — only those intersecting the query box.
//! for (key, entry) in reader.entries() {
//!     if entry.point_count == 0 { continue; }
//!     if !key.bounds(&root_bounds).intersects(&my_query_box) { continue; }
//!
//!     let chunk = reader.fetch_chunk(key).await?;
//!     let points = reader.read_points(&chunk)?;
//!     // each `point` has .x, .y, .z, .gps_time, .color, etc.
//! }
//! ```
//!
//! # Load everything at once
//!
//! If you don't need spatial filtering, pull the full hierarchy in one call:
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
