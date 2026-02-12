# L2 Mock: Design & Decisions

This document captures the design decisions and implementation plan for the L2 Mock phase of SFS.

## Goal

Introduce an L2 trait (the block storage abstraction) and make L3 generic over it. Implement:

1. **BlockLayer trait** — the L2 contract that L3 uses.
2. **BlocksFromFiles** — an L2 mock that stores each block as a numbered file on disk (`n.block`).
3. **StreamsFromBlocks** — the real L3 implementation that links blocks together into numbered streams using the pyramid data structure described in the architecture.

This forces L3 to become a **real stream abstraction on top of numbered blocks**: stream descriptors, the Streams stream, pyramid block linking, redirector blocks — all as described in the architecture doc.

In the L3 Mock phase, the filesystem did the heavy lifting (each stream was a real file). Now L3 must do that work itself, using only numbered blocks from L2.

## What Changes from L3 Mock

| Aspect | L3 Mock (before) | L2 Mock (after) |
|---|---|---|
| Stream storage | Each stream is a real file (`n.stream`) | L3 links blocks from L2 into streams using pyramid structure |
| Stream metadata | `meta` file tracks `next_stream_id` | Stream Descriptors stored in the Streams stream; out-of-band descriptor for the Streams stream itself |
| Block management | N/A (no blocks) | L2 allocates/frees blocks; L3 builds streams from them |
| Stream growth | File system extends the `.stream` file | L3 allocates new blocks from L2, updates redirector hierarchy |
| Stream truncation | File system truncates the `.stream` file | L3 deallocates unneeded blocks, shrinks pyramid |
| Thread safety | Single Mutex over all bookkeeping | RWLock on Streams stream + Mutex for bookkeeping + per-stream locking |
| On-disk layout | `n.stream` files in a directory | `n.block` files in a directory (L2 mock) |

## SFS Header Format

The SFS header identifies the file and stores each layer's metadata. In the real architecture this occupies the start of a single file; for the mock it is stored as a `header` file in the directory.

### Magic and layout version

```
| "stream_fs" (9 bytes, UTF-8) | layout_version: u8 |
```

`layout_version` is `0` for this version. The magic allows any tool to identify an SFS file. The layout version tells the reader how to parse the subsequent layer headers (in version 0: each layer header is preceded by a u16 length).

### Layer header format

Each layer writes a header section. Per the architecture, the length field includes its own size:

```
| length: u16 | identifier: [u8; 6] | version: u8 | layer-specific data... |
```

- `length` (u16, little-endian): Total size of this layer section in bytes, **including** the 2-byte length field itself. Minimum value is 9 (2 for length + 6 for identifier + 1 for version).
- `identifier` (6 bytes, UTF-8): Identifies which layer implementation wrote this section.
- `version` (u8): Version of this layer's header format. Currently `0` for all layers.

### Layer identifiers

| Layer | Identifier | Description |
|-------|------------|-------------|
| L4 | `"filing"` | Filing abstraction |
| L3 | `"pyra  "` | Pyramid-based stream layer (two trailing spaces) |
| L3 | `"strfil"` | StreamsFromFiles mock L3 (file-backed streams) |
| L2 | `"blocks"` | Block storage layer (real) |
| L2 | `"blkfil"` | BlocksFromFiles mock L2 (file-backed blocks) |
| L1 | `"ondisk"` | File system abstraction (✅ implemented) |

### Complete header layout (L2 Mock, no L1)

```
Offset  Size  Content
------  ----  -------
 0       9    "stream_fs" (magic)
 9       1    0x00 (layout version 0)
10       2    L2 length = 11
12       6    "blkfil"
18       1    version (0x00)
19       1    block_size_shift (u8)
20       1    block_index_width (u8)
21       2    L3 length = 33
23       6    "pyra  "
29       1    version (0x01)
30       8    streams_size (u64, little-endian)
38       8    streams_top_block (u64, little-endian)
46       8    streams_reserved (u64, little-endian)
54       2    L4 length = 17
56       6    "filing"
62       1    version (0x00)
63       8    root_dir_stream_id (u64, little-endian)
------  ----
Total: 71 bytes
```

The version bytes in each layer section identify the format version of that layer's header data. L3 uses version `0x01` (descriptor includes `reserved` field); other layers use version `0x00`. Readers should verify the version byte and reject unknown versions.

**Note:** L1 is not present in the mock. When L1 is implemented, it will take over writing the magic and prepend its own section before L2's. The header layout version may be incremented if the format changes.

### Verification on open

Each layer verifies its own identifier when reading its header section:
- L2 verifies the magic bytes and `"blkfil"` identifier.
- L3 verifies `"pyra  "` identifier.
- L4 verifies `"filing"` identifier.

If any identifier doesn't match, it's an error (incompatible layer implementation or corrupted file).

## On-Disk Layout (L2 Mock)

When a caller creates an SFS file at path `foo.sfs`, L2 mock creates a directory `foo.sfs/` containing block files:

```
foo.sfs/
  header          <- SFS header (magic + L2/L3/L4 layer headers, 63 bytes)
  meta            <- L2 runtime state (next_block_id only)
  0.block         <- block 0 (e.g. data block for root directory stream)
  1.block         <- block 1
  2.block         <- block 2
  ...
```

