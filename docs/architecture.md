# Stream File System

This documentation provides a high level overview of the Stream File System (SFS) library (lib).

## SFS Purpose

The SFS crate is a Rust library that enable users to create, manage and use SFS storage files. An SFS storage file is a "file system in a file", wherein there is 0..n data streams, addressable by a string name. Each data stream can be read and written similar to how an real file can be read and written, meaning after a data stream has been opened, it returns a handle from which the user can obtain the length, a position (where reads and writes happen) and the contents of the data stream itself. After such a data stream has been opened, there are functions to read and write data from and to the data stream. 

## Data streams

Data streams appear to the user as a contiguous byte stream, much like a file. While they cannot be addressed like an array, the underlying library hides the underlying storage location, much like a virtual memory manager hides the memory pages of an operating system. Seeking a new position on the stream is an O(log n) operation.

Data streams are addressable by "directory" (recognised by ending with a forward-slash "/"). A data stream name must not contain forward-slash or null terminators, and it cannot have the same name as data stream in the same directory, but it can otherwise be named as the user sees fit. In this way, the user can store a data stream hierarchy that could look as follows:

* image.png
* another_image.png
* textures/skin.png
* textures/another_image.png
* an empty folder/

Data streams must be opened and closed, like a regular file in a file system. Once opened, a set of functions exist to operate on the data stream. Data streams should be closed when reading and writing is no longer needed, which ensures that any last writes are written to the data stream. A data stream can be opened for writing by only one active writer, whereas it can be opened for reading by multiple simultaneous readers.

It is not possible to "open" a directory, i.e. in the above example, the user cannot open "an empty folder/". The user can only open data streams for reading and writing - the "directories" function the same way as a real file system directory functions, in that they are used to contain things, including other directories.

Functions are provided to iterate over data streams and directories, akin to typing "ls" in a real file system, and to detect which entries are directories (that can be descended into) and which are files (that can be opened for reading a writing).

Functions are provided for deleting both data streams and directories and for shrinking the length of an existing data stream. If a data stream write exceeds the current length of the data stream, the data stream is extended automatically, akin to writing a regular file.

If a data stream "position" is set beyond the length of the data stream, an error occurs.

## Use

An SFS file is useful as a generic storage container. Given that it supports extending and shortening data streams, it is particularly useful as a way to provide storage for other libraries that continually write data to a file system and indeed for any writing scenarios where the expected length is not known when the data stream write begins. Some imagined scenarios:

* Storage for a database.
* Log entries.
* Crash recovery files (to capture unsaved work).

While these scenarios are obvious, it is also expected that SFS files will be used for convenience when needing to write a lot of different data (even if the total length of the data to write is known), as a sort of "upgraded" block based file layout, .e.g IFF (interchange file format) file, where data stream names are used instead of IFF block identifiers.

Naturally, to be usable as a storage container for other libraries, which would normally write their data to a regular file system, the using library has to support abstractions on top of the file system so that a write to a file goes instead into an SFS data stream.

## High level architecture

The SFS library is architecturally divided into four layers:

1. File system abstraction (the lowest layer)
2. Block storage abstraction
3. Data stream abstraction
4. Filing abstraction (the highest layer)

### Layer 4: Filing abstraction

A caller only engages with Layer 4 and callers cannot engage directly with Layer 3, 2 or 1. However, callers are often aware of which specific Layer 3, 2 and 1 implementation is used when they create the SFS file.

At Layer 4, directories can be created, deleted and iterated and streams can be opened, closed, written to, and read from.

Layer 4 provides the caller a data structure that wraps a data stream, which the caller must use to write data, read data from, change the head position, and shorten the length of the data stream. Additionally there are utility functions to "reserve" space for the data stream growing, which may be used to decrease the fragmentation of storage in the underlying layers and increase the speed of multiple sequential writes.

This layer also provides utility functions to iterate the named contents of the SFS file, equivalent to typing "ls" on a regular file system to discover the hierarchical content of the SFS file.

Internally, this layer translates between a "filing name" (e.g. "folder1/folder2/image.png") and a data stream number-based identifier. The data stream number-based identifier is never revealed to the caller.

In short, Layer 4 answers the question: Can you build a filing system out of numbered streams?

### Layer 3: Data stream abstraction

At this layer, data streams can be created, lengthened, shortened, deleted, written to and read from. This layer responds to requests from Layer 4 to create a new data stream and it returns a number-based identifier to Layer 4. This layer doesn't know anything about the filing name (that is the concern of Layer 4) or the content of the data streams.

