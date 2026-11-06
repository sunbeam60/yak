<!-- cspell:locale en-GB -->

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
  | `commit`, `commit_and_continue`, `rollback`            | ✓                   |                 |
  | `refresh`, `close`                                     |                     | ✓               |
  

- **An open stream can be renamed.** Readers and Writers hold a stream's identifier, not its name, and a rename only changes directories, so renaming a stream that is open is allowed. Deleting a stream is still refused while a Reader or Writer in the same transaction has it open; Readers in snapshots are unaffected and keep reading their version.
- **Block indices and stream identifiers are u32 for 1.0, through named types.** The code uses `BlockIndex` and `StreamId` (each with its sentinel constant), defined in one place, never a bare `u32`. u32 is enough for 1.0: block indices cap a file at 2^32 - 1 blocks (about 16 TiB with 4 KiB blocks, more with larger blocks), and every stream that holds data needs at least one block, so only empty streams could ever exceed 2^32 identifiers - and other limits, such as the 4 GiB maximum size of a directory stream, are reached long before. Stream identifiers of deleted streams are reused, so the limit is on streams existing at the same time, not streams ever created.
  - Because the width is defined in one place, Yak can be built with larger (or smaller) types by changing those definitions. A file built with one width cannot be read with another, so the width must be recorded through the layer identifiers or versions in the header sections, and opening a file with the wrong width fails cleanly.
  - If more than one width is ever needed in the same build, the planned route is associated types on the layer that owns each concept (`BlockLayer::BlockIndex`, `StreamLayer::StreamId`), so each layer still only knows the one below it. This is not done for 1.0: it makes every on-disk structure generic, the C and Python bindings would need a concrete type per width, and the test suite would need to run per width.
- **Every process is alike: any open can write, one transaction at a time.** Yak is a flexible, durable file format, not a database, but once each transaction and snapshot keeps its own state, letting processes take turns to write costs only a few rules, and no process needs special handling as "the writer". The writer lock is held by a transaction, from `begin_transaction()` to `commit()` or `rollback()`, not by an open. Snapshots in every process never block, and are never blocked by, the transaction. Holding the writer lock for a whole open was considered first, and dropped because it made the writing process a special case. Processes that need to exchange data, rather than share a file, should still use a protocol designed for that.
  - **Threads are treated like processes.** A transaction or a snapshot belongs to the thread that began it, and can neither be shared with nor moved to another thread (in Rust it is neither `Send` nor `Sync`). Threads take turns to write exactly as processes do. Letting several threads write into one transaction was considered and dropped: parallel writers inside one commit need a commit lock, guarded access to the Streams stream and directory streams, and a locked allocator, all to synchronise threads within a process. Without them, L3 and L2 need no locking at all inside a transaction or snapshot.
  - Waiting writers are not served in order: neither OFD locks nor `LockFileEx` promise it, so a process that commits and immediately begins another transaction, in a tight loop, can starve writers in other processes. This is documented rather than solved for now; a gate lock, as CompoundFs uses, would solve it.
- **No side-car file; locks are taken on bytes far beyond the end of the Yak file.** The locks themselves carry the information: a writer byte and one byte per version for readers in other processes (see Locks in L1). A side-car file was considered and rejected: it is found by path, so two paths to the same file (a symbolic link, a hard link, a different mount) would coordinate through different side-cars and corrupt the file silently; it needs write access to the directory; it can never be safely deleted; and its permissions are separate from the Yak file's. A shared-memory side-car, as LMDB and SQLite's WAL mode use, would also have to detect crashed processes itself, where byte-range locks are released by the operating system.
  - On every supported platform, locks belong to the open file, not to the process: `LockFileEx` on Windows, and open file description (OFD) locks on Linux and macOS (10.11 and later). Other code in the process that opens and closes the same file therefore cannot disturb Yak's locks. The BSDs only have process-wide record locks, which are released when the process closes *any* descriptor for the file, so the BSDs are not supported.
- **Block reuse is held back by the oldest snapshot**, as in LMDB: blocks released after the oldest version that any snapshot holds are not reused until that snapshot ends, even if no snapshot needs them. Tracking exactly which versions need which blocks would need the version each block was created in, which is not worth it for snapshots that are expected to be short-lived.
- **Each snapshot and each transaction has its own cache.** A snapshot's version never changes and its blocks are never reused while it is open, so its cache can never go stale and needs no coordination with the writer or with other processes. Snapshots therefore share nothing, at the cost of each one warming its own cache. A cache shared by all snapshots of the same version can be added later if many short-lived snapshots turn out to suffer.
- **`refresh()` and `commit_and_continue()` are kept.** `refresh()` is exactly equivalent to closing a snapshot and beginning a new one, kept for convenience. `commit_and_continue()` keeps the transaction, its open Writers and its warm cache across a commit, which suits a long-running writer such as one appending to a log.
- **Three durability levels: `Full`, `Ordered` and `None`.** A commit's first sync point only needs ordering, so it uses a barrier. `Full` then syncs; `Ordered` uses a second barrier, keeping the file valid after a power loss but possibly losing the latest commits; `None` does neither. Barriers only differ from syncs on macOS (`F_BARRIERFSYNC` against `F_FULLFSYNC`), where they roughly halve the cost of a `Full` commit. The levels are adopted now and benchmarked later.
- **`verify()` checks structure only.** With atomic commits, `verify()` is no longer needed after a crash. It remains for commits made without durability, for finding bugs in Yak (it is run by the test suite), for detecting structural damage to the file, and for reporting leaked blocks. Which versions readers hold is runtime information rather than integrity, so it is not part of `verify()`; a diagnostic for it can be added later.

## Yak purpose

Yak is a Rust library that enables users to create, manage and use Yak storage files. A Yak storage file is a "file system in a file", wherein there are 0..n streams, addressable by a string name. Each stream can be read and written similar to how a real file can be read and written, meaning that after a stream has been opened, it returns a handle from which the user can obtain the length, a position (where reads and writes happen) and the contents of the stream itself. After a stream has been opened, there are functions to read and write data from and to the streams. All changes to Yak files occur within a transaction that is committed or rolled-back as one, which protects a Yak file from becoming inconsistent in the event of a crash or power-loss, and protect simultaineous readers from reading changes in progress. 

## Streams, directories and transactions

Streams appear to the user as a contiguous byte stream, much like a file. While they cannot be addressed like an array, the library hides the underlying storage location, much like a virtual memory manager hides the memory pages of an operating system. Seeking a new position on the stream is an O(log n) operation.

Streams are addressable by "directory" (recognised by ending with a forward-slash "/"). A stream name must not contain forward-slash, and it cannot have the same name as a stream in the same directory, but it can otherwise be named as the user sees fit (including null terminators). In this way, the user can store a stream hierarchy that could look as follows:

* image.png
* another_image.png
* textures/skin.png
* textures/another_image.png
* an empty folder/

It is not possible to "open" a directory, i.e. in the above example, the user cannot open "an empty folder/". The user can only open streams for reading and writing - the "directories" function the same way as a real file system directory functions, in that they are used to contain things, namely other directories and streams.

To make changes to streams and directories, a transaction must be used. Once a transaction has begun, the necessary changes can be made and then committed, enabling changes to occur in the file as one, atomic change. A transaction enables both reading and writing from streams and any changes made in the transaction are immediately visibly to other readers opened by the same transaction. Functions are provided for deleting both streams and directories and for shrinking the length of an existing stream. If a stream write exceeds the current length of the stream, the stream is extended automatically, akin to writing a regular file. If a stream "position" is set beyond the length of the stream, an error occurs. Any changes made only become durable, and readable by later or refreshed snapshots, once committed.   

Alternatively, a snapshot can be created, which only enables the reading of contents of directories and streams. This snapshot is frozen in time from the last last successful transaction commit and it is isolated from changes that occur in simultainous or later transactions. In this way, a Yak file cannot be read in an inconsistent state, provided that the user data is consistent on transaction commit.

## Use

A Yak file is useful as a generic storage container. Given that it supports extending and shortening streams inside of a single file, it is particularly useful as a way to provide storage for other libraries that continually write data to a file system and indeed for any writing scenarios where the expected length is not known when the stream write begins, or changes over the lifetime of the file.

That said, even when the final data length is well known, Yak can be useful as a rapid, relatively well optimized way of writing binary data without having to worry about the particulars of how these are laid out in the file.

Transactions make Yak suitable for data that must survive a crash without degradation, such as an application's document format, where a save must either complete or leave the previous save untouched.

Yak's intended use is effectively as a binary "storage format" for other applications. 

Multiple processes can read and write to the same Yak file - the Yak library coordinates their access to the file so that multiple transactions are serialised and multiple snapshots are isolated from each other and from any transactions in progress or completed since the snapshot was opened. This enables writer/reader scenarios where one or more processes produce data that one or more processes reads, commonly found in multimedia applications and similar. 