Each `.block` file is exactly `block_size` bytes (padded with zeros if necessary). This ensures consistent block sizing, matching the behaviour of a real L2 implementation where blocks are fixed-size regions of a file.

## BlockLayer Trait (L2 API)

The L2 trait defines the contract that L3 uses. Based on the architecture doc (L2 API section).

```rust
pub trait BlockLayer: Send + Sync {
    /// Create a new L2 storage at the given path.
    ///
    /// `block_size_shift` is the power-of-2 exponent for block size
    /// (e.g. 12 -> 4096 bytes).
    /// `block_index_width` is the number of bytes used for block indices
    /// on disk (e.g. 2, 4, or 8).
    fn create(path: &str, block_size_shift: u8, block_index_width: u8) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Open an existing L2 storage at the given path.
    fn open(path: &str) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Block size in bytes (convenience: `1 << block_size_shift`).
    fn block_size(&self) -> usize;

    /// Block size as a power of 2 (e.g. 12 -> 4096 bytes).
    fn block_size_shift(&self) -> u8;

    /// The number of bytes used for block indices on disk.
    fn block_index_width(&self) -> u8;

    /// Allocate a new block. Returns the block index.
    /// The block contents are zeroed.
    /// Fails if the next block index would exceed the maximum representable
    /// value for `block_index_width` (see Index Overflow Protection).
    fn allocate_block(&self) -> Result<u64, SfsError>;

    /// Deallocate a block, returning it for future reuse.
    fn deallocate_block(&self, index: u64) -> Result<(), SfsError>;

    /// Read from a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// Returns the number of bytes actually read.
    fn read_block(&self, index: u64, offset: usize, buf: &mut [u8]) -> Result<usize, SfsError>;

    /// Write to a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// Returns the number of bytes actually written.
    fn write_block(&self, index: u64, offset: usize, buf: &[u8]) -> Result<usize, SfsError>;

    /// Store the upper layers' header sections (L3 + L4, each with their own
    /// length/identifier prefix). L2 prepends the magic and its own section,
    /// then writes the full SFS header to disk.
    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError>;

    /// Load the upper layers' header sections.
    /// L2 reads the full header from disk, verifies the magic and its own
    /// section, then returns the remainder (L3 + L4 sections).
    fn load_header(&self) -> Result<Vec<u8>, SfsError>;
}
```

### Key trait design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Partial block I/O | `read_block`/`write_block` accept `offset` | Avoids read-modify-write for partial updates. L3 can efficiently read individual indices from redirector blocks and write partial data blocks. |
| Block size enforcement | `offset + buf.len() <= block_size` | L2 enforces block boundaries. L3 must break cross-block operations into per-block calls. |
| Zeroed allocation | `allocate_block()` returns zeroed block | Ensures predictable state for new blocks. Important for redirector blocks where unwritten slots must be distinguishable from valid indices. |
| Header API | `store_header()`/`load_header()` | Allows L3 to persist its metadata through L2, following the architecture's model of layered headers. |
| Thread safety | `Send + Sync`, `&self` on all methods | Same pattern as StreamLayer. Interior mutability via Mutex. |
| Block index type | `u64` internally | Same as stream IDs. On-disk serialization in redirector blocks uses `block_index_width` bytes. |
| Sentinel value | `u64::MAX` (all 0xFF bytes) | Reserved block index meaning "no block" / "unused". L2 must never allocate this index. Used in free lists, redirector blocks, and stream descriptors. |
| Index overflow protection | L2 enforces max allocatable index | `allocate_block()` fails if the next index would reach or exceed the sentinel for the configured `block_index_width`. Prevents silent truncation when indices are serialized in redirector blocks. |

### Index Overflow Protection

Since block indices are stored on disk using `block_index_width` bytes (in redirector blocks, free lists, and L4 directory entries), the system must guard against indices that exceed what can be represented.

**Maximum values:**
```
sentinel(w)  = (1 << (w * 8)) - 1    // all 0xFF bytes; reserved as "no block"
max_index(w) = (1 << (w * 8)) - 2    // highest allocatable block index

Examples:
  block_index_width=2: sentinel=0xFFFF (65535), max_index=65534
  block_index_width=4: sentinel=0xFFFFFFFF,     max_index=4294967294
  block_index_width=8: sentinel=u64::MAX,       max_index=u64::MAX - 1
```

**Where enforcement happens:**
- **L2 (`allocate_block`)**: Must fail with an error if the next block index would be >= `sentinel(block_index_width)`. This is the primary guard — if L2 never hands out an unrepresentable index, downstream layers are safe.
- **L3 (`create_stream`)**: Stream IDs are serialized by L4 in `block_index_width` bytes (in directory entries). L3 must also refuse to create a stream if the stream ID would be >= `sentinel(block_index_width)`. This prevents L4 from silently truncating a stream ID that doesn't fit.

**Note:** For `block_index_width=8`, the sentinel is `u64::MAX` and the max index is `u64::MAX - 1`. In practice, reaching this limit is impossible (it would require allocating 2^64 - 1 blocks), but the guard is still implemented for correctness.

## BlocksFromFiles Implementation

### Design philosophy

Like `StreamsFromFiles`, this is a simple mock. It stores each block as a real file on disk. No free list, no block reuse. Block IDs increase monotonically. Deallocated blocks have their files deleted.

### Storage

