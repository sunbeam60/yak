Differences between architecture and implementation

Last reviewed: 2026-02-14

# Major Differences

## L2 doesn't fsync before exiting critical section
Architecture says (architecture.md line 400, "Thread safety in L2" section): "before code exits the critical section that modifies the free block list, changes to the file are flushed to disk."

Code: `allocate_blocks` and `deallocate_block` now persist the L2 header *inside* the mutex, so no thread can observe updated in-memory state before the header write completes. However, there is no `fsync()` call — the write reaches the OS page cache but is not guaranteed durable on disk. A power loss or kernel panic (not a normal process crash) could leave the free list inconsistent. This is an accepted trade-off: SFS is explicitly not ACID, and fsync would impose significant performance cost for durability guarantees most users don't need.

# Minor Differences

## BlocksFromFiles mock has no free list
Architecture says (architecture.md lines 385-393, "Reusing blocks" section) L2 must maintain a free list.

Code: `BlocksFromFiles` (mock) deletes the file on deallocate and always allocates sequentially. `BlocksInFile` (the real implementation) properly implements the free list. Acceptable for a mock, but the mock doesn't match the spec.

# Resolved Differences

These were previously listed as differences but have since been addressed.

## Sfs::close() doesn't flush open stream handles — RESOLVED
Previously listed as major. `Sfs::close(self)` now returns `Result<(), SfsError>` and drains all open stream handles via `flush_open_streams()`, calling `L3::close_stream()` for each. A `Drop` impl on `Sfs` acts as a safety net for cases where `close()` is not called. Tested via `TestCloseFlushesHandles` in `test_single_file.py`.

## reserve() API in L4 — RESOLVED
Previously listed as major. `reserve(handle, n_bytes)` now exists at L4 (`sfs.rs`), L3 (`stream_layer.rs`, `streams_from_blocks.rs`), and is exercised by tests. Pre-allocation of blocks works correctly via `pyramid_reserve()` in L3.

## L2 debug-mode block tracking — RESOLVED
Architecture originally said: "In debug builds, L2 must maintain tracking over which blocks it believes are in use."

Code: Instead of `#[cfg(debug_assertions)]` runtime tracking, a `verify()` chain (L4 -> L3 -> L2 -> L1) validates cross-layer integrity on demand. L4 walks the directory tree to collect stream IDs, L3 walks pyramid structures to collect block IDs, L2 cross-checks against the free list. This is arguably better: it catches orphaned blocks, free-list cycles, and stream/block mismatches in any build, not just debug builds.

## L1 head position vs offsets — RESOLVED
Previously listed as minor. Architecture updated: L1 API (architecture.md lines 412-419) no longer describes a read/write head. The offset-based API (`read(&self, offset, buf)`, `write(&self, offset, data)`) is now the intended design.

## L2 header stores shift exponent — RESOLVED
Previously listed as minor. Architecture updated: header layout diagram (architecture.md line 161) now says `block size shift | block index size shift`, matching the implementation's `bss: u8` and `biw: u8` fields.

## L2 batch allocation skips free-list linking — RESOLVED
Previously listed as minor. Architecture updated: (architecture.md line 393) now says "L2 instead expands creates the missing blocks and returns these", no longer mandating that new blocks be linked into the free list before allocation.

## Condvar instead of RwLock for Streams stream — RESOLVED
Previously listed as minor. Architecture (architecture.md lines 360-362) describes the required behaviour (mutual exclusion, sleeping threads) but does not prescribe a specific synchronisation primitive. The `Condvar + Mutex` implementation achieves exactly the described behaviour.

# Summary Table

|     | Area              | Architecture                  | Code                              | Severity |
| --- | ----------------- | ----------------------------- | --------------------------------- | -------- |
| 1   | Sfs::close        | Flush open handles            | close() + Drop impl              | Resolved |
| 2   | L2 flush          | Flushed to disk               | Written inside mutex, no fsync    | Minor    |
| 3   | L1 head position  | Offset-based (updated)        | Offset-based (stateless)          | Resolved |
| 4   | L2 block size     | Shift exponent (updated)      | Shift exponent                    | Resolved |
| 5   | L2 batch alloc    | Direct return (updated)       | Direct return, skip free list     | Resolved |
| 6   | Streams lock      | Behaviour-based (no primitive) | Condvar+Mutex                    | Resolved |
| 7   | Mock free list    | Required                      | Mock skips it                     | Minor    |
| 8   | L4 reserve        | Required                      | Implemented                       | Resolved |
| 9   | L2 debug tracking | Required in debug builds      | verify() chain instead            | Resolved |

Items 2 and 7 are minor gaps. Item 2 (no fsync) is an accepted trade-off — the mutex ordering is now correct, and only a kernel panic or power loss could cause inconsistency. Item 7 is acceptable (mock-only divergence). All other items are resolved, either by code changes or by architecture updates.

# Conformance notes

The following architectural requirements were verified as correctly implemented:

- **Layer decoupling**: L4 only imports L3 types, L3 only imports L2 types, L2 only imports L1 types, L1 imports no upper-layer types. Zero coupling violations found.
- **Magic header**: L1 correctly writes and validates `"stream_fs"` magic, header format version, and total header length. Each layer has a header slot with identifier and version.
- **File locking**: L1 uses `fs2` for cross-platform shared (read) and exclusive (write) process-level file locks.
- **Free list**: `BlocksInFile` correctly implements a linked free list with sentinel value `0xFFFF...` and proper chain/unchain logic.
- **Block cache**: Write-through, thread-local LRU cache in L2. Redirector blocks cached (`cache=true`), data blocks bypass (`cache=false`), matching architecture intent.
- **Pyramid structure**: L3 correctly implements the hierarchical redirector/data block pyramid with depth calculated from stream size. O(log n) seeks confirmed.
- **Stream descriptors**: 3 fields (top_block, size, reserved), 24 bytes, stored in the "Streams" stream with out-of-band descriptor in L3 header.
- **Per-stream locking**: One writer XOR many readers enforced via `HashMap<u64, LockState>` with Condvar-based blocking.
- **Directory entry format**: Length-prefixed (u16), identifier (block_index_width bytes), name (UTF-8). Entries appended on add, filtered and rewritten on delete, delete+re-add on rename.
- **Root directory**: Created on new file, index stored in L4 header, resolved from header on open.
- **Endianness**: All multi-byte management data stored little-endian throughout.
- **Integrity verification**: Full verify() chain from L4 -> L3 -> L2 -> L1 checks directory trees, pyramid structures, free list integrity, orphaned blocks, and file consistency.
