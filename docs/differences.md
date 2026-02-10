Differences between architecture and implementation

# Major Differences
## No reserve() API in L4
Architecture says: "Additionally there are utility functions to 'reserve' space for the data stream growing, which may be used to decrease the fragmentation of storage in the underlying layers."

Code: No reserve() method exists anywhere. Streams grow on-demand only.

## No L2 debug-mode block tracking
Architecture says: "In debug builds, however, L2 must maintain tracking over which blocks it believes are in use and check, when asked to write, read or deallocate a block that the block has previously been allocated."

Code: Instead of `#[cfg(debug_assertions)]` runtime tracking, a `verify()` chain (L4→L3→L2→L1) validates cross-layer integrity on demand. L4 walks the directory tree to collect stream IDs, L3 walks pyramid structures to collect block IDs, L2 cross-checks against the free list. This is arguably better: it catches orphaned blocks, free-list cycles, and stream/block mismatches in any build, not just debug builds.

## Sfs::close() doesn't flush open stream handles
Architecture says: "Data streams should be closed when reading and writing is no longer needed, which ensures that any last writes are written the data stream."

Code: pub fn close(self) { } just drops self. Since L3 handles are Copy (plain integers), dropping them triggers no cleanup. If a writer handle is still open at close() time, StreamsFromBlocks will never flush its cached descriptor to disk — data loss. There's no Drop impl on Sfs either.

# Minor Differences
## L1 uses explicit offsets, not a read/write head
Architecture says L1 should expose: "Position the read/write head", "Writing to a file", "Reading from a file"

Code: FileLayer::read(&self, offset: u64, buf) and write(&self, offset: u64, data) take explicit offsets. No head position concept. This is arguably better (stateless, thread-safe), but differs from the described API.

## L2 header stores block_size_shift, not block size in bytes
Architecture says: "L2: length | L2 identifier | L2 version | block size (bytes) | block index size (bytes)"

Code: Stores bss: u8 (shift exponent) and biw: u8 (width in bytes). More compact but not literal "block size in bytes".

## L2 extends one block at a time
Architecture says: "L2 instead expands by one or more free blocks, links them together..."

Code: allocate_block only extends by exactly one block. No batch allocation optimization.

## L2 doesn't flush before exiting critical section
Architecture says: "before code exits the critical section that modifies the free block list, changes to the file are flushed to disk."

Code: allocate_block and deallocate_block persist header values but don't call self.file.flush() before releasing the mutex. The write happens but isn't guaranteed to be on disk before other threads see the updated state.

## Condvar instead of RwLock for Streams stream
Architecture says: "protecting this stream by either a reader/writer or a RWLock, where the calling thread awaits access to the Streams stream."

Code: Uses Condvar + Mutex with a HashMap<u64, LockState> instead of RwLock. This was a deliberate redesign to unify blocking/non-blocking behavior. Achieves the same goal differently.

## BlocksFromFiles mock has no free list
Architecture says L2 must maintain a free list.

Code: BlocksFromFiles (mock) deletes the file on deallocate and always allocates sequentially. BlocksInFile (real) properly implements the free list. Acceptable for a mock, but doesn't match the spec.

Summary Table
|     | Area              | Architecture             | Code                     | Severity |
| --- | ----------------- | ------------------------ | ------------------------ | -------- |
| 1   | L4 reserve        | Required                 | Missing                  | Major    |
| 2   | L2 debug tracking | Required in debug builds | verify() chain instead   | Resolved |
| 3   | Sfs::close        | Flush open handles       | Drops without flushing   | Major    |
| 4   | L1 head position  | Explicit head API        | Offset-based (stateless) | Minor    |
| 5   | L2 block size     | Bytes in header          | Shift exponent           | Minor    |
| 6   | L2 batch alloc    | 1+ blocks                | 1 block only             | Minor    |
| 7   | L2 flush          | Before mutex release     | After mutex release      | Minor    |
| 8   | Streams lock      | RwLock                   | Condvar+Mutex            | Minor    |
| 9   | Mock free list    | Required                 | Mock skips it            | Minor    |

Items 4 and 8 were conscious implementation trade-offs that are arguably better than the architecture describes. Item 2 is resolved via `verify()`. Items 1 and 3 are genuine gaps. Item 3 is the most immediate risk (silent data loss).