- `BlocksFromFiles` struct holds:
  - `root: PathBuf` — the directory on disk.
  - `block_size_shift: u8` — read from the L2 header section on open.
  - `block_index_width: u8` — read from the L2 header section on open.
  - `state: Mutex<BlocksState>` — all mutable bookkeeping behind a single mutex.

- `BlocksState` contains:
  - `next_block_id: u64` — counter for allocating new block IDs.

### File naming

- Block files: `{index}.block` (e.g. `0.block`, `1.block`, `42.block`).
- `header` file: Full SFS header (magic + L2/L3/L4 layer sections). See "SFS Header Format" above.
- `meta` file: `| next_block_id: u64 |` (8 bytes, little-endian). Runtime state only — block parameters live in the header.

### Thread safety

All bookkeeping state is behind a single `Mutex`. File I/O uses ephemeral file handles (same pattern as `StreamsFromFiles`).

### Key operations

- `allocate_block()`: Lock mutex, **check that `next_block_id < sentinel(block_index_width)`** (fail with error if not), get next ID, create `{id}.block` (zeroed, exactly `block_size` bytes), increment and persist `next_block_id`, unlock, return ID.
- `deallocate_block(index)`: Verify file exists, delete `{index}.block`. No reuse.
- `read_block(index, offset, buf)`: Open file, seek to `offset`, read into `buf`, close.
- `write_block(index, offset, buf)`: Open file, seek to `offset`, write from `buf`, close.
- `store_header(upper_layers_data)`: Build the full SFS header (magic + L2 section + `upper_layers_data`) and write to the `header` file.
- `load_header()`: Read the `header` file, verify magic and `"blkfil"` identifier, extract L2 params, return the remaining bytes (L3 + L4 sections) to the caller.

---

## StreamsFromBlocks Implementation

This is the **real L3 implementation** that follows the architecture. It links blocks from L2 into numbered streams using the pyramid data structure.

### Stream Descriptor

Each stream is described by a fixed-size descriptor:

```
| size: u64 (8 bytes) | top_block: u64 (8 bytes) |
```

Total: **16 bytes** per descriptor.

- `size`: The total length of stream data in bytes.
- `top_block`: The block index of the top block in the pyramid.

Special values:
- `size = 0, top_block = 0`: Valid empty stream (no blocks allocated).
- `top_block = u64::MAX`: Stream descriptor is **free** (available for reuse).

Using `u64` for both fields gives clean random access: stream N's descriptor is at byte offset `N * 16` in the Streams stream.

### The Streams Stream

All stream descriptors are stored in a single "Streams stream". This stream is itself composed of blocks linked via the pyramid structure. Its own descriptor (the **out-of-band descriptor**) is persisted as part of the header chain (see "Header Chain" section below). L3 combines its own header with L4's header and passes the combined blob down to L2 via `BlockLayer::store_header()`.

### Stream IDs

A stream ID is an index into the Streams stream: stream N's descriptor is at offset `N * 16`.

The number of stream slots = `streams_descriptor.size / 16`.

When creating a new stream:
1. Scan the Streams stream for a free descriptor (`top_block == u64::MAX`).
2. If no free slot found, **check that the new stream ID would be < `sentinel(block_index_width)`** (fail with error if not — L4 serializes stream IDs in `block_index_width` bytes). Then extend the Streams stream by 16 bytes (appending a new descriptor).
3. Write the new descriptor (`size = 0, top_block = 0`).
4. Return the stream ID.

When deleting a stream:
1. Deallocate all blocks belonging to the stream (walking the pyramid).
2. Mark the descriptor as free (`top_block = u64::MAX`).

### Pyramid Block Linking

Blocks are linked together to form a data stream. The architecture's pyramid structure is used:

**Constants:**
```
block_size = 1 << block_size_shift
fan_out = block_size / block_index_width    // entries per redirector block
```

**Depth calculation:**
```
data_blocks_needed = ceil(size / block_size)     [0 if size == 0]

depth 0: data_blocks <= 1           (top IS the data block)
depth 1: data_blocks <= fan_out     (top is redirector -> data blocks)
depth 2: data_blocks <= fan_out^2   (top -> redirectors -> data blocks)
depth d: data_blocks <= fan_out^d
```

**Example** (block_size=4096, block_index_width=4, fan_out=1024):
- Depth 0: up to 4 KB per stream
- Depth 1: up to 4 MB per stream
- Depth 2: up to 4 GB per stream
- Depth 3: up to 4 TB per stream

**Navigating to byte position `pos`:**
```
data_block_idx = pos / block_size
offset_in_block = pos % block_size

current_block = top_block
for level in (depth-1 downto 0):
    span = fan_out^level          // each slot at this level covers `span` data blocks
    slot = data_block_idx / span
    data_block_idx = data_block_idx % span

    // Read block index at slot position from current redirector
    idx_offset = slot * block_index_width
    current_block = read_block_index(current_block, idx_offset)

// current_block is the data block
// Read/write at offset_in_block
```

**Block index serialization in redirector blocks:**

Block indices in redirector blocks use `block_index_width` bytes, little-endian, zero-extended to u64 on read. This maximizes fan-out (entries per redirector block).

Unused slots in redirector blocks contain `0xFF` bytes (all ones for the width), representing the `INVALID_BLOCK` sentinel.