Some imagined scenarios:

* Application file formats with crash-safe saves
* Game save files
* Backup storage
* Log entry storage for multiple processes
* Receiving storage for streaming data
* A container for scientific multi-dimensional array data sets
* A composit multimedia data container
* Undo stack storage

Naturally, to be usable as a storage container for other libraries, which would normally write their data to a regular file system, the using library has to support abstractions on top of the file system so that a write to a file goes instead into a Yak data stream.

Yak is not too dissimilar to other "database in a file" formats, such as SQLite, LMDB, CFB and CompoundFS. The key difference is that Yak "thinks in binary" and aims for stream contiguity, which provides quick reads and quick writes of large binary data sets, and organises its data like a file system, instead of structured, queryable data.

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

Layer 4 provides to the caller a data structure that wraps a stream, which the caller must use to write data, read data from, change the position, and shorten the length of the data stream. Additionally there are utility functions to "reserve" space for the stream growing, which keeps a stream that grows across many commits - a log, say - in a few contiguous runs.

This layer also provides utility functions to iterate the named contents of the Yak file, equivalent to typing "ls" on a regular file system, to discover the hierarchical content of the Yak file.

Internally, this layer translates between a "filing name" (e.g. "folder1/folder2/image.png") and a stream number-based identifier. The stream number-based identifier is never revealed to the caller.

In short, Layer 4 answers the question: Can you build a filing system out of numbered streams?

### Layer 3: Stream abstraction

At this layer, streams can be created, lengthened, shortened, deleted, written to and read from. This layer responds to requests from Layer 4 to create a new stream and it returns a number-based identifier to Layer 4. This layer doesn't know anything about the filing name (that is the concern of Layer 4) or the content of the streams.

Internally, this layer requests new blocks from Layer 2. It links these blocks together to create streams. When the streams are created and lengthened, it requests new blocks from Layer 2 to achieve this. When the streams are deleted or shortened it returns blocks no longer needed to Layer 2.

Layer 3 concerns itself only with streams. It provides functions for Layer 4 to discover how many streams exist and to iterate over each stream identifier. It also provides to Layer 4 the ability to lengthen, shorten, create and delete streams. Layer 3 never reveals to Layer 4 how streams are composed of blocks that are linked together.

Layer 3 is also responsible for optimising the layout of blocks used in streams to optimise towards quick writes and reads (one long read/write instead of lots of little reads/writes) and it provides a capability to amass changes in memory (provided the changes stay within a defined limit, otherwise they write to the file early) and commit them to the file as one transaction.

In short, Layer 3 answers the question: Can you build numbered streams out of numbered blocks?

### Layer 2: Block abstraction

Layer 2 manages blocks but does not know what role each block takes. It provides to Layer 3 new, unused blocks when requested and it keeps track of unused blocks that it has created or received back from Layer 3. Layer 2 uses Layer 1 to create new blocks, which it does by lengthening the the data region, doing whatever is required to initialise free blocks and then adds them to an internal list of unused blocks. Because snapshots are frozen in time from when they were created, Layer 2 holds back blocks that have been released by Layer 3 but may still be read by live snapshots. When a snapshot is closed, Layer 2 handles returning "frozen" blocks as free blocks, ready for re-use.

In short, Layer 2 answers the question: Can you build numbered blocks out of a storage abstraction that behaves like a file?

### Layer 1: Storage abstraction

Layer 1 provides the fundamental storage abstractions that enables Yak to work. It creates and manages the header that stores information from each layer above. It provides access to the data region that will hold blocks and it provides the transaction primitives with which L2 and the layers above change the content atomically. Finally it maintains a system to coordinate between multiple processes reading from and writing to the same Yak file. 

In short, Layer 1 answers the question: Can you build a file that changes atomically, and that several processes & threads can share safely, out of the calls a system provides?

### Layer 0: System abstraction

Layer 0 provides system-specific implementations of file, lock and sync primitives. It has no knowledge of Yak at all and keeps no Yak state of its own. It simply provides a simplified and unified interface for Layer 1 to do its work.

In short, Layer 0 answers the question: Can the underlying system provide operating system primitives that can be opened by name, read from, written to, synced and locked?

# Yak Architecture & Implementation

This section provides a deep dive into how Yak operates at each layer.

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
* The command line Yak tool is not meant as a day to day production tool, but it provides a set of functions to manipulate Yak files during development, including compacting them (see Compacting a Yak file), and, crucially, it provides a set of benchmarks that are useful for profiling and performance optimization.

## Testing

The library is tested using Python/pytest. This allows rapid test development and testing of the Python Yak wrapper.

Additional Rust tests exist in the library itself, to test functionality that can't adequately be tested by pytest.

## Yak file thread, process, machine & crash safety

A Yak file must handle being accessed by multiple threads within the same process and multiple processes on the local machine, with the following considerations:

- It is generally not safe to open a Yak file over a network (SMB, NFS etc.) or through a virtual machine, unless the caller can guarantee singular access. Yak cannot adequately coordinate processes outside of the current machine.
- Any number of processes can have the same Yak file open, for reading or for writing, at the same time. A forked child process must not use the Yak files its parent opened; it must open them itself. 
- If a process has a Yak file opened for reading, it can begin multiple snapshots in any thread.
- A snapshot can open multiple Readers per stream.
- If a process has a Yak file opened for writing, it can begin multiple snapshots in any thread and one transaction per thread. Multiple transactions serialise amongst the threads (if happy to wait, otherwise they fail) across processes, though order is not guaranteed. Opening a second transaction on the same thread fails.
- A transaction can open multiple Readers per stream OR one Writer per stream. 
- An open Yak file can be shared between threads.
- Once opened, a transaction must only be used on the thread it was opened on.
- A snapshot can technically be moved to another thread, but only one thread can use it at a time (which the snapshot does not coordinate). It's often a lot simpler just to open another snapshot instead of sharing.
- Snapshots never block, neither each other nor a transaction, and never prevent a transaction from committing, whichever thread or process they are in.

If a process crash or a system power loss occurs, a Yak file opens to look exactly like it did at the last successful transaction, with no recovery work required by the caller. If a transaction was in progress during the crash it is as if it never happened. In other words, changes to Yak files are ACI(D):
- **A**tomic - they either happened fully or they didn't happen at all.
- **C**onsistent - a Yak file is always "valid" - although since Yak doesn't know what a caller writes to the file, consistency of user data cannot be guaranteed.
- **I**solated - a change is invisible until it happens and every snapshot sees only a committed version.
- **(D)**urable - depending on the options chosen when the caller commits a transaction, Yak can be fully durable.

## Yak file sizing types
By default, Yak uses 32-bit unsigned integers to address its blocks; the value 0xFFFFFFFF is reserved as a sentinel meaning "no block", so a Yak file holds at most 2^32 - 1 blocks. The block size therefore sets the largest possible file (e.g. ~16 TiB with 4 KiB blocks and ~256 TiB with 64 KiB blocks). Files that need to contain larger streams use larger blocks. The minimum (and default) block size is 4 KiB - any smaller and crash safety cannot be guaranteed.

Yak can optionally be built using smaller (u8, U16) or larger (u64) block index type but Yak files created with these settings must be opened by a custom Yak library that supports this type. Block index width (1, 2, 4 or 8 bytes) is stored in a Yak file and an error occurs if a caller attempts to load a Yak file created using a different block index width.

