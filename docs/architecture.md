# Yak

This documentation provides a high level overview of the Yak library (lib).

## Yak Purpose

Yak is a Rust library that enable users to create, manage and use Yak storage files. A Yak storage file is a "file system in a file", wherein there is 0..n data streams, addressable by a string name. Each data stream can be read and written similar to how an real file can be read and written, meaning after a data stream has been opened, it returns a handle from which the user can obtain the length, a position (where reads and writes happen) and the contents of the data stream itself. After such a data stream has been opened, there are functions to read and write data from and to the data stream. 

## Data streams

Data streams appear to the user as a contiguous byte stream, much like a file. While they cannot be addressed like an array, the underlying library hides the underlying storage location, much like a virtual memory manager hides the memory pages of an operating system. Seeking a new position on the stream is an O(log n) operation.

Data streams are addressable by "directory" (recognised by ending with a forward-slash "/"). A data stream name must not contain forward-slash, and it cannot have the same name as data stream in the same directory, but it can otherwise be named as the user sees fit (including null terminators). In this way, the user can store a data stream hierarchy that could look as follows:

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

A Yak file is useful as a generic storage container. Given that it supports extending and shortening data streams inside of a single file, it is particularly useful as a way to provide storage for other libraries that continually write data to a file system and indeed for any writing scenarios where the expected length is not known when the data stream write begins, or changes over the lifetime of the file.

That said, even when the final data length is well known, Yak can be useful as a rapid, relatively well optimized way of writing binary data without having to worry about the particulars.

Some imagined scenarios:

* Log entry storage
* Receiving storage for streaming data
* Undo stack storage
* Application file formats
* Game save files
* Backup storage

Naturally, to be usable as a storage container for other libraries, which would normally write their data to a regular file system, the using library has to support abstractions on top of the file system so that a write to a file goes instead into a Yak data stream.

## High level architecture

The Yak library is architecturally divided into four layers:

1. File system abstraction (the lowest layer)
2. Block storage abstraction
3. Data stream abstraction
4. Filing abstraction (the highest layer)

### Layer 4: Filing abstraction

A caller only engages with Layer 4 and callers cannot engage directly with Layer 3, 2 or 1. However, callers are often aware of which specific Layer 3, 2 and 1 implementation is used when they create the Yak file.

At Layer 4, directories can be created, deleted and iterated and streams can be opened, closed, written to, and read from.

Layer 4 provides the caller a data structure that wraps a data stream, which the caller must use to write data, read data from, change the head position, and shorten the length of the data stream. Additionally there are utility functions to "reserve" space for the data stream growing, which may be used to decrease the fragmentation of storage in the underlying layers and increase the speed of multiple sequential writes.

This layer also provides utility functions to iterate the named contents of the Yak file, equivalent to typing "ls" on a regular file system to discover the hierarchical content of the Yak file.

Internally, this layer translates between a "filing name" (e.g. "folder1/folder2/image.png") and a data stream number-based identifier. The data stream number-based identifier is never revealed to the caller.

In short, Layer 4 answers the question: Can you build a filing system out of numbered streams?

### Layer 3: Data stream abstraction

At this layer, data streams can be created, lengthened, shortened, deleted, written to and read from. This layer responds to requests from Layer 4 to create a new data stream and it returns a number-based identifier to Layer 4. This layer doesn't know anything about the filing name (that is the concern of Layer 4) or the content of the data streams.

Internally, this layer requests new blocks from Layer 2. It links these blocks together to create data streams. When the data streams are created and lengthened, it requests new blocks from Layer 2 to achieve this. When the data streams are deleted or shortened, it returns blocks no longer needed to Layer 2.

Layer 3 concerns itself only with data streams. It provides functions for Layer 4 to discover how many data streams exist and to iterate over each data stream identifier. It also provides to Layer 4 the ability to lengthen, shorten, create and delete data streams. Layer 3 never reveals to Layer 4 how data streams are linked together.

In short, Layer 3 answers the question: Can you build numbered streams out of numbered blocks?

### Layer 2: Block storage abstraction

Layer 2 manages blocks. It provides to Layer 3 a new, unused block when requested and it keeps track of unused blocks that it has created or received back from Layer 3. Layer 2 uses Layer 1 to work with the underlying storage system when it needs to create new blocks, which it does by lengthening the underlying Yak storage file, doing whatever is required to initialise free blocks and then adds them to an internal list of unused blocks.