### Growing a Stream

When a write extends beyond the current stream size:

1. Calculate `old_data_blocks` and `new_data_blocks`.
2. Calculate `old_depth` and `new_depth`.
3. **If depth increases**: Allocate a new redirector block, write the old `top_block` as its first entry, update `top_block` to the new redirector. Repeat if depth increases by more than 1.
4. **Allocate new data blocks**: For each new data block needed, allocate from L2 and write its index into the appropriate redirector slot.
5. **Write data** to the (possibly new) data blocks.
6. **Update the cached descriptor** (new `size` and possibly new `top_block`).

### Shrinking a Stream (Truncation)

When a stream is truncated to a smaller size:

1. Calculate `new_data_blocks` from `new_size`.
2. **Deallocate unneeded blocks**: Walk the pyramid and deallocate data blocks beyond `new_data_blocks`, then deallocate any redirector blocks that are now empty.
3. **If depth decreases**: The top block may collapse. If the top redirector has only one child, replace `top_block` with that child and deallocate the redirector.
4. **Update the cached descriptor** (new `size` and possibly new `top_block`).

Special case: truncating to 0 deallocates ALL blocks (data + redirectors). Descriptor becomes `(size=0, top_block=0)`.

### StreamsFromBlocks Internal Structure

```rust
pub struct StreamsFromBlocks<L2: BlockLayer> {
    layer2: L2,

    // Out-of-band descriptor for the Streams stream, protected by RWLock.
    // Write lock: creating/deleting streams, flushing descriptors.
    // Read lock: reading descriptors (opening streams).
    streams_lock: RwLock<StreamDescriptor>,

    // Cached L4 header blob (loaded during open, updated via store_header).
    l4_header_cache: Mutex<Vec<u8>>,

    // Bookkeeping state: per-stream locks, open handles.
    state: Mutex<StreamsFromBlocksState>,
}

struct StreamDescriptor {
    size: u64,
    top_block: u64,
}

struct StreamsFromBlocksState {
    next_handle_id: u64,
    locks: HashMap<u64, LockState>,
    open_handles: HashMap<u64, OpenHandleInfo>,
}

struct OpenHandleInfo {
    stream_id: u64,
    mode: OpenMode,
    cached_descriptor: StreamDescriptor,  // cached for duration of open
}
```

### Thread Safety

Thread safety follows the architecture's intended approach:

**1. RWLock on the Streams stream:**
- **Write lock** for:
  - Creating a new stream (scanning/extending the Streams stream)
  - Deleting a stream (marking descriptor as free)
  - Flushing a cached descriptor on close (writing updated size/top_block)
- **Read lock** for:
  - Opening a stream (reading its descriptor from the Streams stream)
  - Checking if a stream exists

The out-of-band descriptor (for the Streams stream itself) is stored inside the `RwLock<StreamDescriptor>`. When the Streams stream grows (new descriptors added), its own descriptor is updated while holding the write lock.

**2. Mutex for bookkeeping:**
- Per-stream lock state (readers/writers count)
- Open handle tracking
- Handle ID allocation

