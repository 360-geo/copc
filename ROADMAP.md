# Roadmap

Follow-up work after the `Chunk` / `Fields` landing. Each item lists what
it does, what depends on upstream changes in `las-rs`, and where it lives.

## Work we can do in-tree (no upstream changes)

### 1. Two-pass selective decode for highly-selective spatial queries

`reader::query_points` decodes every chunk with `Fields::ALL`. For very
selective bounds queries on large chunks, a two-pass strategy could help:

1. Fetch with `Fields::Z` (cheap — one layer)
2. Compute `indices_in_bounds`
3. If non-empty, refetch with `Fields::ALL` and materialize only matching indices

Only a win when filters are highly selective _and_ chunks are large. The
double decode hurts otherwise. Probably belongs as an opt-in strategy
(`query_points_selective` or a config flag on the reader), not the default.

Needs a benchmark harness to decide if it earns its complexity.

### 2. Parallel chunk fetch

`query_chunks` / `query_chunks_to_level` fetch chunks sequentially in a
`for` loop. For workloads with many small chunks on IO-bound sources
(HTTP, S3) this leaves a lot of throughput on the table.

A `query_chunks_concurrent(bounds, fields, concurrency)` variant using
`FuturesUnordered` would let callers express "fetch up to N chunks in
flight." Obstacle: the current async design is non-`Send` (targets WASM
single-threaded runtimes). `FuturesUnordered` needs the futures to be
`'static` and ideally `Send`, which conflicts with references into
`&self`. May require restructuring to take `Arc<Self>` or similar.

Alternative: expose the building blocks (`visible_keys` +
`fetch_chunk_with_source`) and let callers drive their own parallelism.
That's already supported today, so "parallel query_chunks" is a
convenience, not a capability gap.

### 3. Integration test with a real COPC fixture

None of the current tests exercise the actual LAZ decompression path, the
`set_selection` wiring, or end-to-end fetch+filter+materialize. Unit tests
cover `Chunk` construction, field guards, and index filters using
synthetic `PointCloud`s built via `from_raw_bytes`, but the real decode
path is untested.

Commit a small `.copc.laz` fixture (a few thousand points, format 6 or 7,
known field values) and add:

- `fetch_chunk(&key, Fields::ALL)` end-to-end → expected point values
- `fetch_chunk(&key, Fields::Z)` → positions match, other column accessors
  return `None`
- `fetch_chunk(&key, Fields::Z | Fields::GPS_TIME)` → gps times match
- `query_points(&bounds)` round-trip
- `TemporalCache::query_points` end-to-end (if the fixture has a temporal
  EVLR, otherwise skip)

Blocker: we need to either generate a fixture with `copc-rs` / PDAL or
pull a small public sample.

### 4. Selective-decode benchmark harness

Once a fixture exists, a Criterion bench that reads it and runs
`fetch_chunk` with different `Fields` masks would validate the
selective-decode claims and let us track regressions. Per-chunk timing
is more meaningful than full-file benchmarks because typical COPC chunks
are ~64K–1M points.

Report decode + materialization CPU separately; show the speedup factor
as a function of mask size.

## Upstream asks on `las-rs`

None of these block the in-tree work above — everything listed here is a
refinement that would let us delete local workarounds or unlock a cleaner
API shape, not a blocker for any capability.

Listed in rough order of "how much it would clean up our code."

### A. `PointCloud::point_owned(i: usize) -> Result<Point>` (or equivalent)

**What it unlocks:** `Chunk::points_at` currently vendors a
`Cursor + raw::Point::read_from + Point::new` loop to materialize specific
indices. It works but duplicates what `PointCloud::points()` already does
internally, and walks a cursor sequentially for what is really random
access.

With a direct "give me an owned `Point` at index `i`" method on
`PointCloud`, `points_at` becomes a one-liner:

```rust
indices.iter().map(|&i| self.cloud.point_owned(i as usize)).collect()
```

No cursor, no `raw::Point` import in copc, cleaner call site.

**Upstream size:** ~15 lines on `PointCloud`, reusing the existing
`raw::Point::read_from` path.

