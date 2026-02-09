# L3 Mock: Design & Decisions

This document captures the design decisions and implementation plan for the L3 Mock phase of SFS.

## Goal

Introduce an L3 trait (the data stream abstraction) and make L4 generic over it. Implement an L3 mock that stores each stream as a numbered file on disk (`n.stream`), rather than linking blocks together (that's L2's job later).

This forces L4 to become a **real filing system on top of numbered streams**: directory streams, data streams, stream entry serialization, and path resolution through directory stream lookups -- all as described in the architecture doc.

In the L4 Mock phase, the filesystem did the heavy lifting (real directories, real named files). Now L4 must do that work itself, using only numbered streams from L3.

## What Changes from L4 Mock

| Aspect | L4 Mock (before) | L3 Mock (after) |
|---|---|---|
| On-disk layout | SFS dirs → real subdirs, streams → named files | Flat directory of numbered `n.stream` files |
| Directory management | Real `mkdir`/`rmdir`/`readdir` calls | L4 reads/writes **directory streams** containing serialized stream entries |
| Stream naming | Filename = stream name on disk | L4 maps names → stream IDs; L3 only knows numbers |
| Stream I/O | Direct file open by name | L4 asks L3 to open stream by ID; L3 returns a handle |
| Path resolution | `root.join(path)` on filesystem | L4 walks directory stream hierarchy: root dir stream → child dir stream → ... |
| Locking | Tracked in L4 by path string | **All locking in L3** (by stream ID). L4 has no lock tracking -- it delegates to L3 and propagates errors. |

## On-Disk Layout (L3 Mock)

When a caller creates an SFS file at path `foo.sfs`, L3 mock creates a directory `foo.sfs/` containing numbered stream files:

```
foo.sfs/
  meta            ← metadata file (block_index_width, block_size_shift, next_stream_id)
  0.stream        ← root directory stream (created automatically)
  1.stream        ← e.g. data stream for "image.png"
  2.stream        ← e.g. directory stream for "textures/"
  3.stream        ← e.g. data stream for "textures/skin.png"
```

Each `.stream` file is a plain file on disk containing raw bytes -- either directory stream entries (for directory streams) or user data (for data streams).

This is inspectable with a regular file manager, though less intuitive than L4 mock's direct mapping.

## L3 Trait

The L3 trait defines the contract that L4 uses. Based on the architecture doc (L3 API section).

`block_index_width` and `block_size_shift` are **runtime values** passed at creation time and stored in the meta file. They are read back at open time. Internally, all stream IDs use `u64`; on-disk serialization (in L4 directory entries) uses only `block_index_width` bytes.

```rust
pub trait StreamLayer: Send + Sync {
    /// Handle type for open streams. Must be Copy so L4 can extract handles
    /// from its internal maps without borrow conflicts.
    type Handle: Copy;

    /// Create a new L3 storage at the given path.
    /// `block_index_width` is the number of bytes used for block indices on
    /// disk (e.g. 2, 4, or 8).
    /// `block_size_shift` is the power-of-2 exponent for block size
    /// (e.g. 12 → 4096 bytes).
    fn create(path: &str, block_index_width: u8, block_size_shift: u8) -> Result<Self, SfsError>
    where Self: Sized;

    /// Open an existing L3 storage at the given path.
    /// Reads `block_index_width` and `block_size_shift` from the stored metadata.
    fn open(path: &str) -> Result<Self, SfsError>
    where Self: Sized;

    /// The number of bytes used for block indices on disk.
    fn block_index_width(&self) -> u8;

    /// Block size as a power of 2 (e.g. 12 → 4096 bytes).
    fn block_size_shift(&self) -> u8;

    /// Create a new stream. Returns the stream identifier.
    fn create_stream(&self) -> Result<u64, SfsError>;

    /// Check whether a stream with the given identifier exists.
    fn stream_exists(&self, id: u64) -> bool;

    /// Open an existing stream by identifier.
    /// Enforces locking: one writer OR many readers per stream.
    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, SfsError>;

    /// Close a stream handle.
    fn close_stream(&self, handle: Self::Handle) -> Result<(), SfsError>;

    /// Delete a stream by identifier. Fails if the stream is currently open.
    fn delete_stream(&self, id: u64) -> Result<(), SfsError>;

    /// Read from a stream at the given position.
    /// Returns the number of bytes actually read.
    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError>;

    /// Write to a stream at the given position.
    /// Extends the stream if writing past the end.
    /// Returns the number of bytes written.
    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, SfsError>;

    /// Get the total length of a stream in bytes.
    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, SfsError>;

    /// Truncate a stream to the given length.
    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), SfsError>;
}
```