It follows that stream identifiers are the same datatype as block indices, since a stream typically are at least 1 block and there cannot be more streams than there are blocks possible (technically, a stream can take no storage other than the stream descriptor in the Streams stream, but it's hard to imagine a Yak file with more than 4 billion completely empty streams). For stream identifiers, 0xFFFFFFFF is reserved as a sentinel value as well.

Byte counts, such as a stream's size and reserved size, are u64, because a single stream can be larger than 4 GiB.

## Endianness

All multi-byte management data in a Yak file (block indices, stream lengths, header fields, directory entry lengths, etc.) is stored in little-endian byte order. This applies to all platforms -- on big-endian systems, the library performs the necessary byte-swapping transparently.

Naturally Yak cannot make any guarantees about the endianness of user-written stream data, since it cannot know what is written. Callers are responsible for their own data encoding, including that of encoding the byte order should they need to swap Yak files between opposing byte-order systems.

## Transactions & snapshots

All modifications made in a Yak ultimately end up as modifications to the bytes held in the stream blocks and to the header of the Yak file. In a storage library without transaction support, changes would be written directly to the file, but in Yak (like in other embedded storage libraries with transaction support), changes made by a transaction are kept separate from the original file; instead of performing those changes on the original blocks and the original header, all changes occur in *new* blocks and a *new* header that is separate from the original blocks and header. Only when the transaction is committed are the changes written to the Yak file.

### Committing a transaction

When a user needs to change the value of 10 bytes in a stream, say, Yak first copies the original block where those 10 bytes are held into memory and then performs the change on this in-memory block, not the original, physical block on disk. The transaction keeps hold of this in-memory block, and any other blocks that are modified during the transaction, and whenever further changes, or further reads, happen in those modified blocks, the transaction uses the in-memory, stand-in blocks rather than the blocks on disks. In effect, a transaction pretends that the changes have occurred in the original file, but they have actually only occurred in the stand-in blocks.

When a transaction is committed all the stand-in blocks are written to the Yak file - not on top of the existing blocks, but as new blocks. These blocks are connected together by redirectors, from bottom up, as they are written into the file. An additional upside of knowing which blocks have changed, and which streams they belong to, is that the blocks can be written out as contiguously as possible, promoting optimised read and write speeds on the actual file.

Once the blocks are known to have actually been written to disk, the header is updated with a new version and also written to disk. To avoid the new header overwriting the old header that may be in the process of being read by another thread or process, the header is written to a second header slot and a higher version is written as the very last thing in the write. This makes the second slot the one with the highest version, which causes new readers to recognise the new header as the current header. In this way, writers alternate between two header slots, using the version to signal which one is the latest one.

As a practical example, observe Fig 1.a. Here the current header is version 410 and its out-of-band stream descriptor (which points to the Streams stream) indicates that block 84 is the top block in the Streams stream. Inside block 41 there are 5 stream descriptors, with the last one pointing to a data stream at block 140.

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

We now need to change the descriptor that points to this modified stream, because the top block, which is a redirector, is no longer 140, but 77. So we wish to modify the stream descriptor at 41, but once again, we never modify data in-place. Instead we allocate a new block, 61, copy 41 into 61 and write our updated stream descriptor there. Because we've now created a new top block for the Streams stream, we allocate a new top block (60), copy 84 into 60 and then write our modified redirector there.

Finally we update the header that now points to the Streams stream (the top block has moved from 84 to 60). Once again, we do not do this in the original header, but instead write it into the previously used header (Header 2), where we update the version to now be higher than the previous header (411 > 410). This marks Header 2 as the current header and the transaction has now committed. From now on, any new snapshot that begins finds the highest header, now version 411 and reads the data from here. By starting with this new header, the Streams stream is located in block 60 and the fifth stream is located not in 140, but in block 77.

It follows from this that a Yak file could continually grow; every time a transaction is committed, it writes changes to unused or brand new blocks. If we proceeded like this, a Yak file would grow indefinitely large as transactions are committed, because no blocks would ever be re-used. This is clearly not practical, so the lifetime of blocks have to be managed. A simplistic solution would be mark blocks free once the transaction has committed, but this would prevent other readers from continuing to read once a transaction had been committed. We must, in other words, hold on to blocks that could still be read and only free them once we know for certain that no other snapshot could need to read from the blocks.

#### File system consistency

File writes - to Yak files or indeed any other file found on a machine - are generally not guaranteed to be done in the order they were issued. In addition, file system and storage hardware caches can often mean the file system reports "changes written to disk" before they actually are. To guarantee that a new header update doesn't get written to disk before the changed blocks, which would cause inconsistency for a new reader that might read the new header and assume that the blocks had indeed been written to disk, Yak uses file system sync points to guarantee that a write has fully completed; first sync point is after the transaction blocks have been written, the second is after the new header has been written. This ensures things occur in order and that the header gets written as the very last thing. Callers can choose between "Full" durability, which guarantees that bytes are written to disk before the commit call returns, "Ordered" durability, which ensures things are written in order (ensuring Yak file integrity and process/thread coordination) or durability "None", which is fast but should only be used for singular writers that can tolerate an inconsistent file in case of a crash.

### Reading a snapshot

A snapshot is a "frozen" view into a Yak file, as it was when the snapshot was begun. This means a snapshot is isolated from any changes in an ongoing transaction and it is isolated from transactions that were committed since the snapshot was begun. Snapshots are used to read from streams in a Yak file without being affected by what other threads or processes are doing to the Yak file.

To achieve this, snapshots must freeze their view of Yak file. They achieve this by copying the current header, from which all other streams can be found, and preventing any block that is used in that view of the Yak file from being reused.

A snapshot begins by reading both header slots and choosing the header with the highest version, which it copies into memory. In this header copy is the Streams stream descriptor, which enables the snapshot to locate all other streams in the Yak file. Because transactions write to new blocks, not the existing blocks, as long as the blocks that were in use when the header copy was taken are still there, the snapshot can continue to read the blocks. Changed blocks only occur from committing transactions and they are always written into new blocks.

Because nothing a snapshot can read ever changes while it is open, each snapshot keeps its own cache of the blocks it has read (see Block caching in L2). That cache never needs to be cleared or checked, whatever other snapshots, the transaction or other processes do.

Put another way, as long as a snapshot exists to read a block, the block needs to continue to stick around, in original form, at the original location.

### Managing block lifetime with transactions & snapshots

When a block is freed in L3, because a stream has been deleted, truncated or changes have been written to the stream (which are written to new blocks, causing the blocks with the original content to no longer be required), the block is returned to L2 as "pending free". From the perspective of a transaction the block is free, but snapshots may still exist that need to read from this block and so it cannot be freed for re-use. Instead it is tagged with the version created by the commit that released it, and kept by L2 until it is known that no snapshot holds a version older than that tag, since only those snapshots could still read from the block.

When a later write transaction needs unused blocks (adding blocks to a stream, or needing new blocks because changes need to be written to existing blocks) Yak needs to either re-use blocks or add new blocks at the end of the Yak file.

Yak first tries to re-use blocks. For each "pending free" block in L2 it checks if any snapshots could conceivably still need to read from the block. If every snapshot holds a version at or after the block's tag, the block cannot possibly be read by any of those snapshots and it is therefore eligible to actually be reused.

For example, looking at Fig 1.b, blocks 84, 41, 140, 15 have been marked as "pending free" by L3 in the commit that created version 411, so they are tagged 411. Because the blocks could still be read by snapshots begun at version 410, the blocks cannot be actually freed yet. But when all snapshots are version 411 or later, those four blocks can now actually be freed and reused.

If L2 cannot find any blocks eligble for reuse, it extends the file, thereby creating new blocks, and it adds those blocks to the free list in the current header, thereby creating new blocks. These blocks are now freed to be used and as they are, they come off the free list. 

If, at this very point, the Yak process should crash, the transaction header is not written to the file and while the file is actually longer than it should be, those newly created blocks were never created officially (in that they weren't added to the free list that was written into the file). When the next transaction starts, the transaction checks the current header and if the file is longer than it should be by the current header, the Yak file is truncated, effectually cleaning up the blocks that the previous, crashing transaction had created.

### Synchronising access to a Yak file across processes

Throughout this section, the word "thread" is used consistently. In this discussion it doesn't matter whether the tread belongs to the current process, or to another process that shares access to the Yak file.

So far, the transaction & snapshot explanations above have assumed that transactions and snapshots occur within the same thread. Yak supports multiple other threads reading from and making changes to the Yak file

To facilitate this, Yak needs to coordinate two elements:
1) No more than one transaction may be ongoing at the same time.
2) No block may be re-used if a snapshot exists that can still read from in, no matter which thread that could occur in.

Yak achieves this coordination using a portable method of locking byte-ranges on the Yak file, far, far beyond the end of the file. This is why Yak requires a 64 bit file system; it reserves the byte ranges from 2^62 --> (2^63-1) for thread coordination. The range stops at 2^63-1 because that is the largest offset POSIX can lock. On Linux, MacOS and Windows byte ranges beyond the actual end of a file can still be locked, which Yak uses as signalling method between threads. This creates some obvious advantages and disadvantages:

Advantages:
- Yak files do not need a side-car file-based coordination with write access. As such, Yak files can be opened in directories where the caller has no write access.
- Yak files are singular and users are not confused by the presence of a side-car file when copying Yak files around.
- If a thread crashes, the operating system frees any byte range locks it has so that no crashing thread can hang on a transaction, nor prevent blocks from being freed because a snapshot (which has now crashed) ostensibly use it.

Disadvantages:
- Yak files can only be 2^62-1 bytes (4 EiB) large. This is considered an acceptable maximum size for a single file.
- Yak files can theoretically run out of transactions; if Yak ran one million transactions a second, a Yak file would run out of transactions after ~148,000 years. This is considered an acceptable transaction load and lifetime for a storage format such as Yak.
- Yak files rely on operating system support for byte range locks that fulfil Yak's criteria. Since this is the case for Linux, MacOS (and its device-based cousins) and Windows, with the only known exception being BSD-based operating systems, Yak considers this tradeoff acceptable.