Internally, this layer requests new blocks from Layer 2. It links these blocks together to create data streams. When the data streams are created and lengthened, it requests new blocks from Layer 2 to achieve this. When the data streams are deleted or shortened, it returns blocks no longer needed to Layer 2.

Layer 3 concerns itself only with data streams. It provides functions for Layer 4 to discover how many data streams exist and to iterate over each data stream identifier. It also provides to Layer 4 the ability to lengthen, shorten, create and delete data streams. Layer 3 never reveals to Layer 4 how data streams are linked together.

In short, Layer 3 answers the question: Can you build numbered streams out of numbered blocks?

### Layer 2: Block storage abstraction

Layer 2 manages blocks. It provides to Layer 3 a new, unused block when requested and it keeps track of unused blocks that it has created or received back from Layer 3. Layer 2 uses Layer 1 to work with the underlying storage system when it needs to create new blocks, which it does by lengthening the underlying SFS storage file, doing whatever is required to initialise free blocks and then adds them to an internal list of unused blocks.

In short, Layer 2 answers the question: Can you build numbered blocks out of a storage abstraction that behaves like a file?

### Layer 1: File system abstraction

At this layer, real file system access is shielded away from Layer 2. This layer can create a storage representation that acts like a fie on the underlying storage system, which it wraps away in some handle that is provided to Layer 2. At Layer 1, functions exist to create a storage representation, write to it, read from it, reposition the reading/writing head and shorten the storage representation. Layer 2 never touches the underlying file system directly; instead of works with Layer 1 to modify the underlying SFS storage.

In short, Layer 1 answers the question: Can the underlying storage be represented like a file, which can be locked, written to and read from.

### Layer composition opportunities

Layer 4 (the filing abstraction layer) provides to the caller a "virtual file system", with directories and files. Similarly Layer 1 (the file system abstraction) handles access to a file system, with directories and files.

Because Layer 4 and Layer 1 operate on the same constructs - files and directories - you could write a layer 1 that writes to and reads from a data stream in another SFS file, in the way of Matryoshka dolls.

This reveals some interesting opportunities, such as:

- One SFS file could be operating on top of a real file system, using very large blocks (say 512 kb). Large data streams could be written directly to this SFS file. For smaller data streams, a stream inside this SFS file (say "small_files.sfs") could operate with much smaller blocks (say 4 kb) to store small files. Whenever this small_files SFS file needed to expand to accommodate more small files, it would grow the small_files.efs data stream, in effect obtaining more 512 kb blocks to parcel out to in 1 kb increments.

- One SFS with large-ish blocks (e.g. 64 kb) file could use a custom Layer 2 (block storage layer) that, instead of storing its blocks through a Layer 1 file representation like normal, compressed and stored the 64 kb blocks it handled in an SFS file (with each block simply given its block number as a name) with much smaller blocks (say 4kb), thereby enabling real-time, transparent compression/decompression (and possibly encryption) of blocks.

- Instead of a Layer 2 that retrieves blocks from a single Layer 1 file, an alternative Layer 2 could be written that creates a directory instead of a .SFS file and store each block as a real file inside this directory. This would ease testability as each block could be inspected using a regular file manager.

- Instead of a Layer 3 file that links blocks together from Layer 2, an alternative Layer 3 could be written that creates a directory instead of an .SFS file and store each stream as a real file inside this directory. This would ease testability as new streams could be inspected using a regular file manager.

- Instead of a Layer 2 that stores and retrieves blocks into a regular file stream provided by Layer 1, it could encrypt blocks (with a length preserving encryption algorithm like AES-XTS) when they were written and decrypt them when they were read.

# SFS Architecture & Implementation notes

This documentation provides architectural support notes and considerations.

Throughout this document, layers are described by L1, L2, L3 and L4, denoting Layer 1, 2, 3 and 4 respectively.

## Project structure

There must be 4 projects across the workspace:

* SFS module, implemented in Rust: ./stream_fs/
* C ABI SFS wrapper module, implemented in Rust: ./stream_fs_c/
* Command line SFS manipulator, implemented in Rust: ./sfs_cl/
* Testing harness and test, implement in Python/pytest: ./sfs_pytest

## Implementation language