### Key trait design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Handle type | Associated type `Self::Handle: Copy` | Allows each L3 impl to use its own handle type. `Copy` bound avoids borrow conflicts when L4 copies handles out of its internal maps. |
| Position in read/write | Caller passes `pos` explicitly | Architecture says L3 handles don't track head position -- that's L4's job. So read/write take a position parameter. |
| Block parameters | Runtime values via `create()` params and getters | A single binary must be able to open SFS files with different block sizes. Compile-time generics would prevent this. |
| Stream IDs | `u64` internally | Plenty of streams for any practical use. On-disk serialization uses only `block_index_width` bytes (e.g. 4 bytes for u32-equivalent). |
| Locking | Enforced inside L3 | One writer OR many readers per stream. L3 tracks this per stream ID internally. L4 does **not** duplicate locking. |
| Thread safety | `&self` on all methods, `Send + Sync` bound | L3 implementations use interior mutability (e.g. `Mutex`) to allow safe sharing across threads. |
| create/open on trait | Associated functions returning `Self` | L3 is responsible for creating/opening the underlying storage. |

## L4 Changes: Filing System on Numbered Streams

This is the most significant change. L4 must now implement the filing system described in the architecture:

### Root Directory Stream

- When L4 creates a new SFS file, it calls `L3::create()` to create the storage, then `L3::create_stream()` to create stream 0 -- the **root directory stream**.
- L4 stores the root directory stream ID (always 0 for a new file; read from state for an open file).
- When L4 opens an existing SFS file, it calls `L3::open()` and knows stream 0 is the root directory stream.

### Directory Streams

A directory stream contains serialized **stream entries**. Each entry maps a name to a stream identifier:

```
| length: u16 | identifier: [u8; block_index_width] | name: [u8] |
```

- `length` (u16): Total size of this entry in bytes, including the length field itself. So: `2 + block_index_width + name_bytes.len()`.
- `identifier` ([u8; block_index_width]): The L3 stream ID this entry points to, serialized as the low `block_index_width` bytes of the u64 ID in little-endian order.
- `name` (UTF-8 bytes): The entry name. If the name ends with `/`, this entry points to another directory stream. Otherwise, it points to a data stream.

When listing a directory, L4 opens the directory stream via L3, reads all bytes, and parses the stream entries.

When creating a stream or directory, L4:
1. Calls `L3::create_stream()` to get a new stream ID.
2. Serializes a new stream entry (with `/` suffix for directories).
3. Appends it to the parent directory stream.

When deleting, L4 removes the entry from the parent directory stream (shifting subsequent entries up) and calls `L3::delete_stream()`.

### Path Resolution

To resolve a path like `"textures/skin.png"`:

1. Start with the root directory stream (ID 0).
2. Open it, read all stream entries, find the one named `"textures/"`.
3. That entry's identifier points to another directory stream. Open that.
4. Read its entries, find `"skin.png"`.
5. That entry's identifier is the data stream ID.

This replaces the L4 mock's `resolve_path()` which simply did `root.join(path)`.

### Data Streams

Data streams are unchanged from the caller's perspective. They contain raw bytes written by the user. The difference is that L4 now accesses them through L3 by stream ID, not by filename on disk.

### L4 Internal State

```rust
pub struct Sfs<L3: StreamLayer> {
    layer3: L3,
    root_dir_stream_id: u64,
    state: Mutex<SfsState<L3::Handle>>,
}

struct SfsState<H> {
    next_handle_id: u64,
    open_streams: HashMap<u64, OpenStreamInfo<H>>,
}

struct OpenStreamInfo<H> {
    path: String,
    stream_id: u64,
    l3_handle: H,
    position: u64,
    mode: OpenMode,
}
```

L4 keeps its own handle system (returning `StreamHandle` to callers) and maps internally to L3 handles + position tracking. Note: **no `path_locks` map** -- L4 delegates all locking to L3 (see Locking Strategy below).

All L4 state is protected by a `Mutex`, and all methods take `&self` (interior mutability), making `Sfs` thread-safe.

## Locking Strategy

**All locking lives in L3.** L4 does not maintain its own lock tracking.