In practice, there are two byte-ranges that can be locked:
- A write lock byte, positioned at byte 2^62, used to indicate that a thread is writing to the Yak file.
- A range of snapsnot bytes, positioned between 2^62+1 --> 2^63-1, used to indicate the lowest (oldest) snapshot held by each thread operating on the Yak file.

As implied before, it is important to understand that a Yak file can be 100 kb large and still have bytes locked above 2^62. The size of the Yak file does not limit which bytes can be locked. On the other hand, the range of bytes that are required for thread coordination does limit how large a Yak file can grow (size limit 2^62 - 1), because if it grew into the byte ranges that were locked, the locking would prevent Yak from functioning in those byte ranges.

#### Locking the write byte
When a transaction wishes to commit, which causes writes to the actual Yak file, it attempts to exclusively lock the write lock byte, indicating whether it's willing to wait (i.e. be blocked) or fail early (i.e. be told "you can't lock this right now"). If it's willing to wait, the operating system holds the thread until the other thread(s) have completed their write and unlock the write lock byte. The order in which the operating system grants another thread the exclusive write lock is not guaranteed to be in order that it was requested.

Eventually, the write lock byte is exclusively locked. This means the thread now knows that it is the only thread writing to the Yak file. It performs the necessary writes and unlocks the write lock byte again, potentially allowing another thread write access to the file.

If a thread crashes during the commit, i.e. during the time it holds the write lock byte exclusively, two things occur automatically:
- The operating system releases any locks held by the thread, including the exclusive write lock byte.
- The commit never completes, in that the new header doesn't get written to the Yak file.

#### Locking reader bytes
For a transaction to determine which blocks it can reuse, i.e. which blocks it can move from "pending free" to actually free, it needs to know the oldest snapshots currently live across all threads. Any blocks that were kept alive for snapshots that no longer exist can be moved from "pending free" to free. Conversely, any blocks that are kept alive for snapshots that are still in progress need to still be readable.

Yak uses byte range locks to coordinate which snapshots are the oldest across threads. For example, if there are four snapshots currently in progress, one which was begun at version 41, one begun at version 45, one begun at 46 and one begun at 50, the oldest snapshot is version 41. As a result, Yak share-locks byte 2^62+1+41 (2^62 is the base lock offset, the +1 is the first byte which is used to indicate writing lock and +41 is for the byte that indicates that version 41 is the oldest snapshot). A share lock is used because other threads may also need to indicate that they've got a snapshot of 41.

When snapshot 41 is complete, Yak checks for the next-oldest snapshot, which is 45. It locks byte 2^61+1+45 and then unlocks byte 2^61+2+41 and it thereby signals that all blocks between version 41 and version 44 can be actually freed.

## Opening and creating a Yak file

An API is provided to create a new Yak file. This takes in a L3 type, a L2 type, a L1 type and a L0 type, which Yak uses to open the file and initialise the file for writing. A new file that is created is automatically opened for writing, with L1 holding the writer lock through L0 while it initialises the file. The new file is created with version 1, an empty root directory, and both header slots written so that the file is valid from the start.

An API is also provided to open an existing Yak file, also taking in L3, L2, L1 and L0 types. When opening a file, a caller must specify either for reading - in which case only snapshots can be begun - or for writing - in which case transactions can also be begun. 

## Yak headers
A Yak file is predominantly made of blocks; uniformly sized chunks of data from which streams are composed. Ahead of these blocks is a header, which enables each layer to store the necessary meta-information required for the layer to operate. When a Yak file is created, each layer passes the size that it needs for its own meta information down through the layers and receives back a "header slot" in return. Layer 1, at bottom of this chain and having now received the necessary information to create the header section, creates the Yak file and begins by writing space for two headers section that is sized to hold all the layers' headers. Two identical headers are written, one after another and aligned to 4 KiB, to support transactions.

What these sections contain is up to each layer; L1 transparently manages the two header slots, the version and the checksum. In a normal Yak file that uses the layers defined by the YakDefault type, the layout is as follows:

| Offset | Size (bytes) | Field                 | Contents                                                                                                                                                              |
| ------ | ------------ | --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0      | 7            | Magic                 | `89 59 41 4B 0D 0A 1A` (`\x89YAK\r\n\x1A`), copying PNG file header tricks to catch common transfer errors                                                            |
| 7      | 1            | Header layout version | Always `1` for now - reserved for future format changes                                                                                                               |
| 8      | 8            | Version               | Unsigned 8 byte integer. The valid slot with the highest version is live                                                                                              |
| 16     | ...          | L1 section            | length \| L1 identifier \| L1 format version                                                                                                                          |
| ...    | ...          | L2 section            | length \| L2 identifier \| L2 format version \| block size shift \| total blocks \| free list head \| pending list head \| encryption flag \| [encryption parameters] |
| ...    | ...          | L3 section            | length \| L3 identifier \| L3 format version \| Streams stream descriptor                                                                                             |
| ...    | ...          | L4 section            | length \| L4 identifier \| L4 format version \| root directory stream index                                                                                           |
| ...    | ...          | (padding)             | Zeros, up to offset 4092                                                                                                                                              |
| 4092   | 4            | Checksum              | CRC32C over bytes 0-4091                                                                                                                                              |

When a Yak file is opened, L1 (as it opens the file) reads the first 8 KiB of the file and checks that at least one header shows the magic bytes (a crashed commit may have screwed up one of the headers) and chooses the highest version header as the "current one".

It then verifies that the header layout format is something it can read, checks the CRC value (discarding the header if the checksum does not match) and then reads all the header blocks. Of course, at this point L1 doesn't know anything about the other layer headers other than their size. The layer headers are passed up through the layers who each read their own block and take the neccessary actions to set themselves up layer by layer. Provided that all layers report success upwards, the file has successfully opened. In particular, L2 looks at the block size and the total number of blocks expected in the file, which it uses to calculate the expected size of the file. If the file is longer than it should be, which will occur if a transaction crashed or was rolled back, it will be truncated to the expected size. 

## L4: The public API

L4 deals with organising a filing system on top of L3. Where L3 deals with streams, identified by an index, L4 deals with "stream names" and "directory names".

L4's public API is similar to a file system API and pertains to:

* Beginning, committing (optionally continuing) and rolling back transactions,
* Beginning, refreshing and closing snapshots,
* Creating, renaming and deleting a named stream,
* Creating, renaming and deleting a directory,
* Iterating over all streams inside of a Yak directory, including the root directory,
* Understanding the length of a Yak stream
* Reading from a stream, based on current head position,
* Positioning the read/write head inside of a stream,
* Writing to a stream, based on current head position (possibly enlarging the stream),
* Closing the Yak stream.

Operations that make changes, like changing names, modifying stream contents, creating and deleting streams can only be made through a write transaction. They take effect in the transaction's working version immediately (e.g. creating a new stream by name is visible from the perspective of the transaction immediately after the call has been made) and become durable in the Yak file when the transaction commits (i.e. only become visible to later snapshots & transactions after they've been committed).

Operations that read - listing a directory, opening a stream, asking for a stream's length or version etc - can be executed both from a transaction (where the transaction's changes are visible) or a snapshot (where only transactions committed before the snapshot was created are visible).

### Stream Readers & Writers
A Reader can be opened from a snapshot and a transaction, given a stream name. It manages a head position, which determines where reads happen. The head can be moved like a regular file handle, but it cannot be moved beyond the end of the stream. If a Reader is opened from a transaction, it cannot be opened on a stream that already has a Writer opened.

A Writer can be opened from a transaction, given a stream name. It has a head position like a Reader, which determines where reads and writes happen from and to. The head can be moved like a regular handle, but like Readers it cannot be moved beyond the end of the stream. If writing pushes the head beyond the end of the stream, the stream lengths. A Writer cannot be opened on a stream that already has a Reader opened in the same transaction. In the same transaction, anything written to a stream will be readable by other Readers and Writers opened after the original Writer is closed.

**** NEW DESIGN DOWN TO HERE ****

** CLEAN TO HERE **

### A filesystem from streams

Two types of streams exist inside of a Yak file: Directory streams and streams.

Streams contain the data that a caller has written to the Yak file, with no padding, nothing added, nothing taken away.

Directory streams are used to provide an addressing system for the streams. It associates stream identifiers (which are given and used by L3) with a name, using a "stream entry" structure, e.g.

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

Stream entries in the directory streams are serialized compactly (i.e. no gaps between entries). Since the length of their names vary, they are encoded as follows:

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

Versions are read from the version the transaction or snapshot sees. In a snapshot, they come from the snapshot's committed version. In a transaction, a stream or directory that the transaction has changed reports the version the transaction will commit as (the committed version plus one), which is exactly what snapshots will see once it commits; after `commit_and_continue()`, later changes report the version after that. Two changes made in the same transaction share a version and cannot be told apart, which only the transaction's own thread could observe.

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

