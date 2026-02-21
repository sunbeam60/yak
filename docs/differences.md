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

### Stream descriptor free list now documented (fixed)

The architecture previously said "One possible optimization is to maintain a free list of streams. For now, a sequential scan ... is acceptable." The architecture now documents the implemented free list: 8-byte head at the start of the Streams stream, singly-linked chain through freed descriptors, reuse on allocation.

### Name table and O(log n) directory lookups now documented (fixed)

The architecture previously described sequential scanning with per-entry hash comparison. It now documents the Name Table approach: sorted (hash, offset) pairs appended after stream entries, binary search for O(log n) lookups, and the insert/delete mechanics.

## Active divergences

### Directory entry field order differs from architecture

**Architecture says:** Entry format is `| length (2 bytes) | stream identifier (2-8 bytes) | name |`

**Reality:** The code serializes entries as `| id (biw bytes) | name_len (2 bytes) | name |` — the stream identifier comes first, then the name length. The fields are the same but in a different order.

### Directory footer stores offset rather than size

**Architecture says:** The footer is "an 8 byte that indicates the size of the Name Table."

**Reality:** The footer is 4 bytes storing `name_table_offset: u32` — the byte position where the name table begins. The entry count is derived from `(stream_len - footer_size - name_table_offset) / slot_size`. Both approaches locate the name table equally well; the implementation chose offset (4 bytes) over size (8 bytes).

### Stream descriptor format has an additional field

**Architecture says:** Stream descriptors have three fields: size (u64), top_block (u64), and reserved (u64) — 24 bytes.

**Reality:** Stream descriptors are 25 bytes. A `flags: u8` field was added at byte 24 to support per-stream compression (`STREAM_FLAG_COMPRESSED = 1`). The architecture's compression section discusses compression conceptually but doesn't mention this flag in the descriptor format.

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