**Alternatives:**
- `impl From<(PointRef<'_>, &Format, &Vector<Transform>)> for Point` —
  same effect, different ergonomics. `point_owned(i)` is simpler because
  `PointCloud` already has the format and transforms cached.
- `PointRef::to_point(&self, format: &Format, transforms: &Vector<Transform>) -> Point` —
  inherent method on `PointRef`. Loses index-locality (PointRef doesn't
  know its own index), so a caller still has to look up by index.

### B. Fields tracking on `PointCloud` itself

**What it unlocks:** today `Chunk` carries `Fields` and guards every
column accessor. If callers reach into `chunk.cloud()` to use `PointCloud`
accessors directly (for fields we don't expose on `Chunk`, or to use
`x_raw` / `y_raw` etc.), they get valid-looking zero data for skipped
fields — no panic, no None, just silent zeros. It's a footgun.

Move the tracking upstream: `PointCloud::with_fields(Fields)` sets a mask,
column accessors return `None` if the field isn't in the mask, `rgb()` /
`gps_time()` / etc. combine the existing format check with the new field
check. Then `Chunk::fields` can go away entirely — or become a pure
passthrough.

**Upstream size:** ~50-80 lines. Medium-sized but mechanical.

**Coordination note:** `laz::DecompressionSelection` would also need to
be visible in the las API surface to make the connection obvious. Might
want to take a `DecompressionSelection` rather than a copc-specific
`Fields` bitflag — `las-rs` already depends on `laz`.

### C. Selective decode wired through `Reader::read_into_cloud`

**What it unlocks:** `Reader::read_into_cloud(cloud, n)` currently always
decodes every field. For non-copc users doing full-file reads with
selective decode (e.g. someone computing bounds statistics on a multi-GB
LAZ file who only needs x/y/z), they have no way to ask for a subset.

Add `read_into_cloud_with(cloud, n, selection)` that passes the selection
through to the laz backend's decompressor. Doesn't benefit copc directly
(we drive the decompressor ourselves via `LayeredPointRecordDecompressor`)
but it's the natural companion to making selective decode a first-class
citizen of the `PointCloud` API.

**Upstream size:** ~20 lines.

### D. Missing column iterators on `PointCloud`

**What it unlocks:** `PointCloud` exposes column iterators for `x`, `y`,
`z`, `intensity`, `classification`, `return_number`, `number_of_returns`,
`scan_angle_degrees`, `user_data`, `point_source_id`, `gps_time`, `rgb`,
`nir`. Missing from the column API but present on `PointRef`:

- `scanner_channel()`
- Flag bits: `is_synthetic()`, `is_key_point()`, `is_withheld()`, `is_overlap()`
- `waveform()` (full-waveform blob)
- `extra_bytes()` (per-point extra bytes slice)

All of these are legitimate LAS 1.4 fields that callers can already
reach via `chunk.cloud().iter().map(|p| p.scanner_channel())` or
equivalent; adding them to the column API is pure parity with `PointRef`.

**Upstream size:** ~40 lines — mechanical, matches the existing pattern.

### E. `rgb() -> [u16; 3]` instead of tuple

**What it unlocks:** `PointCloud::rgb()` returns
`Iterator<Item = (u16, u16, u16)>`. Arrays are more natural for GPU
vertex buffer uploads (memcpy-friendly) and for callers that need
indexed access. Minor ergonomic polish.

**Upstream size:** either a breaking signature change or a parallel
`rgb_array()` method.

## Summary of "do in-tree" vs "needs upstream"

| Item | Depends on upstream? |
|---|---|
| Two-pass selective decode | No |
| Parallel chunk fetch | No (may need async restructure) |
| Real-fixture integration tests | No (needs fixture file) |
| Benchmark harness | No (needs fixture file) |
| Simpler `Chunk::points_at` | Yes — A |
| Remove field-guard duplication | Yes — B |
| Missing column iterators | Yes — D |

None of the in-tree follow-ups are blocked waiting on `las-rs`. Upstream
changes would shrink the copc crate and eliminate a few footguns, but
every capability is already achievable today.