L3 enforces per-stream locking by stream ID:
- One writer OR many readers per stream.
- `open_stream(id, Write)` fails if any reader or writer is already open on that stream.
- `open_stream(id, Read)` fails if a writer is already open on that stream.
- `delete_stream(id)` fails if the stream is currently open (any readers or writer).

L4 relies entirely on L3's locking. When L4 needs to check whether a stream is open (e.g. before deleting or renaming), it checks its own `open_streams` map for matching stream IDs and propagates any `LockConflict` error from L3.

This avoids duplicating lock state between layers. L3 is the single source of truth for lock state.

**Directory stream locking is also handled by L3.** When L4 opens a directory stream for writing (to add/remove entries), L3's locking prevents concurrent modifications to that directory stream. This gives us directory-level concurrency safety for free.

## StreamsFromFiles Implementation Details

### Storage

- `StreamsFromFiles` struct holds:
  - `root: PathBuf` -- the directory on disk.
  - `block_index_width: u8` -- stored from create, read back from meta on open.
  - `block_size_shift: u8` -- stored from create, read back from meta on open.
  - `state: Mutex<StreamsState>` -- all mutable bookkeeping behind a single mutex.

- `StreamsState` contains:
  - `next_stream_id: u64` -- counter for allocating new stream IDs.
  - `next_handle_id: u64` -- counter for allocating handle IDs.
  - `locks: HashMap<u64, LockState>` -- per-stream lock state (reader count + has_writer flag).
  - `open_handles: HashMap<u64, HandleInfo>` -- tracking open handles and their associated stream ID.

### File naming

- Stream files: `{id}.stream` (e.g. `0.stream`, `1.stream`, `42.stream`).
- A `meta` file in the directory with format: `| block_index_width: u8 | block_size_shift: u8 | next_stream_id: u64 |` (10 bytes total, little-endian).

### Thread safety

All bookkeeping state is behind a single `Mutex`. File I/O uses **ephemeral file handles** (opened and closed on each read/write/truncate operation) so no file descriptors are stored in state. This avoids file handle lifetime issues across threads.

### Create flow

1. `StreamsFromFiles::create(path, block_index_width, block_size_shift)`:
   - Create directory at `path`.
   - Write `meta` file with the given params and `next_stream_id = 0`.
   - Return the instance.

2. L4 calls `create_stream()` for the root directory stream → gets ID 0 → `0.stream` is created (empty).

### Open flow

1. `StreamsFromFiles::open(path)`:
   - Verify directory exists.
   - Read `meta` file to get `block_index_width`, `block_size_shift`, and `next_stream_id`.
   - Return the instance.

### Stream operations

- `create_stream()`: Lock mutex, allocate next ID, create `{id}.stream` file, increment and persist `next_stream_id`, unlock.
- `open_stream(id, mode)`: Check `{id}.stream` exists, check/update lock state, allocate handle, return handle.
- `close_stream(handle)`: Look up handle info, update lock state, remove handle.
- `delete_stream(id)`: Verify not open (check lock state), remove `{id}.stream` file. ID is not reused.
- `read(handle, pos, buf)`: Open file ephemerally, seek to `pos`, read into `buf`, close file.
- `write(handle, pos, buf)`: Open file ephemerally, seek to `pos`, write from `buf`, close file.
- `stream_length(handle)`: Query file metadata for size.
- `truncate(handle, new_len)`: Open file ephemerally, truncate, close file.

## C ABI Impact

The C ABI layer (`stream_fs_c`) uses `SfsDefault` (which is `Sfs<StreamsFromFiles>`).

- `sfs_create(path, block_index_width, block_size_shift)` takes the runtime params.
- `sfs_open(path)` reads params from the meta file.
- All other functions continue to work through opaque handles.

## Python Wrapper

The Python wrapper wraps the C ABI into a Pythonic API:

```python
class Sfs:
    @staticmethod
    def create(path: str, block_index_width: int = 4, block_size_shift: int = 12) -> "Sfs"

    @staticmethod
    def open(path: str) -> "Sfs"
```

Default values (4, 12) give u32-equivalent block indices and 4096-byte blocks.

## Test Suite

- **49/49 tests passing (100%)** across all phases
- Phase 1 (Lifecycle): 5 tests - create, open, close
- Phase 2 (Directories): 12 tests - mkdir, rmdir, list, rename
- Phase 3 (Streams): 6 tests - create, delete, rename
- Phase 4 (Stream I/O): 11 tests - read, write, seek, tell, truncate
- Phase 5 (Locking): 7 tests - multi-reader, reader/writer blocking
- Phase 6 (Edge Cases): 5 tests - empty streams, deep nesting, spaces
- Phase 7 (Thread Safety): 2 tests - concurrent reads, shared-instance concurrent writes
  - Configurable burn duration via `--burn-seconds` (default 10s)
  - 40 threads per test, each looping for the burn duration

