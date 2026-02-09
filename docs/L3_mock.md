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
  0.stream        ← root directory stream (created automatically)
  1.stream        ← e.g. data stream for "image.png"
  2.stream        ← e.g. directory stream for "textures/"
  3.stream        ← e.g. data stream for "textures/skin.png"
```

Each `.stream` file is a plain file on disk containing raw bytes -- either directory stream entries (for directory streams) or user data (for data streams).

This is inspectable with a regular file manager, though less intuitive than L4 mock's direct mapping.

## L3 Trait

The L3 trait defines the contract that L4 uses. Based on the architecture doc (L3 API section).

The trait is generic over two parameters that are forwarded from L2's concerns but must be visible at the L3 level:

- **`BlockIndex`**: The type used for block indices (e.g. `u16`, `u32`, `u64`). This determines how many blocks an SFS file can address. In `StreamsFromFiles` this is academic (no actual blocks), but the parameter must be present for when `StreamsFromBlocks` is implemented later.
- **`BLOCK_SIZE_SHIFT`**: A `u8` constant where the actual block size is `2^BLOCK_SIZE_SHIFT` bytes. For example, `12` means 4096-byte blocks (`2^12 = 4096`). Again academic for `StreamsFromFiles`, but required for the trait contract.

```rust
pub trait StreamLayer<BlockIndex, const BLOCK_SIZE_SHIFT: u8> {
    type Handle;

    /// Initialise L3 for a new SFS file at the given path.
    /// Creates the underlying storage (in StreamsFromFiles: a directory).
    /// Returns the L3 instance ready for use.
    fn create(path: &str) -> Result<Self, SfsError> where Self: Sized;

    /// Open an existing SFS file's L3 layer.
    fn open(path: &str) -> Result<Self, SfsError> where Self: Sized;

    /// Create a new stream. Returns the stream identifier (a number).
    fn create_stream(&mut self) -> Result<u32, SfsError>;

    /// Check whether a stream with the given identifier exists.
    fn stream_exists(&self, id: u32) -> bool;

    /// Open an existing stream by identifier, in the given mode.
    /// Returns a handle for subsequent read/write operations.
    fn open_stream(&mut self, id: u32, mode: OpenMode) -> Result<Self::Handle, SfsError>;

    /// Close a stream handle.
    fn close_stream(&mut self, handle: Self::Handle) -> Result<(), SfsError>;

    /// Delete a stream by identifier. Must not be currently open.
    fn delete_stream(&mut self, id: u32) -> Result<(), SfsError>;

