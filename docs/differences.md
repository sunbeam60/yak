Differences between architecture and implementation

Last reviewed: 2026-02-14

# Major Differences

## L2 doesn't flush before exiting critical section
Architecture says (architecture.md line 400, "Thread safety in L2" section): "before code exits the critical section that modifies the free block list, changes to the file are flushed to disk."

Code: Both `allocate_blocks` and `deallocate_block` drop the mutex *before* calling `persist_l2_header()`. The header write happens, but it is not guaranteed to be on disk before other threads see the updated in-memory state. No `flush()` call exists anywhere in the L1 trait or its implementation. A crash between the mutex release and the header persist could leave the free list inconsistent.

# Minor Differences

## L1 uses explicit offsets, not a read/write head
Architecture says (architecture.md lines 412-419, "L1 API" section) L1 should expose: "Position the read/write head", "Writing to a file", "Reading from a file"

Code: `FileLayer::read(&self, offset: u64, buf)` and `write(&self, offset: u64, data)` take explicit offsets. No head position concept exists. This is arguably better (stateless, thread-safe), but differs from the described API.

## L2 header stores block_size_shift, not block size in bytes
Architecture says (architecture.md line 161, header layout diagram): "L2: length | L2 identifier | L2 version | block size (bytes) | block index size (bytes)"

Code: Stores `bss: u8` (shift exponent) and `biw: u8` (width in bytes). The block size is derived as `1 << bss`. More compact but not the literal "block size in bytes" described in the architecture.

## L2 batch allocation skips free-list linking
Architecture says (architecture.md line 393, "Reusing blocks" section): "L2 instead expands by one or more free blocks, links them together by writing the index of the next block into the start of each block, and then allocates a free block from this newly formed list."

Code: `allocate_blocks(count)` grows the file once for all needed blocks and returns them directly without first chaining them into the free list. This is more efficient (fewer writes) but skips the described mechanism of linking new blocks into a free list before allocating from it.

## Condvar instead of RwLock for Streams stream
Architecture says (architecture.md lines 360-362, "Thread safety in L3" section): "it is guarded to prevent multiple threads attempting to modify this stream at the same time. If one thread is modifying the Streams stream, and another thread needs to do the same at the same time, the second thread is put to sleep until the first thread is done with the modification."
Note: the previously quoted text ("protecting this stream by either a reader/writer or a RWLock") does not appear in the current architecture.md. The architecture describes the behaviour but does not name a specific synchronisation primitive.

Code: Uses `Condvar + Mutex` with a `HashMap<u64, LockState>` instead of RwLock. This was a deliberate redesign to unify blocking/non-blocking lock acquisition. Achieves the same goal differently.

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

# Summary Table

|     | Area              | Architecture                  | Code                              | Severity |
| --- | ----------------- | ----------------------------- | --------------------------------- | -------- |
| 1   | Sfs::close        | Flush open handles            | close() + Drop impl              | Resolved |
| 2   | L2 flush          | Before mutex release          | After mutex release, no flush()   | Major    |
| 3   | L1 head position  | Explicit head API             | Offset-based (stateless)          | Minor    |
| 4   | L2 block size     | Bytes in header               | Shift exponent                    | Minor    |
| 5   | L2 batch alloc    | Link into free list, then use | Direct return, skip free list     | Minor    |
| 6   | Streams lock      | RwLock                        | Condvar+Mutex                     | Minor    |
| 7   | Mock free list    | Required                      | Mock skips it                     | Minor    |
| 8   | L4 reserve        | Required                      | Implemented                       | Resolved |
| 9   | L2 debug tracking | Required in debug builds      | verify() chain instead            | Resolved |

Items 3 and 6 were conscious implementation trade-offs that are arguably better than the architecture describes. Items 1, 8, and 9 are resolved. Item 2 is the remaining genuine gap — it could cause free-list corruption on crash during concurrent use.

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
