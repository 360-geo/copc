# copc

[![Crates.io](https://img.shields.io/crates/v/copc-streaming)](https://crates.io/crates/copc-streaming)
[![docs.rs](https://docs.rs/copc-streaming/badge.svg)](https://docs.rs/copc-streaming)
[![CI](https://github.com/360-geo/copc/actions/workflows/ci.yaml/badge.svg)](https://github.com/360-geo/copc/actions/workflows/ci.yaml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Rust libraries for reading [COPC](https://copc.io/) (Cloud-Optimized Point Cloud) files, with support for the [COPC Temporal Index Extension](copc-temporal/docs/temporal-index-spec.md).

## Crates

### [`copc-streaming`](copc-streaming/)

[![Crates.io](https://img.shields.io/crates/v/copc-streaming)](https://crates.io/crates/copc-streaming)
[![docs.rs](https://docs.rs/copc-streaming/badge.svg)](https://docs.rs/copc-streaming)

Async streaming COPC reader. Loads octree hierarchy and point data incrementally via a `ByteSource` trait — works with HTTP range requests, local files, or any random-access byte source.

- LAS header and VLR parsing via the `las` crate
- LAZ decompression via the `laz` crate
- Incremental hierarchy page loading
- Individual chunk fetch + decompression
- `ByteSource` trait with `FileSource` (local) and `Vec<u8>` (in-memory) implementations
- No `Send` requirement on futures — WASM compatible

### [`copc-temporal`](copc-temporal/)

[![Crates.io](https://img.shields.io/crates/v/copc-temporal)](https://crates.io/crates/copc-temporal)
[![docs.rs](https://docs.rs/copc-temporal/badge.svg)](https://docs.rs/copc-temporal)

Reader for the COPC Temporal Index Extension. Enables time-range filtering of octree nodes without decompressing point data.

- Parse temporal index EVLR (paged layout for incremental loading)
- Per-node GPS time range queries
- Intra-node point range estimation via binary search
- Incremental page loading via `ByteSource` — only fetch index pages relevant to your query
- Combined spatial + temporal filtering

Depends on `copc-streaming` for core COPC types (`VoxelKey`, `Aabb`, `ByteSource`).

## Use case

When multiple survey passes cover the same area at different times, a single merged COPC file may contain overlapping data from many collection epochs. A client querying a spatial region often needs only points from a specific time window — not every pass that ever traversed that location. Without the temporal index, all spatially overlapping nodes must be decompressed. With the temporal index, irrelevant epochs are skipped before any decompression.

## Quick start

```rust
use copc_streaming::{CopcStreamingReader, FileSource};
use copc_temporal::{TemporalCache, GpsTime};

let mut reader = CopcStreamingReader::open(FileSource::open("points.copc.laz")?).await?;
reader.load_all_hierarchy().await?;

if let Some(mut temporal) = TemporalCache::from_reader(&reader).await? {
    temporal.load_all_pages(reader.source()).await?;
    let matching = temporal.nodes_in_range(GpsTime(start), GpsTime(end));
}
```

## Related

- [copc-converter](https://github.com/360-geo/copc-converter) — CLI tool that produces COPC files with optional temporal index
- [COPC specification](https://copc.io/)
- [Temporal index spec](copc-temporal/docs/temporal-index-spec.md)