Reserved space that is never written is wasted until the stream is truncated or the file is compacted (see Compacting a Yak file). When a stream is truncated, all blocks - including those in any extended reserve - are returned as free blocks, in effect making the stream's size and reserved size very near to each other (reserved will include the unwritten space in the last data block, so tends to be ever so slightly larger).

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

A transaction, and every Reader and Writer opened in it, is only ever used by the thread that began it, and the same holds for a snapshot (see Yak file thread, process, machine & crash safety). L3 therefore needs no locks inside a transaction or a snapshot: everything it keeps for one of them is touched by one thread only.

Streams must still be opened and closed like regular files before they can be written to and read from, including the Streams stream. Within a transaction, this tracks that there is at most one Writer (with no Readers) or any number of Readers (with no Writer) per stream, since all of them work on the transaction's uncommitted changes. The record of open Readers and Writers per stream is kept internally and found from the stream handle that L3 exposes to L4. It is a check, not a lock: opening a Writer on a stream that already has a Reader or Writer open in the transaction fails.

Snapshots are not tracked per stream. A snapshot only reads blocks of a committed version, which no transaction changes, so it needs no lock against the transaction. What it does need is for its version's blocks not to be reused while it reads; that is handled by pinning versions (see Pending blocks in L2 and Reader registry in L1).

The Streams stream and directory streams change every time a stream or directory is created, renamed or deleted. L4 changes them through a separate, non-public L3 API, within a single call, so they are never left open between calls and never conflict with a caller's Readers and Writers.

The dirty map belongs to the transaction, and is divided by stream; since only the transaction's thread changes it, it needs no locking.

Commit, `commit_and_continue()` and rollback are calls on the transaction like any other, so they can never run while another call on the same transaction is in progress. Snapshots, in other threads or processes, read committed versions only; a snapshot that begins during a commit simply gets the version that was live before it.

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

The management of blocks in L2 assumes nothing about what blocks contain or which stream they belong to. It is assumed that L3, where streams are managed, will appropriately handle access to a stream by its Readers and Writers and that block writes are dispatched by L3 in the right order. Only a single writer can exist per stream at a time (as per Yak's thread/process safety constraints) so any writes to blocks in a stream will be guarded by L3.

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

When the file is grown, the new length is recorded in memory only. The `total blocks` field in L2's header section is updated as part of the commit. If the process crashes before the commit, the file may be longer than the committed version's `total blocks`; when the next transaction begins, in whichever process, L2 shortens it to that length, which discards anything the crashed transaction wrote there. Snapshots are unaffected, since no version reaches beyond its own `total blocks`.

Blocks taken from the free list are not zeroed when allocated. Every block placed by L3 is written in full, and reserved space beyond a stream's size is never read, so zeroing would only add writes. Blocks gained by growing the file are already zero.

### Reusing blocks

When a block is released, it will often be in the middle of the underlying file that L1 is managing. For efficiency, blocks are reused by way of keeping a free list of blocks.

The free list is stored in the free blocks themselves, as a chain of *trunk* blocks. Each trunk block holds the index of the next trunk (4 bytes) followed by fixed-size free extents:

| Field  | Size (bytes) | Notes                                            |
| ------ | ------------ | ------------------------------------------------ |
| Start  | 4            | Little-endian u32; index of the first free block |
| Length | 4            | Little-endian u32; number of free blocks         |

With 4 KiB blocks a trunk holds (4096 - 4) / 8 = 511 extents, with 4 bytes left over. L2 has a "free list head" block index in its header section, which points to the first trunk (using 0xFFFFFFFF when there is no free block available). Pending-list trunks are laid out the same way, with a tag added to each extent (see Pending blocks), so both kinds of trunk are read and followed the same way:

* **New trunks are added at the head of the chain**, which is the only end a copy-on-write chain can change cheaply.
* **The head trunk fills from the back.** Extents are placed from the end of the block towards its front, so adding one never moves another. The unused part of the head trunk is the gap between the next pointer and its first used slot, and the bytes left over when the extents do not fill the block exactly are part of that gap.
* **Every trunk except the head is full.**
* **Empty slots are all zeros.** No extent has a length of 0, so the first slot with a length that is not 0 is the head trunk's first used slot. No count of used slots is stored.

```ASCII
L2 header              head trunk 811                                    trunk 95
┌──────────────┐      ┌────────────────────────────────────────────┐    ┌──────────────────────────────────────────────┐
│ Free: 811    │────► │ next: 95 │ 0+0 │ 0+0 │ 812+4 │ 1200+38 │ 77+1 │──► │ next: 0xFFFFFFFF │ 96+12 │ 3001+250 │ … │
└──────────────┘      └────────────────────────────────────────────┘    └──────────────────────────────────────────────┘
                                    gap
```

When blocks are freed, L2 sorts them and coalesces them into extents before adding them to the head trunk, starting a new head trunk (taken from the freed blocks themselves) when the current one is full. This promotes them being later re-allocated in run-order, enabling multi-block read and write operations (i.e. IO operations that span across as many blocks as possible, thereby lowering the number of IO calls).

#### The free list is copy-on-write too

The free list is part of a version, just like the streams are: a version's free list must stay intact for as long as that version is the one a crash would fall back to, because after such a crash, the next transaction allocates from it.

During a transaction, L2 holds the part of the free list it is using in memory: it takes extents from the head trunks as blocks are allocated, and adds released blocks that have become free. Trunks reachable from the live header are never modified in place. At commit, L2 writes the head trunks it changed as new trunk blocks (taken from the free extents themselves) and records the new free list head in its header section. Because the chain is consumed and extended at its head, a commit typically rewrites one or two trunk blocks. Trunks further down the chain are shared with the previous version, unchanged. L2 packs the extents of the trunks it rewrites from the oldest end, so every trunk except the head stays full, and the head holds what is left over, at its back. Extents taken from the middle of a trunk leave no holes on disk, since that trunk is one of those rewritten.

Trunk blocks that the new version no longer uses are only needed by the previous version's free list, which only the writer ever reads. They are released in the same way as stream blocks, except that no reader can be holding them, so they become free as soon as the next transaction begins.

### Pending blocks

A block released by a commit cannot go straight back on the free list. Readers holding an older version may still be reading it, and until the commit's header is ordered before later writes, a crash could fall back to the previous version, which uses it.

L2 therefore keeps a *pending list*, whose head is in L2's header section. It is laid out exactly like the free list (see Reusing blocks), except that each extent is preceded by a *tag*: the version created by the commit that released it.

| Field  | Size (bytes) | Notes                                                                         |
| ------ | ------------ | ----------------------------------------------------------------------------- |
| Tag    | 8            | Little-endian u64; the version created by the commit that released the extent |
| Start  | 4            | Little-endian u32; as in the free list                                        |
| Length | 4            | Little-endian u32; as in the free list                                        |

With 4 KiB blocks a pending trunk holds (4096 - 4) / 16 = 255 entries, with 12 bytes left over. Moving an entry to the free list just drops its tag.

Only three things differ from the free list:

* **Pending trunks never live in the blocks they describe.** They are taken from the free list, or from growing the file. A pending block may still be read by a snapshot on an older version, and writing a trunk into it would change what that snapshot reads.
* **Entries are in descending tag order, newest first.** Commits take turns through the writer lock and each one creates a higher version, so adding entries at the head, as the free list does, puts the newest tag in the head trunk's first used slot and the oldest at the end of the last trunk. The tags that are clear are therefore always the oldest ones, at the far end of the chain. A commit that releases more extents than the head trunk has room for continues in a new head trunk, so the same tag can appear in two trunks; the order is then descending or equal, which changes nothing below.
* **An empty slot is recognised by its tag.** No commit releases blocks when creating version 1, the first version of a file, so a tag is never 0.

```ASCII
L2 header              head trunk 520                                trunk 133
┌──────────────┐      ┌───────────────────────────────────────┐     ┌──────────────────────────────────────┐
│ Pending: 520 │────► │ next: 133 │ 0 │ 0 │ 412 │ 411 │ 411 │ ────► │ next: 0xFFFFFFFF │ 410 │ 409 │ … │ 400 │
└──────────────┘      └───────────────────────────────────────┘     └──────────────────────────────────────┘
                                   gap   newest ──────────────────────────────────────────────────► oldest
```

Each commit adds its entries to the head trunk, so a trunk holds the releases of many commits and the chain stays short even while a snapshot holds back reuse for a long time.

A block released by the commit that created version *v* may be reused once both of these are true:

* Version *v*'s header is ordered before anything written after it, so a crash can no longer fall back to an earlier version that still uses the block while the block has been overwritten. A `Full` or `Ordered` commit guarantees this at its second sync point (see Commit ordering in L1). A `None` commit gives no guarantee after a power loss anyway, so it does not hold back reuse.
* No reader holds a version older than *v*.

At the start of each transaction, and again at each `commit_and_continue()`, L2 asks L1 whether any snapshot, in this process or another, holds a version older than a given version (see Reader registry in L1). An entry tagged with version *v* is eligible when the answer for *v* is no. The answer can only change from yes to no as *v* gets smaller, so L2 finds the newest eligible tag with a few questions rather than one per entry: it asks first about the newest tag, the head trunk's first used slot, and only if that is not yet clear does it walk the chain and search among the tags, which are in order at fixed offsets. The extents of that entry and of every entry after it are moved into the free list in memory.

When every entry is eligible, which is the common case, the pending list head simply becomes the sentinel and nothing is rewritten. When only some are, L2 cuts the chain after the last entry that is still held back, which rewrites the trunks from the head down to that point. These are exactly the trunks that are still held back, and this only happens when a snapshot holding back reuse moves on, since otherwise no new entry becomes eligible. L2 repacks the entries it keeps as it rewrites them, as it does for the free list. Trunks that are no longer reachable from the pending list are released like free-list trunks (see The free list is copy-on-write too). The move is recorded on disk by the next commit, which writes the new free list head and pending list head together. If the process crashes before then, the blocks are still pending when the file reopens and are released again, so a crash can delay reuse but never lose a block.

An open knows which durability level its own commits used, but not which one another open used: the last commit may have come from another process, with `Durability::None`, leaving it unordered in the operating system's cache. When a transaction begins and the live header was not written by this open, L1 therefore issues one barrier before any block is reused, which orders every commit already in the file before anything the new transaction writes.

A snapshot that is held open for a long time prevents every block released since its version from being reused, so the file grows. This is the cost of snapshots that never block. A diagnostic reporting the oldest version held by a snapshot, to help find ones that are held for too long, may be added later; it is not part of `verify()`.

### Block caching

L2 will often be requested to read the same block over and over again. This is most often the case when data is being written to or read from a larger stream (i.e. a stream that must use redirector blocks) and the redirector blocks need constant access to locate the data in the stream. L2 therefore manages a cache of blocks and it provides to L3 a way to use this cache for some blocks, while not for others. L3 uses the cache for redirector blocks. The actual data written/read to a stream circumvents the cache, under the assumption that callers know what data they've placed in the Yak file (and therefore won't be asking the Yak file to read that data again) and, when reading, receive a copy of the stream into their memory buffer and therefore won't ask to read the same data again.