    /// Read from a stream at the given position into buf.
    /// Returns the number of bytes actually read.
    fn read(&mut self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError>;

    /// Write to a stream at the given position.
    /// Extends the stream if writing past the end.
    /// Returns the number of bytes written.
    fn write(&mut self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, SfsError>;

    /// Get the total length of a stream in bytes, by handle.
    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, SfsError>;

    /// Truncate a stream to the given length.
    fn truncate(&mut self, handle: &Self::Handle, new_len: u64) -> Result<(), SfsError>;
}
```

### Key trait design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Handle type | Associated type `Self::Handle` | Allows each L3 impl to use its own handle type. `StreamsFromFiles` can use a simple struct; `StreamsFromBlocks` will differ. |
| Position in read/write | Caller passes `pos` explicitly | Architecture says L3 handles don't track head position -- that's L4's job. So read/write take a position parameter. |
| Block index type | Generic parameter `BlockIndex` | Don't hardcode `u32`. Different implementations may use `u16`, `u32`, or `u64`. Academic for `StreamsFromFiles` but needed for the trait contract. |
| Block size | Const generic `BLOCK_SIZE_SHIFT: u8` | Block size = `2^BLOCK_SIZE_SHIFT`. E.g. 12 → 4096 bytes. Academic for `StreamsFromFiles` but needed for the trait contract. |
| Stream IDs | `u32` | Plenty of streams for any practical use. Could be made generic later if needed. |
| Locking | Enforced inside L3 | One writer OR many readers per stream. L3 tracks this per stream ID internally. L4 does **not** duplicate locking -- see Locking Strategy below. |
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
| length: u16 | identifier: u32 | name: [u8] |
```

- `length` (u16): Total size of this entry in bytes, including the length field itself. So: `2 + 4 + name_bytes.len()`.
- `identifier` (u32): The L3 stream ID this entry points to.
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
pub struct Sfs<L3: StreamLayer<BlockIndex, BLOCK_SIZE_SHIFT>, BlockIndex, const BLOCK_SIZE_SHIFT: u8> {
    layer3: L3,
    root_dir_stream_id: u32,
    next_handle_id: u64,
    open_streams: HashMap<u64, OpenStreamInfo<L3::Handle>>,
}

struct OpenStreamInfo<H> {
    path: String,
    stream_id: u32,
    l3_handle: H,
    position: u64,
    mode: OpenMode,
}
```

L4 keeps its own handle system (returning `StreamHandle` to callers) and maps internally to L3 handles + position tracking. Note: **no `path_locks` map** -- L4 delegates all locking to L3 (see Locking Strategy below).

## Locking Strategy

**All locking lives in L3.** L4 does not maintain its own lock tracking.

L3 enforces per-stream locking by stream ID:
- One writer OR many readers per stream.
- `open_stream(id, Write)` fails if any reader or writer is already open on that stream.
- `open_stream(id, Read)` fails if a writer is already open on that stream.
- `delete_stream(id)` fails if the stream is currently open (any readers or writer).

L4 relies entirely on L3's locking. When L4 needs to check whether a stream is open (e.g. before deleting or renaming), it simply attempts the operation on L3 and propagates any `LockConflict` error.

This avoids duplicating lock state between layers. L3 is the single source of truth for lock state.

**Directory stream locking is also handled by L3.** When L4 opens a directory stream for writing (to add/remove entries), L3's locking prevents concurrent modifications to that directory stream. This gives us directory-level concurrency safety for free.

## StreamsFromFiles Implementation Details

### Storage

- `StreamsFromFiles` struct holds:
  - `root: PathBuf` -- the directory on disk.
  - `next_stream_id: u32` -- counter for allocating new stream IDs.
  - `locks: HashMap<u32, LockState>` -- per-stream lock state (reader count + has_writer flag).
  - `open_handles: HashMap<handle_id, OpenFileInfo>` -- tracking open file handles and their associated stream ID.

### File naming

- Stream files: `{id}.stream` (e.g. `0.stream`, `1.stream`, `42.stream`).
- A `meta` file in the directory storing `next_stream_id` as a little-endian u32.

### Thread safety for stream creation

The `meta` file must be exclusively locked (OS file lock) during `create_stream()` to prevent race conditions where two threads both read the same `next_stream_id`, create the same stream file, and corrupt state. The sequence is:

1. Open and exclusively lock the `meta` file.
2. Read `next_stream_id`.
3. Create the `{id}.stream` file.
4. Write `next_stream_id + 1` back to the `meta` file.
5. Release the lock.

### Create flow

1. `StreamsFromFiles::create(path)`:
   - Create directory at `path`.
   - Write `meta` file with `next_stream_id = 0`.
   - Return the instance.

2. L4 calls `create_stream()` for the root directory stream → gets ID 0 → `0.stream` is created (empty).

### Open flow

1. `StreamsFromFiles::open(path)`:
   - Verify directory exists.
   - Read `meta` file to get `next_stream_id`.
   - Return the instance.

### Stream operations

- `create_stream()`: Exclusively lock `meta`, allocate next ID, create `{id}.stream` file, increment and persist `next_stream_id`, unlock.
- `open_stream(id, mode)`: Open `{id}.stream`, check/update lock state, return handle.
- `close_stream(handle)`: Close file handle, update lock state.
- `delete_stream(id)`: Verify not open (check lock state), remove `{id}.stream` file. ID is not reused.
- `read(handle, pos, buf)`: Seek to `pos` in the file, read into `buf`.
- `write(handle, pos, buf)`: Seek to `pos` in the file, write from `buf`.
- `stream_length(handle)`: Return file size.
- `truncate(handle, new_len)`: Truncate the file.

## C ABI Impact

The C ABI layer (`stream_fs_c`) should remain largely unchanged from the caller's perspective. The public API signatures stay the same. Internally:

- `sfs_create` / `sfs_open` will instantiate `Sfs<StreamsFromFiles>` instead of `Sfs`.
- All other functions continue to work through opaque handles.

The C ABI will need to be compiled against a specific L3 type (monomorphized). For now, that's `StreamsFromFiles`.

## Python Wrapper / Test Impact

- The Python wrapper (`sfs_pytest/sfs/`) should require **no changes** -- it talks to the C ABI, which hasn't changed its public interface.
- **All 46 existing tests should continue to pass** after the refactor. This is our key validation: same behavior, different internal architecture.
- Additional tests may be added to verify L3-specific behavior (e.g. that `.stream` files appear on disk with expected numbering).

## Implementation Order

### Phase 1: L3 Trait Definition
1. Define the `StreamLayer` trait in a new file `stream_fs/src/stream_layer.rs`.
2. Export it from `lib.rs`.

### Phase 2: StreamsFromFiles Implementation
3. Implement `StreamsFromFiles` in `stream_fs/src/streams_from_files.rs`.
4. Unit test `StreamsFromFiles` directly in Rust (basic create/open/read/write/locking).

### Phase 3: Refactor L4 to Use L3
5. Make `Sfs` generic: `Sfs<L3: StreamLayer>`.
6. Implement directory stream entry serialization/deserialization in L4.
7. Implement path resolution by walking directory streams.
8. Rewrite all L4 operations (mkdir, rmdir, list, create_stream, open_stream, etc.) to use L3 instead of direct filesystem calls.
9. This is the biggest phase -- L4 is essentially rewritten, though the public API stays the same.

### Phase 4: Update C ABI
10. Update `stream_fs_c` to use `Sfs<StreamsFromFiles>`.
11. Verify it compiles and the public C API is unchanged.

### Phase 5: Run Existing Tests
12. Build the full stack (Rust → C ABI → Python).
13. Run all 46 existing pytest tests. They must all pass.
14. Fix any issues until green.

### Phase 6: Additional Tests (Optional)
15. Add tests verifying `.stream` files exist on disk with expected naming.
16. Add tests verifying directory stream content (stream entry format).
17. Add tests for concurrent stream creation (meta file locking).

## Resolved Questions

1. **Stream ID reuse**: No -- IDs are monotonically increasing. This simplifies `StreamsFromFiles`. Real L3 (`StreamsFromBlocks`) will handle ID reuse through the Stream Descriptors mechanism described in the architecture doc.

2. **Root directory stream ID**: Yes, assume it's always 0 for now. The first `create_stream()` call returns 0. We will revisit this when implementing headers (L3 header contains the "Streams stream descriptor").

3. **Meta file format**: Minimal -- just `next_stream_id` as a little-endian u32. Can be extended later if needed.

4. **Error propagation**: Pass through and enrich. `SfsError` is used by both L3 and L4. L4 adds context where useful (e.g. "while resolving path 'textures/skin.png'") via the error message strings, but does not map error types.

---

## Implementation Status

### ✅ COMPLETE - L3 Mock Phase

**All components implemented and tested:**

1. **L3 Trait** (`stream_fs/src/stream_layer.rs`)
   - `StreamLayer<BlockIndex, BLOCK_SIZE_SHIFT>` trait with associated `Handle: Copy` type
   - Generic over block index type and block size (2^BLOCK_SIZE_SHIFT)
   - Position-based read/write (L3 handles have no head position)
   - Locking enforced inside L3 (one writer OR many readers per stream)

2. **StreamsFromFiles** (`stream_fs/src/streams_from_files.rs`)
   - Each stream stored as `{id}.stream` file in a directory
   - `meta` file tracks next available stream ID (little-endian u32)
   - Per-stream lock state tracking (reader count + has_writer flag)
   - Implements `StreamLayer` for any `BlockIndex` and `BLOCK_SIZE_SHIFT`

3. **L4 Rewrite** (`stream_fs/src/sfs.rs`)
   - `Sfs<L3, BlockIndex, BLOCK_SIZE_SHIFT>` — fully generic over L3
   - Directory streams with serialized stream entries: `| length: u16 | identifier: u32 | name: [u8] |`
   - Path resolution by walking directory stream hierarchy
   - Root directory stream (ID 0) created automatically on `Sfs::create`
   - No path-level lock tracking — all locking delegated to L3
   - `SfsDefault` type alias: `Sfs<StreamsFromFiles, u32, 12>`

4. **C ABI** (`stream_fs_c/src/lib.rs`)
   - Updated to use `SfsDefault as Sfs`
   - Public C API unchanged — callers see no difference
   - Directory operations now use `&mut` internally (transparent to C callers)

5. **CLI** (`sfs_cl/src/main.rs`)
   - Updated to use `SfsDefault as Sfs`
   - All commands working as before

6. **Test Suite**
   - **47/47 tests passing (100%)** — all existing tests pass unchanged
   - Python wrapper required no changes (C ABI interface unchanged)