In short, Layer 2 answers the question: Can you build numbered blocks out of a storage abstraction that behaves like a file?

### Layer 1: File abstraction

At this layer, real file system access is shielded away from Layer 2. This layer can create a storage representation that acts like a file on the underlying storage system, which it wraps away in some handle that is provided to Layer 2. At Layer 1, functions exist to create a storage representation, write to it, read from it, read/write layer header data and shorten the storage representation. Layer 2 never touches the underlying file system directly; instead of works with Layer 1 to modify the underlying Yak storage.

In short, Layer 1 answers the question: Can the underlying storage be represented like a file, which can be locked, written to and read from.

### Layer composition opportunities

Layer 4 (the filing abstraction layer) provides to the caller a "virtual file system", with directories and files. Similarly Layer 1 (the file system abstraction) handles access to a file system, with directories and files.

Because Layer 4 and Layer 1 operate on the same constructs - files and directories - you could write a layer 1 that writes to and reads from a data stream in another Yak file, in the way of Matryoshka dolls.

This reveals some interesting opportunities, such as:

- One Yak file could be operating on top of a real file system, using very large blocks (say 512 kb). Large data streams could be written directly to this Yak file. For smaller data streams, a stream inside this Yak file (say "small_files.yak") could operate with much smaller blocks (say 4 kb) to store small files. Whenever this small_files Yak file needed to expand to accommodate more small files, it would grow the small_files data stream, in effect obtaining more 512 kb blocks to parcel out to in 1 kb increments.

- In addition to stream names stored in directory blocks at L4, an "extended attribute" stream could tie a property-value system together with the streams, hiding the properties away from the normal stream hierarchy.

- One Yak file with large-ish blocks (e.g. 64 kb) could use a custom Layer 2 (block storage layer) that, instead of storing its blocks through a Layer 1 file representation like normal, compressed and stored the 64 kb blocks it handled in a Yak file (with each block simply given its block number as a name) with much smaller blocks (say 4kb), thereby enabling real-time, transparent compression/decompression (and possibly encryption) of blocks.

- Instead of a Layer 2 that retrieves blocks from a single Layer 1 file, an alternative Layer 2 could be written that creates a directory instead of a `.yak` file and store each block as a real file inside this directory. This would ease testability as each block could be inspected using a regular file manager.

- Instead of a Layer 3 file that links blocks together from Layer 2, an alternative Layer 3 could be written that creates a directory instead of a `.yak` file and store each stream as a real file inside this directory. This would ease testability as new streams could be inspected using a regular file manager.

- Instead of a Layer 2 that stores and retrieves blocks into a regular file stream provided by Layer 1, it could encrypt blocks (with a length preserving encryption algorithm like AES-XTS) when they were written and decrypt them when they were read.

# Yak Architecture & Implementation notes

This documentation provides architectural support notes and considerations.

Throughout this section, layers are described by L1, L2, L3 and L4, denoting Layer 1, 2, 3 and 4 respectively.

## Project structure

There are 5 projects across the workspace:

| Module                    | Language      | Path        |
| ------------------------- | ------------- | ----------- |
| Yak library               | Rust          | yak/        |
| C ABI Yak wrapper         | Rust          | yak_c/      |
| Python Yak wrapper        | Rust          | yak_python/ |
| Command line Yak tool     | Rust          | yak_cl/     |
| Testing harness and tests | Python/pytest | yak_pytest/ |

* The Yak library is published as a crate on crates.io
* The C ABI wrapper produces a dynamic and a static library with non-mangled names for linking into a C/C++ project - and for use in any environment that supports C FFI libraries (e.g. LuaJIT). A Yak header file is generated using cbindgen, which defines the function names used in the libraries.
* The Python Yak wrapper is published as a PyPI wheel. It provides a full Yak experience in Python.
* The command line Yak tool is not meant as a day to day production tool, but it provides a set of functions to manipulate Yak files during development and, crucially, it provides a set of benchmarks that are useful for profiling and performance optimization.

## Testing

The library is tested using Python/pytest. This allows us to rapidly develop tests and to test the Python Yak wrapper.

Additional rust tests exist in the library itself, to test functionality that can't adequately be tested by pytest.

## Yak file thread, process, machine & crash safety

A Yak file must handle being accessed by multiple threads within the same process and multiple processes on the local machine, with the following considerations:

- Within a process, a stream can be simultaneously opened for reading many times. If a stream is opened for reading, it cannot additionally be opened for writing. When a stream is opened for writing, all other attempts to open the stream, whether for reading or writing, fails. Multiple streams can, however, be opened at the same time, some for reading and some for writing. In short: Each stream can have one writer OR many readers.
- Within a machine, multiple processes may open a Yak file for reading simultaneously. Alternatively, a single process may open the Yak file for writing. If any process holds a write lock, no other process may open the file; if any process holds a read lock, no process may open for writing. In short: A Yak file can have one writer OR many readers.
- Across a network, no access coordination is attempted. Multiple machines accessing the same Yak file simultaneously is undefined behaviour. In short: Don't access a Yak file over the network unless you know you're the only one accessing the file.

Yak files are not ACID compliant. A process crash during a write can leave the Yak file in an inconsistent state. A function to verify the integrity of a Yak file is provided, although it cannot spot all the ways in which the integrity of a Yak file can fail.

## Endianness

All multi-byte management data in a Yak file (block indices, stream lengths, header fields, directory entry lengths, etc.) is stored in little-endian byte order. This applies to all platforms -- on big-endian systems, the library performs the necessary byte-swapping transparently.

Naturally Yak cannot make any guarantees about the endianness of user-written stream data, since it cannot know what is written. Callers are responsible for their own data encoding, including that of encoding the byte order should they need to swap Yak files between opposing byte-order systems.

## Opening and creating a Yak file

An API is provided to create a new Yak file. This takes in a L3 type, a L2 type and a L1 type, which Yak uses to open the file and initialising the file for writing. A new file that is created is automatically opened for writing, internally using a L1 API call to lock the file for exclusive writing.

An API is also provided to open an existing Yak file, also taking in L3, L2 and L1 types. When opening a file, a caller must specify either for reading - in which case other processes can also open the file for reading - or for writing - in which no other process can. This is handled by L1 file locking calls.

When the four layers instantiate for creation, they request through L1 a "header slot", indicating how much data the layer needs to store in its header. When the header slot has been allocated by L1, each layer can read and write their header slot, passing in a byte array. What these headers contain is up to each layer; L1 transparently manages slot reading/writing and the guarding of the integrity of the headers. In a normal Yak file that uses the layers defined by the YakDefault type, the layout is as follows:

```
Magic: File magic header | header layout version | total layer header length
L1: length | L1 identifier | L1 version
L2: length | L2 identifier | L2 version | block size shift | block index width | free list head | encryption flag | [additional encryption parameters]
L3: length | L3 identifier | L3 version | Streams stream descriptor
L4: length | L4 identifier | L4 version | root directory stream index
```

