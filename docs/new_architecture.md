<!-- cspell:locale en-GB,da -->

# Yak

This documentation provides a high level overview of the Yak library (lib).

## AI decisions
This section lists decisions reached together with an AI. The rest of the document is still being writtena and may not yet be consistent with these decisions:
- **Transactions and snapshots.** What were write transactions are now *transactions*, and what were read transactions are now *snapshots*, so that "read" and "write" are never used as adjectives for them. A transaction is the only way to change a Yak file; a snapshot is a read-only view pinned to one committed version. They are begun on the Yak file and ended on themselves: `yk.begin_transaction()` returns a transaction, ended by `tx.commit()` or `tx.rollback()`, and `yk.begin_snapshot()` returns a snapshot, ended by `snap.close()`. Ending methods consume the transaction or snapshot, so the compiler prevents using it afterwards, and every Reader and Writer borrows it, so none can outlive it. The suffix belongs on the Yak file's methods, where it says what is being begun; on the transaction or snapshot itself it would be redundant.
  - In the C binding, which has no methods, every function carries the suffix: `yak_begin_transaction`, `yak_commit_transaction`, `yak_rollback_transaction`, `yak_begin_snapshot`, `yak_close_snapshot`. The Python binding follows Rust, and adds context managers (`with yk.begin_transaction() as tx:` commits on success and rolls back on an exception; `with yk.begin_snapshot() as snap:` closes on exit).
- **Stream handles are called Reader and Writer.** "Read handle" and "write handle" are replaced by `Reader` and `Writer`, so that the access mode is the noun (a Reader opened in a transaction is clear; a read handle in a write transaction is not). `Cursor` was rejected: it clashes with Rust's `std::io::Cursor`, means key iteration in databases such as LMDB, and would still need read/write variants. Reader implements `std::io::Read` and `Seek`; Writer implements `Read`, `Write` and `Seek`, so Yak streams can be passed to any library that takes them.
  - A Writer can only be opened in a transaction (`tx.open_writer(path)`, `tx.create_stream(path)`). A Reader can be opened in a transaction or a snapshot (`open_reader(path)`): in a transaction it reads the working version, in a snapshot that snapshot's version.
- **Directories have no handle or lister object.** `list(path)` returns a `Vec<DirEntry>`, a copy of the listing, in both transactions and snapshots. A lazy iterator would have to hold the directory stream open, which in a transaction blocks every name change in that directory until it is dropped (and deadlocks a caller who creates a name while iterating). If large directories ever make the copy too expensive, a lazy `read_dir(path)` returning a `ReadDir` iterator can be added for snapshots only, whose version never changes.
- **Name changes are plain methods on the transaction**, named consistently: `create_dir`, `delete_dir` (must be empty) and `rename_dir` replace `mkdir`, `rmdir` and `rename_dir`, alongside `create_stream`, `delete_stream` and `rename_stream`. Snapshots do not have them, so the compiler prevents changing names in a read-only view. The resulting API:

  |                                                        | Transaction         | Snapshot        |
  | ------------------------------------------------------ | ------------------- | --------------- |
  | `list(path)` → `Vec<DirEntry>`                         | ✓ (working version) | ✓ (its version) |
  | `open_reader(path)` → Reader                           | ✓                   | ✓               |
  | `open_writer(path)` → Writer                           | ✓                   |                 |
  | `create_stream(path)` → Writer                         | ✓                   |                 |
  | `delete_stream`, `rename_stream`                       | ✓                   |                 |
  | `create_dir`, `delete_dir`, `rename_dir`               | ✓                   |                 |
  | `stream_version`, `dir_version`, `version` → `Version` | ✓ (working version) | ✓ (its version) |
  | `commit`, `rollback`                                   | ✓                   |                 |
  | `refresh`, `close`                                     |                     | ✓               |
  

- **An open stream can be renamed.** Readers and Writers hold a stream's identifier, not its name, and a rename only changes directories, so renaming a stream that is open is allowed. Deleting a stream is still refused while a Reader or Writer in the same transaction has it open; Readers in snapshots are unaffected and keep reading their version.
- **Block indices and stream identifiers are u32 for 1.0, through named types.** The code uses `BlockIndex` and `StreamId` (each with its sentinel constant), defined in one place, never a bare `u32`. u32 is enough for 1.0: block indices cap a file at 2^32 - 1 blocks (about 16 TiB with 4 KiB blocks, more with larger blocks), and every stream that holds data needs at least one block, so only empty streams could ever exceed 2^32 identifiers - and other limits, such as the 4 GiB maximum size of a directory stream, are reached long before. Stream identifiers of deleted streams are reused, so the limit is on streams existing at the same time, not streams ever created.
  - Because the width is defined in one place, Yak can be built with larger (or smaller) types by changing those definitions. A file built with one width cannot be read with another, so the width must be recorded through the layer identifiers or versions in the header sections, and opening a file with the wrong width fails cleanly.
  - If more than one width is ever needed in the same build, the planned route is associated types on the layer that owns each concept (`BlockLayer::BlockIndex`, `StreamLayer::StreamId`), so each layer still only knows the one below it. This is not done for 1.0: it makes every on-disk structure generic, the C and Python bindings would need a concrete type per width, and the test suite would need to run per width.

## Yak purpose

Yak is a Rust library that enables users to create, manage and use Yak storage files. A Yak storage file is a "file system in a file", wherein there are 0..n streams, addressable by a name. Each stream can be read and written similar to how a real file can be read and written, meaning that after a stream has been opened, it returns a handle from which the user can obtain the length, a position (where reads and writes happen) and the contents of the stream itself. After such a stream has been opened, there are functions to read and write data from and to the streams. All changes to Yak files occur within a transaction that are committed or rolled-back as one, and which protects a Yak file from becoming inconsistent in the event of a crash or power-loss, and protect simultaineous readers from reading changes in progress. 

## Streams, directories and transactions

Streams appear to the user as a contiguous byte stream, much like a file. While they cannot be addressed like an array, the library hides the underlying storage location, much like a virtual memory manager hides the memory pages of an operating system. Seeking a new position on the stream is an O(log n) operation.

Streams are addressable by "directory" (recognised by ending with a forward-slash "/"). A stream name must not contain forward-slash, and it cannot have the same name as a stream in the same directory, but it can otherwise be named as the user sees fit (including null terminators). In this way, the user can store a stream hierarchy that could look as follows:

* image.png
* another_image.png
* textures/skin.png
* textures/another_image.png
* an empty folder/

It is not possible to "open" a directory, i.e. in the above example, the user cannot open "an empty folder/". The user can only open streams for reading and writing - the "directories" function the same way as a real file system directory functions, in that they are used to contain things, namely other directories and streams.

To make changes to streams and directories, a transaction must be begun. Using this transaction, the necessary changes can be made and then committed, enabling atomic changes to occur in the file. A transaction enables both reading and writing from streams and any changes made in the transaction are immediately visibly to other readers created by the same transaction. 

Alternatively, a snapshot can be created, which enables the reading of contents of directories and streams. This snapshot is frozen in time from the last last successful transaction commit and it is isolated from changes that occur in later transactions. A snapshot can be manually refreshed, which brings it up to date to the last successful transaction, meaning its view of the Yak file is always consistent.

From a transaction, functions are provided for deleting both streams and directories and for shrinking the length of an existing stream. If a stream write exceeds the current length of the stream, the stream is extended automatically, akin to writing a regular file. If a stream "position" is set beyond the length of the stream, an error occurs. Any changes made only become durable, and readable by later or refreshed snapshots, once committed. 

## Use

A Yak file is useful as a generic storage container. Given that it supports extending and shortening streams inside of a single file, it is particularly useful as a way to provide storage for other libraries that continually write data to a file system and indeed for any writing scenarios where the expected length is not known when the stream write begins, or changes over the lifetime of the file.

That said, even when the final data length is well known, Yak can be useful as a rapid, relatively well optimized way of writing binary data without having to worry about the particulars.

Transactions make Yak suitable for data that must survive a crash intact, such as an application's document format, where a save must either complete or leave the previous save untouched.

Some imagined scenarios:

* Log entry storage
* Receiving storage for streaming data
* Undo stack storage
* Application file formats with crash-safe saves
* Game save files
* Backup storage
* A storage backend for multi-dimensional array data

Naturally, to be usable as a storage container for other libraries, which would normally write their data to a regular file system, the using library has to support abstractions on top of the file system so that a write to a file goes instead into a Yak data stream.

## High level architecture

The Yak library is architecturally divided into five layers:

0. System abstraction (the lowest layer)
1. Storage abstraction
2. Block abstraction
3. Data stream abstraction
4. Filing abstraction (the highest layer)

These layers work together to deliver the capabilities of the Yak library. Each upper layer depends only on the layer immediately below it and is unaware of layers below that.

### Layer 4: Filing abstraction

A caller only engages with Layer 4 and callers cannot engage directly with Layer 3, 2, 1 or 0. However, callers are often aware of which specific Layer 3, 2, 1 and 0 implementation is used when they create the Yak file.

Layer 4 is where a caller reads and writes named streams, both done through transactions and snapshots; transactions to change anything in the Yak file and snapshots to be isolated from changes that occur elsewhere.

At Layer 4, directories can be created, deleted and listed and streams can be opened, closed, written to, and read from.

Layer 4 provides to the caller a data structure that wraps a stream, which the caller must use to write data, read data from, change the position, and shorten the length of the data stream. Additionally there are utility functions to "reserve" space for the stream growing, which keeps a stream that grows across many commits - a log, say - in one contiguous run.

This layer also provides utility functions to iterate the named contents of the Yak file, equivalent to typing "ls" on a regular file system to discover the hierarchical content of the Yak file.

Internally, this layer translates between a "filing name" (e.g. "folder1/folder2/image.png") and a stream number-based identifier. The stream number-based identifier is never revealed to the caller.

In short, Layer 4 answers the question: Can you build a filing system out of numbered streams?

### Layer 3: Stream abstraction

At this layer, streams can be created, lengthened, shortened, deleted, written to and read from. This layer responds to requests from Layer 4 to create a new stream and it returns a number-based identifier to Layer 4. This layer doesn't know anything about the filing name (that is the concern of Layer 4) or the content of the streams.

Internally, this layer requests new blocks from Layer 2. It links these blocks together to create streams. When the streams are created and lengthened, it requests new blocks from Layer 2 to achieve this. When the streams are deleted or shortened, it returns blocks no longer needed to Layer 2.

Layer 3 concerns itself only with streams. It provides functions for Layer 4 to discover how many streams exist and to iterate over each stream identifier. It also provides to Layer 4 the ability to lengthen, shorten, create and delete streams. Layer 3 never reveals to Layer 4 how streams are composed of blocks that are linked together.

