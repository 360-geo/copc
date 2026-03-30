# COPC Temporal Index Extension

**Version:** 1.0 draft

## 1. Introduction

COPC (Cloud-Optimized Point Cloud) files organize point data spatially in an octree, enabling efficient partial loading of spatial data. The COPC Temporal Index Extension adds an optional Extended Variable Length Record (EVLR) that enables efficient temporal queries over GPS time.

The temporal index provides a per-node lookup table of sampled GPS timestamps, allowing clients to:

- Determine which octree nodes contain points within a given time range without decompressing point data.
- Estimate the approximate point index within a node for a given time boundary.

This draft introduces a **paged layout** that mirrors the hierarchical structure of COPC spatial data. Clients can load the index incrementally — reading only the pages relevant to their spatial and temporal query — rather than being forced to load the entire index at once.

### 1.1 Motivation

A motivating use case is repeated survey coverage. When multiple collection passes cover the same area at different times, a single merged COPC file may contain overlapping data from many epochs. At busy intersections, the same spatial area may contain points from 10+ passes at different times.

A concrete example: a client needs points within a 30m radius of a known location, collected within a specific 10-second window. Without a temporal index, the client must decompress all spatially overlapping nodes — including points from every other pass through that area. With the temporal index, the client eliminates nodes from unrelated passes before any decompression occurs.

For clients that access the file remotely or through constrained I/O, loading the entire temporal index upfront would be counterproductive — the index for a large file can be tens of megabytes. The paged layout in this version allows the client to load only the few kilobytes of index data relevant to its query, regardless of the underlying access method (local disk, HTTP range requests, custom streaming protocol, etc.).

## 2. Scope

This specification defines a single optional EVLR that may be appended to any COPC 1.0 file. A file with this EVLR remains fully COPC 1.0 compliant. Readers that do not recognize the EVLR SHALL ignore it per standard LAS 1.4 behavior.

## 3. Definitions

| Term | Definition |
|---|---|
| Node | An octree node identified by a VoxelKey (level, x, y, z) |
| Chunk | The compressed point data for a single node |
| Stride | The sampling interval: every S-th point is recorded in the index |
| Sample | A GPS time value recorded in the index |
| Page | A contiguous block of temporal node entries within the EVLR data |

## 4. Requirements

### 4.1 Point Format

Input files MUST use a LAS point format that includes GPS time (formats 1, 3, 4, 5, 6, 7, 8, 9, or 10). Formats 0 and 2 do not contain GPS time and are not compatible with this extension.

### 4.2 Point Ordering

Points within each octree node MUST be sorted in non-decreasing order of GPS time before the index is constructed. This ordering is a prerequisite for the index to be meaningful: it ensures that samples are monotonically non-decreasing and that sample positions correspond to contiguous point ranges.

### 4.3 COPC Compliance

The file MUST be a valid COPC 1.0 file. The LAS 1.4 header field `number_of_evlrs` MUST be incremented to account for the temporal index EVLR.

## 5. EVLR Identification

| Field | Value |
|---|---|
| user_id | `copc_temporal` (null-padded to 16 bytes) |
| record_id | `1000` |

The `copc_temporal` user_id is distinct from the standard `copc` user_id to avoid ambiguity with current or future COPC records.

## 6. Binary Layout

All multi-byte values are **little-endian**.

### 6.1 Header (32 bytes)

The header is always at byte offset 0 within the EVLR data payload.

| Offset | Type | Field | Description |
|--------|------|-------|-------------|
| 0 | uint32 | version | Format version. MUST be `1`. |
| 4 | uint32 | stride | Sampling stride S. MUST be >= 1. |
| 8 | uint32 | node_count | Total number of node entries across all pages. |
| 12 | uint32 | page_count | Total number of pages. |
| 16 | uint64 | root_page_offset | Absolute file offset of the root page. |
| 20 | uint32 | root_page_size | Size of the root page in bytes. |
| 24 | uint32 | reserved | MUST be `0`. Readers SHOULD ignore this field. |

Note: `root_page_offset` is an **absolute file offset**, not relative to the EVLR data start. This allows clients to seek directly to the root page without parsing intermediate data. This matches the convention used by the COPC spatial hierarchy (`root_hier_offset`). In practice the root page will typically immediately follow the header at offset `evlr_data_start + 32`, but writers MAY place it elsewhere.

### 6.2 Page Structure

A page is a contiguous sequence of **entries**. Each entry is either a **node entry** (containing temporal samples for a node) or a **page pointer** (referencing a child page containing entries for deeper octree levels).

#### 6.2.1 Node Entry

| Offset | Type | Field | Description |
|--------|------|-------|-------------|
| 0 | int32 | level | Octree level (0 = root) |
| 4 | int32 | x | Voxel X coordinate |
| 8 | int32 | y | Voxel Y coordinate |
| 12 | int32 | z | Voxel Z coordinate |
| 16 | uint32 | sample_count | Number of GPS time samples. MUST be >= 1. |
| 20 | float64[] | samples | `sample_count` GPS time values |

Size: `20 + sample_count * 8` bytes.

#### 6.2.2 Page Pointer Entry

