Differences between architecture and implementation

# Major Differences
## Endianness support — completely missing
The architecture explicitly states: "SFS keeps track of the endianness of files that it opens and respects the endianness of the file when it writes. For new files that SFS creates, SFS uses the endianness of the platform creating the file."

The code uses hardcoded little-endian everywhere (to_le_bytes() / from_le_bytes()). There's no endianness field in the header, no detection on open, and no mechanism to handle big-endian files. A file created on a big-endian platform would be unreadable on little-endian and vice versa.

## L3 API: delete_stream takes ID, not handle
Architecture says: "Deleting an existing stream by handle."

Code: fn delete_stream(&self, id: u64) — takes a stream ID. The stream must be closed first. The architecture implies deleting while the stream is open (via handle), but the implementation requires it closed.

## L3 API: create_stream returns ID, not handle
Architecture says: "Creating a new stream (returns a stream handle)."

Code: fn create_stream(&self) -> Result<u64, SfsError> — returns a stream ID. The stream is not auto-opened; L4 must call open_stream separately after creating. This is a deliberate design choice that simplifies L3, but deviates from the spec.

## No L3 stream iteration/enumeration API
Architecture says: "It provides functions for Layer 4 to discover how many data streams exist and to iterate over each data stream identifier."

Code: StreamLayer has stream_exists(id: u64) -> bool but no way to enumerate or count streams. L4 discovers streams through directory entries, which works in practice, but an external L4 implementation couldn't iterate the raw stream namespace.

## No reserve() API in L4
Architecture says: "Additionally there are utility functions to 'reserve' space for the data stream growing, which may be used to decrease the fragmentation of storage in the underlying layers."

Code: No reserve() method exists anywhere. Streams grow on-demand only.

## No L2 debug-mode block tracking
Architecture says: "In debug builds, however, L2 must maintain tracking over which blocks it believes are in use and check, when asked to write, read or deallocate a block that the block has previously been allocated."

Code: Neither BlocksInFile nor BlocksFromFiles has #[cfg(debug_assertions)] tracking. A double-free or write-to-freed-block would go undetected.

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
| 1   | Endianness        | Track & respect          | Hardcoded LE             | Major    |
| 2   | L3 delete         | By handle                | By ID, must be closed    | Major    |
| 3   | L3 create         | Returns handle           | Returns ID               | Major    |
| 4   | L3 iteration      | Enumerate streams        | Only stream_exists       | Major    |
| 5   | L4 reserve        | Required                 | Missing                  | Major    |
| 6   | L2 debug tracking | Required in debug builds | Missing                  | Major    |
| 7   | Sfs::close        | Flush open handles       | Drops without flushing   | Major    |
| 8   | L1 head position  | Explicit head API        | Offset-based (stateless) | Minor    |
| 9   | L2 block size     | Bytes in header          | Shift exponent           | Minor    |
| 10  | L2 batch alloc    | 1+ blocks                | 1 block only             | Minor    |
| 11  | L2 flush          | Before mutex release     | After mutex release      | Minor    |
| 12  | Streams lock      | RwLock                   | Condvar+Mutex            | Minor    |
| 13  | Mock free list    | Required                 | Mock skips it            | Minor    |

Items 2, 3, 8, and 12 were conscious implementation trade-offs that are arguably better than the architecture describes. Items 1, 5, 6, and 7 are genuine gaps. Item 7 is the most immediate risk (silent data loss).