Layer 3 is also responsible for optimising the layout of blocks used in streams to optimise towards quick writes and reads (one long read/write instead of lots of little reads/writes) and it provides a capability to amass changes in memory and commit them to the file as one.

In short, Layer 3 answers the question: Can you build numbered streams out of numbered blocks?

### Layer 2: Block abstraction

Layer 2 manages blocks but does not know what role each block takes. It provides to Layer 3 a new, unused block when requested and it keeps track of unused blocks that it has created or received back from Layer 3. Layer 2 uses Layer 1 to create new blocks, which it does by lengthening the the data region, doing whatever is required to initialise free blocks and then adds them to an internal list of unused blocks. Because snapshots are frozen in time from when they were created, Layer 2 holds back blocks that have been released by Layer 3 but may still be read by live snapshots. When a snapshot is closed, Layer 2 handles returning "frozen" blocks as free blocks, ready for re-use.

In short, Layer 2 answers the question: Can you build numbered blocks out of a storage abstraction that behaves like a file?

### Layer 1: Storage abstraction

Layer 1 provides the fundamental storage abstractions that enables Yak to work. It creates and manages the header that stores information from each layer above. It provides access to the data region that will hold blocks and it provides the transaction primitives which with L2 and the layers above change the content atomically. Finally it maintains a system to coordinate between multiple processes reading from and writing to the same Yak file. 

In short, Layer 1 answers the question: Can you build a file that changes atomically, and that several processes & threads can share safely, out of the calls a system provides?

### Layer 0: System abstraction

Layer 0 provides system-specific implementations of file, lock and sync primitives. It has no knowledge of Yak at all and keeps no Yak state of its own. It simply provides a simplified and unified interface for Layer 1 to do its work.

In short, Layer 0 answers the question: Can the underlying system provide operating system primitives that can be opened by name, read from, written to, synced and locked?

### Layer composition opportunities

Layer 4 (the filing abstraction layer) provides to the caller a "virtual file system", with directories and files. Similarly Layer 0 (the storage abstraction) handles access to a file system, with directories and files.

Because Layer 4 and Layer 0 operate on the same constructs - files and directories - you could write a Layer 0 that writes to and reads from a stream in another Yak file, in the way of Matryoshka dolls.

This reveals some interesting opportunities, such as:

- One Yak file could be operating on top of a real file system, using very large blocks (say 512 KB). Large streams could be written directly to this Yak file. For smaller streams, a stream inside this Yak file (say "small_files.yak") could operate with much smaller blocks (say 4 KB) to store small files. Whenever this small_files Yak file needed to expand to accommodate more small files, it would grow the small_files data stream, in effect obtaining more 512 KB blocks to parcel out in 4 KB increments.

- In addition to stream names stored in directory blocks at L4, an "extended attribute" stream could tie a property-value system together with the streams, hiding the properties away from the normal stream hierarchy.

- One Yak file with large-ish blocks (e.g. 64 KB) could use a custom Layer 2 (block storage layer) that, instead of storing its blocks in Layer 1's data region like normal, compressed and stored the 64 KB blocks it handled in a Yak file (with each block simply given its block number as a name) with much smaller blocks (say 4 KB), thereby enabling real-time, transparent compression/decompression (and possibly encryption) of blocks.

- Instead of a Layer 2 that retrieves blocks from Layer 1's data region, an alternative Layer 2 could be written that creates a directory instead of a `.yak` file and store each block as a real file inside this directory. This would ease testability as each block could be inspected using a regular file manager.

- Instead of a Layer 3 that links blocks together from Layer 2, an alternative Layer 3 could be written that creates a directory instead of a `.yak` file and store each stream as a real file inside this directory. This would ease testability as new streams could be inspected using a regular file manager.

- Instead of a Layer 2 that stores and retrieves blocks into a regular file stream provided by Layer 1, it could encrypt blocks (with a length preserving encryption algorithm like AES-XTS) when they were written and decrypt them when they were read.

- A Layer 0 could record every write and sync it receives, and on request simulate a power loss by discarding writes that were never synced or tearing the last write in half. This makes it possible to test that every crash leaves a Yak file at either the old or the new version (see Testing). Because Layer 0 knows nothing about commits, this tests the real Layer 1 commit logic, not a copy of it.

- When a Yak file is nested inside a stream of another Yak file, the inner file's commit is just a set of writes to the outer stream. The inner commit is only durable once the outer Yak file has also committed, so a nested Yak file's Layer 0 should hold a write transaction on the outer file and commit it (with `commit_and_continue`, see Committing and ending transactions) when asked to sync. Such a Layer 0 keeps its locks in memory, so a nested Yak file can only be used by the process that opened it.

# Yak Architecture & Implementation notes

This documentation provides architectural support notes and considerations.

Throughout this section, layers are described by L0, L1, L2, L3 and L4, denoting Layer 0, 1, 2, 3 and 4 respectively.

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

The library is tested using Python/pytest. This allows rapid test development and testing of the Python Yak wrapper.

Additional Rust tests exist in the library itself, to test functionality that can't adequately be tested by pytest.

Crash consistency is tested with a fault-injecting L0 (see Layer composition opportunities), which can introduce fake crashes.

## Yak file thread, process, machine & crash safety

A Yak file must handle being accessed by multiple threads within the same process and multiple processes on the local machine, with the following considerations:

- Multiple processes can open a Yak file for both reading and writing. Each process can only open a Yak file once.
- If a process has a Yak file opened for reading, it can open multiple snapshots in any thread.
- If a process has a Yak file opened for both reading and writing, it can open multiple snapshots in any thread and one transaction across all threads.
- Regardless of how many processes have opened the file for writing, transactions run serially. All other attempts to begin a transaction in another process either wait their turn (block) or fail if not willing to wait.
- Regardless of how many processes have opened the file for reading and writing, snapshots never block (neither for other snapshots or transactions) and never prevent a transaction from occurring. 
- A snapshot can open multiple Readers per stream.
- A transaction can open multiple Readers per stream OR one Writer per stream.
- All access coordination happens on the same machine. It is not safe to open a Yak file over the network, unless callers know they are the only ones accessing the file.

If a process crash or a system power loss occurs, a Yak file opens to look exactly like it did at the last successful transaction, with no recovery work required by the caller. If a transaction was in progress during the crash it is as if it never happened. In other words, changes to Yak files are ACID:
- **A**tomic - they either happened fully or they didn't happen at all.
- **C**onsistent - a Yak file is always "valid" - although since Yak doesn't know what a caller writes to the file, consistency of user data cannot be guaranteed.
- **I**solated - a change is invisible until it happens and every snapshot sees only a committed version.
- **D**urable - when a commit has returned successfully, data is guaranteed to be stored.

Since commits can be optionally specified to occur with no durability (increases speed when the Yak file's integrity isn't critical or can easily be recreated), a function to verify the integrity of a Yak file is provided.

## Block indices and stream identifiers
Block indices are defined to be 32-bit unsigned integers (u32), whatever the block size. The value 0xFFFFFFFF is reserved as a sentinel meaning "no block", so a Yak file holds at most 2^32 - 1 blocks. The block size therefore sets the largest possible file (e.g. ~16 TiB with 4 KiB blocks and ~256 TiB with 64 KiB blocks). Files that need to be larger use larger blocks. Yak can optionally be built using smaller (u8, U16) or larger (u64) blocks but Yak files created with these settings must be opened by a custom Yak library that supports this type. Block index width (1, 2, 4 or 8 bytes) is stored in a Yak file and an error occurs if a caller attempts to load a Yak file created using a different block index width.