A page pointer is distinguished from a node entry by `sample_count == 0`.

| Offset | Type | Field | Description |
|--------|------|-------|-------------|
| 0 | int32 | level | Octree level of the subtree root |
| 4 | int32 | x | Voxel X coordinate of the subtree root |
| 8 | int32 | y | Voxel Y coordinate of the subtree root |
| 12 | int32 | z | Voxel Z coordinate of the subtree root |
| 16 | uint32 | sample_count | MUST be `0` (identifies this as a page pointer) |
| 20 | uint64 | child_page_offset | Absolute file offset of the child page |
| 28 | uint32 | child_page_size | Size of the child page in bytes |
| 32 | float64 | subtree_time_min | Minimum GPS time across all nodes in the subtree |
| 40 | float64 | subtree_time_max | Maximum GPS time across all nodes in the subtree |

Size: 48 bytes (fixed).

The `subtree_time_min` and `subtree_time_max` fields summarize the time range of the entire subtree rooted at this key. This allows clients to **prune entire subtrees** without fetching the child page: if the subtree time range does not overlap the query time range, the child page can be skipped entirely.

The VoxelKey in a page pointer identifies the subtree root node. The child page contains entries for nodes within that subtree (the node itself, if it has points, plus its descendants).

### 6.3 Page Organization

The root page MUST contain entries (node entries or page pointers) for all octree nodes at levels 0 through some writer-chosen depth. Writers SHOULD aim to keep the root page small enough to load in a single read operation (recommended: under 16 KB).

A practical strategy: the root page contains node entries for levels 0-3 (typically dozens of nodes) and page pointers for each level-3 subtree that has deeper descendants. Each child page contains node entries for its subtree. Writers MAY nest pages deeper (page pointers within child pages) for very large files.

Pages MUST NOT overlap: every node with `point_count > 0` in the hierarchy MUST appear as a node entry in exactly one page. Page pointers do not count as node entries for this requirement.

### 6.4 C Struct Definitions

```c
struct VoxelKey {
    int32_t level;
    int32_t x;
    int32_t y;
    int32_t z;
};

struct TemporalIndexHeader {
    uint32_t version;           // Must be 1
    uint32_t stride;            // Sampling stride (>= 1)
    uint32_t node_count;        // Total node entries across all pages
    uint32_t page_count;        // Total number of pages
    uint64_t root_page_offset;  // Absolute file offset of root page
    uint32_t root_page_size;    // Size of root page in bytes
    uint32_t reserved;          // Must be 0
};

// Temporal data for a single node (sample_count >= 1)
struct TemporalNodeEntry {
    VoxelKey key;
    uint32_t sample_count;
    // double samples[sample_count];  // variable-length
};

// Pointer to a child page (sample_count == 0)
struct TemporalPagePointer {
    VoxelKey key;
    uint32_t sample_count;        // Must be 0
    uint64_t child_page_offset;   // Absolute file offset
    uint32_t child_page_size;     // Bytes
    double   subtree_time_min;    // Min GPS time in subtree
    double   subtree_time_max;    // Max GPS time in subtree
};
```

### 6.5 Node Entry Ordering

Within each page, node entries and page pointers SHOULD appear in breadth-first VoxelKey order (level ascending, then x, y, z). This is not required for correctness but aids sequential reading.

## 7. Sampling Rules

Given a node containing `N` points sorted by GPS time and a stride of `S`:

1. The GPS time at point index `0` is always sampled.
2. The GPS time at each point index that is a multiple of `S` is sampled (indices `0, S, 2S, 3S, ...`).
3. The GPS time at point index `N-1` (the last point) is always sampled, even if `N-1` is not a multiple of `S`.
4. `sample_count` equals the number of distinct indices sampled by rules 1-3.

Consequently:

- `samples[0]` is the minimum GPS time in the node.
- `samples[sample_count - 1]` is the maximum GPS time in the node.
- All samples are in monotonically non-decreasing order.
- A node with a single point has `sample_count = 1`.

## 8. Client Usage

### 8.1 Incremental Loading

A client performing a combined spatial and temporal query proceeds as follows:

1. **Fetch the header** (32 bytes at the EVLR data start). This can typically be combined with the initial file probe that reads the LAS header and COPC info VLR.

2. **Fetch the root page** (using `root_page_offset` and `root_page_size`). Parse all entries in the root page.

3. **For each node entry in the root page**: check spatial and temporal overlap with the query. Matching nodes are candidates for point decompression.

4. **For each page pointer in the root page**: check whether `subtree_time_min`/`subtree_time_max` overlaps the query time range, AND whether the subtree root's spatial bounds overlap the query bounds. If either check fails, **skip the entire subtree** — do not fetch the child page. If both checks pass, add the child page to the fetch list.

5. **Batch-load all needed child pages** (these are independent read operations that can be issued in parallel). Parse and filter entries in each child page, recursing into nested page pointers as needed.

The number of read operations is bounded by the page tree depth. In practice, most files will have 2-3 levels of pages, requiring 2-3 rounds of reads.

### 8.2 Node-Level Filtering

For each node entry encountered during traversal, determine overlap:

- If `samples[sample_count - 1] < t_start`, skip.
- If `samples[0] > t_end`, skip.
- Otherwise, the node may contain points in the range.

### 8.3 Intra-Node Point Estimation

For a node that passes the filter in 8.2, a client can estimate the approximate point range:

1. Binary-search `samples` for the first index `i` where `samples[i] >= t_start`.
2. Binary-search `samples` for the last index `j` where `samples[j] <= t_end`.
3. The approximate starting point index is `i * stride`.
4. The approximate ending point index is `min(j * stride + stride - 1, point_count - 1)`.

### 8.4 Combined Spatial + Temporal Traversal

The recommended client traversal pattern integrates spatial and temporal filtering at every level:

```
fetch root temporal page
for each entry in page:
    if page_pointer:
        if subtree spatial bounds ∩ query bounds == ∅: skip
        if [subtree_time_min, subtree_time_max] ∩ [t_start, t_end] == ∅: skip
        else: add child page to fetch list
    if node_entry:
        if node spatial bounds ∩ query bounds == ∅: skip
        if [samples[0], samples[-1]] ∩ [t_start, t_end] == ∅: skip
        else: add to result set with estimated point range

load all child pages in parallel
recurse
```

## 9. Writer Guidelines

### 9.1 Page Sizing

Writers SHOULD target a root page size of **4-16 KB**. This typically accommodates all nodes through level 3-4 of the octree, which is sufficient for coarse pruning.

Child pages SHOULD be **16-256 KB** each. Smaller pages increase the number of read operations; larger pages waste I/O when only a small portion of the page is relevant.

### 9.2 Page Boundaries

A natural page boundary strategy:

1. The root page contains node entries for levels 0 through `L` (writer chooses `L`; typically 3-4), plus one page pointer per level-`L` subtree that has descendants with point data.
2. Each child page contains all node entries for one level-`L` subtree.
3. For very large subtrees (> 256 KB of temporal data), the writer MAY split the subtree into further nested pages.

### 9.3 Subtree Time Range Computation

When writing a page pointer, the writer MUST compute `subtree_time_min` and `subtree_time_max` accurately across all descendant node entries. These values are critical for client-side pruning and MUST NOT be approximate.

### 9.4 Stride Selection

The stride controls the trade-off between index size and estimation accuracy. Guidelines:

| File size | Recommended stride |
|---|---|
| < 100M points | 100 |
| 100M - 1B points | 500 - 1000 |
| > 1B points | 1000 - 5000 |

Smaller strides produce more samples per node, improving intra-node point range estimates at the cost of larger pages.

## 10. Compatibility

### 10.1 Forward Compatibility

- This EVLR is additive. A file containing it remains a valid COPC 1.0 file.
- The `copc_temporal` user_id does not collide with any user_id defined by the COPC 1.0 or LAS 1.4 specifications.
- Readers that do not recognize this EVLR SHALL skip it per LAS 1.4 EVLR handling rules.
- Writers that do not support this extension need not produce it.

### 10.2 Version Negotiation

Clients MUST check the `version` field in the header before parsing. The version field is at byte offset 0 within the EVLR data, enabling a quick check before committing to a full parse.

## 11. Example: Read Sequence for a Spatial + Temporal Query

Given a 5.7 GB COPC file with 1.2 billion points, 42,000 nodes, and a temporal index:

```
Read 1: Load LAS header + COPC info VLR + temporal index header
        → 1 read operation, ~1 KB
        → Learn: file bounds, root page at offset X, size 8 KB

Read 2: Load root temporal page (8 KB)
        → 1 read operation
        → Contains: 60 node entries (levels 0-3) + 44 page pointers
        → After spatial + temporal pruning: 2 page pointers survive

Read 3: Load 2 child pages in parallel
        → 2 read operations, ~50 KB each
        → Contains: ~800 node entries for the 2 relevant subtrees
        → After spatial + temporal pruning: 21 nodes survive

Total: 4 read operations, ~110 KB loaded
Result: 21 nodes identified, 1.7 MB of point data to decompress
Compared to: 11 MB flat index load, or 5.7 GB without any index
```

## 12. Changelog

### Draft 2

Changed the binary layout from a flat list of all entries to a **paged structure** that supports incremental loading.

Key differences from draft 1:

| Aspect | Draft 1 | Draft 2 |
|---|---|---|
| Layout | Flat: header + all entries in a single blob | Paged: header + pages containing entries and page pointers |
| Header size | 16 bytes (version, stride, node_count, reserved) | 32 bytes (adds page_count, root_page_offset, root_page_size) |
| Page pointers | N/A | Entries with sample_count == 0 reference child pages |
| Subtree pruning | N/A | Page pointers carry subtree_time_min/max for skip-ahead |
| Incremental loading | Must load entire EVLR before querying | Incremental: load root page, prune, load only needed child pages |

Motivation: the flat layout required clients to load the entire temporal index (potentially tens of megabytes) before performing any query. This negated much of the benefit for clients that can perform partial reads (whether via HTTP range requests, local file seeks, or other mechanisms). The paged layout typically requires 3-5 read operations totaling ~100 KB.
