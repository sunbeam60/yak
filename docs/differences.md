# Differences from architecture.md

This file tracks divergences between the implementation and the architecture document.

## Resolved

### Publication status (fixed)

The workspace `Cargo.toml` previously set `publish = false`. As of v0.9.0, `yak` and `yak_cl` are published to crates.io, and Python bindings are published to PyPI as `libyak` (the `yak` name was taken on PyPI). The architecture says `pip install yak` but the actual command is `pip install libyak`; the import name is `import libyak` (or `import libyak as yak` for convenience).

### Python wrapper path (fixed)

The project table previously listed the Python wrapper path as `yak_python/`. The architecture now correctly says `yak_python/`.

### Terminology: "virtual block" -> "compressed block" (fixed)

The architecture previously used "compressed virtual blocks" in diagrams and "virtual block" in prose. The codebase uses "compressed block" exclusively. The architecture now matches: all references to "virtual block" have been replaced with "compressed block".

### L2 encryption now documented (fixed)

The architecture previously said nothing about encryption at the block layer. It now documents L2's optional AES-XTS block-level encryption, including a full L2 header table with all encryption fields (salt, Argon2id parameters, verification hash, wrapped key).

### Stream descriptor free list now documented (fixed)

The architecture previously said "One possible optimization is to maintain a free list of streams. For now, a sequential scan ... is acceptable." The architecture now documents the implemented free list: 8-byte head at the start of the Streams stream, singly-linked chain through freed descriptors, reuse on allocation.

### Name table and O(log n) directory lookups now documented (fixed)

The architecture previously described sequential scanning with per-entry hash comparison. It now documents the Name Table approach: sorted (hash, offset) pairs appended after stream entries, binary search for O(log n) lookups, and the insert/delete mechanics.

### Directory entry field order now matches (fixed)

The differences file previously claimed the architecture had `| length | identifier | name |` while the code had `| identifier | length | name |`. The architecture now shows the same order as the code: `| stream identifier (2-8 bytes) | length (2 bytes) | name |`.

### Directory footer now matches (fixed)

The differences file previously claimed the architecture said "an 8 byte that indicates the size of the Name Table." The architecture now says "an 4 byte that indicates the offset of the Name Table", matching the implementation's 4-byte `name_table_offset: u32`.

### Stream descriptor flags byte now documented (fixed)

The architecture previously described only 3 fields (size, top_block, reserved = 24 bytes). It now documents the full 25-byte layout including `flags (1 byte)` for per-stream compression.

## Active divergences

### L2 API is broader than documented

**Architecture says:** L2's API should be restricted to: allocate a new block, write a buffer to a block, read a block to a buffer, and deallocate a used block.

**Reality:** The `BlockLayer` trait also exposes:
- `allocate_blocks()` / `deallocate_blocks()` -- batch operations that sort indices to promote contiguous reuse
- `read_contiguous_blocks()` / `write_contiguous_blocks()` -- multi-block I/O across runs of contiguous blocks
- `invalidate_thread_local_cache()` -- clears the calling thread's per-thread cache (used before accessing shared streams like directories)
- `is_encrypted()` -- query whether file uses encryption
- `verify()` -- integrity checking

These are performance-critical additions that emerged during optimization work.

### L3 API is broader than documented

**Architecture says:** L3's API should concern itself with: creating, verifying existence, opening, closing, deleting, reading, writing, and shortening streams.

**Reality:** The `StreamLayer` trait also exposes:
- `create_stream(compressed: bool)` -- compression flag parameter not in architecture's API list
- `reserve()` -- pre-allocate block capacity for a stream
- `stream_length()` / `stream_reserved()` -- query stream size and allocated capacity
- `stream_count()` / `stream_ids()` -- enumerate active streams
- `is_stream_compressed()` -- query per-stream compression status
- `compressed_block_size_shift()` -- query compression block size config
- `is_encrypted()` -- query encryption status
- `open_stream_blocking()` -- blocking variant that waits when a lock is held by another thread (used internally by L4 for directory operations)
- `verify()` -- integrity checking

The architecture discusses reserved capacity and compression conceptually but doesn't list these functions in the L3 API.

### L4 API is broader than documented

**Architecture says:** L4 provides creating/renaming/deleting streams and directories, iterating, understanding length, reading, writing, positioning, and closing streams.

**Reality:** The `Yak` struct also exposes:
- `create(path, CreateOptions)` -- uses an options struct (with `block_index_width`, `block_size_shift`, `compressed_block_size_shift`, `password`) rather than individual parameters
- `open_encrypted()` -- separate method for opening encrypted files with a password
- `close(self)` -- file-level close that consumes self and flushes all open streams
- `create_stream(path, compressed)` -- takes a `compressed: bool` parameter
- `is_stream_compressed()` -- query per-stream compression
- `is_encrypted()` -- query file encryption
- `tell()` -- query current head position (complement to `seek()`)
- `reserve()` / `stream_reserved()` -- pre-allocate and query stream capacity
- `block_index_width()` / `block_size_shift()` / `compressed_block_size_shift()` -- query file configuration
- `optimize()` -- compaction and defragmentation (returns bytes reclaimed)
- `verify()` -- integrity checking

### Verification chain not documented in API sections

**Architecture says:** "A function to verify the integrity of a Yak file is provided" (in the thread safety section).

**Reality:** `verify()` is implemented as a chain across all four layers (L4 -> L3 -> L2 -> L1), each layer checking its own invariants and passing claimed resource lists down. This is a significant feature not described in any layer's API section.

### Block cache is a dual-cache system with configurable budget

**Architecture says:** L2 manages a write-through cache of blocks (no mention of configurability or dual caching).

**Reality:** `BlocksInFile<L1, const CACHE_BUDGET_BYTES: usize>` takes a const generic parameter for the cache memory budget (default: 2 MB). The cache is a dual system:
- **Thread-local LRU** — per-thread, lock-free, used for user stream redirector blocks (`CacheMode::ThreadLocal`)
- **Shared LRU** — Mutex-guarded, cross-thread coherent, used for Streams stream redirector blocks (`CacheMode::Shared`)
- **Bypass** — `CacheMode::None` for contiguous block I/O and data blocks

Both caches are write-through. The `CacheMode` enum is threaded through L3 to select the appropriate cache for each operation. The architecture describes a single thread-local cache; the dual-cache avoids full cache invalidation when threads share access to internal streams (like the Streams stream that holds descriptors).

### Architecture header example has a terminology inconsistency

**Architecture says** (in the header layout example): `block index size shift`

**Reality:** The field is `block_index_width` -- it represents the number of bytes per block index (e.g. 4 = 32-bit), not a shift. The architecture's own L2 header table later correctly calls it "Block index width". The header layout example at the top should say `block index width` instead of `block index size shift`.