The `total layer header length` in the magic section records the total size of all the layer headers, which equals the data offset where block data begins in the file (immediately after L4's section). 

When a Yak file is opened, L1 (as it opens the file) checks the file magic header and the header layout version to ensure it can read the headers. This ensures that:

* This is indeed a Yak file, and
* The layout of the headers follows a form that the code can read (in this version it's simply "the length of all headers is stored immediately after the version number" and "each layer header is preceded by a length")

Assuming that it can, L1 continues and provides to the upper layer an ability for each layer to locate its header slot, so they can read their own headers to initialse.

If all layers have found a header they are capable of handling, the file has successfully opened.

## L4: The public API

L4 deals with organising a filing system on top of L3. Where L3 deals with streams, identified by an index, L4 deals with "stream names" and "directory names".

L4's public API is similar to a file system API and pertains to:

* Creating, renaming and deleting a named stream,
* Creating, renaming and deleting a directory,
* Iterating over all streams inside of a Yak directory, including the root directory,
* Understanding the length of a Yak stream
* Reading from a stream, based on current head position,
* Positioning the read/write head inside of a stream,
* Writing to an stream, based on current head position (possibly enlarging the stream)
* Closing the Yak stream.

### Stream handles

 A stream handle that's returned has no public information, but L4 locate the internal information it needs to handle streams. A stream handle in L4 internally manages a head position, which determines where reads and writes occur from, like reading and writing to a regular file.

A stream handle can either be opened for reading or writing. When a new stream handle is created, it is opened for writing.

### A filesystem from streams

Two types of streams exist inside of a Yak file: Directory streams and data streams.

Data streams contain the data that a caller has written to the Yak file, with no padding, nothing added, nothing taken away.

Directory streams are used to provide an addressing system for the data streams. It associates stream identifiers with a name, using a "stream entry" structure, e.g.

| Stream entry no. | Stream identifier | Name              |
| ---------------- | ----------------- | ----------------- |
| 0                | 43                | image.png         |
| 1                | 101               | another_image.png |
| 2                | 36                | textures/         |
| 3                | 930               | an empty folder/  |

When a stream entry ends with "/" the stream identifier points to another directory stream. If a stream entry does not end with a "/", the identifier points to a data stream.

When a new Yak file is created, it immediately creates a directory stream for the root directory and stores the index for this stream in the L4 header.

When an existing Yak file is opened, it reads the root directory stream index number from the header so it can locate the root directory listing.

#### Stream entry management

Stream entries in the directory streams are serialized compactly. Since the length of their names vary, they are encoded as follows:

```ASCII
| stream identifier (2-8 bytes) | length (2 bytes) | name (1-65,522 bytes) ........ | 
```

Following all the name/stream ID entries is a so called "Name Table". The purpose of this table is to facilitate fast O(log n) searcing for a named stream. Of course, in a directory with 5 streams, this is rather overkill, but in a directory with 50,000 streams it drastically cuts down the time require to locate the right stream.

At the very, very end of the directory stream is an 4 byte that indicates the offset of the Name Table. When Yak needs to locate a stream by name, it jumps to the end (less 4 bytes) of the directory stream, reads the offset of the Name Table, jumps to that offset and reads the name table itself.

The Name Table consists of pairs of string hashes and offsets and it is sorted by string hashes. With the hash of the name the caller is searching for, Yak does a binary search to find the one or more hashes that matches the hash of the name the caller is attempting to open. If one or more identical hashes have been found in the Name Table, Yak jumps to these entries, compares the length of the string (for early rejection), then does a string compare for the caller requested name. If there's a match, L4 now has the Stream ID and can proceed to open this stream.

```ASCII
[Stream entries]* | [Name Table Entry]* | Size of Name Table
```

When we insert a new Stream entry, we copy the Name Table, write the new Stream entry at the current end of the Stream entries (overwriting the existing Name Table), then insert the new Name table entry into the Name Table (keeping this table always sorted by hash) and then write the size of the Name Table. This is a quick operation.

When we deleted a Stream entry, we "pull upwards" the remainders of streams by copying chunk by chunk forwards in the stream, then remove the hash/offset pair from the Name Table, write out the refreshed name table and finish by writing the size of the name table. This can be a slow operation.

If an entry is renamed, it is deleted and re-added.

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

Blocks are linked together to form a data stream. L3 must separately keep track of the total length of the stream and the index of the first block (known as the "top block"), in a so called "Stream Descriptor".

* When the size of the stream is 0, the top block index is ignored, but must be 0 to avoid being 0xFFFF...(this value indicates that the Stream Descriptor is available for reuse, see below). A stream with nothing written to it thus takes no blocks of storage (Fig 1)
* When the size of the stream is less than the length of one block, the "top block" contains the stream data (Fig 2)
* When the size of the stream is more than the length of one block, the "top block" now contains block indices and is called a redirector block (Fig 3). This top block can now hold sizeof(block) / sizeof(block index) indices. Each index points to another block index, which could potentially also be a redirector block (Fig 4). 
* In this way, the blocks involved in a data stream are either data blocks (at the lowest level of the block hierarchy) or redirector blocks (above the lowest level of data blocks), forming a "pyramid" data structure.

```ASCII
Fig 1.             Fig 2.              Fig 3.                Fig 4.
┌──────────┐       ┌──────────┐        ┌──────────┐          ┌──────────┐        
│ Size: 0  │       │ Size: 6  │        │ Size: 17 │          │ Size: 58 │
│ Top: 0   │  -->  │ Top: 4   │   -->  │ Top: 15  │   -->    │ Top: 140 │
└──────────┘       └────┬─────┘        └────┬─────┘          └────┬─────┘
                        ▼                   ▼                     ▼      
                  4┌──────────┐      15┌──────────┐       140┌──────────┐
                   │......    │        │4 |10|  | │          │15|11|  | │
                   └──────────┘        └─┬────────┘          └─┬────────┘
                                         ├ 4┌──────────┐       ├15┌──────────┐           
                                         │  │..........│       │  │4 |10|98|6│
                                         │  └──────────┘       │  └─┬────────┘
                                         └10┌──────────┐       │    ├ 4┌──────────┐
                                            │.......   │       │    │  │..........│
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
                                                                       │........  │
                                                                       └──────────┘                         
```



Accordingly, the lifetime of a new and growing data stream is as follows:

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

The corollary case of a truncated stream is relatively simple; start at the new top (most often a redirector node), free everything not associated with the redirector hierarchy from this node downward. If, at the end, the top node is a redirector node with only one entry, also free this node. Finally, point the stream decription's "top block" to point to the top of the new node hiearrchy.

#### Rationale

The "pyramid" block linking layout has a number of advantages:

* The number of redirection blocks are kept minimal and for very small data streams even kept at 0.
* The lookup time for finding a random point in the stream is O(log n).
* The lengthening and shortening of data streams doesn't need to move any blocks, only allocate and return blocks, as the number of redirector blocks grow and shrink above the data blocks.
* The depth of the redirector blocks can be calculated directly from the size of the stream. In addition, there's no overhead to mark a block out as a particular type; since we can reason about the depth, we know how far down the hierarchy the data blocks lie and everything before that is a redirector block.
* Even with relatively small block sizes a *large* data stream can be tracked with low depth of tracking blocks. For example, if blocks are 4 kb and uint32 indices, a Yak stream can can grow to 4+ GB before needing a third redirector level.

#### Additional feature & additional complexity: Reserved length
For clarity of explanation, the pyramid data structure explanation above does not deal with reserved capacity of a stream. A stream can be extended into being longer than needed to store the data in the stream. So while the explanation above is correct, it leaves out the fact that a stream descriptor actually has 3 fields: Top block, size and reserved, and that the resulting pyramid can therefore be larger than it needs to be to strictly hold the user-written data.

L3 separates between size (how much data the caller has written to the stream) and reserved (how much data the stream has space for). This is to create the capability to reserve space in the stream, which creates a couple of significant performance opportunities in the Yak library: 

* Callers can achieve significantly faster stream enlargement when it happens at once.
* Long runs of stream data can be allocated as contiguous runs of blocks on the underlying storage. Contiguity may seem a strange property to seek in a block-based storage format, but both stream read and stream write operations can achieve significant speed-ups when they detect contiguous runs of stream leaf-node data blocks, as they can read and write multiple blocks with one call of IO.
* Callers can achieve significantly faster trucations of streams when blocks contiguity is high.

When a stream is truncated, all blocks - including those in any extended reserve - are returned as free blocks, in effect making the stream's size and reserved size very near to each other (reserved will include the unwritten space in the last data block, so tends to be every so slightly larger).

#### Additional feature & additional complexity: Compression
For additional clarity of explanation, the pyramid data structure above does not deal with compression. To achieve transparent compression/decompression of data streams, an additional layer of indirection can be found at L3.

In an uncompressed data stream, the leaf nodes contain the data written by the caller. Observe Fig 1. below, with an uncompressed data stream, size 17, with a redirector block (15) pointing to two data blocks (4 and 10).

```ASCII
  Fig 1. Uncompressed.       Fig 2. Compressed                                               
  ┌──────────┐                ┌──────────┐                                                    
  │ Size: 17 │                │ Size: 75 │                                                    
  │ Top: 15  │       -->      │ Top: 15  │ 
  │ Cmprs: N │                │ Cmprs: Y │                                                    
  └────┬─────┘                └────┬─────┘                                                    
       ▼                           ▼                                                          
15┌──────────┐              15┌──────────┐                                                    
  │4 |10|  | │                │4 |10|  | │◄───────────── Redirector                           
  └─┬────────┘                └─┬────────┘                                                    
    ├ 4┌──────────┐             ├─4┌──────────┐                                               
    │  │..........│             │  │14|50|33| │◄──────── Compressed block               
    │  └──────────┘             │  └─┬────────┘                                               
    └10┌──────────┐             │    ├50┌──────────┐                                          
       │.......   │             │    │  │..........│◄──┐                                      
       └──────────┘             │    │  └──────────┘   ├ Compressed data            
                                │    └33┌──────────┐   │ (was size 40, compressed to size 14) 
                                │       │....      │◄──┘                                      
                                │       └──────────┘                                          
                                └─10┌──────────┐                                              
                                    │28|21|90|8│◄──────── Compressed block              
                                    └─┬────────┘                                              
                                      ├21┌──────────┐                                         
                                      │  │..........│◄──┐                                     
                                      │  └──────────┘   │                                     
                                      ├90┌──────────┐   │                                     
                                      │  │..........│◄──┼ Compressed data           
                                      │  └──────────┘   │ (was size 35, compressed to size 28)
                                      └─8┌──────────┐   │                                     
                                         │........  │◄──┘                                     
                                         └──────────┘                                         
```
In a compressed data stream however, "compressed blocks" are used instead of regular data blocks. In each compressed block is stored the compressed length of the data, followed by a set of block indices where this compressed data is stored.

Compressed blocks are, in effect, simply just another level of the pyramid data tree, treated slightly differently, but it is simpler to conceptually think of them as data blocks that can magically hold more data than could be stored in a physical block. And the way they achieve this magical feat is simply to compress the larger chunk of data they are magically defined to hold, reserve some more blocks to hold it, and then stores the compressed length of the data they're holding and the block indices where that compressed data is held.

When a Yak file is created, both the size of the physical block and the size of the compressed block is defined. It follows that a compressed block must be significantly larger than a physical block, or no compression savings could be achieved. For example, if a physical block was 4 kb and a compressed block was also 4 kb, then we might be able to compress this data down to 2 kb, but it would still need to be stored in a 4 kb block *and* we would now need a compressed block to point to the compressed data. By default Yak defines a compressed block size as 8 times larger than a physical block (i.e. a 4 KB physical block and a 32 KB compressed block).

On the other hand, if compressed blocks are defined to hold 32 kb, their data might be compressible to, say, 10 kb (if we're lucky), which could be stored in three 4 kb blocks. Then we'd need a compressed block to point to this compressed data, in effect cutting our real block usage from 8 blocks (32/4) down to 4 blocks (3 blocks for the compressed data + 1 block to point to this compressed data).

This does, of course, introduce some slow downs in that every time a compressed block needs modification, we need to locate the compressed data, decompress the data, modify the data and then recompress the data. Yak uses LZ4 as the compression algorithm, broadly considered the fastest, "decent" compression algorithm. The gain, on the other hand, is that we often need less IO (because we need to write and read less from the disk due to the data being smaller). In some scenarios, using compression actually improves the read and writing speed of Yak, but broadly it is best to think that compression trades reading/writing time for storage space requirements. Callers must decide through testing whether compression is worthwhile for their scenario. It is given, but is nevertheless worth stating, that data that's already compressed before it is added to the Yak file will not compress further and will, in fact, end up larger if added to a Yak file as a compressed stream.

Since some streams can be compressed and some streams won't be compressed, the Stream Descriptor has a single byte to hold flags at the end of the Stream Descriptor so that the actual layout looks as follows:
```ASCII
size (8 bytes) | top block (8 bytes) | reserved size (8 bytes) | flags (1 byte)
```

Currently only one flag (compression on or off) is stored in this byte, leaving space for 7 additional flags.

### Tracking streams in L3

L3 has a list of Stream Descriptors for all streams that exist. This list itself stored in a stream. To track the stream that stores Stream Descriptors, a single Stream Descriptor is held "out of band"; lets call this descriptor "Streams" here for clarity of description. 

When a new stream is created by a caller, we need to create a new Stream Descriptor. All Stream Descriptors are written into the Streams stream, which is expanded and contracted like every other stream. The Streams stream is of course initialised with 0 Stream Descriptors, because no other streams exist. As other streams are created, the Streams stream expands to hold these other Stream Descriptors; this expansion happens in L2 block-size chunks (eventually we will have to expand into new blocks when we add Stream Descriptors; we may as well do a full block and keep some in reserve to avoid having to continually expanding the Streams stream every time a new stream is added), which leaves a buffer of available stream descriptors for the next streams.

Eventually some streams are deleted and the associated Stream Descriptor in Streams is marked free. At the start of the Streams stream is written an 8 byte offset to the first free Stream Descriptor. If there are no free Stream Descriptors, a magic value if 0xFFF... is used to imply no free Stream Descriptors exist. In the Stream Descriptor pointed to by the offset in front of the Streams stream is written the value of the next free Stream Descriptor and so on, ensuring that all free Stream Descriptors are linked together as a chain. 

When a new Stream Descriptor is required and the first free offset (the first 8 bytes of the Streams stream) points to something valid, the Stream Descriptors are plucked from the front of this chain of free Stream Descriptors and reused.

### Thread safety in L3

Streams must be opened and closed like regular files before they can written to and read from, including the Streams stream. This is to track that there is at most one active writer (with no readers) or many active readers (with no writer). The list of active readers and writers per stream, which is not exposed directly to L4, but kept internally and found from the stream handle that L3 exposes to L4, must be guarded with mutex to avoid the multi-threaded creation of simultaneous readers and writer.

The Streams stream need modication every time a stream is created or deleted. Because many threads can do that at the same time, it is guarded to prevent multiple threads attempting to modify this stream at the same time. If one thread is modifying the Streams stream, and another thread needs to do the same at the same time, the second thread is put to sleep until the first thread is done with the modification. A separate, non-public L3 API enables L4 to read and write in a blocking fashion to ensure multiple threads do modifications to the Streams stream in an orderly fashion.

## L2: Blocks

L2 deals with presenting a file in a series of blocks. It uses L1 to make changes to a file and it presents only the opportunity to allocate and free blocks to L3.

### L2 API

L2's API, which is used by L3, concerns itself with creating, managing and returning blocks. It should be restricted to:

* Allocate a new block, which is returned to L3 by index
* Writing an buffer to a block, given by block index
* Reading a block to a buffer, given by block index
* Deallocate a used block, given by index

### Managing blocks

The management of blocks in L2 assumes nothing about "transactions" that modify multiple blocks. It is assumed that L3, where streams are managed, will appropriately handle access to a stream by multiple threads and that block writes are dispatched by L3 in the right order. Only a single writer can exist per stream at a time (as per Yak's thread/process safety constraints) so any writes to blocks in a stream will be guarded by L3.

Which block is allocated to which stream is untracked in L2. It is assumed that L3 remembers what blocks it has been given and what blocks it is handing back. L2 doesn't perform any checking what whether it *should* be writing a block, or returning a block to the free list. In other words, L3 is responsible for the integrity of blocks only being assigned to one stream, or available.

#### Reusing blocks

When a block is deallocated, it will often be in the middle of the underlying file that L1 is managing. For efficiency, blocks are reused by way of keeping a free list of blocks.

L2 has a "first free" block index in its L2 header, which points to the first free block in the Yak file (using 0xFFFF... when there is no free block available). In the first bytes of of first free block is written the index of the next free block, which is turn points to the next free block, and so on, until there are no more free blocks, indicated by 0xFFFF...

In that way, when L2 initialises on a new file, it writes 0xFFFF... in the "first free" block index, because there are no free blocks in the Yak file.

When a new block needs allocating, L2 attempts to deliver these from the free list before enlarging the file to create new blocks. It is beneficial, although not a requirement, for this free list of blocks to be in the order the blocks are laid out in the file. This way L2 can support L3 by delivering contiguous runs of blocks (to the extent it is possible), enabling multi-block read and write operations (i.e. IO operations that span across as many blocks as possible, thereby lowering the number of IO calls).

When blocks are deallocated, L2 therefore first sorts the block indicies given to it in order to promote them being later re-allocated in run-order. It then writes the index of the previous "first free" block into the tail of this newly deallocated chain of blocks, before setting the "first free" index to point to the start of the newly deallocated blocks.

While this approach doesn't guarantee that all free blocks sit in run order in the free list, it does promote many contiguous runs of blocks for a small runtime penalty, which helps to speed up stream operations from blocks that have been re-used.

### Block caching
L2 will often be requested to write and (especially) read the same block over and over again. This is most often the case when data is being written or read to a larger stream (i.e. a stream that must use redirector blocks) and the redirector blocks need constant access to locate the data in the stream. L2 therefore manages a write-through cache of blocks and it provides to L3 a way to use this write-through cache for some blocks, while not for others. As L3 writes or reads redirector blocks, it asks L2 to place and read these commonly accessed blocks in the write-through cache. Then, when L3 comes to read the redirector blocks, it is instead served from the in-memory cache and placed in the write-through cache before being written to disk. The actual data written/read to a stream circumvents the write-through cache, under the assumption that callers know what data they've placed in the Yak file (and therefore won't be asking the Yak file to read that data again) and, when reading, receive a copy of the data stream into their memory bufffer and therefore won't asking to read the same data again.

### Additional feature & additional complexity: Block encryption
Occasionnally, a caller may desire data stored in a Yak file to be encrypted. The default L2 implementation in Yak offers optional whole-file encryption for all data streams in a Yak file.

Yak intentionally takes a standard, industry-accepted approach towards encryption, but no one should trust their secrets to Yak's implementation or hope that there no vulnerabilities; undoubtedly there are many. 

When a block is requested by L3, L2 loads the block, decrypts it and returns it. When L2 is requested to store a block, L2 encrypts it and stores it back to disk. 

L2 uses a block cache to return often used blocks quickly, which is typically used to store hot redirector paths. Blocks in this write-through cache are stored unencrypted in memory; only when they are written to the Yak file are they encrypted.

Blocks are encrypted with a key that is then encrypted/decrypted with the caller's key (AKA "wrapping") and the stored in the L2 header. This allows a password change in the Yak file without having to reencrypt all blocks.

The caller key is derived from the user's password using Argon2id. The parameters for this derivation are also stored in the L2 header. Yak uses AES-XTS to encrypt each block, using the block number as the "tweak". 

To increase the computational cost for an attacker, all encryption is salted with a 16 byte salt, which is also stored in the header.

When encryption is enabled in a Yak file, L2's header is extended to store all these parameters so that the full L2 header looks like this:

| Field             | Size (bytes) | Notes                                                                                                           |
| ----------------- | ------------ | --------------------------------------------------------------------------------------------------------------- |
| Identifier        | 6            | ASCII "blocks" — identifies this as an L2 header                                                                |
| Version           | 1            | Header layout version (currently 1)                                                                             |
| Block size shift  | 1            | Block size = 2^shift bytes (e.g. 12 = 4 KiB)                                                                    |
| Block index width | 1            | Bytes per block index (e.g. 4 = 32-bit indices)                                                                 |
| Free list head    | 8            | Little-endian u64; index of first free block (sentinel = no free blocks)                                        |
| Encryption flag   | 1            | 1 = encrypted, therefore L2's header is extended with fields below.                                             |
| Salt              | 16           | Random salt for Argon2id key derivation                                                                         |
| m_cost            | 4            | Argon2id memory cost parameter (little-endian u32)                                                              |
| t_cost            | 4            | Argon2id time/iteration cost parameter (little-endian u32)                                                      |
| p_cost            | 1            | Argon2id parallelism parameter (single byte; values above 255 are impractical)                                  |
| Verification hash | 32           | SHA-256 hash of extra Argon2id-derived bytes; used for fast password verification without attempting key unwrap |
| Wrapped key       | 72           | 64-byte AES-256-XTS master key wrapped with AES-256-KW (adds 8-byte integrity overhead)                         |
| **Total**         | **18 / 147** | 18 bytes without encryption, 147 bytes with encryption                                                          |

It is important to know that the entire header of a Yak file is not encrypted.

Encryption and decryption is sped up using hardware instructions, where applicable. This often achieves a 20x speed up, although even with that, encrypted Yak files suffer a performance penalty (unencrypted Yak files tend to be around 3x faster than encrypted ones). 

If encryption is enabled together with compression the runtime penalty is less than the penalty for encryption and the penalty for compression added together. This is because compression ensures there is less data to encrypt/decrypt.

### Thread safety in L2

Since multiple threads can attempt to allocate or deallocate blocks at the same time (because different streams from the same Yak file can be opened for writing at the same time), changes to the "first free" variable and associated changes to the underlying Yak file are protected by a mutex - and before code exits the critical section that modifies the free block list, changes to the file are written to disk so they're visible by other threads in the same process.

The write-through cache is thread-local, i.e. each thread has its own cache. If multiple threads could write to a stream at the same time (which they cannot according to the Yak library's thread safety constraints), this thread-local approach wouldn't work. But since we've defined that only one thread will be writing to one stream at a time, a thread local approach ensures correctness without the need for additional threading synchronization.

## L1: File system

L1 deals with accessing an underlying file system. Most commonly this would be a real file-system, but adapters could be written for other situations, like writing a Yak file to memory.

In addition L1 deals with writing, reading and verifying Yak file "magic headers" to enable checking that a valid Yak file has been opened and to allow each layer above L1 to write their management data outside the regular block format.

### L1 API

L1's API should expose the functions necessary to wrap actual file system calls for:

* Creating a file, locking it for exclusive writing
* Opening a file, locking it either for reading or exclusive writing
* Writing to a file
* Reading from a file
* Writing a layer header
* Reading a layer header

### Thread & process safety in L1

L1 uses OS provided file-based locks to prevent other processes from writing to the file (if the caller is reading) or reading from the file (if the caller is writing.)