The library must be implemented as a Rust module, but support a C-compatible ABI on all public functions (using extern "C" and #[no_mangle] etc.). This ensures that the library can be linked into and used by as many other programming languages as possible. The public API must be a wrapper around a regular Rust L4 implementation so that other Rust libraries can use SFS without going through the C-style API. As far as possible, the Rust implementation must use static dispatch, i.e. generics, rather than dynamic dispatch.

## Testing

The library must be tested using Python/pytest. This allows us to rapidly develop tests. Wherever possible, an approach of writing tests first, then implementing the library to make those tests pass, also known as Test Driven Development (TDD), should be used. A python library that wraps all the SFS C-ABI calls must be written to simplify testing in Python.

## SFS file thread, process, machine & crash safety

An SFS file must handle being accessed by multiple threads within the same process and multiple processes on the local machine, with the following considerations:

- Within a process, a stream can be simultaneously opened for reading many times. If a stream is opened for reading, it cannot additionally be opened for writing. As long as a stream is opened for writing, all other attempts to open the stream, whether for reading or writing, fails. Multiple streams can, however, be opened at the same time, some for reading and some for writing. In short: Each stream can have one writer OR many readers.
- Within a machine, multiple processes may open an SFS file for reading simultaneously. Alternatively, a single process may open the SFS file for writing. If any process holds a write lock, no other process may open the file; if any process holds a read lock, no process may open for writing. In short: An SFS file can have one writer OR many readers.
- Across a network, no access coordination is attempted. Multiple machines accessing the same SFS file simultaneously is undefined behaviour. In short: Don't access an SFS file over the network unless you know you're the only one accessing the file.

SFS files are not ACID compliant. A process crash during a write is likely to leave the SFS file in an inconsistent state.

## Endianness

All multi-byte management data in an SFS file (block indices, stream lengths, header fields, directory entry lengths, etc.) is stored in little-endian byte order. This applies to all platforms — on big-endian systems, the library performs the necessary byte-swapping transparently.

Naturally SFS cannot make any guarantees about the endianness of user-written stream data, since it cannot know what is written. Callers are responsible for their own data encoding.

## Opening and creating a SFS file

An API is provided to create a new SFS file. This takes in a L3 type, a L2 type and a L1 type, which SFS uses to open the file and initialising the file for writing. A new file that is created is automatically opened for writing, internally using a L1 API call to lock the file for exclusive writing.

An API is also provided to open an existing SFS file, also taking in L3, L2 and L1 types. When opening a file, a caller must specify either for reading - in which case other processes can also open the file for reading - or for writing - in which no other process can. This is handled by L1 file locking calls.

When the four layers instantiate for creation, they each pass down a header (in layer order 4, 3, 2 and 1) as a byte array. Ultimately, on L1, the file is created writing out the byte arrays in the oppositive order (1, 2, 3 and then 4). What these headers contain is up to each layer, but all layer headers must start with a length descriptor. In a normal SFS file, the layout is as follows:

```
Magic: File magic header | header layout version
L1: length | L1 identifier | L1 version | data offset
L2: length | L2 identifier | L2 version | block size (bytes) | block index size (bytes)
L3: length | L3 identifier | L3 version | Streams stream descriptor
L4: length | L4 identifier | L4 version | root directory stream index
```

The `data offset` in L1's header records where block data begins in the file (immediately after L4's section). L1 needs this because it has no knowledge of how many upper layer sections exist or how large they are — `data offset` provides the definitive boundary between headers and data.

When a SFS file is opened, L1 (as it opens the file) checks the file magic header and the header layout version to ensure it can read the headers. This ensures that:

* This is indeed an SFS file, and
* The layout of the headers follows a form that the code can read (in this version it's simply "each layer header is preceded by a length")

Assuming that it can, it reads the L1 header section (whose length is encoded in the section's length field) and validates the L1 identifier. It then reads the `data offset` from the L1 section — this tells L1 exactly how many bytes of upper layer header sections follow. L1 reads that exact range and passes it up the call stack.

Each layer pops a header from the stack and compares it to what this layer expects to find, reading in the necessary information from the header to initialise this layer.

If all layers have found a header they are capable of handling, the file has successfully opened.

## L4: The public API

L4 deals with organising a filing system on top of L3. Where L3 deals with streams, identified by an index, L4 deals with "stream names" and "directory names".

L4's public API is similar to a file system API and pertains to:

* Creating, renaming and deleting a named stream,
* Creating, renaming and deleting a directory,
* Iterating over all streams inside of an SFS directory, including the root directory,
* Understanding the length of an SFS stream
* Reading from a stream, based on current head position,
* Positioning the read/write head inside of a stream,
* Writing to an stream, based on current head position (possibly enlarging the stream)
* Closing the SFS stream.

### Stream handles

 A stream handle that's returned has no public information, but L4 locate the internal information it needs to handle streams. A stream handle in L4 internally manages a head position, which determines where reads and writes occur from, like reading and writing to a regular file.

A stream handle can either be opened for reading or writing. When a new stream handle is created, it is opened for writing.

### A filesystem from streams

Two types of streams exist inside of an SFS file: Directory streams and data streams.

Data streams contain the data that a caller has written to the SFS file, with no padding, nothing added, nothing taken away.

Directory streams associate stream identifiers with a name, using a "stream entry" structure, e.g.

* Identifier: 43, Name: image.png
* Identifier: 101, Name: another_image.png
* Identifier: 36, Name: textures/
* Identifier: 930, Name: an empty folder/

When a stream entry ends with "/" the stream identifier points to another directory stream. If a stream entry does not end with a "/", the identifier points to a data stream.

When a new SFS file is created, it immediately creates a directory stream for the root directory and stores the index for this stream in the L4 header.

When an existing SFS file is opened, it reads the root directory stream index number from the header so it knows where to start the listing of named entries in the SFS file.

#### Stream entry management

Since stream entries can have variable length names, when they are serialised to a directory stream, each entry has its length prepended, so that the stream entries look as follows:

```
| length | identifier | name.............. | length | identifier | name.... | length | identifier | name......................| length | identifier | name... |
```

The length field for each entry includes its own size, i.e. if the length field was a uint16, the stream identifier a uint32 and the name was 12 bytes, the length would be 2 + 4 + 12 = 18 bytes.

When an entry is deleted from this stream, all the following entries are copied upwards in the stream and the stream is shortened, i.e.:

```
Before deletion	: | entry 1 | entry 2 | entry 3 | entry 4 | entry 5
-- Entry 2 is deleted --
After deletion	: | entry 1 | entry 3 | entry 4 | entry 5
```

When an entry is added, it is appended to the end of the stream, i.e.:

```
Before new entry:	| entry 1 | entry 2 | entry 3 
--- New entry inserted ---
After new entry:	| entry 1 | entry 2 | entry 3 | entry 4
```

If an entry is renamed, it is deleted and re-added at the end.

## L3 : Streams

L3 deals with linking blocks together from L2 and presenting them as identified data streams to L4.

### L3 API

L3's API, which is used by L4, should solely concern itself with:

* Creating a new stream (returns a stream identifier).
* Verifying that a stream exists, by identifier.
* Opening an existing stream in read or write mode, by identifier (returns a stream handle).
* Closing an existing stream, by handle.
* Deleting an existing stream by identifier. The stream must be closed before deletion.
* Reading from a stream, by handle.
* Writing to a stream, by handle and starting position, potentially enlarging the stream in the process.
* Shortening an existing stream, by handle.

### Stream handles

Unlike L4 stream handles, L3 stream handles do not contain a head position. It is the responsibility of L4 to read and write from/to a valid position.

### Streams from blocks

Blocks are linked together to form a data stream. L3 (data stream abstraction) must separately keep track of the total length of the stream and the index of the "top block", in a so called "Stream Descriptor":

```ASCII
Fig 1.             Fig 2.              Fig 3.                Fig 4.
┌──────────┐       ┌──────────┐        ┌──────────┐          ┌──────────┐        
│ Size: 0  │       │ Size: n  │        │ Size: n  │          │ Size: n  │
│ Top: 0   │  -->  │ Top: 4   │   -->  │ Top: 15  │   -->    │ Top: 140 │
└──────────┘       └────┬─────┘        └────┬─────┘          └────┬─────┘
                        ▼                   ▼                     ▼      
                  4┌──────────┐      15┌──────────┐       140┌──────────┐
                   │.....n    │        │4 |10|  | │          │15|11|  | │
                   └──────────┘        └─┬────────┘          └─┬────────┘
                                         ├ 4┌──────────┐       ├15┌──────────┐           
                                         │  │..........│       │  │4 |10|98|6│
                                         │  └──────────┘       │  └─┬────────┘
                                         └10┌──────────┐       │    ├ 4┌──────────┐
                                            │....n     │       │    │  │..........│
                                            └──────────┘       │    │  └──────────┘
                                                               │    ├10┌──────────┐
                                                               │    │  │..........│
                                                               │    │  └──────────┘
                                                               │    ├98┌──────────┐
                                                               │    │  │..........│
                                                               │    │  └──────────┘
                                                               │    └ 6┌──────────┐
                                                               │       │..........│
                                                               │       └──────────┘
                                                               └11┌──────────┐     
                                                                  │55|19|  | │
                                                                  └─┬────────┘
                                                                    ├55┌──────────┐
                                                                    │  │..........│
                                                                    │  └──────────┘
                                                                    └19┌──────────┐
                                                                       │.......n  │
                                                                       └──────────┘                         
```

* When the size of the stream is 0, the top block index is ignored, but must be 0 to avoid being 0xFFFF...(this value indicates that the Stream Descriptor is available for reuse). A stream with nothing written to it thus takes no blocks of storage (Fig 1)
* When the size of the stream is less than the length of one block, the "top block" contains the stream data (Fig 2)
* When the size of the stream is more than the length of one block, the "top block" now contains block indices and is called a redirector block (Fig 3). This top block can now hold sizeof(block) / sizeof(block index) indices. Each index points to another block index, which could potentially also be a redirector block (Fig 4). 
* In this way, the blocks involved in a data stream are either data blocks (at the lowest level of the block hierarchy) or redirector blocks (above the lowest level of data blocks), forming a "pyramid" data structure.

The lifetime of a new and growing data stream thus looks like this:

1. The data stream is created. At this point, no data or redirector blocks are in use. The size of the data stream, tracked separately, is 0 and the top block index is 0 (avoiding it being 0xFFFF... which implies that the stream descriptor is unused) (Fig 1)
2. Data starts to be written onto the stream. As this happens, the size of the stream grows above 0 and it's necessary for L3 to allocate a new block to hold this data. The size gets updated in the stream descriptor and the data gets written into the first, and only, data block (Fig 2, block 4)
3. More data gets written onto the stream and the length of the data stream exceeds what can be held in one block. 
   1. L3 now allocates a redirector block using L2 and it changes the data stream "top block" index to this new block (Fig 3, block 15).
   2. It writes the first index in the new redirector block to point to the original data block (Fig 3, inside block 15, observe index 4)
   3. It allocates another block (Fig 3, block 10), and writes the index of this block as the second index in the "top block" (Fig 3, inside block 15, observe index 10)
4. More data gets written onto the stream. 
   1. Eventually the top block cannot hold any more indices and a new redirector block must be allocated, sitting alongside the original redirector block. 
   2. To track these two redirector blocks, we need a new top block above the two redirector blocks (Fig 4, block 140)
   3. The first index in this new top block is, of course, the index of the old top block (Fig 4, block 140, observe index 15). The second index in the new top block is the new redirector block (Fig 4, block 140, observe index 11). In this way, we've increased the height of the data structure by one more.
5. As more and more stream data is written, the level of redirector blocks grow, from 0 (no redirectors), to 1 and beyond. The more data blocks we need to store, the "taller" the hierarchy of redirector blocks becomes.

The corollary case of a truncated stream is relatively simple 

#### Rationale

The "pyramid shaped" block linking layout has a number of advantages:

* The number of redirection blocks are kept minimal and for very small data streams even kept at 0.
* The lookup time for finding a random point in the stream is O(log n).
* The lengthening and shortening of data streams doesn't need to move any blocks, only allocate and return blocks, as the number of redirector blocks grow and shrink above the data blocks.
* The depth of the redirector blocks can be calculated directly from the size of the stream. In addition, there's no overhead to mark a block out as a particular type; since we can reason about the depth, we know how far down the hierarchy the data blocks lie and everything before that is a redirector block.
* Even with relatively small block sizes a *large* data stream can be tracked with low depth of tracking blocks. For example, if blocks are 4 kb and uint32 indices, an SFS stream can can grow to 4+ GB before needing a third redirector level

### Tracking streams in L3

L3 has a list of Stream Descriptors for all streams that exist. This list itself stored in a stream. To track the stream that stores Stream Descriptors, a single Stream Descriptor is held "out of band"; lets call this descriptor "Streams" here for clarity of description. 

When a new stream is created by a caller, we need to create a new Stream Descriptor. All Stream Descriptors are written into the Streams stream, which is expanded and contracted like every other stream. The Streams stream is of course initialised with 0 Stream Descriptors, because no other streams exist. As other streams are created, the Streams stream expands to hold these other Stream Descriptors.

Eventually some streams are deleted and the Stream Descriptor in Streams is marked free by writing a magic 0xFFFF.... value in the Top Block (this of course means the very highest block available in the SFS file cannot be used as its index is used to denote something special).

One possible optimization is to maintain a free list of streams. For now, a sequential scan of the Streams stream for a free stream descriptor is acceptable. If a free stream descriptor is not found, the Streams stream is enlarged (written to) with a new, unused stream descriptor.

### Thread safety in L3

Streams must be opened and closed like regular files before they can written to and read from, including the Streams stream. This is to track that there is at most one active writer (with no readers) or many active readers (with no writer). The list of active readers and writers per stream, which is not exposed directly to L4, but kept internally and found from the stream handle that L3 exposes to L4, must be guarded with mutex to avoid the multi-threaded creation of simultaneous readers and writer.

Since the Streams stream will be accessed by multiple threads (for example, to create a writable stream in two separate threads, ie.e two writers, one in each thread), the Streams stream must be protected. We cannot do this using a regular writer handle, as that will fail, rather than wait, if other threads have got readers on the Streams stream. So instead we must separate the concern of having a cursor into a stream (which is used to walk the block hierarchy of redirectors - if any - until arriving at a data block) and protecting this stream by either a reader/writer or a RWLock, where the calling thread awaits access to the Streams stream. In other words, it is now the handle to the stream that knows how to walk the block hierarchy, because we need similar functionality in the Streams stream (which we will access "raw", using the RWLock).

## L2: Blocks

L2 deals with presenting a file in a series of blocks. It uses L1 to make changes to a file and it presents only the opportunity to allocate and free blocks to L3.

### L2 API

L2's API, which is used by L3, concerns itself with creating, managing and returning blocks. It should be restricted to:

* Allocate a new block, which is returned to L3 by index
* Writing an buffer to a block, given by block index
* Reading a block to a buffer, given by block index
* Deallocate a used block, given by index

### Managing blocks

The management of blocks in L2 assumes nothing about "transactions" that modify multiple blocks. It is assumed that L3, where streams are managed, will appropriately handle access to a stream by multiple threads and that block writes are dispatched by L3 in the right order. Only a single writer can exist per stream at a time (as per SFS's thread/process safety constraints) so any writes to blocks in a stream will be guarded by L3.

Which block is allocated to which stream is untracked in L2. It is assumed that L3 remembers what blocks it has been given and what blocks it is handing back. In release builds, L2 doesn't perform any checking what whether it *should* be writing a block, or returning a block to the free list. In other words, L3 is responsible for the integrity of block management. In debug builds, however, L2 must maintain tracking over which blocks it believes are in use and check, when asked to write, read or deallocate a block that the block has previously been allocated.

#### Reusing blocks

When a block is deallocated, it will often be in the middle of the underlying file that L1 is managing. For efficiency, blocks are reused by way of keeping a free list of blocks.

L2 has a "first free" block index, which points to the first free block it's aware of (using 0xFFFF... to indicate there isn't a free block available). At the beginning of that first free block is written the index of the next free block, which is turn points to the next free block, and so on, until there are no more free blocks, indicated by 0xFFFF...

In that way, when L2 initialises on a new file, it writes 0xFFFF... in the "first free" block index.

When a block is deallocated, Layer two writes the block index of the previously "first free" block into the deallocated block (even if that is 0xFFFF as that would then terminate the linked list of blocks) and writes the index of the deallocated block into the "first free".

Similarly, when a new block needs allocating, L2 looks up the "first free block" and, if it's not 0xFFFF..., reads the index of the next free block from this block, placing it in the "first free" variable before returning the index of the previous "first free" block. If the "first free" is 0xFFFF, however, L2 instead expands by one or more free blocks, links them together by writing the index of the next block into the start of each block, and then allocates a free block.

### Thread safety in L2

Since multiple threads can attempt to allocate or deallocate blocks at the same time (because different streams from the same SFS file can be opened for writing at the same time), changes to the "first free" variable and associated changes to the underlying SFS file are protected by a mutex - and before code exits the critical section that modifies the free block list, changes to the file are flushed to disk.

## L1: File system

L1 deals with accessing an underlying file system. Most commonly this would be a real file-system, but adapters could be written for other situations, like writing an SFS file to memory. 

### L1 API

L1's API should expose the functions necessary to wrap actual file system calls for:

* Creating a file, locking it for exclusive writing
* Opening a file, locking it either for reading or exclusive writing
* Position the read/write head
* Writing to a file
* Reading from a file

### Thread & process safety in L1

L1 uses OS provided file-based locks to prevent other processes from writing to the file (if the caller is reading) or reading from the file (if the caller is writing.)