## Resolved Questions

1. **Stream ID reuse**: No -- IDs are monotonically increasing. This simplifies `StreamsFromFiles`. Real L3 (`StreamsFromBlocks`) will handle ID reuse through the Stream Descriptors mechanism described in the architecture doc.

2. **Root directory stream ID**: Yes, assume it's always 0 for now. The first `create_stream()` call returns 0. We will revisit this when implementing headers (L3 header contains the "Streams stream descriptor").

3. **Meta file format**: `| block_index_width: u8 | block_size_shift: u8 | next_stream_id: u64 |` (10 bytes, little-endian). Extended from original 4 bytes to include runtime block parameters.

4. **Error propagation**: Pass through and enrich. `SfsError` is used by both L3 and L4. L4 adds context where useful (e.g. "while resolving path 'textures/skin.png'") via the error message strings, but does not map error types.

5. **Block parameters as runtime values**: `block_index_width` and `block_size_shift` cannot be compile-time generics because a single binary must open SFS files with different parameters. They are stored in the meta file and passed as runtime values to `create()`, read back via getters after `open()`.

6. **Internal stream ID width**: All stream IDs are `u64` internally. On-disk serialization in directory entries uses only `block_index_width` bytes (zero-extending on read). This gives maximum flexibility without penalizing disk space.

---

## Implementation Status

### ✅ COMPLETE - L3 Mock Phase

**All components implemented and tested:**

1. **L3 Trait** (`stream_fs/src/stream_layer.rs`)
   - `StreamLayer` trait (no generics) with associated `Handle: Copy` type
   - `Send + Sync` bound for thread safety
   - Runtime `block_index_width` and `block_size_shift` via `create()` params and getters
   - All stream IDs are `u64`
   - Position-based read/write (L3 handles have no head position)
   - All methods take `&self` (interior mutability)
   - Locking enforced inside L3 (one writer OR many readers per stream)

2. **StreamsFromFiles** (`stream_fs/src/streams_from_files.rs`)
   - Each stream stored as `{id}.stream` file in a directory
   - `meta` file: `| block_index_width: u8 | block_size_shift: u8 | next_stream_id: u64 |` (10 bytes)
   - Per-stream lock state tracking (reader count + has_writer flag)
   - Ephemeral file handles (open/close on each operation) for thread safety
   - All bookkeeping behind a single `Mutex`

3. **L4 Rewrite** (`stream_fs/src/sfs.rs`)
   - `Sfs<L3: StreamLayer>` — generic over L3
   - Directory entry format: `| length: u16 | identifier: [u8; block_index_width] | name: [u8] |`
   - Variable-width serialization: writes `block_index_width` bytes per identifier
   - Path resolution by walking directory stream hierarchy
   - Root directory stream (ID 0) created automatically on `Sfs::create`
   - No path-level lock tracking — all locking delegated to L3
   - `SfsDefault` type alias: `Sfs<StreamsFromFiles>`
   - Thread-safe: `Mutex<SfsState>`, all methods take `&self`

4. **C ABI** (`stream_fs_c/src/lib.rs`)
   - `sfs_create(path, block_index_width, block_size_shift)` takes runtime params
   - All other C API functions unchanged
   - Uses `SfsDefault as Sfs`

5. **CLI** (`sfs_cl/src/main.rs`)
   - Updated to use `SfsDefault as Sfs`
   - `create` command uses defaults (4, 12)
   - All commands working as before
   - Recursive listing with `-r` flag

6. **Python Bindings** (`sfs_pytest/sfs/`)
   - `Sfs.create(path, block_index_width=4, block_size_shift=12)` with defaults
   - Updated ctypes signature for `sfs_create`

7. **Test Suite**
   - **49/49 tests passing (100%)**
   - All existing L4 Mock tests pass unchanged through the L3 layer
   - Thread safety burn-in tests (40 threads, configurable `--burn-seconds`)

**Next Phase:** L2 Mock — ✅ COMPLETE. See [L2 Mock](L2_mock.md).
**Current:** L1 — ✅ COMPLETE. See [L1](L1.md) for the single-file backend implementation.