There is no cache shared by the whole Yak file. Each snapshot and each transaction has its own block cache, which L2 keeps for it and drops when it ends. This removes all coordination around caching: nothing outside a snapshot or transaction can make its cache stale.

* **A snapshot's cache never needs evicting for correctness.** Once committed, a block stays the same until a commit releases it, and it is only reused once no snapshot holds a version that can read it (see Pending blocks). So every block a snapshot has cached stays valid until the snapshot ends, whatever the transaction or other processes do. `refresh()` empties the cache: blocks cached for the old version that the new version no longer reaches are no longer protected, and could be reused while the snapshot is still open.
* **A transaction's cache holds committed blocks that the transaction has read.** Blocks it changes are held in L3's dirty map instead (see The dirty map). After `commit_and_continue()`, blocks released by earlier commits may be reused by the same transaction, so when L2 hands the transaction a block, L2 evicts that block from the transaction's cache. No other transaction can reuse blocks at the same time, since only one transaction is open. When blocks are placed at commit, their new content is put into the transaction's cache, so a transaction that continues does not read them back.

Each cache has a memory budget, set when the Yak file is opened, and evicts its least recently used blocks when it exceeds it. The cost of separate caches is that each snapshot warms its own, so many short-lived snapshots each read the same redirectors again, usually from the operating system's cache. If that turns out to matter, all snapshots of the same version could share one cache, since that version's blocks never change; this is left for later.

Caches in other layers, such as directory listings cached by L4, follow the same rule: they belong to a snapshot or a transaction, never to the Yak file.

### Additional feature & additional complexity: Block encryption
Occasionally, a caller may desire data stored in a Yak file to be encrypted. The default L2 implementation in Yak offers optional whole-file encryption for all streams in a Yak file.

Yak intentionally takes a standard, industry-accepted approach towards encryption, but no one should trust their secrets to Yak's implementation or hope that there are no vulnerabilities; undoubtedly there are many.

When a block is requested by L3, L2 loads the block, decrypts it and returns it. When L2 is requested to store a block, L2 encrypts it and stores it back to disk.

L2 uses a block cache to return often used blocks quickly, which is typically used to store hot redirector paths. Blocks in this cache are stored unencrypted in memory; only when they are written to the Yak file are they encrypted. Since each snapshot and transaction has its own cache, each decrypts its own copy of a block it reads. The same applies to the dirty map in L3.

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

L2's in-memory free list, its record of private and released blocks, and its block cache all belong to a transaction or a snapshot, which only one thread uses, so L2 needs no locking for them. Only one transaction exists at a time, so only one thread ever allocates or releases blocks. Snapshots never allocate.

## L1: Commits

L1 turns the plain storage provided by L0 into a Yak file. It writes, reads and verifies the Yak file "magic header", to check that a valid Yak file has been opened, and it carries each layer's header section, so that the layers above can store their management data outside the regular block format. With transactions, L1 makes commits atomic and durable, and keeps track of which versions readers are holding.

L1 has no direct access to a file system; everything it stores, it stores through L0.

### L1 API

L1's API, which is used by L2, should expose:

* Creating a Yak file
* Opening a Yak file for reading or for writing
* Beginning and ending a transaction's hold on the writer lock
* Reading from and writing to the data region, at an offset from its start
* Growing and shortening the data region
* Laying out the header sections at creation, from the section sizes passed down by the layers above
* Giving the section ID of the first section above L1
* Reading the live header slot, as a copy from which each layer reads its section
* Staging a section for the next commit
* Publishing the staged header (the commit point), with the syncs the durability calls for
* Beginning and ending a snapshot's hold on a version, and moving it to the latest version on `refresh()`
* Answering whether any snapshot, in this process or another, holds a version older than a given version

L2 addresses the data region from 0, so block *n* is at offset *n* × block size, and L2 never needs to know where the header ends.

### Commit ordering

Copy-on-write is only crash safe if the header never reaches storage before the blocks it points at. Operating systems and storage devices may reorder writes freely, so L1 enforces the order with L0's barrier and sync (see Syncing in L0). A commit has two sync points:

1. After every block of the commit has been written, and before the header slot is written. This only needs *ordering*: the header must not reach storage before the blocks it points at.
2. After the header slot has been written. This decides whether the commit is durable when `commit()` returns, and orders the header before anything written after it.

The durability level chosen for a commit decides what is done at each point:

