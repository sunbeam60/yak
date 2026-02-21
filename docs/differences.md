# Differences from architecture.md

This file tracks divergences between the implementation and the architecture document.

## Resolved

### Publication status (fixed)

The workspace `Cargo.toml` previously set `publish = false`. As of v0.9.0, `yak` and `yak_cl` are published to crates.io, and Python bindings are published to PyPI as `libyak` (the `yak` name was taken on PyPI). The architecture says `pip install yak` but the actual command is `pip install libyak`; the import name remains `import yak`.

### Python wrapper path (fixed)

The project table previously listed the Python wrapper path as `yak_python/`. The architecture now correctly says `yak_python/`.

### Terminology: "virtual block" -> "compressed block" (fixed)

The architecture previously used "compressed virtual blocks" in diagrams and "virtual block" in prose. The codebase uses "compressed block" exclusively. The architecture now matches: all references to "virtual block" have been replaced with "compressed block".

### L2 encryption now documented (fixed)

The architecture previously said nothing about encryption at the block layer. It now documents L2's optional AES-XTS block-level encryption, including a full L2 header table with all encryption fields (salt, Argon2id parameters, verification hash, wrapped key).

## Active divergences

### Directory entry format has diverged from architecture

**Architecture says:** Stream entries in directory streams are serialized as:
```
| length (2 bytes) | stream identifier (2-8 bytes) | name hash (4 bytes) | name (1-65,522 bytes) |
```
With sequential scanning and hash comparison for lookups.

**Reality:** L4 format version 2 uses a fundamentally different structure. Directory streams now have three regions:

1. **Entry data region** -- variable-length entries: `| id (biw bytes) | name_len (2 bytes) | name (variable) |`
2. **Sorted name table** -- array of `(hash: u32, offset: u32)` pairs, sorted by hash for binary search
3. **Footer** -- `| entry_count: u32 | name_table_offset: u32 |` (8 bytes)

Lookups use binary search on the sorted name table (O(log n)) with FNV-1a hashing, falling back to full string comparison on hash collisions. This replaces the sequential scan described in the architecture. The per-entry `length` prefix from the architecture is also gone; entry length is derived from `name_len`.

### Stream descriptor format has an additional field

**Architecture says:** Stream descriptors have three fields: size (u64), top_block (u64), and reserved (u64) -- 24 bytes.

**Reality:** Stream descriptors are 25 bytes. A `flags: u8` field was added at byte 24 to support per-stream compression (`STREAM_FLAG_COMPRESSED = 1`). The architecture's compression section discusses compression conceptually but doesn't mention this flag in the descriptor format.

### Stream descriptor free list is implemented

**Architecture says:** "One possible optimization is to maintain a free list of streams. For now, a sequential scan of the Streams stream for a free stream descriptor is acceptable."

**Reality:** The free list optimization is fully implemented. The first 8 bytes of the Streams stream store a `free_list_head` index. Freed descriptors form a singly-linked list using the `top_block` field as the next pointer. New stream creation reuses free slots before appending.

### L2 API is broader than documented

**Architecture says:** L2's API should be restricted to: allocate a new block, write a buffer to a block, read a block to a buffer, and deallocate a used block.

**Reality:** The `BlockLayer` trait also exposes:
- `allocate_blocks()` / `deallocate_blocks()` -- batch operations that sort indices to promote contiguous reuse
- `read_contiguous_blocks()` / `write_contiguous_blocks()` -- multi-block I/O across runs of contiguous blocks
- `invalidate_block_cache()` -- thread-local cache management
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

### Block cache has configurable budget via const generic

**Architecture says:** L2 manages a write-through cache of blocks (no mention of configurability).

**Reality:** `BlocksInFile<L1, const CACHE_BUDGET_BYTES: usize>` takes a const generic parameter for the cache memory budget (default: 2 MB). The cache is implemented as a thread-local LRU with a maximum of 4096 entries. The architecture correctly describes the cache as thread-local and write-through but doesn't mention the budget parameter.

### Architecture header example has a terminology inconsistency

**Architecture says** (in the header layout example): `block index size shift`

**Reality:** The field is `block_index_width` -- it represents the number of bytes per block index (e.g. 4 = 32-bit), not a shift. The architecture's own L2 header table later correctly calls it "Block index width". The header layout example at the top should say `block index width` instead of `block index size shift`.
