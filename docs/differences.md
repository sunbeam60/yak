# Differences from architecture.md

This file tracks divergences between the implementation and the architecture document.

## Resolved

### Python wrapper path (fixed)

The project table previously listed the Python wrapper path as `sfs_python/`. The architecture now correctly says `stream_fs_python/`.

## Active divergences

### Publication status claims are premature

**Architecture says:** "The SFS library is published as a crate on crates.io" and "The Python SFS wrapper is published as a PyPI wheel."

**Reality:** The workspace `Cargo.toml` sets `publish = false`. Neither the Rust crate nor the Python wheel are published yet.

### L2 API is broader than documented

**Architecture says:** L2's API should be restricted to: allocate a new block, write a buffer to a block, read a block to a buffer, and deallocate a used block.

**Reality:** The `BlockLayer` trait also exposes:
- `allocate_blocks()` / `deallocate_blocks()` — batch operations that sort indices to promote contiguous reuse
- `read_contiguous_blocks()` / `write_contiguous_blocks()` — multi-block I/O across runs of contiguous blocks
- `invalidate_block_cache()` — thread-local cache management
- `verify()` — integrity checking

These are performance-critical additions that emerged during optimization work.

### L3 API is broader than documented

**Architecture says:** L3's API should concern itself with: creating, verifying existence, opening, closing, deleting, reading, writing, and shortening streams.

**Reality:** The `StreamLayer` trait also exposes:
- `reserve()` — pre-allocate block capacity for a stream
- `stream_length()` / `stream_reserved()` — query stream size and allocated capacity
- `open_stream_blocking()` — blocking variant that waits when a lock is held by another thread (used internally by L4 for directory operations)
- `verify()` — integrity checking

The architecture discusses reserved capacity conceptually in the stream descriptor section but doesn't list these functions in the L3 API.

### L4 API is broader than documented

**Architecture says:** L4 provides creating/renaming/deleting streams and directories, iterating, understanding length, reading, writing, positioning, and closing.

**Reality:** The `Sfs` struct also exposes:
- `tell()` — query current head position
- `reserve()` / `stream_reserved()` — pre-allocate and query stream capacity
- `block_index_width()` / `block_size_shift()` — query file configuration
- `verify()` — integrity checking

The architecture mentions reserve as a utility function but doesn't list it in the API. `tell()` is a natural complement to seek but isn't mentioned.

### Verification chain not documented in API sections

**Architecture says:** "A function to verify the integrity of an SFS file is provided" (in the thread safety section).

**Reality:** `verify()` is implemented as a chain across all four layers (L4 → L3 → L2 → L1), each layer checking its own invariants and passing claimed resource lists down. This is a significant feature not described in any layer's API section.