| Level                 | Sync point 1 | Sync point 2 | After a power loss or operating system crash                                                                    |
| --------------------- | ------------ | ------------ | --------------------------------------------------------------------------------------------------------------- |
| `Durability::Full`    | Barrier      | Sync         | The commit has happened once `commit()` returns.                                                                |
| `Durability::Ordered` | Barrier      | Barrier      | The file is valid, but the most recent commits may be lost. (Like SQLite's WAL mode with `synchronous=NORMAL`.) |
| `Durability::None`    | Nothing      | Nothing      | The file may be damaged; `verify()` can check it.                                                               |

A process crash loses nothing at any level, since the operating system already has every write the process made. Where a platform has no barrier, the barrier is a sync, so `Ordered` costs the same as `Full` there; the benefit is on macOS, where a full sync is expensive. The relative cost of the levels has not been benchmarked yet.

### Header slots

L1 owns the two header slots and the magic section before them (see Opening and creating a Yak file). Each layer stages its header section for the next commit, through the layers below it (see Opening and creating a Yak file); L1 assembles the slot from the staged sections, the next version and a CRC32C checksum over the whole slot, and writes it to the slot that is not live.

L1 does not rely on the storage writing a slot atomically: a slot that was only partly written fails its checksum and is ignored, and the other slot is used. That is what makes a torn header write safe.

There is one failure the checksum cannot recover from: a torn write to one slot that also damages the other, which would leave no valid slot and a file that cannot be opened. This could happen if both slots shared a physical sector, or a file system block that the file system rewrites as a whole. To avoid it, each slot starts at a file offset that is a multiple of 4 KiB (or of the storage's block size, if L0 reports that it is larger) and is padded to a whole number of those units.

Note that alignment within the file is not alignment on the storage device. The file system decides where a file's bytes are stored, and nothing guarantees that a 4 KiB-aligned file offset lands on a sector boundary. On common local file systems (ext4, XFS, NTFS, APFS) files are stored in whole blocks starting at a block boundary, so aligned file offsets do map to whole blocks and sectors; LMDB and SQLite rely on the same property. On file systems that pack small files inline, compress or encrypt transparently, or are accessed over a network, it may not hold. Alignment therefore reduces the chance of losing both slots on common file systems rather than ruling it out on all of them. A Yak file nested in a stream of another Yak file gets its protection from the outer file's commits instead (see Layer composition opportunities).

The live slot alternates between A and B with each commit. A commit never writes the live slot, so the previous version is always available in the other slot until the next commit has completed.

### Locks

L1 coordinates processes with byte-range locks that L0 takes on the Yak file's own storage, at offsets far beyond its end (see Locks in L0). These bytes are never read or written, only locked, and nothing is stored in them: which bytes are locked, and how, is all the information there is. The operating system releases a process's locks when it closes the file or dies, so a crashed process never leaves anything behind.

The lock region starts at the *lock base*, 2^62, a constant defined in one place. L1 never grows the data region into it; at 4 EiB, no real file comes close.

| Offset              | Lock                  | Taken by                                                                                        |
| ------------------- | --------------------- | ----------------------------------------------------------------------------------------------- |
| lock base           | Writer                | Exclusive by the open transaction, from `begin_transaction()` until `commit()` or `rollback()`. |
| lock base + 1 + *v* | Reader of version *v* | Shared by an open whose oldest snapshot holds version *v* (see Reader registry).                |

Opening a Yak file, for reading or for writing, takes no lock, so opening never waits. Beginning a transaction takes the writer lock exclusive, waiting for it or failing at once, according to the options (see Beginning a transaction). `commit_and_continue()` keeps it. Snapshots therefore never conflict with a transaction, and only one transaction exists at a time.

Versions are 64-bit numbers, and lock base + 1 + *v* must stay below the largest offset that can be locked (2^63 - 1 on POSIX), which leaves room for about 4.6 × 10^18 versions. A commit that would exceed it fails rather than corrupting anything.

### Reader registry

Before L2 reuses blocks released by the commit that created version *v*, it asks L1 whether any snapshot, in this process or another, holds a version older than *v* (see Pending blocks in L2). L1 keeps the record that answers it.

For each open of a Yak file, L1 keeps a map from version to the number of open snapshots on that version, guarded by a mutex. The map is ordered, so its first entry is the open's oldest snapshot.

Every open publishes its oldest version, by holding a shared lock on that version's reader byte, whether or not it can also write: any open may hold the next transaction, and that transaction cannot see the other opens' maps. An open holds at most one reader lock at a time. That is enough, because blocks are only reused once no snapshot holds a version older than the commit that released them, so the oldest snapshot protects every newer one. It also matters for correctness: locking the same byte twice through the same open merges into one lock with OFD locks but stacks on Windows, so L1 never does it, and lets the map decide when to lock and unlock. Several opens, in the same process or in different ones, can hold a shared lock on the same byte; each releases its own, independently of the others.

To begin its first snapshot, an open:

1. Reads the header, and notes its version.
2. Locks that version's reader byte, shared, waiting if necessary.
3. Reads the header again. If a newer commit has completed in the meantime, it locks the newer version's byte, unlocks the older one, and repeats this step.

A transaction only checks the registry after it has read the live header it starts from, or published a new one. A snapshot therefore either locked its version before the transaction looked, so the transaction respects it, or it sees that header, or a newer one, in step 3 and moves to it before reading anything. Either way, no block a snapshot can see is reused.

After that, the map drives the lock:

* **A later snapshot** reads the header to find the latest version, begins on it and only adds to the map; the lock on the oldest version already protects it.
* **A snapshot ends.** Its version's count goes down. If that removes the oldest version and other snapshots remain, the open locks the next oldest version's byte, then unlocks the old one, so it is covered throughout. If no snapshots remain, it unlocks.
* **`refresh()`** is a snapshot ending and a new one beginning, with the new one counted before the old one is removed.

To answer whether any snapshot holds a version older than *v*, the transaction's open first checks its own map. If its own oldest snapshot is older than *v*, the answer is yes, with no lock needed. Otherwise *v* is at or below the open's own oldest version, so the range lock base + 1 up to lock base + 1 + *v* cannot contain the open's own reader lock. The transaction tries to lock that range, exclusive and failing at once. If that fails, another open holds an older version; if it succeeds, none does, and the lock is released straight away. Staying below its own reader lock matters with OFD locks, where an exclusive lock over the same open's shared lock silently converts it instead of failing. An open that moves its reader lock while a transaction briefly holds the range waits for a moment, which is why reader locks are taken waiting.

The only blocks a transaction can reuse are on the free list, which needs no check, or on the pending list, whose tags it reads from the live header whichever open committed them. So the only versions it ever asks about are the pending tags: it never needs to know which versions other opens hold, only whether each pending tag is clear yet.

An L0 whose locks only work within one process, such as one keeping the file in memory, gives the same results, since every snapshot is then in the same process.

### Beginning a transaction

A transaction may follow commits made by other opens, in this process or in others, so it carries nothing over from earlier transactions:

1. It takes the writer lock, exclusive, waiting or failing at once according to the options. The operating system cannot tell threads apart, since all threads of one open share its lock owner, so each open puts a mutex in front of the writer lock and records which thread holds it: threads of the same open wait on the mutex, threads of other opens and processes wait on the writer lock, and either way the behaviour is the same. A thread that already holds the open's transaction fails at once instead of waiting for itself.
2. It reads the live header, and every layer starts from the sections in that header. No layer keeps transaction state from one transaction to the next. A layer may keep such state as an optimisation only while the live header is still the one this open last committed, which shows that no other open has committed since.
3. If the live header was not written by this open, L1 issues one barrier (see Pending blocks in L2), and L2 shortens the file to the header's `total blocks` if a crashed transaction left it longer (see Allocating blocks in L2).
4. L2 checks which pending tags are clear (see Reader registry) and moves the extents of those entries onto the free list in memory.

`commit()` and `rollback()` release the writer lock. `commit_and_continue()` keeps it, so the transaction carries on with its own state, which no other open can have changed.

Waiting for the writer lock is not fair: neither OFD locks nor `LockFileEx` serve waiting locks in order. An open that commits and begins a new transaction straight away, in a loop, can keep transactions in other opens waiting indefinitely. This is a documented limitation for now; a gate lock that waiting transactions take first, as CompoundFs uses, would make it fair.

### Several opens in one process

Locks belong to the open file on every supported platform (see Locks in L0), so each open of a Yak file is a separate lock owner, and L1 needs no record of which files a process has open. A second open in the same process behaves exactly like an open from another process: it takes its own reader lock, and its transactions take turns with every other open's through the writer lock.

### Thread & process safety in L1

L1 ensures that only one transaction is open at a time, across all opens and processes, through the writer lock (see Locks), so snapshots can be begun while a transaction is open. Threads of the same open take turns through the open's mutex in front of the writer lock, so a thread behaves exactly like a process (see Beginning a transaction).

The only state L1 shares between an open's threads is the writer mutex and the map of open snapshots per version (see Reader registry), each guarded by its own mutex. Everything else belongs to a transaction or a snapshot, which only one thread uses.

Snapshots do not need a lock to read the Yak file itself. They only read blocks that belong to the version they hold, which no transaction changes, and they find that version through a header slot whose checksum protects against reading it while it is being written.

## L0: Storage

L0 deals with accessing an underlying file system. Most commonly this would be a real file system, but adapters could be written for other situations, like keeping a Yak file in memory, injecting faults for crash testing, or storing a Yak file in a stream of another Yak file (see Layer composition opportunities).

L0 knows nothing about Yak: not the header, not blocks, not versions. Everything it is given, it stores as bytes at the offset it is told.

### L0 API

L0's API, which is used by L1, should expose the functions necessary to wrap actual file system calls for:

* Creating storage, and opening existing storage
* Reading from and writing to the storage, at any offset
* Getting the length of the storage, growing it and shortening it
* Ordering writes with a barrier (every write completed before it reaches storage before any write issued after it)
* Syncing the storage (all completed writes reach storage)
* Reporting the storage's block size, if it is known
* Locking and unlocking byte ranges of the storage, including ranges beyond its end, shared or exclusive, either waiting or failing at once

L0 keeps no process-wide state: everything it holds belongs to the storage itself (a file's handle or, for an L0 that keeps the file in memory, its contents and locks). It does not know which storages are open in the process, or what its locks mean.

### Syncing

L0's sync must not return until every write completed before it has reached storage. L0's barrier only promises order: every write completed before the barrier reaches storage before any write issued after it, but the barrier may return before either has. L1 relies on both to order commits (see Commit ordering in L1).

| Platform | Barrier                 | Sync                 |
| -------- | ----------------------- | -------------------- |
| Linux    | `fdatasync`             | `fdatasync`          |
| Windows  | `FlushFileBuffers`      | `FlushFileBuffers`   |
| macOS    | `fcntl(F_BARRIERFSYNC)` | `fcntl(F_FULLFSYNC)` |

Linux and Windows have no barrier that is cheaper than a sync, so their barrier is a sync. On macOS, plain `fsync` does not flush the drive's own cache, so a sync is `F_FULLFSYNC`, which costs around 20 ms on Apple's internal drives; `F_BARRIERFSYNC` orders writes without waiting for them, which is what makes `Durability::Ordered` worthwhile there.

An L0 that is not backed by durable storage, such as one that keeps the file in memory, may implement both as nothing.

### Locks

L1 coordinates the writer and readers across processes with byte-range locks on the storage itself, at offsets far beyond its end that are never read or written (see Locks in L1). Locks can be shared or exclusive, and taken either waiting or failing at once. A lock is released when its holder releases it, closes the storage, or dies.

For a file on disk, locks are the operating system's byte-range locks, which both POSIX and Windows allow beyond the end of a file:

| Platform     | Mechanism                                                           | Held by       |
| ------------ | ------------------------------------------------------------------- | ------------- |
| Windows      | `LockFileEx` and `UnlockFileEx`                                     | The handle    |
| Linux, macOS | `fcntl` open file description locks (`F_OFD_SETLK`, `F_OFD_SETLKW`) | The open file |

The operating system releases them if the process dies. Locks on Windows are mandatory, so a locked range cannot be read or written through other handles; this does not matter, since nothing reads or writes the locked ranges. A shared lock only needs the file open for reading, so reader processes need no write access.

Because locks belong to the open file on both rows, every open of a file is a separate lock owner, and other code in the process that opens and closes the same file cannot release Yak's locks.

macOS has had open file description locks in its kernel since OS X 10.11. Its headers kept the constants private until the macOS 14 SDK, but their values never changed, so they work on every macOS version Rust supports; the Rust `libc` crate defines them for Apple targets.

The BSDs are not supported. They only have process-wide `fcntl` record locks, which never conflict within a process and are all released when the process closes *any* descriptor for the file.

An L0 that keeps the file in memory, or in a stream of another Yak file, can keep its locks in memory, which works within one process only. The locks must be kept with the stored data, not with each open of it, so that every open of the same storage sees the others' locks, as the operating system does for a file on disk.

L0 does not know what the locked ranges mean; the writer and reader locks are L1's.

## Verifying a Yak file

With atomic commits, a Yak file does not need verifying after a crash: it opens at the last committed version. `verify()` remains for four reasons:

* Commits made with `Durability::None` skip syncs, so a power loss can leave a header whose blocks never reached storage.
* Bugs in Yak itself. The test suite runs `verify()` to check that every state Yak leaves behind, including every state a simulated crash leaves behind, is valid (see Testing).
* Structural damage to the file from the storage it is kept on. Only the header slots carry a checksum, so `verify()` can find damage to structure (indices out of range, impossible lengths, broken chains), but not to the contents of streams.
* Leaked blocks, which show whether compacting the file is worth it (see Compacting a Yak file).

`verify()` is implemented as a chain across the four upper layers (L4 -> L3 -> L2 -> L1; L0 has no invariants of its own to check), each layer checking its own invariants and passing claimed resource lists down. With transactions, verification checks a committed version, and additionally checks:

* That both header slots can be read, and reports which is live and the version of each.
* That no block is both reachable from the committed version and on the free list.
* That no block appears on both the free list and the pending list.
* That every block of the file below `total blocks` is reachable from the committed version, on the free list, or on the pending list. A block that is none of these has leaked; leaked blocks are reported, and reclaimed by compacting the file.

`verify()` does not report which versions snapshots hold. That is information about who has the file open at the moment, not about the file's integrity (see Pending blocks in L2).

Verification of a version can run alongside the writer, because the version it checks does not change.

## Compacting a Yak file

Compacting is not part of the library. The command line Yak tool compacts a Yak file using only the public API: it begins a snapshot of the file, creates a new Yak file, and for each directory and stream in the snapshot creates one with the same name in the new file, reserving the stream's size before copying its data so that the data is written contiguously. The new file then replaces the original. Only the snapshot's version is copied, so pending blocks, leaked blocks, excess reserved capacity and the history of older versions are left behind, and the new file starts from a single version. Streams keep their names but may be given different stream identifiers. Settings fixed at creation, such as the block size, the encryption or the header layout version, can be changed along the way.

Yak does not check whether anyone else has the file open when it is replaced. An open of the original keeps the original's storage, so its snapshots keep working, but anything it commits afterwards goes into the replaced file and is lost. A file should only be compacted when no other process is using it. Keeping compaction out of the library was chosen over making this check: the check needs a presence lock taken by every open and a way for L0 to replace one storage with another atomically, all for one maintenance task that the public API can already do.

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
** TO WORK IN: notes from the process coordination review, to be moved into the sections named in each heading **

#### Lock discipline (for Locks in L1)

OFD locks and `LockFileEx` behave the same only if L1 keeps to three rules:

1. **Never lock a byte that the same open already holds.** OFD locks merge a repeated lock into one, while Windows stacks it and needs one unlock per lock. The map in the reader registry decides when to lock and unlock, and a reader moves by locking the new byte before unlocking the old one, so no byte is ever locked twice.
2. **Never convert a lock in place.** OFD locks silently turn a shared lock into an exclusive one when the same open locks over it, and Windows cannot convert at all. A transaction only probes below its own open's oldest snapshot, so its range never covers its own reader lock.
3. **Unlock exactly the range that was locked.** Windows requires an unlock to match a lock's range exactly, where OFD locks can unlock part of a range. Every lock is a single byte, locked and unlocked on its own, except the writer's probe, which is locked and unlocked as one identical range.

#### Probing for readers cheaply (for Pending blocks in L2)

The probe only answers yes or no: does any other open hold a version older than *v*? Two facts keep the number of probes small:

* **Probe the newest pending tag first.** If no reader holds a version older than it, none holds a version older than any older tag either, so one probe usually releases every pending entry. Only when it fails does the writer search, and only across the tags actually on the pending list.
* **A clear answer stays clear.** A new snapshot always begins on the latest version, and a reader only ever moves its lock to a newer version, so once no reader holds a version older than *v*, that remains true. The writer remembers the newest tag it found clear, and only probes tags newer than that.

In practice a transaction start costs one to three probes. Each is a single system call into the kernel's lock table (roughly a microsecond, not benchmarked), which is negligible next to the sync of a durable commit.

#### Fork in more detail (for Yak file thread, process, machine & crash safety)

A child process created with `fork()` inherits the parent's descriptors, and with them its OFD locks, which belong to the open file rather than to either process. If the child keeps running after the parent closes the file or exits, the child still holds the parent's locks, including the writer lock, until it closes the inherited descriptor or exits itself. Python's `multiprocessing` with the fork start method creates exactly such children. Opening files with `O_CLOEXEC` (the default in Rust) only helps when the child goes on to start another program, which a forked worker does not. macOS has a private way to stop locks being inherited (`F_SETCONFINED`); Linux has none, so this stays a documented rule: a forked child must not use the parent's Yak files, and should not keep running while the parent expects to reopen the file for writing.

#### A stuck writer (for Thread & process safety in L1)

The writer lock is held for as long as a transaction is open. A process that crashes during a transaction releases it at once, but a process that hangs inside a transaction without crashing keeps it, and every transaction that other opens try to begin waits for it, or fails, indefinitely. Yak has no timeout or handover for the writer lock. Snapshots are unaffected, since they never wait for a transaction; and a process that hangs outside a transaction blocks nobody.

#### 64-bit file offsets (for Locks in L0)

The lock region starts at 2^62, so every platform needs 64-bit file offsets and locks, though not a 64-bit operating system. `LockFileEx` takes a 64-bit offset on 32-bit Windows, and macOS has been 64-bit only since 10.15. On 32-bit Linux, OFD locks need the 64-bit lock structure (`flock64`); glibc hides the OFD constants from 32-bit builds without large-file support, so a 32-bit Linux L0 must use `flock64` explicitly. A 32-bit target such as armv7 (for Raspberry Pi-class devices) belongs in CI if those devices matter.

#### Unusual local file systems (for Testing)

Network file systems are already outside what Yak supports (see Yak file thread, process, machine & crash safety), so they are not tested. Some local setups still deserve a test that locks at about 2^62 behave as expected, since no evidence was found either way: file systems implemented in user space (FUSE), and a Windows drive opened from inside WSL, where a Linux process and a Windows process opening the same file may not see each other's locks at all.