It follows that stream identifiers are the same datatype as block indices, since a stream typically are at least 1 block and there cannot be more streams than there are blocks possible (technically, a stream can take no storage other than the stream descriptor in the Streams stream, but it's hard to imagine a Yak file with more than 4 billion completely empty streams). For stream identifiers, 0xFFFFFFFF is reserved as a sentinel value as well.

Byte counts, such as a stream's size and reserved size, are u64, because a single stream can be larger than 4 GiB.

## Endianness

All multi-byte management data in a Yak file (block indices, stream lengths, header fields, directory entry lengths, etc.) is stored in little-endian byte order. This applies to all platforms -- on big-endian systems, the library performs the necessary byte-swapping transparently.

Naturally Yak cannot make any guarantees about the endianness of user-written stream data, since it cannot know what is written. Callers are responsible for their own data encoding, including that of encoding the byte order should they need to swap Yak files between opposing byte-order systems.

## Transactions & snapshots

All modifications made in a Yak ultimately end up as modifications to the bytes held in the stream blocks and to the header of the Yak file. In a storage library without transaction support, changes would be written directly to the file, but in Yak (like in other embedded storage libraries with transaction support), changes made by a transaction are kept separate from the original file; instead of performing those changes on the original blocks and the original header, all changes occur in *new* blocks and a *new* header that is kept in memory by the transaction. 

### Committing a transaction

When a user needs to change the value of 10 bytes in a stream, say, Yak first copies the original block where those 10 bytes are held into memory and then performs the change on this in-memory block, not the original, physical block on disk. The transaction keeps hold of this in-memory block, and any other blocks that are modified during the transaction, and whenever further changes, or further reads, happen in those modified blocks, the transaction uses the in-memory, stand-in blocks rather than the blocks on disks. In effect, a transaction pretends that the changes have occurred in the original file, but they have actually only occurred in the in-memory stand-in blocks.

When a transaction is committed all the in-memory blocks are written to the Yak file - not on top of the existing blocks, but as new blocks. These blocks are connected together by redirectors, from bottom up, as they are written into the file. An additional upside of knowing which blocks have changed, and which streams they belong to, is that the blocks can be written out as contiguously as possible, promoting optimised read and write speeds on the actual file.

Once the blocks are known to have actually been written to disk, the header is updated with a new version number and also written to disk. To avoid the new header overwriting the old header that may be in the process of being read by another thread or process, the header is written to a second header slot and a higher version number is written as the very last thing in the write. This makes the second slot the highest header version, which causes new readers to recognise the new header as the current header. In this way, writers alternate between two header slots, using the version number to signal which one is the latest one.

As a practical example, observer Fig 1.a. Here the current header is version 410 and its out-of-band stream descriptor (which points to the Streams stream) indicates that block 84 is the top block in the Streams stream. Inside block 41 there are 5 stream descriptors, with the last one pointing to a data stream at block 140.

Fig 1.a

 Header 1                Header 2                                                                            
 (current header)                                                                                            
┌──────────────┐        ┌──────────────┐                                                                     
│ Version 410  │        │ Version 409  │                                                                     
│              │        │              │                                                                     
│              │        │              │                                                                     
│ ┌          ┐ │        │ ┌          ┐ │                                                                     
│ │ Size: 14 │ │        │ │ Size: 10 │ │                                                                     
│ │ Top:  84 │ │        │ │ Top:  12 │ │                                                                     
│ └       │  ┘ │        │ └          ┘ │                                                                     
└─────────┼────┘        └──────────────┘                                                                     
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          │                                                                                                  
          ▼  (Streams stream)                                                                                
          84┌──────────┐                                                                                     
            │41|39|  | │                                             (data stream)                           
            └─┬────────┘                                      ┌─►140┌──────────┐                             
              ├41┌──────────┐                   ┌          ┐  │     │15|11|  | │                             
              │  │[][][][][]┼──────────────────►│ Size: 18 ├──┘     └─┬────────┘                             
              │  └──────────┘                   │ Top: 140 │          ├15┌──────────┐                        
              └39┌──────────┐                   └          ┘          │  │..........│                        
                 │[][]      │                                         │  └──────────┘                        
                 └──────────┘                                         └11┌──────────┐                        
                                                                         │........  │                        
                                                                         └──────────┘                        
                                                                                                             
 ─────────────────────────────────────────────────────────────────────────────────────────────────────────── 

Fig 1.b

 Header 1                Header 2                                                                            
                         (current header)                                                                    
┌──────────────┐        ┌──────────────┐                                                                     
│ Version 410  │        │ Version 411  │                                                                     
│              │        │              │                                                                     
│              │        │              │                                                                     
│ ┌          ┐ │        │ ┌          ┐ │                                                                     
│ │ Size: 14 │ │        │ │ Size: 14 │ │                                                                     
│ │ Top:  84 │ │        │ │ Top:  60 │ │                                                                     
│ └       │  ┘ │        │ └       │  ┘ │                                                                     
└─────────┼────┘        └─────────┼────┘                                                                     
          │                       │                                                                          
          │                       │ (new Streams stream)                                                     
          │                       ▼ (copy of 84)                                                             
          │                       60┌──────────┐                                                             
          │                         │61|39|  | │               (copy of stream descriptor                    
          │                         └─┬────────┘                from 41, now modified in 61)                 
          │                           ├61┌──────────┐          ┌          ┐                                  
          │                           │  │[][][][]xx┼─────────►│ Size: 18 ├──────────┐                       
          │                           │  └──────────┘          │ Top:  77 │          │                       
          ▼  (old Streams stream)     │  (copy of 41)          └          ┘          │                       
          84┌──────────┐(-)           │                                              │      (data stream)    
            │41|39|  | │              │                             (data stream)    │      (copy of 140)    
            └─┬────────┘              │                       ┌─►140┌──────────┐(-)  └───►77┌──────────┐     
              ├41┌──────────┐(-)      │         ┌          ┐  │     │15|11|  | │            │41|11|  | │     
              │  │[][][][][]┼─────────┼────────►│ Size: 18 ├──┘     └─┬────────┘            └─┬────────┘     
              │  └──────────┘         │         │ Top: 140 │          ├15┌──────────┐(-)      ├41┌──────────┐
              └39┌──────────┐39───────┘         └          ┘          │  │..........│         │  │...xxxx...│
                 │[][]      │                                         │  └──────────┘         │  └──────────┘
                 └──────────┘                                         └11┌──────────┐11───────┘  (copy of 15)
                                                                         │........  │                        
                                                                         └──────────┘     

In Fig 1.b, we can see the results of changing some data inside of the data stream starting at block 140. This data is located at block 15 and when Yak writes the user data into this stream, it does not write these changes into block 15 - instead it allocates a new block (41), copies the content of block 15 into 41, then modifies the data in 41.

Because we now need to modify the redirector at 140 to point to new block 41, it would be tempting to modify 140 directly. But Yak never touches the data that already exists. Instead it copies the redirector at 140 into a newly allocated block (77), copies the content of 140 into 77, then writes the updated block index 41 over the previous index 15.

We now need to change the descriptor that points to this modified stream, because the top block, the redirector, is no longer 140, but 77. So we wish to modify the stream descriptor at 41, but once again, we never modify data in-place. Instead we allocate a new block, 61, copy 41 into 61 and write our updated stream descriptor there. Because we've now created a new top block for the Streams stream, we allocate a new top block (60), copy 84 into 60 and then write our modified redirector there.

Finally we update the header that now points to the Streams stream (the top block has moved from 84 to 60). Once again, we do not do this in the original header, but we instead write it into the previously used header (Header 2), where we update the version number to now be higher than the previous header (411 > 410). This marks Header 2 as the current header and the transaction has now committed.

#### File system consistency

File writes - to Yak files or indeed any other file found on a machine - are generally not guaranteed to be done in the order they were issued. In addition, file system and storage hardware caches can often mean the file system reports "changes written to disk" before they actually are. To guarantee that a new header update doesn't get written to disk before the changed blocks, which would cause inconsistency for a new reader that might read the new header and assume that the blocks had indeed been written to disk, Yak uses file system sync points to guarantee that a write has fully completed; first sync point is after the transaction blocks have been written, the second is after the new header has been written. This ensures things occur in order and that the header gets written as the very last thing.

### Reading a snapshot

While a transaction is occurring, many snapshots, across threads and processes on the local machine, can still read data from the Yak file, without being affected by any changes the transaction is making. Even after the transaction has committed, and actually written to the disk, the snapshot's view of the Yak file stays frozen from when the snapshot was originally begun. A snapshot does not keep itself up to date with the latest transaction - it can either be manually refreshed or closed and a new snapshot can be begun.

A snapshot begins by reading both header slots and choosing the header with the highest version, which it copies into memory. In this header copy there is the Streams stream descriptor, which enables the snapshot to locate all other streams. This is possible because any later transaction write its changes to new blocks, not the original blocks. So all the blocks of the Streams stream, and all the blocks that make up all the other streams for this version, are still in the file, unchanged from when the snapshot first copied the header. Changed blocks only occur from committing transactions and they are always written into new blocks.

It follows from this that a Yak file could continually grow; every time a transaction is committed, it writes changes to unused or brand new blocks. If we proceeded like this, a Yak file would grow indefinitely large as transactions are committed, because no blocks would ever be re-used. This is clearly not practical, so the lifetime of blocks have to be managed. Put another way, as long as a snapshot exists to read a block, the block needs to continue to stick around, in original form, at the original location.

### Managing block lifetime with transactions & snapshots

To manage block lifetime, transactions mark the blocks that have been superseded (because they have been copied and modified) as "pending free". From the perspective of the transaction, only the content in the new blocks (which are modified copies of the old blocks) and the existing, unmodified blocks matter. Yak does not actually free these superseded blocks yet, however, because a snapshot may still read from them. Each "pending free" block is marked with the version number that the transaction was started at.

When a later write transaction needs unused blocks (adding blocks to a stream, or needing new blocks because changes need to be written to existing blocks) Yak needs to either re-use blocks or add new blocks at the end of the Yak file. For the re-use case, which it checks first, it frees all "pending free" blocks that were marked "pending free" before the oldest snapshots or, if there are no snapshots, the last committed transaction. This is safe because no snapshot now exists to read from these blocks. 

### Synchronising access to a Yak file across processes

So far, the transaction & snapshot explanations above have assumed that transactions and snapshots occur within the same process, where threads can share memory to coordinate.

But when multiple processes wish to read from and write to the same Yak files, snapshots and transactions must coordinate in a different way. This is resolved by a side-car file, which each process maps into shared memory.

## Opening and creating a Yak file

An API is provided to create a new Yak file. This takes in a L3 type, a L2 type, a L1 type and a L0 type, which Yak uses to open the file and initialise the file for writing. A new file that is created is automatically opened for writing, with L1 taking the writer lock through L0. The new file is created with version 1, an empty root directory, and both header slots written so that the file is valid from the start.

An API is also provided to open an existing Yak file, also taking in L3, L2, L1 and L0 types. When opening a file, a caller must specify either for reading - in which case only snapshots can be begun - or for writing - in which case transactions can also be begun. Multiple processes can open a Yak file for writing and Yak will serialise changes to the file so that changes don't conflict. This means one set of changes may block until another set of changes complete. Processes only reading the file are not blocked by a writer.

## Yak headers
A Yak file is predominantly made of blocks; uniformly sized chunks of data from which streams are composed. Ahead of these blocks is a header, which enables each layer to store the necessary meta-information required for the layer to operate. When a Yak file is created, each layer passes the size that it needs for its own meta information down through the layers and receives back a "header slot". Layer 1, having now received the necessary information to create the header section creates the Yak file and begins by writing a header section that is sized to hold all the layers' headers.

When a layer needs to read its own meta-information, stored in the header, it asks its layer below to 

**** NEW DESIGN DOWN TO HERE ****

* **Finding a section.** A layer finds its section ID by asking the layer below for the first section above it. It answers the same question for the layer above by asking the layer below for the section after its own.
* **Reading.** L1 reads a header slot once and passes a copy of it up through the layers, and each layer reads its own section from that copy. All the sections a layer stack works from therefore come from the same slot and the same version, even if a commit happens in between. This is used when a Yak file is opened, and when a read transaction begins.
* **Writing.** A layer stages its section for the next commit by passing it down through the layers below to L1. Nothing is written to the file until L1 publishes the staged sections together in the next header slot (see Header slots in L1).

Passing the sections through each layer also lets a layer take them over. A layer that does not use the layer below for storage - such as an L3 that stores each stream as a real file (see Layer composition opportunities) - ends the chain and stores the sections of the layers above it itself.

What these sections contain is up to each layer; L1 transparently manages the two header slots, the sequence number and the checksum. L0 has no header section, since it knows nothing about the file's format. In a normal Yak file that uses the layers defined by the YakDefault type, the layout is as follows:

```
Magic: File magic header | header layout version | slot size | data offset
Slot (twice, A and B):
  Sequence: sequence number | slot length
  L1: length | L1 identifier | L1 version
  L2: length | L2 identifier | L2 version | block size shift | total blocks | free list head | pending list head | encryption flag | [additional encryption parameters]
  L3: length | L3 identifier | L3 version | Streams stream descriptor
  L4: length | L4 identifier | L4 version | root directory stream index
  Checksum: CRC32C of the slot
```

The magic section is written once, when the file is created, and never changes. The `slot size` is fixed at creation, and each slot starts at a file offset aligned to 4 KiB and is padded to a whole number of 4 KiB units, so that on common file systems the two slots never share a file system block (see Header slots in L1). The `data offset` records where L1's data region, and so block data, begins in the file (immediately after slot B).

When a Yak file is opened, L1 (as it opens the file) checks the file magic header and the header layout version to ensure it can read the headers. This ensures that:

* This is indeed a Yak file, and
* The layout of the headers follows a form that the code can read (in this version it's "two slots of a fixed size follow the magic section, and each layer's section within a slot is preceded by a length")

Assuming that it can, L1 reads both slots, discards any whose checksum fails, and selects the one with the highest sequence number. It then passes a copy of that slot up through the layers, and each layer reads its own section from it to initialise.

If all layers have found a header they are capable of handling, the file has successfully opened. If the file is opened for writing, L2 then shortens the file if it is longer than the committed version's `total blocks` (see L2), which removes anything a crash left behind.

Files created with header layout version 1 (before transaction support) have a single header that is updated in place. Such files can be read, and are converted to layout version 2 by `optimize()`, which already rewrites the whole file.

## L4: The public API

L4 deals with organising a filing system on top of L3. Where L3 deals with streams, identified by an index, L4 deals with "stream names" and "directory names".

L4's public API is similar to a file system API and pertains to:

* Creating, renaming and deleting a named stream,
* Creating, renaming and deleting a directory,
* Iterating over all streams inside of a Yak directory, including the root directory,
* Understanding the length of a Yak stream
* Reading from a stream, based on current head position,
* Positioning the read/write head inside of a stream,
* Writing to a stream, based on current head position (possibly enlarging the stream)
* Closing the Yak stream,
* Beginning, committing and rolling back write transactions,
* Beginning, refreshing and ending read transactions.

Every operation in this list is made through a transaction. Operations that change names - creating, renaming and deleting streams and directories - can only be made through a write transaction. They take effect in the transaction's working version immediately and become part of a version when the transaction commits. Name changes check for clashes against the working version, so a name created earlier in the transaction cannot be created again. A rename, which is a delete followed by an add, is therefore atomic on disk: after a crash the stream has either its old name or its new one.

Operations that read names - listing a directory, opening a stream, asking for a stream's length or version - read the version their transaction sees. Through a write transaction that is the working version: a stream created in the transaction is listed straight away and can be opened, and a deleted one is no longer listed. Through a read transaction it is the transaction's committed version, so listings and contents always come from the same version.

### Stream handles

A stream handle that's returned has no public information, but L4 locates the internal information it needs to handle streams. A stream handle in L4 internally manages a head position, which determines where reads and writes occur from, like reading and writing to a regular file.

A stream handle can either be opened for reading or writing, and belongs to the transaction it was opened in. When a new stream handle is created, it is opened for writing. Only a write transaction can open handles for writing.

A handle opened in a write transaction reads the working version, so it sees the transaction's own uncommitted changes; this is what lets a caller write part of a stream, seek back and read it, before committing. A handle opened in a read transaction reads that transaction's committed version and never sees uncommitted changes.

### A filesystem from streams

Two types of streams exist inside of a Yak file: Directory streams and streams.

Streams contain the data that a caller has written to the Yak file, with no padding, nothing added, nothing taken away.

Directory streams are used to provide an addressing system for the streams. It associates stream identifiers with a name, using a "stream entry" structure, e.g.

| Stream entry no. | Stream identifier | Name              |
| ---------------- | ----------------- | ----------------- |
| 0                | 43                | image.png         |
| 1                | 101               | another_image.png |
| 2                | 36                | textures/         |
| 3                | 930               | an empty folder/  |

When a stream entry ends with "/" the stream identifier points to another directory stream. If a stream entry does not end with a "/", the identifier points to a data stream.

When a new Yak file is created, it immediately creates a directory stream for the root directory and stores the index for this stream in the L4 header.

When an existing Yak file is opened, it reads the root directory stream index number from the header so it can locate the root directory listing.

Directory streams hold stream identifiers, never block indices. When a data stream's blocks move to new locations in a commit, its stream identifier stays the same, so no directory changes. Only creating, renaming and deleting names change a directory stream. This is what stops a change to a stream's contents from rippling up through the directory tree.

#### Stream entry management

Stream entries in the directory streams are serialized compactly. Since the length of their names vary, they are encoded as follows:

```ASCII
| stream identifier (4 bytes) | length (2 bytes) | name (1-65,522 bytes) ........ |
```

Following all the name/stream ID entries is a so called "Name Table". The purpose of this table is to facilitate fast O(log n) searching for a named stream. Of course, in a directory with 5 streams, this is rather overkill, but in a directory with 50,000 streams it drastically cuts down the time required to locate the right stream.

At the very, very end of the directory stream is a 4 byte value that indicates the offset of the Name Table. When Yak needs to locate a stream by name, it jumps to the end (less 4 bytes) of the directory stream, reads the offset of the Name Table, jumps to that offset and reads the name table itself.

The Name Table consists of pairs of string hashes and offsets and it is sorted by string hashes. With the hash of the name the caller is searching for, Yak does a binary search to find the one or more hashes that match the hash of the name the caller is attempting to open. If one or more identical hashes have been found in the Name Table, Yak jumps to these entries, compares the length of the string (for early rejection), then does a string compare for the caller requested name. If there's a match, L4 now has the Stream ID and can proceed to open this stream.

```ASCII
[Stream entries]* | [Name Table Entry]* | Offset of Name Table
```

When we insert a new Stream entry, we copy the Name Table, write the new Stream entry at the current end of the Stream entries (overwriting the existing Name Table), then insert the new Name table entry into the Name Table (keeping this table always sorted by hash) and then write the offset of the Name Table. This is a quick operation.

When we delete a Stream entry, we "pull upwards" the remainders of streams by copying chunk by chunk forwards in the stream, then remove the hash/offset pair from the Name Table, write out the refreshed name table and finish by writing the offset of the name table. This can be a slow operation.

If an entry is renamed, it is deleted and re-added.

With copy-on-write, these are ordinary writes to the directory stream and need no special handling to be crash safe. Their cost differs, though: an insert touches only the blocks at the end of the directory stream, while a delete rewrites every block after the deleted entry, all of which become dirty blocks in the write transaction. Within one transaction the cost is paid once, because blocks already copied are private and are written in place by later deletes. Applications that delete many names should do so within a single transaction.

### Detecting changes

Every stream and every directory has a *version*: the version of the Yak file in which it last changed. `stream_version(path)` and `dir_version(path)` return it, and `version()` on a transaction or snapshot returns the version that transaction or snapshot sees. All of them return the same `Version` type, an ordered number, so a caller can ask both whether something has changed (the versions differ) and whether it has changed since a given version (its version is greater).

A stream's version is stamped when the stream is created, when its contents change (written or shortened), and when it is renamed. Reserving space does not change it, since the contents stay the same. Stamping renames means that a different stream cannot arrive at a name without the version at that name moving forward: a stream created there has the version it was created in, and a stream renamed there has the version of the rename. A directory is a stream too, so its version is stamped in the same way: when it is created or renamed, and when entries in it are created, renamed or deleted. It reflects changes to its own entries only, not to the streams and directories below it.

Versions are read from the version the transaction or snapshot sees. In a snapshot, they come from the snapshot's committed version. In a transaction, a stream or directory that the transaction has changed reports the version the transaction will commit as (the committed version plus one), which is exactly what snapshots will see once it commits; after `commit_and_continue()`, later changes report the version after that. Two changes made in the same transaction share a version and cannot be told apart, which only threads sharing that transaction could observe.

Typical uses:

* A reader process holding a snapshot refreshes it and compares the versions of the streams it has loaded, reloading only those that changed.
* A caller reads its inputs in a snapshot, does slow work without holding a transaction, then begins a transaction and checks that the inputs' versions are unchanged before writing the result, retrying if they are not.
* An application caches data derived from a stream, such as decoded images or parsed configuration, keyed by the stream's version.

Reading a version reads one stream descriptor, which is far cheaper than reading the stream. Directory listings do not include versions, because that would read one descriptor per entry.

## L3 : Streams

L3 deals with linking blocks together from L2 and presenting them as identified streams to L4.

### L3 API

L3's API, which is used by L4, should solely concern itself with:

* Creating a new stream (returns a stream identifier).
* Verifying that a stream exists, by identifier.
* Opening an existing stream for writing, by identifier (returns a stream handle on the working version).
* Opening an existing stream for reading, by identifier and version (returns a stream handle on that version).
* Closing an existing stream, by handle.
* Deleting an existing stream by identifier. The stream must not be open for writing. Readers holding the stream open keep reading their version; its blocks are released when the deletion is committed, and reused once those readers are done.
* Reading from a stream, by handle.
* Writing to a stream, by handle and starting position, potentially enlarging the stream in the process.
* Shortening an existing stream, by handle.
* Committing the write transaction: placing every dirty block and staging the new Streams stream descriptor in L3's header section.
* Rolling back the write transaction.
* Opening a read-only view of a committed version, given a copy of that version's header slot (used for read transactions).
* Passing header sections through for L4: giving the section ID of the first section above L3, reading a section from a header slot copy, and staging a section for the next commit (see Opening and creating a Yak file).

### Stream handles

Unlike L4 stream handles, L3 stream handles do not contain a head position. It is the responsibility of L4 to read and write from/to a valid position.

### Streams from blocks

Blocks are linked together to form a data stream. L3 must separately keep track of the total length of the stream and the index of the first block (known as the "top block"), in a so called "Stream Descriptor".

* When the size of the stream is 0, the top block index is ignored, but must be 0 to avoid being 0xFFFFFFFF (this value indicates that the Stream Descriptor is available for reuse, see below). A stream with nothing written to it thus takes no blocks of storage (Fig 1)
* When the size of the stream is less than the length of one block, the "top block" contains the stream data (Fig 2)
* When the size of the stream is more than the length of one block, the "top block" now contains block indices and is called a redirector block (Fig 3). This top block can now hold block size / 4 indices (1,024 with 4 KB blocks). Each index points to another block index, which could potentially also be a redirector block (Fig 4).
* In this way, the blocks involved in a stream are either data blocks (at the lowest level of the block hierarchy) or redirector blocks (above the lowest level of data blocks), forming a "pyramid" data structure.

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

Accordingly, the lifetime of a new and growing stream is as follows:

1. The stream is created. At this point, no data or redirector blocks are in use. The size of the data stream, tracked separately, is 0 and the top block index is 0 (avoiding it being 0xFFFFFFFF which implies that the stream descriptor is unused) (Fig 1)
2. Data starts to be written onto the stream. As this happens, the size of the stream grows above 0 and it's necessary for L3 to allocate a new block to hold this data. The size gets updated in the stream descriptor and the data gets written into the first, and only, data block (Fig 2, block 4)
3. More data gets written onto the stream and the length of the stream exceeds what can be held in one block.
   1. L3 now allocates a redirector block using L2 and it changes the stream "top block" index to this new block (Fig 3, block 15).
   2. It writes the first index in the new redirector block to point to the original data block (Fig 3, inside block 15, observe index 4)
   3. It allocates another block (Fig 3, block 10), and writes the index of this block as the second index in the "top block" (Fig 3, inside block 15, observe index 10)
4. More data gets written onto the stream.
   1. Eventually the top block cannot hold any more indices and a new redirector block must be allocated, sitting alongside the original redirector block.
   2. To track these two redirector blocks, we need a new top block above the two redirector blocks (Fig 4, block 140)
   3. The first index in this new top block is, of course, the index of the old top block (Fig 4, block 140, observe index 15). The second index in the new top block is the new redirector block (Fig 4, block 140, observe index 11). In this way, we've increased the height of the data structure by one more.
5. As more and more stream data is written, the level of redirector blocks grow, from 0 (no redirectors), to 1 and beyond. The more data blocks we need to store, the "taller" the hierarchy of redirector blocks becomes.

The corollary case of a truncated stream is relatively simple; start at the new top (most often a redirector node), free everything not associated with the redirector hierarchy from this node downward. If, at the end, the top node is a redirector node with only one entry, also free this node. Finally, point the stream descriptor's "top block" to point to the top of the new node hierarchy.

The description above shows the pyramid as it is in a committed version. In the write transaction, the blocks shown are not modified in place; how the pyramid changes is described in Copy-on-write in L3 below.

#### Rationale

The "pyramid" block linking layout has a number of advantages:

* The number of redirection blocks are kept minimal and for very small streams even kept at 0.
* The lookup time for finding a random point in the stream is O(log n).
* The lengthening and shortening of streams doesn't need to move any blocks, only allocate and return blocks, as the number of redirector blocks grow and shrink above the data blocks.
* The depth of the redirector blocks can be calculated directly from the size of the stream. In addition, there's no overhead to mark a block out as a particular type; since we can reason about the depth, we know how far down the hierarchy the data blocks lie and everything before that is a redirector block.
* Even with relatively small block sizes a *large* stream can be tracked with low depth of tracking blocks. For example, with 4 KB blocks (and Yak's 4-byte block indices), a Yak stream can grow to 4+ GB before needing a third redirector level.
* The pyramid suits copy-on-write well. Changing a block requires a new copy of only the redirectors on the path from that block to the top, which is the depth of the pyramid: rarely more than two or three blocks. Every other block is shared, unchanged, between the old version and the new one.

#### Additional feature & additional complexity: Reserved length
For clarity of explanation, the pyramid data structure explanation above does not deal with reserved capacity of a stream. A stream can be extended into being longer than needed to store the data in the stream. So while the explanation above is correct, it leaves out the fact that a stream descriptor actually has a field for reserved size as well as size, and that the resulting pyramid can therefore be larger than it needs to be to strictly hold the user-written data.

L3 separates between size (how much data the caller has written to the stream) and reserved (how much data the stream has space for). Reserving space allocates the blocks for it straight away, as one contiguous run, and adds them to the stream's pyramid, building any redirectors needed above them. Nothing is written into the reserved blocks themselves.

With copy-on-write, it is worth being clear about what reserving does and does not add:

* **Contiguity within one transaction does not need it.** Everything a transaction writes to a stream is placed as one run at commit, however the writes arrived (see Placement at commit). A stream that is written in full and committed once gains almost nothing from being reserved first.
* **Contiguity across many transactions does.** A stream that grows a little in each of many commits - a log committed every few seconds, a recording, a capture - would otherwise get a separate run of blocks from each commit, scattered among the blocks of other streams and metadata committed at the same time. After a thousand commits, it is a thousand fragments. Reserved space is allocated once, so the stream stays in one run however many commits it takes to fill it.
* **Appends become almost free to commit.** Writing into reserved space beyond the committed size is done in place, and the redirectors above the reserved blocks already exist (see Private blocks and in-place writes). Committing an append therefore changes only the stream's size in its descriptor: no data blocks are allocated and no redirector path is copied.
* **Space is claimed up front.** Reserving grows the file once, so running out of disk space shows up when the space is reserved, not part way through writing the stream.

Reserving brings little for compressed streams. Their blocks are rewritten as a whole at commit, so they cannot be written in place, and they get no help with fragmentation across commits beyond what any stream gets.

Reserved space that is never written is wasted until the stream is truncated or the file is optimized. When a stream is truncated, all blocks - including those in any extended reserve - are returned as free blocks, in effect making the stream's size and reserved size very near to each other (reserved will include the unwritten space in the last data block, so tends to be ever so slightly larger).

A possible extension is automatic reservation, in the manner of XFS's speculative preallocation: when a stream is appended to in several consecutive commits, Yak grows its reservation geometrically (say 64 KB, then 128 KB, and so on, up to a cap), and trims any unused reservation when the stream is closed. Streams that grow like logs would then get the benefits above without callers needing to know reserve exists.

#### Additional feature & additional complexity: Compression
For additional clarity of explanation, the pyramid data structure above does not deal with compression. To achieve transparent compression/decompression of streams, an additional layer of indirection can be found at L3.

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
In a compressed stream however, "compressed blocks" are used instead of regular data blocks. In each compressed block is stored the compressed length of the data, followed by a set of block indices where this compressed data is stored.

Compressed blocks are, in effect, simply just another level of the pyramid data tree, treated slightly differently, but it is simpler to conceptually think of them as data blocks that can magically hold more data than could be stored in a physical block. And the way they achieve this magical feat is simply to compress the larger chunk of data they are magically defined to hold, reserve some more blocks to hold it, and then store the compressed length of the data they're holding and the block indices where that compressed data is held.

When a Yak file is created, both the size of the physical block and the size of the compressed block is defined. It follows that a compressed block must be significantly larger than a physical block, or no compression savings could be achieved. For example, if a physical block was 4 KB and a compressed block was also 4 KB, then we might be able to compress this data down to 2 KB, but it would still need to be stored in a 4 KB block *and* we would now need a compressed block to point to the compressed data. By default Yak defines a compressed block size as 8 times larger than a physical block (i.e. a 4 KB physical block and a 32 KB compressed block).

On the other hand, if compressed blocks are defined to hold 32 KB, their data might be compressible to, say, 10 KB (if we're lucky), which could be stored in three 4 KB blocks. Then we'd need a compressed block to point to this compressed data, in effect cutting our real block usage from 8 blocks (32/4) down to 4 blocks (3 blocks for the compressed data + 1 block to point to this compressed data).

This does, of course, introduce some slow downs in that every time a compressed block needs modification, we need to locate the compressed data, decompress the data, modify the data and then recompress the data. Yak uses LZ4 as the compression algorithm, broadly considered the fastest, "decent" compression algorithm. The gain, on the other hand, is that we often need less IO (because we need to write and read less from the disk due to the data being smaller). In some scenarios, using compression actually improves the read and writing speed of Yak, but broadly it is best to think that compression trades reading/writing time for storage space requirements. Callers must decide through testing whether compression is worthwhile for their scenario. It is given, but is nevertheless worth stating, that data that's already compressed before it is added to the Yak file will not compress further and will, in fact, end up larger if added to a Yak file as a compressed stream.

Copy-on-write fits compression naturally, because a compressed block is always rewritten as a whole anyway. In the write transaction, the unit of change for a compressed stream is one compressed block's worth of uncompressed data, held decompressed in the dirty map. At commit it is compressed once, written to newly allocated physical blocks, and a new compressed block is written pointing at them. The old compressed block and its old physical blocks become pending. However many times the data in a compressed block changes within a transaction, it is compressed only once, at commit.

### Tracking streams in L3

L3 has a list of Stream Descriptors for all streams that exist. This list itself is stored in a stream. To track the stream that stores Stream Descriptors, a single Stream Descriptor is held "out of band", in the L3 section of the header; let's call this descriptor "Streams" here for clarity of description.

When a new stream is created by a caller, we need to create a new Stream Descriptor. All Stream Descriptors are written into the Streams stream, which is expanded and contracted like every other stream. The Streams stream is of course initialised with 0 Stream Descriptors, because no other streams exist. As other streams are created, the Streams stream expands to hold these other Stream Descriptors; this expansion happens in L2 block-size chunks (eventually we will have to expand into new blocks when we add Stream Descriptors; we may as well do a full block and keep some in reserve to avoid having to continually expand the Streams stream every time a new stream is added), which leaves a buffer of available stream descriptors for the next streams.

Eventually some streams are deleted and the associated Stream Descriptor in Streams is marked free. At the start of the Streams stream is written the 4 byte stream identifier of the first free Stream Descriptor. If there are no free Stream Descriptors, the sentinel 0xFFFFFFFF is used to imply no free Stream Descriptors exist. In the Stream Descriptor identified at the front of the Streams stream is written the identifier of the next free Stream Descriptor and so on, ensuring that all free Stream Descriptors are linked together as a chain.

When a new Stream Descriptor is required and the first free identifier (the first 4 bytes of the Streams stream) points to something valid, the Stream Descriptors are plucked from the front of this chain of free Stream Descriptors and reused.

The Streams stream plays the same role as an inode table or inode map in a file system. Every stream's descriptor lives in it, so every committed change to any stream changes the Streams stream, and the Streams stream's own descriptor in the header is the root of the whole version. Reading the header slot gives a reader the Streams descriptor; from there it can find every stream of that version.

A Stream Descriptor's layout is as follows:

```ASCII
size (8 bytes) | top block (4 bytes) | reserved size (8 bytes) | flags (1 byte) | version (8 bytes)
```

A Stream Descriptor is 29 bytes. Its stream identifier is not stored in it: a stream's identifier is the position of its descriptor in the Streams stream.

The flags byte holds:

| Bit | Flag       | Meaning                   |
| --- | ---------- | ------------------------- |
| 0   | Compressed | The stream is compressed. |
| 1-7 |            | Reserved for future use.  |

The version is the version of the Yak file in which the stream last changed: it is set when the stream is created, when its contents change and when it is renamed (see Detecting changes in L4). A reused Stream Descriptor belongs to a stream created in a later version, so its version is always newer than that of the stream that used the descriptor before. L3 stamps the version of creations and content changes itself; L4 asks L3 to stamp it when it renames a stream, since L3 knows nothing of names.

### Copy-on-write in L3

L3 is where most of the transaction work happens, because L3 is the only layer that knows how blocks are linked together. L2 below it does not know which blocks are data and which are redirectors, so L3 keeps the changes of the write transaction itself and decides where each changed block will live.

#### Path copying

Fig 5 shows what changing one byte in a large stream means for the pyramid. The byte lies in data block 1416. The committed data block cannot be changed, so its new contents go to a new block (2048). The redirector that pointed at block 1416 must now point at block 2048 instead, so it too is copied, and so is the top block above it. Everything else stays where it is and is shared by both versions.

```ASCII
Fig 5. Changing one byte in data block 1416 of stream 7

 Version 41 (committed)                    Version 42 (after commit)

 ┌───────────┐                             ┌───────────┐
 │ Size: 10M │                             │ Size: 10M │
 │ Top: 300  │                             │ Top: 2050 │
 └────┬──────┘                             └────┬──────┘
      ▼                                         ▼
   300┌───────────┐                         2050┌────────────┐
      │301|640|980│                             │301|2049|980│     301, 980: shared
      └─────┬─────┘                             └──────┬─────┘
            ▼                                          ▼
         640┌──────────────┐                       2049┌──────────────┐
            │1415|1416|1417│                           │1415|2048|1417│   1415, 1417: shared
            └───────┬──────┘                           └───────┬──────┘
                    ▼                                          ▼
                1416┌──────────┐                           2048┌──────────┐
                    │....x.....│                               │....y.....│
                    └──────────┘                               └──────────┘
```

The new top block must be recorded in stream 7's Stream Descriptor, which lives in the Streams stream, so the block of the Streams stream holding that descriptor is copied in the same way, along with the path above it to the Streams stream's top block. The new Streams descriptor goes into the header. Because directory streams hold stream identifiers rather than block indices, no directory changes. In total, a one-byte change committed on its own writes one data block, the redirectors above it, one block of descriptors and the redirectors above that, and one header slot: typically five or six writes.

Most of that cost is shared when several changes are committed together. A block is copied at most once per transaction: once copied, it is private, and later changes to it in the same transaction change the private copy. A thousand changes to the same block in one transaction cost the same as one. A hundred changes spread over a hundred data blocks under one redirector cost a hundred data blocks, but the redirector, the top block, the descriptor block and the Streams stream path above it are each copied only once.

#### The dirty map

Changed blocks are held in memory in the *dirty map* until commit. Entries in the dirty map are keyed by the block's logical position in its stream, not by a block index:

```ASCII
(stream identifier, level, slot)
```

* **Stream identifier** - the stream the block belongs to (including the Streams stream and directory streams).
* **Level** - how high in the pyramid the block sits. Level 0 is data (or, in a compressed stream, compressed blocks); level 1 is the redirectors pointing at data; level 2 the redirectors above those, and so on.
* **Slot** - which block it is at that level, counting from the start of the stream.

For example, with 4 KB blocks (1,024 four-byte indices per redirector), byte 5,000,000 of stream 7 lies in data block (7, 0, 1220), at byte 2,880 of that block. Its redirector is (7, 1, 1), where it is entry 196 (1220 mod 1024), and the top block is (7, 2, 0).

A key says what a block is, never where it will be stored. This is what allows L3 to wait until commit to choose block indices, to place children before their parents, and to lay out the new blocks of a stream as one contiguous run.

The pyramid makes these keys cheap to work out. The depth of a stream, and therefore the level of every block in it, follows from the stream's size (see Rationale), and the slot of a parent is the slot of its child divided by the number of indices per redirector.

Each dirty map entry holds one of:

* **Contents** - the full contents of the changed block, waiting to be placed.
* **Placed** - the index of a private block that already holds the block's contents (after spilling, see below).

Redirector entries record only which of their indices have changed; their other indices are taken from the committed block when the redirector is placed. A dirty data block that only partly changed is filled in from its committed version when it first becomes dirty, so the dirty map always holds whole blocks.

#### Reading through the dirty map

Reads made in a write transaction - through any of its handles, and by L4 when it lists or changes names - look in the dirty map first and fall back to the committed version, so the transaction sees its own changes. Reads made in a read transaction never look at the dirty map; they read only blocks of their own committed version.

#### Placement at commit

A redirector's contents are the indices of its children, so a redirector cannot be written until all of its changed children have their indices. Placement therefore works bottom-up:

1. **Data.** For each stream with dirty data, the dirty level-0 slots are sorted, and L3 asks L2 for a run of blocks large enough to hold them (see Allocating blocks in L2). Consecutive slots are given consecutive blocks, so changed data is laid out contiguously however its writes arrived. The blocks are written.
2. **Redirectors.** Each dirty redirector is built from its committed version with its changed indices patched in, given a block and written, level by level, up to the stream's top block.
3. **Descriptors.** Each changed stream's new top block, size and version are written into its Stream Descriptor, which dirties blocks of the Streams stream.
4. **The Streams stream.** The Streams stream is placed in the same way, last, because every other stream's placement changes it. Its new descriptor is staged in L3's header section, through L2.

Every block replaced in this process - the old data blocks, old redirectors and old blocks of the Streams stream - is handed to L2 as released by this commit (see Pending blocks in L2).

#### Private blocks and in-place writes

L3 remembers which blocks were allocated by the write transaction. These private blocks cannot be reached from any committed version, so when a private block changes again it is written in place, without copying. This covers blocks placed by spilling, and the blocks of streams created in this transaction.

There is one more case where a block can be written in place: a block in the reserved part of an uncompressed stream, wholly beyond the stream's committed size. Readers never read beyond the size recorded in their version's descriptor, so such a block has never been visible to any reader. (A stream that shrinks has the blocks beyond its new size removed from its pyramid, so a block beyond the committed size cannot have been within the size of an older version.) Writing into reserved space does not change any redirector either, because the reserved blocks are already in the pyramid. Appending into reserved space therefore changes only the descriptor's size, which makes it the cheapest way to grow a stream in a transaction (see Reserved length). It is also crash safe: a crash part way through such a write leaves unexpected bytes only beyond the committed size, where no reader looks. The partly filled last block before the committed size is not reserved space; it holds committed data and is copied as usual.

#### Spilling large transactions

The dirty map has a memory budget, set when the Yak file is created or opened. When the dirty map exceeds it, L3 spills: it places the longest runs of dirty level-0 blocks early, exactly as it would at commit, and replaces their entries with Placed entries. Redirectors and descriptors stay in the dirty map, which is small even for very large transactions (a 1 GB change with 4 KB blocks needs about 256 redirector entries). Spilled blocks are private, so later changes to them are made in place.

Spilling trades some layout quality for memory. Blocks placed early are contiguous within each spill, but not necessarily with blocks placed later.

#### Rollback

Rolling back discards the dirty map and tells L2 to return every block allocated by the transaction to free space. Since all such blocks were free in the committed version, nothing on disk needs to change.

#### Reading committed versions

A read-only view of a version needs only that version's Streams descriptor. From it, L3 can find any stream's descriptor and from there any block. Because the version's blocks never change while the version can be read, a read-only view needs no locks and no coordination with the writer.

### Thread safety in L3

Streams must be opened and closed like regular files before they can be written to and read from, including the Streams stream. Within a write transaction, this is to track that there is at most one active writer (with no readers) or many active readers (with no writer) per stream, since all of them work on the transaction's uncommitted changes. The list of active readers and writers per stream, which is not exposed directly to L4, but kept internally and found from the stream handle that L3 exposes to L4, must be guarded with a mutex to avoid the multi-threaded creation of simultaneous readers and writer.

Read transactions are not tracked per stream. A read transaction only reads blocks of a committed version, which no write transaction changes, so it needs no lock against the writer. What it does need is for its version's blocks not to be reused while it reads; that is handled by pinning versions (see Pending blocks in L2 and Reader registry in L1).

The Streams stream needs modification every time a stream is created or deleted. Because many threads can do that at the same time, it is guarded to prevent multiple threads attempting to modify this stream at the same time. If one thread is modifying the Streams stream, and another thread needs to do the same at the same time, the second thread is put to sleep until the first thread is done with the modification. A separate, non-public L3 API enables L4 to read and write in a blocking fashion to ensure multiple threads do modifications to the Streams stream in an orderly fashion.

The dirty map is divided by stream. Since a stream has at most one writer, only one thread changes a stream's part of the dirty map at a time, and no extra locking is needed for it. The parts belonging to the Streams stream and to directory streams are guarded by the blocking opens described above.

Only one write transaction is open at a time; `begin_transaction()` waits on a mutex held for the life of the write transaction. Within the transaction, commit is guarded by a commit lock, a reader-writer lock. Every call that changes the working version holds the shared side of the lock for the duration of the call. Commit, `commit_and_continue()` and rollback take the exclusive side, so they wait for calls in progress in other threads to finish, and calls that arrive during a commit wait for it to complete. Read transactions read committed versions only, so they do not take the commit lock; a read transaction that begins during a commit simply gets the version that was live before it.

## L2: Blocks

L2 deals with presenting a file in a series of blocks. It uses L1 to make changes to a file and it presents only the opportunity to allocate and free blocks to L3.

### L2 API

L2's API, which is used by L3, concerns itself with creating, managing and returning blocks. It should be restricted to:

* Allocate a new block, or a run of blocks, which is returned to L3 by index
* Writing a buffer to a block, or a run of contiguous blocks, given by block index
* Reading a block, or a run of contiguous blocks, to a buffer, given by block index
* Deallocating a block that the write transaction allocated (it was never committed, so it is free again immediately)
* Releasing a block that the new version no longer uses (it may be reused once no version that uses it can be read)
* Committing its own state, and rolling it back
* Passing header sections through for L3 and the layers above: giving the section ID of the first section above L2, reading a section from a header slot copy, and staging a section for the next commit (see Opening and creating a Yak file)

### Managing blocks

The management of blocks in L2 assumes nothing about what blocks contain or which stream they belong to. It is assumed that L3, where streams are managed, will appropriately handle access to a stream by multiple threads and that block writes are dispatched by L3 in the right order. Only a single writer can exist per stream at a time (as per Yak's thread/process safety constraints) so any writes to blocks in a stream will be guarded by L3.

Which block is allocated to which stream is untracked in L2. It is assumed that L3 remembers what blocks it has been given and what blocks it is handing back. L2 doesn't perform any checking of whether it *should* be writing a block, or returning a block to the free list. In other words, L3 is responsible for the integrity of blocks only being assigned to one stream, or available.

L2 does keep track of one thing about blocks: which of the four states each block it hands out or takes back is in, as seen from the write transaction:

| State   | Meaning                                                               | Becomes                                                       |
| ------- | --------------------------------------------------------------------- | ------------------------------------------------------------- |
| Free    | No version uses it.                                                   | Private, when allocated.                                      |
| Private | Allocated by the write transaction.                                   | Used, when the transaction commits; Free, when it rolls back. |
| Used    | Part of the committed version.                                        | Pending, when a commit releases it.                           |
| Pending | Released by a commit, but a version that might still be read uses it. | Free, once no such version remains.                           |

Only the Free and Pending states need recording on disk; Used blocks are recorded by being reachable from the header, and Private blocks exist only until commit.

### Allocating blocks

New blocks are always taken from space the committed version considers free: the free list of the committed version, or new blocks beyond the end of the file. They are never taken from Pending blocks still in use by a readable version.

L3 asks for blocks in runs, with an optional *goal* (a block index that the run should ideally follow, usually the last block of the same stream). L2 tries to satisfy the request with one contiguous extent starting at the goal, then with one contiguous extent anywhere, and only then with several extents. If the free list cannot satisfy the request, the file is grown, once, by the number of blocks still needed.

When the file is grown, the new length is recorded in memory only. The `total blocks` field in L2's header section is updated as part of the commit. If the process crashes before the commit, the file may be longer than the committed version's `total blocks`; when the file is next opened for writing, L2 shortens it to that length, which discards anything the crashed transaction wrote there.

Blocks taken from the free list are not zeroed when allocated. Every block placed by L3 is written in full, and reserved space beyond a stream's size is never read, so zeroing would only add writes. Blocks gained by growing the file are already zero.

### Reusing blocks

When a block is released, it will often be in the middle of the underlying file that L1 is managing. For efficiency, blocks are reused by way of keeping a free list of blocks.

The free list is stored in the free blocks themselves, as a chain of *trunk* blocks. Each trunk block holds the index of the next trunk (4 bytes) followed by as many free extents (start index and length, 4 bytes each) as fit in a block. L2 has a "free list head" block index in its header section, which points to the first trunk (using 0xFFFFFFFF when there is no free block available).

```ASCII
L2 header                trunk 811                       trunk 95
┌──────────────┐        ┌──────────────────────────┐    ┌──────────────────────────┐
│ Free: 811    │──────► │ next: 95                 │──► │ next: 0xFFFFFFFF         │
└──────────────┘        │ 812+4 | 1200+38 | 77+1   │    │ 96+12 | 3001+250 | ...   │
                        └──────────────────────────┘    └──────────────────────────┘
```

When blocks are freed, L2 sorts them and coalesces them into extents before adding them to the head trunk, starting a new trunk (taken from the freed blocks themselves) when the head trunk is full. This promotes them being later re-allocated in run-order, enabling multi-block read and write operations (i.e. IO operations that span across as many blocks as possible, thereby lowering the number of IO calls).

#### The free list is copy-on-write too

The free list is part of a version, just like the streams are: a version's free list must stay intact for as long as that version is the one a crash would fall back to, because after such a crash, the next transaction allocates from it.

During a transaction, L2 holds the part of the free list it is using in memory: it takes extents from the head trunks as blocks are allocated, and adds released blocks that have become free. Trunks reachable from the live header are never modified in place. At commit, L2 writes the head trunks it changed as new trunk blocks (taken from the free extents themselves) and records the new free list head in its header section. Because the chain is consumed and extended at its head, a commit typically rewrites one or two trunk blocks. Trunks further down the chain are shared with the previous version, unchanged.

Trunk blocks that the new version no longer uses are only needed by the previous version's free list, which only the writer ever reads. They are released in the same way as stream blocks, except that no reader can be holding them, so they become free as soon as the next transaction begins.

### Pending blocks

A block released by a commit cannot go straight back on the free list. Readers holding an older version may still be reading it, and until the commit is durable, a crash would fall back to the previous version, which uses it.

L2 therefore keeps a *pending list*: a chain of blocks in the same trunk format as the free list, where each trunk is additionally tagged with the sequence number of the commit that released its blocks. The pending list head is in L2's header section. Each commit adds a trunk (or a few) recording what it released.

A block released by the commit that created version *v* may be reused once both of these are true:

* Version *v* is durable (its commit has completed), so a crash can no longer fall back to an earlier version.
* No reader holds a version older than *v*.

L2 asks L1 for the oldest version held by any read transaction (see Reader registry in L1) at the start of each write transaction, and again at each `commit_and_continue()`. Every pending trunk tagged with a version at or below that is eligible, and its extents are moved into the free list in memory. The move is recorded on disk by the next commit, which writes the new free list head and pending list head together. If the process crashes before then, the blocks are still pending when the file reopens and are released again, so a crash can delay reuse but never lose a block.

A read transaction that is held open for a long time prevents every block released since its version from being reused, so the file grows. This is the cost of readers that never block. `verify()` reports the oldest version held by a read transaction, which helps find ones that are held for too long.

### Block caching

L2 will often be requested to read the same block over and over again. This is most often the case when data is being written to or read from a larger stream (i.e. a stream that must use redirector blocks) and the redirector blocks need constant access to locate the data in the stream. L2 therefore manages a cache of blocks and it provides to L3 a way to use this cache for some blocks, while not for others. L3 uses the cache for redirector blocks. The actual data written/read to a stream circumvents the cache, under the assumption that callers know what data they've placed in the Yak file (and therefore won't be asking the Yak file to read that data again) and, when reading, receive a copy of the stream into their memory buffer and therefore won't ask to read the same data again.

L2 block caching inside the process is relatively simple. Once committed, a block stays the same until it is marked "pending free" (by a transaction) and then finally freed for reuse (when a transaction detects that no more snapshots could possibly read from that block). When a block is reused, L2 evicts it from the cache so no readers accidentally read stale block content. During a transaction any modified block exists in transaction memory and these blocks are only written to the actual file when the transaction commits. During this write, the new content of the block is placed back into the cache so that later reads can read the new content.

Block caching across processes is less simple: As part of a transaction commit in another process, blocks can be reused for placement and nothing notifies this process' cache that one or more blocks in the cache have gone stale. This is solved by looking out for commits from other processes and clearing caches when they have been detected. Practically it happens when a transaction or snapshot begins, or a snapshot is refreshed: L1 compares the header version it finds in the file with the last version this process committed itself. If they are different Yak knows that another process has committed and all caches (the L2 block cache, the L4 directory cache) are cleared before proceeding. This requires new reading from the file, warming up the caches again.

### Additional feature & additional complexity: Block encryption
Occasionally, a caller may desire data stored in a Yak file to be encrypted. The default L2 implementation in Yak offers optional whole-file encryption for all streams in a Yak file.

Yak intentionally takes a standard, industry-accepted approach towards encryption, but no one should trust their secrets to Yak's implementation or hope that there are no vulnerabilities; undoubtedly there are many.

When a block is requested by L3, L2 loads the block, decrypts it and returns it. When L2 is requested to store a block, L2 encrypts it and stores it back to disk.

L2 uses a block cache to return often used blocks quickly, which is typically used to store hot redirector paths. Blocks in this cache are stored unencrypted in memory; only when they are written to the Yak file are they encrypted. The same applies to the dirty map in L3.

Blocks are encrypted with a key that is then encrypted/decrypted with the caller's key (AKA "wrapping") and then stored in the L2 header section. This allows a password change in the Yak file without having to re-encrypt all blocks. A password change is a commit like any other: the newly wrapped key is written to the next header slot.

The caller key is derived from the user's password using Argon2id. The parameters for this derivation are also stored in the L2 header section. Yak uses AES-XTS to encrypt each block, using the block number as the "tweak". Copy-on-write does not change this: a block's contents are encrypted with the number of the block they are written to, and since changes are always written to new blocks, the same contents are encrypted under different tweaks in different versions.

To increase the computational cost for an attacker, all encryption is salted with a 16 byte salt, which is also stored in the header.

When encryption is enabled in a Yak file, L2's header section is extended to store all these parameters so that the full L2 header section looks like this:

| Field             | Size (bytes) | Notes                                                                                                           |
| ----------------- | ------------ | --------------------------------------------------------------------------------------------------------------- |
| Identifier        | 6            | ASCII "blocks" - identifies this as an L2 header                                                                |
| Version           | 1            | Header layout version (2 for copy-on-write files)                                                               |
| Block size shift  | 1            | Block size = 2^shift bytes (e.g. 12 = 4 KiB)                                                                    |
| Total blocks      | 4            | Little-endian u32; number of blocks in this version. On opening for writing, the file is shortened to this.     |
| Free list head    | 4            | Little-endian u32; index of first free list trunk (sentinel = no free blocks)                                   |
| Pending list head | 4            | Little-endian u32; index of first pending list trunk (sentinel = no pending blocks)                             |
| Encryption flag   | 1            | 1 = encrypted, therefore L2's header is extended with fields below.                                             |
| Salt              | 16           | Random salt for Argon2id key derivation                                                                         |
| m_cost            | 4            | Argon2id memory cost parameter (little-endian u32)                                                              |
| t_cost            | 4            | Argon2id time/iteration cost parameter (little-endian u32)                                                      |
| p_cost            | 1            | Argon2id parallelism parameter (single byte; values above 255 are impractical)                                  |
| Verification hash | 32           | SHA-256 hash of extra Argon2id-derived bytes; used for fast password verification without attempting key unwrap |
| Wrapped key       | 72           | 64-byte AES-256-XTS master key wrapped with AES-256-KW (adds 8-byte integrity overhead)                         |
| **Total**         | **21 / 150** | 21 bytes without encryption, 150 bytes with encryption                                                          |

It is important to know that the entire header of a Yak file is not encrypted. The header checksum detects torn writes; it does not protect against deliberate tampering.

Encryption and decryption is sped up using hardware instructions, where applicable. This often achieves a 20x speed up, although even with that, encrypted Yak files suffer a performance penalty (unencrypted Yak files tend to be around 3x faster than encrypted ones).

If encryption is enabled together with compression the runtime penalty is less than the penalty for encryption and the penalty for compression added together. This is because compression ensures there is less data to encrypt/decrypt.

### Thread safety in L2

Since multiple threads can attempt to allocate or deallocate blocks at the same time (because different streams from the same Yak file can be opened for writing at the same time), changes to L2's in-memory free list and its record of private and released blocks are protected by a mutex. Because the free list on disk is only written at commit, allocation no longer needs to write anything to the file while holding the mutex.

L2's commit runs while L3 holds the exclusive side of the commit lock, so no allocation can happen during it.

## L1: Commits

L1 turns the plain storage provided by L0 into a Yak file. It writes, reads and verifies the Yak file "magic header", to check that a valid Yak file has been opened, and it carries each layer's header section, so that the layers above can store their management data outside the regular block format. With transactions, L1 makes commits atomic and durable, and keeps track of which versions readers are holding.

L1 has no direct access to a file system; everything it stores, it stores through L0.

### L1 API

L1's API, which is used by L2, should expose:

* Creating a Yak file, as the writer
* Opening a Yak file, either as a reader or as the writer
* Reading from and writing to the data region, at an offset from its start
* Growing and shortening the data region
* Laying out the header sections at creation, from the section sizes passed down by the layers above
* Giving the section ID of the first section above L1
* Reading the live header slot, as a copy from which each layer reads its section
* Staging a section for the next commit
* Publishing the staged header (the commit point), with the syncs the durability calls for
* Registering and releasing a reader's hold on a version, and reporting the oldest version held

L2 addresses the data region from 0, so block *n* is at offset *n* × block size, and L2 never needs to know where the header ends.

### Commit ordering

Copy-on-write is only crash safe if the header never reaches storage before the blocks it points at. Operating systems and storage devices may reorder writes freely, so L1 enforces the order with L0's sync (see Syncing in L0). A commit with `Durability::Full` syncs twice:

1. After every block of the commit has been written, and before the header slot is written. This guarantees that the header can only ever point at blocks that are on storage.
2. After the header slot has been written. This guarantees that the commit has happened when `commit()` returns.

With `Durability::None`, L1 does not call sync at all.

### Header slots

L1 owns the two header slots and the magic section before them (see Opening and creating a Yak file). Each layer stages its header section for the next commit, through the layers below it (see Opening and creating a Yak file); L1 assembles the slot from the staged sections, the next sequence number and a CRC32C checksum over the whole slot, and writes it to the slot that is not live.

L1 does not rely on the storage writing a slot atomically: a slot that was only partly written fails its checksum and is ignored, and the other slot is used. That is what makes a torn header write safe.

There is one failure the checksum cannot recover from: a torn write to one slot that also damages the other, which would leave no valid slot and a file that cannot be opened. This could happen if both slots shared a physical sector, or a file system block that the file system rewrites as a whole. To avoid it, each slot starts at a file offset that is a multiple of 4 KiB (or of the storage's block size, if L0 reports that it is larger) and is padded to a whole number of those units.

Note that alignment within the file is not alignment on the storage device. The file system decides where a file's bytes are stored, and nothing guarantees that a 4 KiB-aligned file offset lands on a sector boundary. On common local file systems (ext4, XFS, NTFS, APFS) files are stored in whole blocks starting at a block boundary, so aligned file offsets do map to whole blocks and sectors; LMDB and SQLite rely on the same property. On file systems that pack small files inline, compress or encrypt transparently, or are accessed over a network, it may not hold. Alignment therefore reduces the chance of losing both slots on common file systems rather than ruling it out on all of them. A Yak file nested in a stream of another Yak file gets its protection from the outer file's commits instead (see Layer composition opportunities).

The live slot alternates between A and B with each commit. A commit never writes the live slot, so the previous version is always available in the other slot until the next commit has completed.

### Reader registry

L2 must know the oldest version that any reader holds, in this process or any other, before it reuses pending blocks. L1 keeps that record.

Within a process, L1 keeps a count of open read transactions per version, in memory.

For other processes, L1 uses L0's lock storage (see Lock storage in L0), divided into fixed-size reader slots. Each slot holds the oldest version that any read transaction in that process holds. Raising the value when an old read transaction ends is always safe; when a read transaction begins on a newer version, the process's oldest version already protects it, because blocks are only reused once no reader holds a version older than the commit that released them. A reading process claims a free slot by taking an exclusive lock on it, and keeps the lock for as long as it reads. L0 releases the lock if the reader dies, so a crashed reader can never hold back reuse forever: when the writer finds a slot it can lock, the slot is free, whatever it contains.

To begin its first read transaction, a reading process:

1. Claims a slot.
2. Reads the header and writes the live sequence number into its slot.
3. Reads the header again. If a newer commit has completed in the meantime, it writes the newer sequence number and repeats this step.

The writer only reads the registry after it has published a new header. A reader therefore either registered its version before the writer looked, so the writer respects it, or it sees the writer's newer header in step 3 and moves to it. Either way, no block the reader can see is reused.

Each later read transaction, and each `refresh()`, repeats steps 2 and 3 to find the latest version, without lowering the value in the slot. Closing the Yak file releases the slot.

### Thread & process safety in L1

L1 ensures that only one process opens the Yak file for writing at a time. The writer's lock is an exclusive lock on a dedicated range of L0's lock storage, rather than on the Yak file itself, so readers can open the Yak file while a writer has it open. Opening for writing waits for, or fails on, another writer's lock, according to the open options.

Readers do not need a lock to read the Yak file itself. They only read blocks that belong to the version they hold, which the writer guarantees not to change, and they find that version through a header slot whose checksum protects against reading it while it is being written.

## L0: Storage

L0 deals with accessing an underlying file system. Most commonly this would be a real file system, but adapters could be written for other situations, like keeping a Yak file in memory, injecting faults for crash testing, or storing a Yak file in a stream of another Yak file (see Layer composition opportunities).

L0 knows nothing about Yak: not the header, not blocks, not versions. Everything it is given, it stores as bytes at the offset it is told.

### L0 API

L0's API, which is used by L1, should expose the functions necessary to wrap actual file system calls for:

* Creating storage, and opening existing storage
* Reading from and writing to the storage, at any offset
* Getting the length of the storage, growing it and shortening it
* Syncing the storage (all completed writes reach storage)
* Reporting the storage's block size, if it is known
* Opening the lock storage, and reading, writing and locking ranges of it
* Replacing one storage with another atomically (used by `optimize()`)

### Syncing

L0's sync must not return until every write completed before it has reached storage. L1 relies on this to order commits (see Commit ordering in L1). On Linux this is `fdatasync`, on Windows `FlushFileBuffers`, and on macOS `fcntl(F_FULLFSYNC)` (plain `fsync` on macOS does not flush the drive's own cache).

An L0 that is not backed by durable storage, such as one that keeps the file in memory, may implement sync as nothing.

### Lock storage

L1 needs to coordinate the writer and readers across processes without locking the Yak file itself. L0 therefore provides a small *lock storage* alongside the main storage, which L1 can read and write like the main storage and in which it can lock byte ranges. Locks can be exclusive, and taken either waiting or failing at once. A lock is released when its holder releases it, closes the storage, or dies.

For a file on disk, the lock storage is a small file next to the Yak file (`example.yak-lock` for `example.yak`), and locks are OS byte-range locks: `fcntl` open file description locks on Linux, `fcntl` record locks elsewhere on POSIX, and `LockFileEx` on Windows. The operating system releases them if the process dies. An L0 that keeps the file in memory can keep the lock storage and its locks in memory, which works within one process only.

L0 does not know what the lock storage holds or what its ranges mean; the writer lock and the reader slots are L1's (see Reader registry and Thread & process safety in L1).

## Verifying a Yak file

`verify()` is implemented as a chain across the four upper layers (L4 -> L3 -> L2 -> L1; L0 has no invariants of its own to check), each layer checking its own invariants and passing claimed resource lists down. With transactions, verification checks a committed version, and additionally checks:

* That both header slots can be read, and reports which is live and the sequence number of each.
* That no block is both reachable from the committed version and on the free list.
* That no block appears on both the free list and the pending list.
* That every block of the file below `total blocks` is reachable from the committed version, on the free list, or on the pending list. A block that is none of these has leaked; leaked blocks are reported, and reclaimed by `optimize()`.
* The oldest version held by any reader.

Verification of a version can run alongside the writer, because the version it checks does not change.

## Optimizing a Yak file

`optimize()` rewrites a Yak file into a new file, copying only the committed version's directories and streams, stripping excess reserved capacity, and writing each stream's data contiguously. The new file then atomically replaces the original. Pending blocks, leaked blocks and the history of older versions are not copied, so optimizing also resets the file to a single version. Because optimizing replaces the file, it requires that no other process has the file open, either as a reader or as the writer.

```ASCII
                            Header 1          Header 2                                                                               
                           ┌──────────────┐  ┌──────────────┐                                                                        
                           │              │  │              │                                                                        
                           │              │  │              │                                                                        
                           │              │  │              │                                                                        
                           │ ┌          ┐ │  │ ┌          ┐ │                                                                        
                           │ │ Size: 12 │ │  │ │ Size:    │ │                                                                        
                           │ │ Top: 84  │ │  │ │ Top:     │ │                                                                        
                           │ └       │  ┘ │  │ └          ┘ │                                                                        
                           └─────────┼────┘  └──────────────┘                                                                        
                                     │                                                                                               
                                     │                                                                                               
                                     │                                                                                               
                                     │                    ┌          ┐                                                               
                                     │                ┌──►│ Size: 58 ├──────┐                                                        
                                     ▼                │   │ Top: 140 │      ▼                                                        
                                     84┌──────────┐   │   └          ┘      140┌──────────┐                                          
                                       │41|39|  | │   │                        │15|11|  | │                                          
                                       └─┬────────┘   │                        └─┬────────┘                                          
                                         ├41┌─────────┼┐                         ├15┌──────────┐                                     
                                         │  │[][][][][]│                         │  │4 |10|98|6│                                     
                                         │  └──────────┘                         │  └─┬────────┘                                     
                                         └39┌──────────┐                         │    ├ 4┌──────────┐                                
                                            │[]        │                         │    │  │..........│                                
                                            └──────────┘                         │    │  └──────────┘                                
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
                                                                                                                                     
                                                                                                                                     
                                                                                                                                     
 Header 1          Header 2                                                                                                          
┌──────────────┐  ┌──────────────┐                                                                                                   
│              │  │              │                                                                                                   
│              │  │              │                                                                                                   
│              │  │              │                                                                                                   
│ ┌          ┐ │  │ ┌          ┐ │                                                                                                   
│ │ Size: 12 │ │  │ │ Size:    │ │                                                                                                   
│ │ Top: 84  │ │  │ │ Top:     │ │                                                                                                   
│ └       │  ┘ │  │ └          ┘ │                                                                                                   
└─────────┼────┘  └──────────────┘                                                                                                   
          │                                                                                                                          
          │                                                                                                                          
          │                                                                                                                          
          │                                               ┌          ┐                                                               
          │                                               │ Size: 58 │ ───────────────────────────────────────────────┐              
          ▼                                               │ Top: 101 │                                                ▼              
          84┌──────────┐                                  └          ┘     140┌──────────┐                            101┌──────────┐
            │41|39|  | │                                                   ▲  │15|11|  | │                               │80|11|  | │
            └─┬────────┘                                                   │  └─┬────────┘                               └─┬────────┘
              ├41┌──────────┐                             ┌          ┐     │    ├15┌──────────┐       ┌──────────┐80───────┤         
              │  │[][][][][]├────────────────────────────►│ Size: 58 │ ────┘    │  │4 |10|98|6│       │4 |22|98|6│         │         
              │  └──────────┘                             │ Top: 140 │          │  └─┬────────┘       └─┬────────┘         │         
              └39┌──────────┐                             └          ┘          │    ├ 4┌──────────┐    │                  │         
                 │[]        │                                                   │    │  │..........├────┤                  │         
                 └──────────┘                                                   │    │  └──────────┘    │                  │         
                                                                                │    ├10┌──────────┐    ├22┌──────────┐    │         
                                                                                │    │  │..........│    │  │..xxxx....│    │         
                                                                                │    │  └──────────┘    │  └──────────┘    │         
                                                                                │    ├98┌──────────┐    │                  │         
                                                                                │    │  │..........├────┤                  │         
                                                                                │    │  └──────────┘    │                  │         
                                                                                │    └ 6┌──────────┐    │                  │         
                                                                                │       │..........├────┘                  │         
                                                                                │       └──────────┘                       │         
                                                                                └11┌──────────┐ ───────────────────────────┘         
                                                                                   │55|19|  | │                                      
                                                                                   └─┬────────┘                                      
                                                                                     ├55┌──────────┐                                 
                                                                                     │  │..........│                                 
                                                                                     │  └──────────┘                                 
                                                                                     └19┌──────────┐                                 
                                                                                        │........  │                                 
                                                                                        └──────────┘                                 
```