**3. Per-stream data protection:**
- Enforced by the one-writer-OR-many-readers locking model (same as before).
- A writer caches the stream descriptor on open and flushes on close.
- Readers cache the descriptor on open (it cannot change while readers exist, since there's no writer).

**4. Block allocation (L2):**
- Protected by L2's own internal Mutex (within BlocksFromFiles).
- Multiple threads can allocate/deallocate blocks concurrently (through L2's mutex).

**Flow: Creating a stream**
1. Acquire `streams_lock` write lock.
2. Scan Streams stream for a free descriptor slot (or extend).
3. Write new descriptor (`size=0, top_block=0`) to Streams stream.
4. Persist the combined header (L3 out-of-band descriptor + cached L4 header) via `L2::store_header()`.
5. Release write lock.
6. Lock `state` mutex, allocate handle ID, return.

**Flow: Opening a stream**
1. Lock `state` mutex, check/update per-stream lock state, allocate handle.
2. Acquire `streams_lock` read lock.
3. Read the stream's descriptor from the Streams stream.
4. Cache the descriptor in the handle info.
5. Release read lock.
6. Return handle.

**Flow: Writing to a stream**
1. Lock `state` mutex briefly to get `stream_id` and `cached_descriptor`. Unlock.
2. Navigate pyramid, allocate blocks from L2, write data blocks.
3. Lock `state` mutex briefly to update `cached_descriptor` (new size, possibly new top_block). Unlock.
4. No Streams stream access needed during data writes.

**Flow: Closing a stream (writer)**
1. Lock `state` mutex, extract cached descriptor and handle info. Unlock.
2. Acquire `streams_lock` write lock.
3. Write the cached descriptor back to the Streams stream.
4. Persist the combined header (L3 out-of-band descriptor + cached L4 header) via `L2::store_header()`.
5. Release write lock.
6. Lock `state` mutex, update per-stream lock state, remove handle. Unlock.

**Flow: Closing a stream (reader)**
1. Lock `state` mutex, update per-stream lock state, remove handle. Unlock.
2. No Streams stream access needed (descriptor is unchanged).

### Implementing StreamLayer

`StreamsFromBlocks<L2: BlockLayer>` implements `StreamLayer`:

```rust
impl<L2: BlockLayer> StreamLayer for StreamsFromBlocks<L2> {
    type Handle = BlockStreamHandle;

    fn create(path: &str, block_index_width: u8, block_size_shift: u8) -> Result<Self, SfsError> {
        let layer2 = L2::create(path, block_size_shift, block_index_width)?;
        let descriptor = StreamDescriptor { size: 0, top_block: 0 };
        // Persist initial header (L3 section only, no L4 section yet)
        let l3_section = build_l3_section(descriptor.size, descriptor.top_block);
        layer2.store_header(&l3_section)?;
        Ok(StreamsFromBlocks { layer2, streams_lock: RwLock::new(descriptor), ... })
    }

    fn open(path: &str) -> Result<Self, SfsError> {
        let layer2 = L2::open(path)?;
        let upper_sections = layer2.load_header()?;  // L3 section + L4 section
        // Parse L3 section: read length, verify "pyra  " identifier, extract descriptor
        let l3_len = u16::from_le_bytes([upper_sections[0], upper_sections[1]]) as usize;
        let descriptor = parse_l3_section(&upper_sections[..l3_len])?;
        // Cache the L4 section (everything after L3's section)
        let l4_section = upper_sections[l3_len..].to_vec();
        Ok(StreamsFromBlocks { layer2, streams_lock: RwLock::new(descriptor), l4_header_cache: l4_section, ... })
    }

    fn block_index_width(&self) -> u8 { self.layer2.block_index_width() }
    fn block_size_shift(&self) -> u8 { self.layer2.block_size_shift() }

    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
        let streams_desc = self.streams_lock.read();
        let l3_section = build_l3_section(streams_desc.size, streams_desc.top_block);
        let mut combined = l3_section;
        combined.extend_from_slice(upper_layers);  // L4 section
        self.layer2.store_header(&combined)
    }

    fn load_header(&self) -> Result<Vec<u8>, SfsError> {
        // Return the L4 section cached during open()
        Ok(self.l4_header_cache.lock().unwrap().clone())
    }

    // ... remaining methods delegate to internal pyramid logic
}
```

### Internal Helper: Pyramid I/O

The pyramid navigation logic is shared between regular streams and the Streams stream. A set of internal methods operate on a `StreamDescriptor` + `&L2`:

```rust
// Read `buf.len()` bytes from a stream at position `pos`.
fn pyramid_read(layer2: &L2, descriptor: &StreamDescriptor, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError>

// Write `buf.len()` bytes to a stream at position `pos`.
// May allocate new blocks and grow the pyramid. Updates the descriptor.
fn pyramid_write(layer2: &L2, descriptor: &mut StreamDescriptor, pos: u64, buf: &[u8]) -> Result<usize, SfsError>

// Truncate a stream to `new_len` bytes.
// Deallocates unneeded blocks and shrinks the pyramid. Updates the descriptor.
fn pyramid_truncate(layer2: &L2, descriptor: &mut StreamDescriptor, new_len: u64) -> Result<(), SfsError>
```

These operate on any stream (regular or Streams stream) given its descriptor and a reference to L2.

---

## Header Chain: L4 -> L3 -> L2 -> Disk

Following the architecture, each layer persists its metadata by passing it down to the layer below. Each layer formats its own header section (with length prefix and identifier) and the layer below concatenates it with the upper layers' sections.

```
L4 formats its section and calls L3::store_header(L4_section)
L3 formats its section, prepends it, and calls L2::store_header(L3_section + L4_section)
L2 prepends magic + L2 section and writes the full header to disk
```

On load, the reverse: each layer strips its own section from the front and passes the remainder up.

### StreamLayer trait additions

`StreamLayer` gains the same header API as `BlockLayer`:

```rust
pub trait StreamLayer: Send + Sync {
    // ... existing methods ...

    /// Store header sections for this layer and all layers above.
    /// `upper_layers` contains the already-formatted header sections from
    /// the layer(s) above (each with their own length/identifier prefix).
    /// The implementation prepends its own section and passes everything
    /// down to the layer below.
    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError>;

    /// Load header sections for the layers above this one.
    /// Returns the concatenated header sections that were passed to
    /// `store_header()` — i.e. everything except this layer's own section.
    fn load_header(&self) -> Result<Vec<u8>, SfsError>;
}
```

### How each layer formats its section

Each layer builds: `| length: u16 | identifier: [u8; 6] | version: u8 | layer-specific data |`

**L4 ("filing"):**
```rust
fn build_l4_section(root_dir_stream_id: u64) -> Vec<u8> {
    let data_len = 8;  // root_dir_stream_id
    let total_len: u16 = 2 + 6 + 1 + data_len;  // = 17
    let mut buf = Vec::new();
    buf.extend_from_slice(&total_len.to_le_bytes());
    buf.extend_from_slice(b"filing");
    buf.push(0);  // version
    buf.extend_from_slice(&root_dir_stream_id.to_le_bytes());
    buf  // 17 bytes
}
```

**L3 ("pyra  "):**
```rust
fn build_l3_section(streams_size: u64, streams_top_block: u64) -> Vec<u8> {
    let data_len = 8 + 8;  // streams_size + streams_top_block
    let total_len: u16 = 2 + 6 + 1 + data_len;  // = 25
    let mut buf = Vec::new();
    buf.extend_from_slice(&total_len.to_le_bytes());
    buf.extend_from_slice(b"pyra  ");
    buf.push(0);  // version
    buf.extend_from_slice(&streams_size.to_le_bytes());
    buf.extend_from_slice(&streams_top_block.to_le_bytes());
    buf  // 25 bytes
}
```

**L2 ("blkfil"):**
```rust
fn build_l2_section(block_size_shift: u8, block_index_width: u8) -> Vec<u8> {
    let data_len = 1 + 1;  // block_size_shift + block_index_width
    let total_len: u16 = 2 + 6 + 1 + data_len;  // = 11
    let mut buf = Vec::new();
    buf.extend_from_slice(&total_len.to_le_bytes());
    buf.extend_from_slice(b"blkfil");
    buf.push(0);  // version
    buf.push(block_size_shift);
    buf.push(block_index_width);
    buf  // 11 bytes
}
```

### store_header / load_header at each layer

**L2 (BlocksFromFiles):**

```rust
fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
    let mut header = Vec::new();
    // Magic
    header.extend_from_slice(b"stream_fs");
    header.push(0);  // layout version 0
    // L2 section
    header.extend_from_slice(&build_l2_section(self.block_size_shift, self.block_index_width));
    // L3 + L4 sections (passed through from above)
    header.extend_from_slice(upper_layers);
    fs::write(self.root.join("header"), &header)
}

fn load_header(&self) -> Result<Vec<u8>, SfsError> {
    let data = fs::read(self.root.join("header"))?;
    // Verify magic (bytes 0..9 == "stream_fs", byte 9 == 0)
    // Read L2 section length at offset 10, skip L2 section
    // Return remainder (L3 + L4 sections)
    let l2_len = u16::from_le_bytes([data[10], data[11]]) as usize;
    Ok(data[10 + l2_len ..].to_vec())
}
```

**L3 (StreamsFromBlocks):**

```rust
fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
    let streams_desc = self.streams_lock.read();
    let l3_section = build_l3_section(streams_desc.size, streams_desc.top_block);

    let mut combined = Vec::new();
    combined.extend_from_slice(&l3_section);
    combined.extend_from_slice(upper_layers);  // L4 section

    self.layer2.store_header(&combined)
}

fn load_header(&self) -> Result<Vec<u8>, SfsError> {
    // L3's own section was already parsed during open().
    // Return only the L4 section (cached during open).
    Ok(self.l4_header_cache.lock().unwrap().clone())
}
```

During `StreamsFromBlocks::open()`:
1. Call `L2::load_header()` to get L3 + L4 sections.
2. Read L3 section length, verify `"pyra  "` identifier, extract Streams descriptor.
3. The remaining bytes after L3's section are the L4 section — cache them.
4. When L4 calls `L3::load_header()`, return the cached L4 section.

**L3 (StreamsFromFiles):**

`StreamsFromFiles` is the bottom layer in the file-backed path (no L2 beneath it). It writes the full header file itself, using its own L3-mock identifier and storing L4's section:

```rust
fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
    let mut header = Vec::new();
    // Magic
    header.extend_from_slice(b"stream_fs");
    header.push(0);  // layout version 0
    // L3-mock section (StreamsFromFiles uses different params than "pyra  ")
    // For StreamsFromFiles we write a minimal L3 section with its own identifier
    let l3_len: u16 = 2 + 6 + 1 + 8;  // = 17 (next_stream_id)
    header.extend_from_slice(&l3_len.to_le_bytes());
    header.extend_from_slice(b"strfil");  // StreamsFromFiles identifier
    header.push(0);  // version
    header.extend_from_slice(&self.next_stream_id().to_le_bytes());
    // L4 section
    header.extend_from_slice(upper_layers);
    fs::write(self.root.join("header"), &header)
}

fn load_header(&self) -> Result<Vec<u8>, SfsError> {
    let path = self.root.join("header");
    if !path.exists() { return Ok(Vec::new()); }
    let data = fs::read(&path)?;
    // Skip magic (10 bytes), read L3 section length, skip L3 section
    let l3_len = u16::from_le_bytes([data[10], data[11]]) as usize;
    Ok(data[10 + l3_len ..].to_vec())
}
```

### L4 (Sfs) usage

L4 formats its own section and passes it down. On open, L4 receives its section back and parses it:

```rust
impl<L3: StreamLayer> Sfs<L3> {
    pub fn create(path: &str, biw: u8, bss: u8) -> Result<Self, SfsError> {
        let layer3 = L3::create(path, biw, bss)?;
        let root_id = layer3.create_stream()?;

        // Build and persist L4 header section
        let l4_section = build_l4_section(root_id);
        layer3.store_header(&l4_section)?;

        Ok(Sfs { layer3, root_dir_stream_id: root_id, ... })
    }

    pub fn open(path: &str) -> Result<Self, SfsError> {
        let layer3 = L3::open(path)?;

        // Restore L4 header from L3
        let l4_section = layer3.load_header()?;
        // Parse: skip length (2) + identifier (6) + version (1) = 9 bytes
        // Verify identifier == "filing"
        let root_id = u64::from_le_bytes(l4_section[9..17].try_into().unwrap());

        Ok(Sfs { layer3, root_dir_stream_id: root_id, ... })
    }
}
```

This replaces the current hardcoded `root_dir_stream_id: 0` in `Sfs::open()`.

---

## Changes to Existing Code

### stream_fs/src/lib.rs

```rust
mod block_layer;          // NEW
mod blocks_from_files;    // NEW
mod streams_from_blocks;  // NEW
mod sfs;
mod stream_layer;
mod streams_from_files;

pub use block_layer::BlockLayer;
pub use blocks_from_files::BlocksFromFiles;
pub use streams_from_blocks::StreamsFromBlocks;
pub use sfs::{DirEntry, EntryType, OpenMode, Sfs, SfsError, StreamHandle};
pub use stream_layer::StreamLayer;
pub use streams_from_files::StreamsFromFiles;

/// Default SFS configuration: block-backed streams.
pub type SfsDefault = Sfs<StreamsFromBlocks<BlocksFromFiles>>;

/// File-backed streams (debugging/testing tool).
pub type SfsFileBacked = Sfs<StreamsFromFiles>;
```

### stream_fs/src/stream_layer.rs

Add `store_header()` and `load_header()` methods to the `StreamLayer` trait.

### stream_fs/src/streams_from_files.rs

Add `store_header()` and `load_header()` implementations. Stores upper header as a `header` file in the directory.

### stream_fs/src/sfs.rs

- `Sfs::create()`: Call `L3::store_header()` after creating root directory stream.
- `Sfs::open()`: Call `L3::load_header()` to restore `root_dir_stream_id` instead of hardcoding 0.

### stream_fs_c and sfs_cl

No code changes needed. They use `SfsDefault`, which transparently changes to the new stack.

### sfs_pytest

No Python wrapper changes needed. The C ABI is unchanged.

---

## Testing Strategy

### TDD Approach

Following the project's TDD methodology:

1. **Write new Python tests first** that exercise multi-block scenarios.
2. **Watch them fail** (initially with `StreamsFromFiles`, which won't exercise blocks, but after switching `SfsDefault` they exercise `StreamsFromBlocks`).
3. **Implement** `BlockLayer`, `BlocksFromFiles`, `StreamsFromBlocks`.
4. **Switch `SfsDefault`** and run all tests.

### New Python Tests (Multi-block scenarios)

Use **small block sizes** (`block_size_shift=6` -> 64-byte blocks, `block_index_width=4` -> fan_out=16) to exercise the pyramid with small test data.

**Phase 8: Block-level behaviour** (new tests):
- Write data larger than one block (e.g., 200 bytes with 64-byte blocks -> 4 data blocks, depth 1)
- Write data requiring depth 2 (e.g., >1024 bytes with 64-byte blocks and fan_out=16 -> 16+ blocks)
- Read back data that spans multiple blocks
- Random-access seek + read across block boundaries
- Truncate a multi-block stream to smaller size (pyramid shrinks)
- Truncate a multi-block stream to 0 (all blocks freed)
- Write, close, reopen, read back (descriptor persistence)
- Overwrite data in the middle of a multi-block stream
- Extend a stream by writing past the end after a seek

### Existing Tests

All 49 existing tests must continue to pass with the new `SfsDefault` (`StreamsFromBlocks<BlocksFromFiles>`). These tests use default block sizes (4096 bytes) and write small amounts of data, so they exercise depth-0 pyramid only — but they validate that the full stack works end-to-end.

### Rust Unit Tests

Add unit tests in `stream_fs` for:
- `BlocksFromFiles`: allocation, deallocation, read, write, header storage.
- `StreamsFromBlocks`: pyramid depth calculation, navigation, growing, shrinking.
- Pyramid edge cases: exact block boundary writes, single-byte writes, empty streams.

---

## Implementation Order

| Step | Description | Files |
|------|-------------|-------|
| 1 | Write `L2_mock.md` (this document) | `docs/L2_mock.md` |
| 2 | Write new Python tests (Phase 8: block-level) | `sfs_pytest/tests/` |
| 3 | Add `store_header`/`load_header` to `StreamLayer` trait | `stream_fs/src/stream_layer.rs` |
| 4 | Add header support to `StreamsFromFiles` | `stream_fs/src/streams_from_files.rs` |
| 5 | Update `Sfs` to persist/restore `root_dir_stream_id` via header chain | `stream_fs/src/sfs.rs` |
| 6 | Implement `BlockLayer` trait | `stream_fs/src/block_layer.rs` |
| 7 | Implement `BlocksFromFiles` | `stream_fs/src/blocks_from_files.rs` |
| 8 | Implement `StreamsFromBlocks` (pyramid core + StreamLayer + header chain) | `stream_fs/src/streams_from_blocks.rs` |
| 9 | Update `lib.rs` (new modules, `SfsDefault` change) | `stream_fs/src/lib.rs` |
| 10 | Build and fix compilation | All Rust crates |
| 11 | Run all tests (existing 49 + new Phase 8) | `sfs_pytest/` |
| 12 | Thread safety burn-in testing | `sfs_pytest/` |
| 13 | Update `CLAUDE.md` project phase | `.claude/CLAUDE.md` |

---

## Resolved Decisions

1. **Partial block I/O**: Yes — `read_block`/`write_block` accept `offset` parameter for efficient partial access.

2. **L3 header persistence**: Via `BlockLayer::store_header()`/`load_header()` API. `BlocksFromFiles` stores this as a `header` file in the directory.

3. **Stream descriptor format**: Fixed 16 bytes (`u64 size + u64 top_block`). Free marker is `top_block == u64::MAX`.

4. **Descriptor caching**: Cache on open, flush to Streams stream on close (writer). Readers use cached descriptor (immutable while readers exist).

5. **BlocksFromFiles simplicity**: Simple mock — no free list, no block reuse, monotonically increasing IDs, delete file on deallocation.

6. **Block files are fixed-size**: Each `.block` file is exactly `block_size` bytes, zero-padded. Matches real L2 behaviour.

7. **Sentinel value**: `u64::MAX` (all 0xFF bytes in `block_index_width` width). Reserved for "no block" / "unused slot". L2 never allocates this index.

8. **SfsDefault change**: `Sfs<StreamsFromBlocks<BlocksFromFiles>>`. `StreamsFromFiles` remains as `SfsFileBacked` for testing/debugging.

9. **Test strategy**: Small block sizes (`block_size_shift=6`, 64-byte blocks) for new tests to exercise multi-block pyramid with small data.

10. **Redirector block index width**: Block indices in redirector blocks use `block_index_width` bytes (little-endian, zero-extended to u64 on read). Maximizes fan-out.

11. **Index overflow protection**: Both L2 (`allocate_block`) and L3 (`create_stream`) enforce that indices stay below `sentinel(block_index_width)`. The sentinel is `(1 << (block_index_width * 8)) - 1` (all 0xFF bytes). This prevents silent truncation when indices are serialized in `block_index_width` bytes on disk (in redirector blocks, free lists, and L4 directory entries).

12. **Header chain with structured format**: Each layer formats its own section (`| length: u16 | identifier: [u8;6] | version: u8 | data |`) and passes it down. L2 prepends magic (`"stream_fs"` + version 0) and its own section, then writes the full header to disk. On load, each layer strips its section and passes the remainder up. Layer identifiers: L4=`"filing"`, L3=`"pyra  "`, L2=`"blocks"`, L1=`"ondisk"` (future). Mock identifiers: L3=`"strfil"`, L2=`"blkfil"`. Total header size: 63 bytes.

13. **StreamsFromFiles identifier**: `"strfil"` — distinguishes the mock L3 from the real pyramid-based L3 (`"pyra  "`).

---

## Open Questions

None. All key decisions have been resolved.

---

## Implementation Status

### ✅ COMPLETE - L2 Mock Phase

**All components implemented and tested:**

1. **BlockLayer Trait** (`stream_fs/src/block_layer.rs`)
   - `BlockLayer` trait with `Send + Sync` bound
   - Partial block I/O (`read_block`/`write_block` with offset)
   - `allocate_block`/`deallocate_block` for block management
   - `store_header`/`load_header` for header chain persistence
   - Index overflow protection (sentinel guard)

2. **BlocksFromFiles** (`stream_fs/src/blocks_from_files.rs`)
   - L2 mock: each block is a numbered `.block` file on disk
   - Fixed-size blocks (zero-padded to `block_size`)
   - Monotonically increasing block IDs (no reuse)
   - Sentinel guard in `allocate_block` prevents index overflow
   - Header file: magic + L2/L3/L4 layer sections
   - Meta file: `next_block_id` only (block params in header)

3. **StreamsFromBlocks** (`stream_fs/src/streams_from_blocks.rs`)
   - Real L3 implementation with pyramid block linking
   - Stream descriptors (16 bytes: `u64 size + u64 top_block`) in Streams stream
   - Out-of-band descriptor for Streams stream persisted in header chain
   - Pyramid navigation: depth 0 (single block), depth 1 (redirector → data), depth 2+ (nested redirectors)
   - Stream growth: allocates new blocks, increases pyramid depth as needed
   - Stream truncation: deallocates unneeded blocks, shrinks pyramid
   - Free descriptor reuse (`top_block == u64::MAX` marks free slots)
   - `block_sentinel(biw)` = all 0xFF in `block_index_width` bytes for unused redirector slots
   - RWLock on Streams stream + Mutex for bookkeeping + per-stream locking

4. **Header Chain** (L4 → L3 → L2 → disk)
   - Each layer: `| length: u16 | identifier: [u8;6] | version: u8 | data |`
   - Layer identifiers: L4=`"filing"`, L3=`"pyra  "` (real) / `"strfil"` (mock), L2=`"blkfil"` (mock)
   - `StreamLayer` trait extended with `store_header`/`load_header`
   - `StreamsFromFiles` updated with header support (acts as bottom layer)
   - `Sfs::open()` restores `root_dir_stream_id` from header (no longer hardcoded)

5. **Updated Defaults** (`stream_fs/src/lib.rs`)
   - `SfsDefault = Sfs<StreamsFromBlocks<BlocksFromFiles>>`
   - `SfsFileBacked = Sfs<StreamsFromFiles>` kept for debugging

6. **Test Suite**
   - **68/68 tests passing (100%)**
   - All 49 existing tests pass through the new L2 layer
   - Phase 8 multi-block tests (19 tests) with small blocks (64 bytes, fan_out=16)
   - Thread safety burn-in tests (40 threads, 10s default) pass cleanly

**Next Phase:** L1 — ✅ COMPLETE. See [L1](L1.md) for the single-file backend implementation.
