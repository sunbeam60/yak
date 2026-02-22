use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::block_layer::{BlockLayer, CacheMode};
use crate::stream_layer::StreamLayer;
use crate::{HeaderSlotId, OpenMode, YakError};

/// L3 payload size: "pyra  "(6) + version(1) + size(8) + top_block(8) + reserved(8) + cbss(1) = 32 bytes.
const L3_PAYLOAD_SIZE: u16 = 32;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sentinel value for stream descriptors (full u64 fields).
/// A top_block of u64::MAX means the descriptor is free.
const FREE_DESCRIPTOR_MARKER: u64 = u64::MAX;

/// Size of a stream descriptor in bytes: u64 size + u64 top_block + u64 reserved + u8 flags.
const DESCRIPTOR_SIZE: u64 = 25;

/// Size of the free-list head prefix at the start of the Streams stream.
/// The first 8 bytes store the index of the first free descriptor slot.
const STREAMS_HEADER_SIZE: u64 = 8;

/// Sentinel for "no free descriptor" / end of free list.
const FREE_LIST_EMPTY: u64 = u64::MAX;

/// Flag bit: this stream is compressed (leaf entries are compression redirectors).
const STREAM_FLAG_COMPRESSED: u8 = 1;

/// Well-known stream ID for the Streams stream in the lock map.
/// u64::MAX can never be a valid stream ID (it would require an impossible
/// offset in the Streams stream, and equals FREE_DESCRIPTOR_MARKER).
const STREAMS_STREAM_ID: u64 = u64::MAX;

/// Compute the sentinel value for a given block_index_width.
/// All 0xFF bytes in `w` bytes, zero-extended to u64.
/// This value is used as "invalid" / "unused slot" in redirector blocks.
fn block_sentinel(block_index_width: u8) -> u64 {
    let w = block_index_width as u32;
    if w >= 8 {
        u64::MAX
    } else {
        (1u64 << (w * 8)) - 1
    }
}

/// Number of descriptor slots given the Streams stream size.
/// Accounts for the 8-byte free-list head prefix.
fn num_descriptor_slots(streams_size: u64) -> u64 {
    if streams_size < STREAMS_HEADER_SIZE {
        0
    } else {
        (streams_size - STREAMS_HEADER_SIZE) / DESCRIPTOR_SIZE
    }
}

/// Byte offset of a descriptor within the Streams stream.
fn descriptor_offset(stream_id: u64) -> u64 {
    STREAMS_HEADER_SIZE + stream_id * DESCRIPTOR_SIZE
}

// ---------------------------------------------------------------------------
// Stream descriptor
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct StreamDescriptor {
    size: u64,
    top_block: u64,
    /// Allocated capacity in bytes (always a multiple of block_size or
    /// compressed_block_size, or 0). Invariant: reserved >= size.
    reserved: u64,
    /// Bit flags — see STREAM_FLAG_COMPRESSED.
    flags: u8,
}

impl StreamDescriptor {
    fn is_free(&self) -> bool {
        self.top_block == FREE_DESCRIPTOR_MARKER
    }

    fn is_compressed(&self) -> bool {
        self.flags & STREAM_FLAG_COMPRESSED != 0
    }

    fn to_bytes(self) -> [u8; DESCRIPTOR_SIZE as usize] {
        let mut buf = [0u8; DESCRIPTOR_SIZE as usize];
        buf[0..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8..16].copy_from_slice(&self.top_block.to_le_bytes());
        buf[16..24].copy_from_slice(&self.reserved.to_le_bytes());
        buf[24] = self.flags;
        buf
    }

    fn from_bytes(data: &[u8; DESCRIPTOR_SIZE as usize]) -> Self {
        StreamDescriptor {
            size: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            top_block: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            reserved: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            flags: data[24],
        }
    }
}

// ---------------------------------------------------------------------------
// Handle type
// ---------------------------------------------------------------------------

/// Handle for an open stream in StreamsFromBlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockStreamHandle(u64);

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// Per-stream lock state.
#[derive(Default)]
struct LockState {
    readers: u32,
    has_writer: bool,
}

/// Metadata for an open stream handle.
struct HandleInfo {
    stream_id: u64,
    mode: OpenMode,
    cached_descriptor: StreamDescriptor,
}

/// Bookkeeping state protected by a Mutex.
struct StreamsFromBlocksState {
    next_handle_id: u64,
    locks: HashMap<u64, LockState>,
    open_handles: HashMap<u64, HandleInfo>,
}

// ---------------------------------------------------------------------------
// StreamsFromBlocks
// ---------------------------------------------------------------------------

/// Real L3 implementation that links blocks from L2 into numbered streams
/// using the pyramid data structure described in the architecture.
///
/// Stream descriptors are stored in a "Streams stream" — itself composed
/// of blocks linked via the pyramid structure. The out-of-band descriptor
/// for the Streams stream is stored in the header chain.
pub struct StreamsFromBlocks<L2: BlockLayer> {
    layer2: L2,

    /// Out-of-band descriptor for the Streams stream.
    /// Access control is via the lock map (STREAMS_STREAM_ID entry);
    /// this Mutex is only for safe value access (never contends in practice).
    streams_descriptor: Mutex<StreamDescriptor>,

    /// L3's own header slot ID.
    my_slot: HeaderSlotId,

    /// Compressed block size shift (0 = no compression configured).
    /// When non-zero, compressed streams use 2^cbss bytes as their
    /// compressed block size (the unit of compression). Filesystem-wide setting.
    compressed_block_size_shift: u8,

    /// Bookkeeping state: per-stream locks, open handles.
    state: Mutex<StreamsFromBlocksState>,

    /// Signalled when any stream lock is released, waking threads blocked
    /// in `acquire_lock`. Paired with `state`.
    lock_released: Condvar,

    /// Cached head of the free descriptor list. FREE_LIST_EMPTY = no free slots.
    /// Protected by STREAMS_STREAM_ID write lock; this Mutex is only for safe
    /// value access (never contends in practice, same pattern as streams_descriptor).
    free_list_head: Mutex<u64>,
}

// ---------------------------------------------------------------------------
// Pyramid I/O helpers (operate on any stream given its descriptor + L2)
// ---------------------------------------------------------------------------

/// Calculate the number of data blocks needed for `size` bytes.
fn data_blocks_needed(size: u64, block_size: usize) -> u64 {
    size.div_ceil(block_size as u64)
}

/// Calculate the pyramid depth for a given number of data blocks.
/// depth 0: <= 1 data block (top IS the data block)
/// depth d: <= fan_out^d data blocks
fn pyramid_depth(num_data_blocks: u64, fan_out: u64) -> u32 {
    if num_data_blocks <= 1 {
        return 0;
    }
    let mut depth: u32 = 0;
    let mut capacity: u64 = 1;
    loop {
        if capacity >= num_data_blocks {
            return depth;
        }
        depth += 1;
        capacity = capacity.saturating_mul(fan_out);
        if capacity >= num_data_blocks {
            return depth;
        }
    }
}

/// Read a block index from a redirector block at the given slot.
fn read_block_index<L2: BlockLayer>(
    layer2: &L2,
    redirector_block: u64,
    slot: u64,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<u64, YakError> {
    let biw = block_index_width as usize;
    let offset = (slot as usize) * biw;
    let mut buf = [0u8; 8];
    layer2.read_block(redirector_block, offset, &mut buf[..biw], cache)?;
    Ok(u64::from_le_bytes(buf))
}

/// Write a block index to a redirector block at the given slot.
fn write_block_index<L2: BlockLayer>(
    layer2: &L2,
    redirector_block: u64,
    slot: u64,
    index: u64,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<(), YakError> {
    let biw = block_index_width as usize;
    let offset = (slot as usize) * biw;
    let bytes = index.to_le_bytes();
    layer2.write_block(redirector_block, offset, &bytes[..biw], cache)?;
    Ok(())
}

/// Fill an entire redirector block with sentinel values in a single write.
/// Since the sentinel for any block_index_width is all-0xFF bytes,
/// filling the entire block with 0xFF is correct regardless of slot width.
fn fill_sentinel<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<usize, YakError> {
    let _ = block_index_width; // sentinel is always all-0xFF regardless of width
    let buf = vec![0xFFu8; block_size];
    layer2.write_block(block, 0, &buf, cache)
}

/// Navigate from root to the leaf redirector containing `data_block_idx`.
/// Returns the block ID of the leaf redirector.
///
/// For depth 1, the root IS the leaf — returns `descriptor.top_block`.
/// For depth >= 2, walks through intermediate redirector levels.
///
/// Precondition: depth >= 1 (depth 0 has no redirectors).
fn navigate_to_leaf<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    data_block_idx: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<u64, YakError> {
    if depth <= 1 {
        return Ok(descriptor.top_block);
    }

    let mut current_block = descriptor.top_block;
    let mut remaining_idx = data_block_idx;

    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;

        let child = read_block_index(layer2, current_block, slot, block_index_width, cache)?;
        if child == block_sentinel(block_index_width) {
            return Err(YakError::IoError(format!(
                "navigate_to_leaf: invalid block at depth {}, slot {}",
                level, slot
            )));
        }
        current_block = child;
    }

    Ok(current_block)
}

/// Scan for a contiguous ascending run of block indices. `count` is the max
/// number of indices to inspect and `get_index(i)` returns the i-th index.
/// Returns the run length (always >= 1 when `count > 0`).
///
/// Using a closure allows the caller to lazily decode indices from a
/// byte buffer without allocating an array of u64 indices first.
#[inline]
fn scan_contiguous_run<F: Fn(usize) -> u64>(count: usize, get_index: F) -> usize {
    if count == 0 {
        return 0;
    }
    let first = get_index(0);
    let mut run: usize = 1;
    for i in 1..count {
        if get_index(i) != first + run as u64 {
            break;
        }
        run += 1;
    }
    run
}

/// Scan a pre-read leaf redirector buffer for a contiguous run of physical
/// block indices starting at `start_slot`. Returns (first_physical_block, run_length).
///
/// The buffer must contain the full leaf block (block_size bytes).
/// `max_slots` caps how many slots to scan (remaining data blocks in stream).
/// No allocation, no I/O — purely in-memory.
fn scan_run_from_buffer(
    leaf_buf: &[u8],
    start_slot: u64,
    max_slots: u64,
    block_index_width: u8,
) -> Result<(u64, u64), YakError> {
    let biw = block_index_width as usize;
    let base_offset = start_slot as usize * biw;
    let sentinel = block_sentinel(block_index_width);

    // Decode the first index and check for sentinel
    let mut idx_buf = [0u8; 8];
    idx_buf[..biw].copy_from_slice(&leaf_buf[base_offset..base_offset + biw]);
    let first_block = u64::from_le_bytes(idx_buf);

    if first_block == sentinel {
        return Err(YakError::IoError(format!(
            "scan_run_from_buffer: sentinel at slot {}",
            start_slot
        )));
    }

    // Delegate to shared contiguity scanner with lazy byte-buffer decode
    let run_length = scan_contiguous_run(max_slots as usize, |i| {
        let pos = base_offset + i * biw;
        let mut buf = [0u8; 8];
        buf[..biw].copy_from_slice(&leaf_buf[pos..pos + biw]);
        u64::from_le_bytes(buf)
    });

    Ok((first_block, run_length as u64))
}

/// Compute fan-out (redirector slots per block) from an L2 layer.
fn compute_fan_out<L2: BlockLayer>(layer2: &L2) -> u64 {
    (layer2.block_size() / layer2.block_index_width() as usize) as u64
}

/// Write data block indices into leaf redirectors in batches.
///
/// Instead of writing each data block's index one at a time (with a full
/// pyramid navigation per block), this groups blocks by their parent leaf
/// redirector and writes all indices for each leaf in a single `write_block`
/// call. Intermediate redirectors are allocated on demand and sentinel-filled
/// in one call each.
fn batch_fill_leaf_redirectors<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    new_blocks: &[u64],
    starting_data_block_idx: u64,
    block_index_width: u8,
    depth: u32,
    cache: CacheMode,
) -> Result<(), YakError> {
    let fan_out = compute_fan_out(layer2);
    if depth == 0 || new_blocks.is_empty() {
        return Ok(());
    }

    let block_size = layer2.block_size();
    let biw = block_index_width as usize;
    let sentinel = block_sentinel(block_index_width);
    let mut i: usize = 0;

    while i < new_blocks.len() {
        let data_block_idx = starting_data_block_idx + i as u64;

        // Navigate from root to the leaf redirector for this data_block_idx,
        // allocating intermediate redirectors as needed.
        let mut current_block = descriptor.top_block;
        let mut remaining_idx = data_block_idx;

        for level in (1..depth).rev() {
            let span = fan_out.pow(level);
            let slot = remaining_idx / span;
            remaining_idx %= span;

            let mut child =
                read_block_index(layer2, current_block, slot, block_index_width, cache)?;
            if child == sentinel {
                // Allocate new intermediate redirector, fill with sentinels
                child = layer2.allocate_block()?;
                fill_sentinel(layer2, child, block_size, block_index_width, cache)?;
                write_block_index(layer2, current_block, slot, child, block_index_width, cache)?;
            }
            current_block = child;
        }

        // current_block is the leaf redirector; remaining_idx is the start slot
        let start_slot = remaining_idx as usize;
        let slots_available = fan_out as usize - start_slot;
        let blocks_remaining = new_blocks.len() - i;
        let batch_size = slots_available.min(blocks_remaining);

        // Build a buffer of block indices for this batch
        let mut buf = Vec::with_capacity(batch_size * biw);
        for j in 0..batch_size {
            let bytes = new_blocks[i + j].to_le_bytes();
            buf.extend_from_slice(&bytes[..biw]);
        }

        // Write all indices to the leaf in one call
        let offset = start_slot * biw;
        layer2.write_block(current_block, offset, &buf, cache)?;

        i += batch_size;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// PyramidOps trait — abstracts leaf-level differences between uncompressed
// and compressed streams so structural operations (reserve, truncate,
// ensure_capacity, tree deallocation) can be shared.
// ---------------------------------------------------------------------------

trait PyramidOps<L2: BlockLayer> {
    /// Logical block size in bytes (block_size for uncompressed,
    /// compressed_block_size for compressed).
    fn logical_block_size(&self) -> usize;

    /// How many logical blocks are needed for `size` bytes.
    fn blocks_needed(&self, size: u64) -> u64;

    /// Initialize a newly allocated block for use as a leaf-level entry.
    fn init_leaf_block(&self, layer2: &L2, block: u64, cache: CacheMode) -> Result<(), YakError>;

    /// Collect all physical blocks owned by a leaf slot value, for deallocation.
    fn collect_leaf_blocks_for_dealloc(
        &self,
        layer2: &L2,
        leaf_value: u64,
        blocks: &mut Vec<u64>,
        cache: CacheMode,
    ) -> Result<(), YakError>;

    /// Collect all physical blocks owned by a leaf slot value, for verification.
    /// Same as dealloc variant but reports errors via TreeCollector instead of
    /// returning Result.
    fn collect_leaf_blocks_verify(
        &self,
        layer2: &L2,
        leaf_value: u64,
        collector: &mut TreeCollector<'_>,
        cache: CacheMode,
    );
}

/// Uncompressed streams: leaf slots point directly to data blocks.
struct UncompressedOps {
    block_size: usize,
}

impl<L2: BlockLayer> PyramidOps<L2> for UncompressedOps {
    fn logical_block_size(&self) -> usize {
        self.block_size
    }

    fn blocks_needed(&self, size: u64) -> u64 {
        size.div_ceil(self.block_size as u64)
    }

    fn init_leaf_block(
        &self,
        _layer2: &L2,
        _block: u64,
        _cache: CacheMode,
    ) -> Result<(), YakError> {
        Ok(()) // Data blocks need no initialization
    }

    fn collect_leaf_blocks_for_dealloc(
        &self,
        _layer2: &L2,
        leaf_value: u64,
        blocks: &mut Vec<u64>,
        _cache: CacheMode,
    ) -> Result<(), YakError> {
        blocks.push(leaf_value);
        Ok(())
    }

    fn collect_leaf_blocks_verify(
        &self,
        _layer2: &L2,
        leaf_value: u64,
        collector: &mut TreeCollector<'_>,
        _cache: CacheMode,
    ) {
        collector.blocks.push(leaf_value);
    }
}

/// Compressed streams: leaf slots point to compression redirector blocks,
/// each of which references physical data blocks holding compressed data.
struct CompressedOps {
    block_size: usize,
    compressed_block_size: usize,
    block_index_width: u8,
}

impl<L2: BlockLayer> PyramidOps<L2> for CompressedOps {
    fn logical_block_size(&self) -> usize {
        self.compressed_block_size
    }

    fn blocks_needed(&self, size: u64) -> u64 {
        size.div_ceil(self.compressed_block_size as u64)
    }

    fn init_leaf_block(&self, layer2: &L2, block: u64, cache: CacheMode) -> Result<(), YakError> {
        // Initialize compression redirector with compressed_length=0
        let zero_buf = [0u8; 4];
        layer2.write_block(block, 0, &zero_buf, cache)?;
        Ok(())
    }

    fn collect_leaf_blocks_for_dealloc(
        &self,
        layer2: &L2,
        leaf_value: u64,
        blocks: &mut Vec<u64>,
        cache: CacheMode,
    ) -> Result<(), YakError> {
        let redir = read_comp_redir(
            layer2,
            leaf_value,
            self.block_size,
            self.block_index_width,
            cache,
        )?;
        for &db in &redir.data_blocks {
            blocks.push(db);
        }
        blocks.push(leaf_value);
        Ok(())
    }

    fn collect_leaf_blocks_verify(
        &self,
        layer2: &L2,
        leaf_value: u64,
        collector: &mut TreeCollector<'_>,
        cache: CacheMode,
    ) {
        collector.blocks.push(leaf_value);
        match read_comp_redir(
            layer2,
            leaf_value,
            self.block_size,
            self.block_index_width,
            cache,
        ) {
            Ok(redir) => {
                for &db in &redir.data_blocks {
                    collector.blocks.push(db);
                }
            }
            Err(e) => {
                collector.issues.push(format!(
                    "L3: stream {}: error reading comp redirector block {}: {}",
                    collector.label, leaf_value, e
                ));
            }
        }
    }
}

/// Read bytes from a stream described by `descriptor`, starting at `pos`.
/// pyramid_read attempts to minimise the number of L2 read operations
/// by scanning for runs of contiguous blocks and treating them like a regular
/// buffer read when possible, while still correctly handling non-contiguous blocks.
fn pyramid_read<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    pos: u64,
    buf: &mut [u8],
    block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<usize, YakError> {
    let fan_out = compute_fan_out(layer2);
    if pos >= descriptor.size {
        return Ok(0);
    }

    // cap the read to the lowest of the stream size and descriptor size
    let available = (descriptor.size - pos) as usize;
    let to_read = buf.len().min(available);
    if to_read == 0 {
        return Ok(0);
    }

    let capacity = descriptor.reserved.max(descriptor.size);
    let num_data_blocks = data_blocks_needed(capacity, block_size);
    let depth = pyramid_depth(num_data_blocks, fan_out);

    // Depth 0: single data block, no redirectors
    if depth == 0 {
        let offset_in_block = (pos % block_size as u64) as usize;
        return layer2.read_contiguous_blocks(
            descriptor.top_block,
            offset_in_block,
            &mut buf[..to_read],
        );
    }

    // Depth >= 1: use leaf caching to avoid re-navigating the same leaf
    let mut bytes_read = 0;
    let mut current_pos = pos;
    let mut cached_leaf_start: u64 = u64::MAX;
    let mut leaf_buf = vec![0u8; block_size];

    while bytes_read < to_read {
        let data_block_idx = current_pos / block_size as u64;
        let offset_in_block = (current_pos % block_size as u64) as usize;

        // Which leaf does this data block belong to?
        let leaf_start = (data_block_idx / fan_out) * fan_out;

        // Only navigate and read the leaf when we cross a leaf boundary
        if leaf_start != cached_leaf_start {
            let leaf_block = navigate_to_leaf(
                layer2,
                descriptor,
                data_block_idx,
                depth,
                fan_out,
                block_index_width,
                cache,
            )?;
            layer2.read_block(leaf_block, 0, &mut leaf_buf, cache)?;
            cached_leaf_start = leaf_start;
        }

        // Scan for contiguous run within the cached leaf buffer
        // This enables us to read as much possibe within the run
        // This allows us to treat block based storage as byte based for
        // better performance when there are contiguous blocks
        let slot_in_leaf = data_block_idx - leaf_start;
        let max_slots = (num_data_blocks - data_block_idx).min(fan_out - slot_in_leaf);
        let (first_phys_block, run_length) =
            scan_run_from_buffer(&leaf_buf, slot_in_leaf, max_slots, block_index_width)?;

        let bytes_in_run = run_length as usize * block_size - offset_in_block;
        let chunk = (to_read - bytes_read).min(bytes_in_run);

        // now that we know how much we can read, read the whole run in one go
        let n = layer2.read_contiguous_blocks(
            first_phys_block,
            offset_in_block,
            &mut buf[bytes_read..bytes_read + chunk],
        )?;
        bytes_read += n;
        current_pos += n as u64;

        if n < chunk {
            break; // Short read
        }
    }

    Ok(bytes_read)
}

/// Write bytes to a stream described by `descriptor`, starting at `pos`.
/// May grow the pyramid. Updates the descriptor.
fn pyramid_write<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    pos: u64,
    buf: &[u8],
    block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<usize, YakError> {
    let fan_out = compute_fan_out(layer2);
    if buf.is_empty() {
        return Ok(0);
    }

    let end_pos = pos + buf.len() as u64;

    // Ensure we have enough blocks for the write endpoint (or current size, whichever is larger)
    let ops = UncompressedOps { block_size };
    pyramid_reserve(
        layer2,
        descriptor,
        end_pos.max(descriptor.size),
        block_size,
        block_index_width,
        &ops,
        cache,
    )?;

    let old_size = descriptor.size;

    // Compute depth after reserve (pyramid structure is now stable for this write)
    let capacity = descriptor.reserved.max(descriptor.size);
    let num_data_blocks = data_blocks_needed(capacity, block_size);
    let depth = pyramid_depth(num_data_blocks, fan_out);

    // Depth 0: single data block, no redirectors
    if depth == 0 {
        let offset_in_block = (pos % block_size as u64) as usize;
        let n = layer2.write_contiguous_blocks(descriptor.top_block, offset_in_block, buf)?;
        let actual_end = pos + n as u64;
        descriptor.size = actual_end.max(old_size);
        return Ok(n);
    }

    // Depth >= 1: use leaf caching to avoid re-navigating the same leaf
    let mut bytes_written = 0;
    let mut current_pos = pos;
    let mut cached_leaf_start: u64 = u64::MAX;
    let mut leaf_buf = vec![0u8; block_size];

    while bytes_written < buf.len() {
        let data_block_idx = current_pos / block_size as u64;
        let offset_in_block = (current_pos % block_size as u64) as usize;

        // Which leaf does this data block belong to?
        let leaf_start = (data_block_idx / fan_out) * fan_out;

        // Only navigate and read the leaf when we cross a leaf boundary
        if leaf_start != cached_leaf_start {
            let leaf_block = navigate_to_leaf(
                layer2,
                descriptor,
                data_block_idx,
                depth,
                fan_out,
                block_index_width,
                cache,
            )?;
            layer2.read_block(leaf_block, 0, &mut leaf_buf, cache)?;
            cached_leaf_start = leaf_start;
        }

        // Scan for contiguous run within the cached leaf buffer
        let slot_in_leaf = data_block_idx - leaf_start;
        let max_slots = (num_data_blocks - data_block_idx).min(fan_out - slot_in_leaf);
        let (first_phys_block, run_length) =
            scan_run_from_buffer(&leaf_buf, slot_in_leaf, max_slots, block_index_width)?;

        let bytes_in_run = run_length as usize * block_size - offset_in_block;
        let chunk = (buf.len() - bytes_written).min(bytes_in_run);

        let n = layer2.write_contiguous_blocks(
            first_phys_block,
            offset_in_block,
            &buf[bytes_written..bytes_written + chunk],
        )?;
        bytes_written += n;
        current_pos += n as u64;

        if n < chunk {
            break;
        }
    }

    // Update size to reflect actual write extent
    let actual_end = pos + bytes_written as u64;
    descriptor.size = actual_end.max(old_size);

    Ok(bytes_written)
}

/// Accumulator for collecting block IDs and issues during pyramid tree walks.
struct TreeCollector<'a> {
    blocks: &'a mut Vec<u64>,
    issues: &'a mut Vec<String>,
    label: &'a str,
}

/// Check if all slots in a redirector block are block_sentinel(block_index_width).
fn is_redirector_empty<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    fan_out: u64,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<bool, YakError> {
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width, cache)?;
        if child != block_sentinel(block_index_width) {
            return Ok(false);
        }
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Pyramid structural operations (parameterized by PyramidOps)
// ---------------------------------------------------------------------------

/// Recursively collect all block IDs in a pyramid tree for deallocation.
/// At depth 0, delegates to `ops.collect_leaf_blocks_for_dealloc` to handle
/// the leaf-level resource (data block or compression redirector + data blocks).
fn collect_tree_blocks_for_dealloc<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_index_width: u8,
    blocks: &mut Vec<u64>,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    let fan_out = compute_fan_out(layer2);
    if depth == 0 {
        return ops.collect_leaf_blocks_for_dealloc(layer2, block, blocks, cache);
    }

    // Pyramid redirector: recurse into children, then collect self
    let sentinel = block_sentinel(block_index_width);
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width, cache)?;
        if child != sentinel {
            collect_tree_blocks_for_dealloc(
                layer2,
                child,
                depth - 1,
                block_index_width,
                blocks,
                ops,
                cache,
            )?;
        }
    }
    blocks.push(block);
    Ok(())
}

/// Deallocate all blocks in a pyramid tree rooted at `block`.
/// Collects all block IDs first, then batch-deallocates them so the free list
/// is written in sorted order with a single header persist.
fn deallocate_tree<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_index_width: u8,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    let mut blocks = Vec::new();
    collect_tree_blocks_for_dealloc(
        layer2,
        block,
        depth,
        block_index_width,
        &mut blocks,
        ops,
        cache,
    )?;
    layer2.deallocate_blocks(&mut blocks)?;
    Ok(())
}

/// Recursively collect all block IDs in a pyramid tree for verification.
/// At depth 0, delegates to `ops.collect_leaf_blocks_verify` to handle the
/// leaf-level resource (data block or compression redirector + data blocks).
fn collect_tree_blocks<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_index_width: u8,
    collector: &mut TreeCollector<'_>,
    ops: &P,
    cache: CacheMode,
) {
    let fan_out = compute_fan_out(layer2);
    if depth == 0 {
        ops.collect_leaf_blocks_verify(layer2, block, collector, cache);
        return;
    }

    // Pyramid redirector: collect self, then recurse into children
    collector.blocks.push(block);
    let sentinel = block_sentinel(block_index_width);
    for slot in 0..fan_out {
        match read_block_index(layer2, block, slot, block_index_width, cache) {
            Ok(child) => {
                if child != sentinel {
                    collect_tree_blocks(
                        layer2,
                        child,
                        depth - 1,
                        block_index_width,
                        collector,
                        ops,
                        cache,
                    );
                }
            }
            Err(e) => {
                collector.issues.push(format!(
                    "L3: stream {}: error reading redirector block {} slot {}: {}",
                    collector.label, block, slot, e
                ));
            }
        }
    }
}

/// Deallocate the leaf-level entry at `block_idx` and mark its leaf slot as
/// sentinel. Also deallocates empty parent redirectors on the way back up.
fn deallocate_slot_at<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    top_block: u64,
    block_idx: u64,
    depth: u32,
    block_index_width: u8,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    let fan_out = compute_fan_out(layer2);
    let sentinel = block_sentinel(block_index_width);

    if depth == 0 {
        // Top is the leaf entry itself — don't deallocate here, caller handles
        return Ok(());
    }

    // Navigate to find the leaf entry
    let mut path: Vec<(u64, u64)> = Vec::new(); // (block, slot)
    let mut current_block = top_block;
    let mut remaining_idx = block_idx;

    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;
        path.push((current_block, slot));

        let child = read_block_index(layer2, current_block, slot, block_index_width, cache)?;
        if child == sentinel {
            return Ok(()); // Already deallocated
        }
        current_block = child;
    }

    // current_block is the bottom (leaf) redirector, remaining_idx is the slot
    let slot = remaining_idx;
    let leaf_value = read_block_index(layer2, current_block, slot, block_index_width, cache)?;
    if leaf_value == sentinel {
        return Ok(());
    }

    // Collect and deallocate all physical blocks owned by this leaf entry
    let mut to_dealloc = Vec::new();
    ops.collect_leaf_blocks_for_dealloc(layer2, leaf_value, &mut to_dealloc, cache)?;
    layer2.deallocate_blocks(&mut to_dealloc)?;

    // Mark the leaf slot as sentinel
    write_block_index(
        layer2,
        current_block,
        slot,
        sentinel,
        block_index_width,
        cache,
    )?;

    // Check if leaf redirector is now empty; if so, deallocate it and mark in parent
    if is_redirector_empty(layer2, current_block, fan_out, block_index_width, cache)? {
        layer2.deallocate_block(current_block)?;
        if let Some(&(parent_block, parent_slot)) = path.last() {
            write_block_index(
                layer2,
                parent_block,
                parent_slot,
                sentinel,
                block_index_width,
                cache,
            )?;
        }
    }

    Ok(())
}

/// Ensure the pyramid has enough slots for `target_blocks` leaf-level entries.
/// Grows the pyramid as needed (increasing depth, allocating redirectors and
/// leaf entries). Uses `ops.init_leaf_block` to initialize new leaf entries.
fn ensure_capacity<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    target_blocks: u64,
    block_size: usize,
    block_index_width: u8,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    let fan_out = compute_fan_out(layer2);
    if target_blocks == 0 {
        return Ok(());
    }

    let current_blocks = ops.blocks_needed(descriptor.reserved);
    let current_depth = pyramid_depth(current_blocks, fan_out);
    let target_depth = pyramid_depth(target_blocks, fan_out);

    // Handle empty stream: allocate first leaf entry
    if descriptor.reserved == 0 && descriptor.top_block == 0 {
        let block = layer2.allocate_block()?;
        ops.init_leaf_block(layer2, block, cache)?;
        descriptor.top_block = block;

        if target_blocks <= 1 {
            return Ok(());
        }
    }

    // Grow depth if needed: wrap current top in new redirector layers
    let effective_current_depth = if descriptor.reserved == 0 && current_blocks == 0 {
        0
    } else {
        current_depth
    };

    let mut current_top = descriptor.top_block;
    for _ in effective_current_depth..target_depth {
        let new_redirector = layer2.allocate_block()?;
        fill_sentinel(layer2, new_redirector, block_size, block_index_width, cache)?;
        write_block_index(
            layer2,
            new_redirector,
            0,
            current_top,
            block_index_width,
            cache,
        )?;
        current_top = new_redirector;
    }
    descriptor.top_block = current_top;

    // Allocate and initialize missing leaf entries
    let effective_current = if current_blocks == 0 {
        1
    } else {
        current_blocks
    };
    let blocks_needed = target_blocks - effective_current;
    if blocks_needed > 0 {
        let new_blocks = layer2.allocate_blocks(blocks_needed)?;

        // Initialize each new leaf entry
        for &block in &new_blocks {
            ops.init_leaf_block(layer2, block, cache)?;
        }

        // Fill leaf slots in the pyramid tree
        batch_fill_leaf_redirectors(
            layer2,
            descriptor,
            &new_blocks,
            effective_current,
            block_index_width,
            target_depth,
            cache,
        )?;
    }

    Ok(())
}

/// Pre-allocate capacity so the stream can hold at least `n_bytes`.
/// Does not change the stream's logical size. Errors if `n_bytes < descriptor.size`.
fn pyramid_reserve<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    n_bytes: u64,
    block_size: usize,
    block_index_width: u8,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    if n_bytes < descriptor.size {
        return Err(YakError::IoError(format!(
            "cannot reserve {} bytes: stream size is already {}",
            n_bytes, descriptor.size
        )));
    }
    if n_bytes <= descriptor.reserved {
        return Ok(());
    }

    let lbs = ops.logical_block_size() as u64;
    let target_blocks = ops.blocks_needed(n_bytes);

    ensure_capacity(
        layer2,
        descriptor,
        target_blocks,
        block_size,
        block_index_width,
        ops,
        cache,
    )?;
    descriptor.reserved = target_blocks * lbs;
    Ok(())
}

/// Truncate stream to `new_len` bytes, deallocating excess leaf entries.
fn pyramid_truncate<L2: BlockLayer, P: PyramidOps<L2>>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    new_len: u64,
    block_index_width: u8,
    ops: &P,
    cache: CacheMode,
) -> Result<(), YakError> {
    let fan_out = compute_fan_out(layer2);
    if new_len >= descriptor.size {
        return Ok(());
    }

    let lbs = ops.logical_block_size() as u64;

    if new_len == 0 {
        let capacity = descriptor.reserved.max(descriptor.size);
        if capacity > 0 && descriptor.top_block != 0 {
            let old_blocks = ops.blocks_needed(capacity);
            let old_depth = pyramid_depth(old_blocks, fan_out);
            deallocate_tree(
                layer2,
                descriptor.top_block,
                old_depth,
                block_index_width,
                ops,
                cache,
            )?;
        }
        descriptor.size = 0;
        descriptor.top_block = 0;
        descriptor.reserved = 0;
        return Ok(());
    }

    let capacity = descriptor.reserved.max(descriptor.size);
    let old_blocks = ops.blocks_needed(capacity);
    let new_blocks = ops.blocks_needed(new_len);
    let old_depth = pyramid_depth(old_blocks, fan_out);
    let new_depth = pyramid_depth(new_blocks, fan_out);

    // Deallocate excess leaf entries
    if new_blocks < old_blocks {
        for block_idx in new_blocks..old_blocks {
            deallocate_slot_at(
                layer2,
                descriptor.top_block,
                block_idx,
                old_depth,
                block_index_width,
                ops,
                cache,
            )?;
        }
    }

    // Collapse depth if needed
    let mut current_top = descriptor.top_block;
    for _ in new_depth..old_depth {
        let child = read_block_index(layer2, current_top, 0, block_index_width, cache)?;
        layer2.deallocate_block(current_top)?;
        current_top = child;
    }
    descriptor.top_block = current_top;
    descriptor.size = new_len;
    descriptor.reserved = new_blocks * lbs;

    Ok(())
}

// ---------------------------------------------------------------------------
// Compressed pyramid leaf cache
// ---------------------------------------------------------------------------

/// Reusable leaf-block cache for compressed pyramid traversal. Holds the
/// cached leaf buffer and the leaf-start index so that consecutive lookups
/// within the same leaf avoid re-navigating and re-reading the block.
struct LeafCache {
    cached_start: u64,
    buf: Vec<u8>,
}

impl LeafCache {
    fn new(block_size: usize) -> Self {
        Self {
            cached_start: u64::MAX,
            buf: vec![0u8; block_size],
        }
    }
}

/// Resolve the physical redirector block for compressed block `cblock_idx`.
/// At depth 0 this is simply `descriptor.top_block`. At depth >= 1 the
/// pyramid is navigated (with leaf caching via `cache`) and the slot is
/// decoded from the leaf buffer.
fn resolve_redir_block<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    cblock_idx: u64,
    depth: u32,
    block_index_width: u8,
    leaf_cache: &mut LeafCache,
    cache: CacheMode,
) -> Result<u64, YakError> {
    let fan_out = compute_fan_out(layer2);
    if depth == 0 {
        return Ok(descriptor.top_block);
    }

    let biw = block_index_width as usize;
    let leaf_start = (cblock_idx / fan_out) * fan_out;

    if leaf_start != leaf_cache.cached_start {
        let leaf_block = navigate_to_leaf(
            layer2,
            descriptor,
            cblock_idx,
            depth,
            fan_out,
            block_index_width,
            cache,
        )?;
        layer2.read_block(leaf_block, 0, &mut leaf_cache.buf, cache)?;
        leaf_cache.cached_start = leaf_start;
    }

    let slot = (cblock_idx % fan_out) as usize;
    let offset = slot * biw;
    let mut idx_buf = [0u8; 8];
    idx_buf[..biw].copy_from_slice(&leaf_cache.buf[offset..offset + biw]);
    Ok(u64::from_le_bytes(idx_buf))
}

// ---------------------------------------------------------------------------
// Compression redirector helpers
// ---------------------------------------------------------------------------

/// A compression redirector parsed from a single physical block.
/// Format: [compressed_length: u32] [block_ptr_0] [block_ptr_1] ... [block_ptr_N]
/// where N = ceil(compressed_length / block_size).
struct CompRedir {
    compressed_length: u32,
    data_blocks: Vec<u64>,
}

/// Read a compression redirector from the given block.
/// Reads the entire redirector in a single `read_block` call.
fn read_comp_redir<L2: BlockLayer>(
    layer2: &L2,
    redir_block: u64,
    block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<CompRedir, YakError> {
    // Read the full block once, then parse compressed length and block
    // pointers from the buffer.
    let mut buf = vec![0u8; block_size];
    layer2.read_block(redir_block, 0, &mut buf, cache)?;

    let compressed_length = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);

    if compressed_length == 0 {
        return Ok(CompRedir {
            compressed_length: 0,
            data_blocks: Vec::new(),
        });
    }

    let biw = block_index_width as usize;
    let n_blocks = (compressed_length as usize).div_ceil(block_size);

    let mut data_blocks = Vec::with_capacity(n_blocks);
    for i in 0..n_blocks {
        let offset = 4 + i * biw;
        let mut idx_buf = [0u8; 8];
        idx_buf[..biw].copy_from_slice(&buf[offset..offset + biw]);
        data_blocks.push(u64::from_le_bytes(idx_buf));
    }

    Ok(CompRedir {
        compressed_length,
        data_blocks,
    })
}

/// Decompress the data described by a compression redirector into `out_buf`.
/// If the redirector has compressed_length == 0, fills `out_buf` with zeros.
/// Uses `compressed_buf` as a reusable scratch buffer for reading compressed data
/// from physical blocks, avoiding per-call allocation.
fn decompress_cblock_data<L2: BlockLayer>(
    layer2: &L2,
    redir: &CompRedir,
    block_size: usize,
    compressed_buf: &mut Vec<u8>,
    out_buf: &mut [u8],
) -> Result<(), YakError> {
    if redir.compressed_length == 0 {
        out_buf.fill(0);
        return Ok(());
    }

    // Read compressed data from physical blocks, batching contiguous runs
    let total_compressed = redir.compressed_length as usize;
    compressed_buf.resize(total_compressed, 0);
    let mut bytes_remaining = total_compressed;
    let mut buf_offset = 0;
    let blocks = &redir.data_blocks;

    let mut i = 0;
    while i < blocks.len() {
        let run_len = scan_contiguous_run(blocks.len() - i, |j| blocks[i + j]);
        let run_bytes = (run_len * block_size).min(bytes_remaining);
        layer2.read_contiguous_blocks(
            blocks[i],
            0,
            &mut compressed_buf[buf_offset..buf_offset + run_bytes],
        )?;
        buf_offset += run_bytes;
        bytes_remaining -= run_bytes;
        i += run_len;
    }

    // Decompress
    let decompressed = lz4_flex::decompress_size_prepended(compressed_buf)
        .map_err(|e| YakError::IoError(format!("lz4 decompression failed: {}", e)))?;

    // Copy into out_buf, zero-padding if the valid extent is smaller than the buffer
    let dec_len = decompressed.len();
    out_buf[..dec_len].copy_from_slice(&decompressed);
    out_buf[dec_len..].fill(0);
    Ok(())
}

/// Read and decompress a compressed block's data from a compression redirector.
/// Writes the decompressed data into `out_buf` (whose length determines the
/// compressed block size). Uses `compressed_buf` as a reusable scratch buffer for
/// reading compressed data from physical blocks, avoiding per-call allocation.
fn decompress_cblock<L2: BlockLayer>(
    layer2: &L2,
    redir_block: u64,
    block_size: usize,
    block_index_width: u8,
    compressed_buf: &mut Vec<u8>,
    out_buf: &mut [u8],
    cache: CacheMode,
) -> Result<(), YakError> {
    let redir = read_comp_redir(layer2, redir_block, block_size, block_index_width, cache)?;
    decompress_cblock_data(layer2, &redir, block_size, compressed_buf, out_buf)
}

/// Reusable scratch buffers for `compress_cblock_data`, allocated once and
/// reused across loop iterations to avoid per-call heap allocations.
struct CompressWorkspace {
    data_blocks: Vec<u64>,
    excess_blocks: Vec<u64>,
    redir_buf: Vec<u8>,
}

impl CompressWorkspace {
    fn new() -> Self {
        Self {
            data_blocks: Vec::new(),
            excess_blocks: Vec::new(),
            redir_buf: Vec::new(),
        }
    }
}

/// Compress `data` and write it to physical blocks, reconciling block counts
/// against `old_redir` (reusing, allocating, or freeing blocks as needed), then
/// writing the updated compression redirector to `redir_block`.
fn compress_cblock_data<L2: BlockLayer>(
    layer2: &L2,
    redir_block: u64,
    old_redir: &CompRedir,
    data: &[u8],
    block_index_width: u8,
    ws: &mut CompressWorkspace,
    cache: CacheMode,
) -> Result<(), YakError> {
    let block_size = layer2.block_size();
    let biw_sz = block_index_width as usize;

    // Compress the data
    let compressed = lz4_flex::compress_prepend_size(data);
    let new_compressed_len = compressed.len();
    let new_block_count = new_compressed_len.div_ceil(block_size);
    let old_block_count = old_redir.data_blocks.len();

    // Reconcile physical blocks: reuse existing, allocate/free the delta
    ws.data_blocks.clear();

    // Reuse as many existing blocks as we can
    let reuse_count = old_block_count.min(new_block_count);
    ws.data_blocks
        .extend_from_slice(&old_redir.data_blocks[..reuse_count]);

    if new_block_count > old_block_count {
        // Need more blocks
        let extra = (new_block_count - old_block_count) as u64;
        let allocated = layer2.allocate_blocks(extra)?;
        ws.data_blocks.extend_from_slice(&allocated);
    } else if old_block_count > new_block_count {
        // Free excess blocks
        ws.excess_blocks.clear();
        ws.excess_blocks
            .extend_from_slice(&old_redir.data_blocks[new_block_count..]);
        layer2.deallocate_blocks(&mut ws.excess_blocks)?;
    }

    // Write compressed data to physical blocks, batching contiguous runs
    let mut comp_remaining = new_compressed_len;
    let mut comp_offset = 0;
    let mut wi = 0;
    while wi < ws.data_blocks.len() {
        let run_len = scan_contiguous_run(ws.data_blocks.len() - wi, |j| ws.data_blocks[wi + j]);
        let run_bytes = (run_len * block_size).min(comp_remaining);
        layer2.write_contiguous_blocks(
            ws.data_blocks[wi],
            0,
            &compressed[comp_offset..comp_offset + run_bytes],
        )?;
        comp_offset += run_bytes;
        comp_remaining -= run_bytes;
        wi += run_len;
    }

    // Write updated compression redirector
    let redir_buf_size = 4 + ws.data_blocks.len() * biw_sz;
    ws.redir_buf.resize(redir_buf_size, 0);
    ws.redir_buf[0..4].copy_from_slice(&(new_compressed_len as u32).to_le_bytes());
    for (i, &block_ptr) in ws.data_blocks.iter().enumerate() {
        let off = 4 + i * biw_sz;
        let bytes = block_ptr.to_le_bytes();
        ws.redir_buf[off..off + biw_sz].copy_from_slice(&bytes[..biw_sz]);
    }
    layer2.write_block(redir_block, 0, &ws.redir_buf[..redir_buf_size], cache)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Compressed pyramid operations
// ---------------------------------------------------------------------------

/// Calculate the number of compressed blocks needed for `size` bytes.
fn compressed_blocks_needed(size: u64, compressed_block_size: usize) -> u64 {
    size.div_ceil(compressed_block_size as u64)
}

/// Read bytes from a compressed stream.
fn pyramid_read_compressed<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    pos: u64,
    buf: &mut [u8],
    compressed_block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<usize, YakError> {
    let fan_out = compute_fan_out(layer2);
    if pos >= descriptor.size {
        return Ok(0);
    }

    let block_size = layer2.block_size();
    let available = (descriptor.size - pos) as usize;
    let to_read = buf.len().min(available);
    if to_read == 0 {
        return Ok(0);
    }

    let capacity = descriptor.reserved.max(descriptor.size);
    let num_cblocks = compressed_blocks_needed(capacity, compressed_block_size);
    let depth = pyramid_depth(num_cblocks, fan_out);
    let sentinel = block_sentinel(block_index_width);

    let mut bytes_read = 0;
    let mut current_pos = pos;

    // Reusable buffers — allocated once, reused across loop iterations
    let mut cblock_buf = vec![0u8; compressed_block_size];
    let mut compressed_read_buf: Vec<u8> = Vec::new();
    let mut leaf_cache = LeafCache::new(block_size);

    while bytes_read < to_read {
        let cblock_idx = current_pos / compressed_block_size as u64;
        let offset_in_cblock = (current_pos % compressed_block_size as u64) as usize;

        // find the physical redirector block for this compressed block index
        let redir_block = resolve_redir_block(
            layer2,
            descriptor,
            cblock_idx,
            depth,
            block_index_width,
            &mut leaf_cache,
            cache,
        )?;

        if redir_block == sentinel {
            // Unallocated compressed block — return zeros
            let cblock_remaining = compressed_block_size - offset_in_cblock;
            let chunk = (to_read - bytes_read).min(cblock_remaining);
            buf[bytes_read..bytes_read + chunk].fill(0);
            bytes_read += chunk;
            current_pos += chunk as u64;
            continue;
        }

        // Read and decompress this compressed block into reusable buffer
        decompress_cblock(
            layer2,
            redir_block,
            block_size,
            block_index_width,
            &mut compressed_read_buf,
            &mut cblock_buf,
            cache,
        )?;

        // Copy the requested range from the decompressed data
        let cblock_remaining = compressed_block_size - offset_in_cblock;
        let chunk = (to_read - bytes_read).min(cblock_remaining);
        buf[bytes_read..bytes_read + chunk]
            .copy_from_slice(&cblock_buf[offset_in_cblock..offset_in_cblock + chunk]);
        bytes_read += chunk;
        current_pos += chunk as u64;
    }

    Ok(bytes_read)
}

/// Write bytes to a compressed stream. May grow the pyramid.
fn pyramid_write_compressed<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    pos: u64,
    buf: &[u8],
    compressed_block_size: usize,
    block_index_width: u8,
    cache: CacheMode,
) -> Result<usize, YakError> {
    let fan_out = compute_fan_out(layer2);
    if buf.is_empty() {
        return Ok(0);
    }

    let block_size = layer2.block_size();
    let end_pos = pos + buf.len() as u64;

    // Ensure tree has enough capacity for the write endpoint
    let comp_ops = CompressedOps {
        block_size,
        compressed_block_size,
        block_index_width,
    };
    pyramid_reserve(
        layer2,
        descriptor,
        end_pos.max(descriptor.size),
        block_size,
        block_index_width,
        &comp_ops,
        cache,
    )?;

    let old_size = descriptor.size;
    let capacity = descriptor.reserved.max(descriptor.size);
    let num_cblocks = compressed_blocks_needed(capacity, compressed_block_size);
    let depth = pyramid_depth(num_cblocks, fan_out);
    let sentinel = block_sentinel(block_index_width);
    let biw = block_index_width;

    let mut bytes_written = 0;
    let mut current_pos = pos;

    // Reusable buffers — allocated once, reused across loop iterations
    let mut cblock_buf = vec![0u8; compressed_block_size];
    let mut compressed_read_buf: Vec<u8> = Vec::new();
    let mut compress_ws = CompressWorkspace::new();
    let mut leaf_cache = LeafCache::new(block_size);

    while bytes_written < buf.len() {
        let cblock_idx = current_pos / compressed_block_size as u64;
        let offset_in_cblock = (current_pos % compressed_block_size as u64) as usize;

        let redir_block = resolve_redir_block(
            layer2,
            descriptor,
            cblock_idx,
            depth,
            biw,
            &mut leaf_cache,
            cache,
        )?;

        if redir_block == sentinel {
            return Err(YakError::IoError(format!(
                "compressed write: no redirector at cblock {}",
                cblock_idx
            )));
        }

        // Read and decompress existing compressed block data
        let old_redir = read_comp_redir(layer2, redir_block, block_size, biw, cache)?;
        decompress_cblock_data(
            layer2,
            &old_redir,
            block_size,
            &mut compressed_read_buf,
            &mut cblock_buf,
        )?;

        // Overlay the write data
        let cblock_remaining = compressed_block_size - offset_in_cblock;
        let chunk = (buf.len() - bytes_written).min(cblock_remaining);
        cblock_buf[offset_in_cblock..offset_in_cblock + chunk]
            .copy_from_slice(&buf[bytes_written..bytes_written + chunk]);

        // Determine the valid extent for compression: how much of this cblock
        // actually contains stream data (not trailing zeros beyond stream end)
        let cblock_start = cblock_idx * compressed_block_size as u64;
        let stream_size_after = end_pos.max(old_size);
        let valid_extent = if cblock_start + compressed_block_size as u64 <= stream_size_after {
            compressed_block_size
        } else {
            (stream_size_after - cblock_start) as usize
        };

        // Compress and write back, reconciling physical blocks
        compress_cblock_data(
            layer2,
            redir_block,
            &old_redir,
            &cblock_buf[..valid_extent],
            biw,
            &mut compress_ws,
            cache,
        )?;

        bytes_written += chunk;
        current_pos += chunk as u64;
    }

    // Update size
    let actual_end = pos + bytes_written as u64;
    descriptor.size = actual_end.max(old_size);

    Ok(bytes_written)
}

// ---------------------------------------------------------------------------
// Streams stream I/O helpers
// ---------------------------------------------------------------------------

impl<L2: BlockLayer> StreamsFromBlocks<L2> {
    fn block_size(&self) -> usize {
        self.layer2.block_size()
    }

    fn fan_out(&self) -> u64 {
        self.layer2.block_size() as u64 / self.layer2.block_index_width() as u64
    }

    fn block_index_width_val(&self) -> u8 {
        self.layer2.block_index_width()
    }

    fn sentinel(&self) -> u64 {
        let w = self.layer2.block_index_width() as u32;
        if w >= 8 {
            u64::MAX
        } else {
            (1u64 << (w * 8)) - 1
        }
    }

    /// Acquire a per-stream lock. If `blocking` is true, waits via Condvar
    /// until the lock can be acquired. If false, returns LockConflict immediately.
    fn acquire_lock(&self, id: u64, mode: OpenMode, blocking: bool) -> Result<(), YakError> {
        let mut state = self.state.lock().unwrap();
        loop {
            let lock = state.locks.entry(id).or_default();
            match mode {
                OpenMode::Read => {
                    if !lock.has_writer {
                        lock.readers += 1;
                        return Ok(());
                    }
                }
                OpenMode::Write => {
                    if !lock.has_writer && lock.readers == 0 {
                        lock.has_writer = true;
                        return Ok(());
                    }
                }
            }
            if !blocking {
                // Clean up the default entry if we just created it
                let lock = state.locks.get(&id).unwrap();
                if lock.readers == 0 && !lock.has_writer {
                    state.locks.remove(&id);
                }
                return Err(YakError::LockConflict(format!(
                    "lock conflict on stream {}",
                    id
                )));
            }
            state = self.lock_released.wait(state).unwrap();
        }
    }

    /// Release a per-stream lock and notify all waiters.
    fn release_lock(&self, id: u64, mode: OpenMode) {
        let mut state = self.state.lock().unwrap();
        if let Some(lock) = state.locks.get_mut(&id) {
            match mode {
                OpenMode::Read => lock.readers = lock.readers.saturating_sub(1),
                OpenMode::Write => lock.has_writer = false,
            }
            if lock.readers == 0 && !lock.has_writer {
                state.locks.remove(&id);
            }
        }
        drop(state);
        self.lock_released.notify_all();
    }

    /// Open a stream, optionally blocking on lock contention.
    fn open_stream_inner(
        &self,
        id: u64,
        mode: OpenMode,
        blocking: bool,
    ) -> Result<BlockStreamHandle, YakError> {
        if id == STREAMS_STREAM_ID {
            return Err(YakError::InvalidPath(
                "cannot open Streams stream directly".to_string(),
            ));
        }

        // 1. Clear stale thread-local cache entries. Another thread may have
        //    modified this stream's blocks since our last access (e.g. a shared
        //    directory stream).
        self.layer2.invalidate_thread_local_cache();

        // 2. Acquire per-stream lock
        self.acquire_lock(id, mode, blocking)?;

        // 2. Read the stream's descriptor (acquire STREAMS read lock, always blocking)
        if let Err(e) = self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true) {
            self.release_lock(id, mode);
            return Err(e);
        }
        let desc_result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            self.read_descriptor(&streams_desc, id)
        };
        self.release_lock(STREAMS_STREAM_ID, OpenMode::Read);

        let desc = match desc_result {
            Ok(d) => d,
            Err(e) => {
                self.release_lock(id, mode);
                return Err(e);
            }
        };

        if desc.is_free() {
            self.release_lock(id, mode);
            return Err(YakError::NotFound(format!("stream {}", id)));
        }

        // 3. Create handle
        let mut state = self.state.lock().unwrap();
        let handle_id = state.next_handle_id;
        state.next_handle_id += 1;
        state.open_handles.insert(
            handle_id,
            HandleInfo {
                stream_id: id,
                mode,
                cached_descriptor: desc,
            },
        );

        Ok(BlockStreamHandle(handle_id))
    }

    /// Read a stream descriptor from the Streams stream.
    /// Caller must hold at least a read lock on STREAMS_STREAM_ID.
    fn read_descriptor(
        &self,
        streams_desc: &StreamDescriptor,
        stream_id: u64,
    ) -> Result<StreamDescriptor, YakError> {
        let offset = descriptor_offset(stream_id);
        if offset + DESCRIPTOR_SIZE > streams_desc.size {
            return Err(YakError::NotFound(format!("stream {}", stream_id)));
        }
        let mut buf = [0u8; DESCRIPTOR_SIZE as usize];
        let bs = self.block_size();
        let biw = self.block_index_width_val();
        pyramid_read(
            &self.layer2,
            streams_desc,
            offset,
            &mut buf,
            bs,
            biw,
            CacheMode::Shared,
        )?;
        Ok(StreamDescriptor::from_bytes(&buf))
    }

    /// Write a stream descriptor to the Streams stream.
    /// Caller must hold the write lock on STREAMS_STREAM_ID.
    fn write_descriptor(
        &self,
        streams_desc: &mut StreamDescriptor,
        stream_id: u64,
        desc: &StreamDescriptor,
    ) -> Result<(), YakError> {
        let offset = descriptor_offset(stream_id);
        let buf = desc.to_bytes();
        let bs = self.block_size();
        let biw = self.block_index_width_val();
        pyramid_write(
            &self.layer2,
            streams_desc,
            offset,
            &buf,
            bs,
            biw,
            CacheMode::Shared,
        )?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Free-list helpers
    // -----------------------------------------------------------------------

    /// Write the free-list head to the first 8 bytes of the Streams stream.
    /// Caller must hold the STREAMS_STREAM_ID write lock.
    fn write_free_list_head(
        &self,
        streams_desc: &mut StreamDescriptor,
        head: u64,
    ) -> Result<(), YakError> {
        let bs = self.block_size();
        let biw = self.block_index_width_val();
        pyramid_write(
            &self.layer2,
            streams_desc,
            0,
            &head.to_le_bytes(),
            bs,
            biw,
            CacheMode::Shared,
        )?;
        Ok(())
    }

    /// Pop a free descriptor from the free list.
    /// Returns `Some(stream_id)` or `None` if the list is empty.
    /// Updates the cached head and writes the new head to disk.
    /// Caller must hold the STREAMS_STREAM_ID write lock.
    fn pop_free_descriptor(
        &self,
        streams_desc: &mut StreamDescriptor,
        cached_head: &mut u64,
    ) -> Result<Option<u64>, YakError> {
        if *cached_head == FREE_LIST_EMPTY {
            return Ok(None);
        }

        let slot = *cached_head;
        // Read the descriptor at the head to get the next-free pointer (stored in `size`)
        let desc = self.read_descriptor(streams_desc, slot)?;
        let next = desc.size; // next-free index (or FREE_LIST_EMPTY)

        *cached_head = next;
        self.write_free_list_head(streams_desc, next)?;

        Ok(Some(slot))
    }

    /// Extend the Streams stream by a batch of free descriptors (roughly one
    /// L2 block's worth, but at least one), chaining them into the free list.
    /// Lowest IDs are allocated first (LIFO: the lowest new ID becomes the
    /// free-list head). Caller must hold the STREAMS_STREAM_ID write lock.
    fn bulk_extend_free_list(
        &self,
        streams_desc: &mut StreamDescriptor,
        cached_head: &mut u64,
    ) -> Result<(), YakError> {
        let bs = self.block_size();
        let biw = self.block_index_width_val();

        // At least 1 descriptor per batch — pyramid_write handles cross-block spans,
        // so this works even when block_size < DESCRIPTOR_SIZE.
        let descriptors_per_block = ((bs as u64) / DESCRIPTOR_SIZE).max(1);
        let base = num_descriptor_slots(streams_desc.size);

        // Cap at sentinel to avoid stream ID overflow
        let max_new = self.sentinel().saturating_sub(base);
        let count = descriptors_per_block.min(max_new);
        if count == 0 {
            return Err(YakError::IoError(format!(
                "stream ID overflow: no room for new descriptors (base={}, sentinel={})",
                base,
                self.sentinel()
            )));
        }

        // Build a buffer of `count` free descriptors, chained lowest→highest.
        // Each slot points to the next: base→base+1→…→base+count-1→old_head.
        let buf_len = count as usize * DESCRIPTOR_SIZE as usize;
        let mut buf = vec![0u8; buf_len];
        let free_desc = StreamDescriptor {
            size: 0, // patched per-slot below
            top_block: FREE_DESCRIPTOR_MARKER,
            reserved: 0,
            flags: 0,
        };
        for i in 0..count {
            let mut desc = free_desc;
            desc.size = base + i + 1; // next in chain
            let off = i as usize * DESCRIPTOR_SIZE as usize;
            buf[off..off + DESCRIPTOR_SIZE as usize].copy_from_slice(&desc.to_bytes());
        }
        // Patch the last descriptor to link back to the old free-list head
        let last_off = (count - 1) as usize * DESCRIPTOR_SIZE as usize;
        let mut tail = free_desc;
        tail.size = *cached_head;
        buf[last_off..last_off + DESCRIPTOR_SIZE as usize].copy_from_slice(&tail.to_bytes());

        // Single pyramid_write for the entire batch
        let write_offset = descriptor_offset(base);
        pyramid_write(
            &self.layer2,
            streams_desc,
            write_offset,
            &buf,
            bs,
            biw,
            CacheMode::Shared,
        )?;

        // Update free-list head to the first new slot
        *cached_head = base;
        self.write_free_list_head(streams_desc, base)?;

        Ok(())
    }

    /// Serialize the L3 header (no length prefix).
    /// Format: | "pyra  ": [u8;6] | version: u8 | size: u64 | top_block: u64 | reserved: u64 | cbss: u8 |
    fn serialize_header(streams_desc: &StreamDescriptor, cbss: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L3_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(b"pyra  ");
        buf.push(3); // version 3: descriptor free-list (8-byte prefix in Streams stream)
        buf.extend_from_slice(&streams_desc.size.to_le_bytes());
        buf.extend_from_slice(&streams_desc.top_block.to_le_bytes());
        buf.extend_from_slice(&streams_desc.reserved.to_le_bytes());
        buf.push(cbss);
        buf
    }

    /// Deserialize the L3 header (no length prefix).
    /// Input: 32 bytes — identifier starts at byte 0.
    /// Returns (streams_descriptor, compressed_block_size_shift).
    fn deserialize_header(data: &[u8]) -> Result<(StreamDescriptor, u8), YakError> {
        if data.len() < L3_PAYLOAD_SIZE as usize {
            return Err(YakError::IoError(format!(
                "L3 payload too short: {} < {}",
                data.len(),
                L3_PAYLOAD_SIZE
            )));
        }
        if &data[0..6] != b"pyra  " {
            return Err(YakError::IoError(format!(
                "expected L3 identifier 'pyra  ', got '{}'",
                String::from_utf8_lossy(&data[0..6])
            )));
        }
        let version = data[6];
        if version != 3 {
            return Err(YakError::IoError(format!(
                "unsupported L3 version: {} (expected 3; re-create the file)",
                version
            )));
        }
        // payload[7..15] = streams_size, [15..23] = streams_top_block,
        // [23..31] = streams_reserved, [31] = cbss
        let streams_size = u64::from_le_bytes(data[7..15].try_into().unwrap());
        let streams_top_block = u64::from_le_bytes(data[15..23].try_into().unwrap());
        let streams_reserved = u64::from_le_bytes(data[23..31].try_into().unwrap());
        let cbss = data[31];
        Ok((
            StreamDescriptor {
                size: streams_size,
                top_block: streams_top_block,
                reserved: streams_reserved,
                flags: 0, // Streams-stream descriptor doesn't use flags
            },
            cbss,
        ))
    }

    /// Persist the L3 header payload to L2 via its slot.
    /// Caller must hold the STREAMS_STREAM_ID lock.
    fn persist_l3_header(&self, streams_desc: &StreamDescriptor) -> Result<(), YakError> {
        let payload = Self::serialize_header(streams_desc, self.compressed_block_size_shift);
        self.layer2.write_header_slot(self.my_slot, &payload)
    }
}

// ---------------------------------------------------------------------------
// StreamLayer implementation
// ---------------------------------------------------------------------------

impl<L2: BlockLayer> StreamLayer for StreamsFromBlocks<L2> {
    type Handle = BlockStreamHandle;

    fn create(
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
        compressed_block_size_shift: u8,
        mut slot_sizes: VecDeque<u16>,
        password: Option<&[u8]>,
    ) -> Result<Self, YakError>
    where
        Self: Sized,
    {
        // Push L3 payload size to front (on-disk order: L3 before L4)
        slot_sizes.push_front(L3_PAYLOAD_SIZE);

        let layer2 = L2::create(
            path,
            block_size_shift,
            block_index_width,
            slot_sizes,
            password,
        )?;
        let my_slot = layer2.header_slot_for_upper(0);

        // Write initial L3 payload
        let mut streams_desc = StreamDescriptor {
            size: 0,
            top_block: 0,
            reserved: 0,
            flags: 0,
        };

        // Write free-list head prefix (8 bytes, FREE_LIST_EMPTY = no free slots yet)
        let bs = 1usize << block_size_shift;
        let biw = block_index_width;
        pyramid_write(
            &layer2,
            &mut streams_desc,
            0,
            &FREE_LIST_EMPTY.to_le_bytes(),
            bs,
            biw,
            CacheMode::Shared,
        )?;

        let l3_payload = Self::serialize_header(&streams_desc, compressed_block_size_shift);
        layer2.write_header_slot(my_slot, &l3_payload)?;

        Ok(StreamsFromBlocks {
            layer2,
            streams_descriptor: Mutex::new(streams_desc),
            my_slot,
            compressed_block_size_shift,
            state: Mutex::new(StreamsFromBlocksState {
                next_handle_id: 0,
                locks: HashMap::new(),
                open_handles: HashMap::new(),
            }),
            lock_released: Condvar::new(),
            free_list_head: Mutex::new(FREE_LIST_EMPTY),
        })
    }

    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError>
    where
        Self: Sized,
    {
        let layer2 = L2::open(path, mode, password)?;
        let my_header_slot = layer2.header_slot_for_upper(0);

        // Read and parse L3 payload
        let header_buffer = layer2.read_header_slot(my_header_slot)?;
        let (streams_desc, cbss) = Self::deserialize_header(&header_buffer)?;

        // Read free-list head from the first 8 bytes of the Streams stream
        let free_head = if streams_desc.size >= STREAMS_HEADER_SIZE {
            let bs = layer2.block_size();
            let biw = layer2.block_index_width();
            let mut buf = [0u8; STREAMS_HEADER_SIZE as usize];
            pyramid_read(
                &layer2,
                &streams_desc,
                0,
                &mut buf,
                bs,
                biw,
                CacheMode::Shared,
            )?;
            u64::from_le_bytes(buf)
        } else {
            FREE_LIST_EMPTY
        };

        Ok(StreamsFromBlocks {
            layer2,
            streams_descriptor: Mutex::new(streams_desc),
            my_slot: my_header_slot,
            compressed_block_size_shift: cbss,
            state: Mutex::new(StreamsFromBlocksState {
                next_handle_id: 0,
                locks: HashMap::new(),
                open_handles: HashMap::new(),
            }),
            lock_released: Condvar::new(),
            free_list_head: Mutex::new(free_head),
        })
    }

    fn block_index_width(&self) -> u8 {
        self.layer2.block_index_width()
    }

    fn block_size_shift(&self) -> u8 {
        self.layer2.block_size_shift()
    }

    fn compressed_block_size_shift(&self) -> u8 {
        self.compressed_block_size_shift
    }

    fn is_encrypted(&self) -> bool {
        self.layer2.is_encrypted()
    }

    fn create_stream(&self, compressed: bool) -> Result<u64, YakError> {
        if compressed && self.compressed_block_size_shift == 0 {
            return Err(YakError::IoError(
                "compression not configured for this filesystem".to_string(),
            ));
        }

        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;

        let result = (|| -> Result<u64, YakError> {
            let mut streams_desc = self.streams_descriptor.lock().unwrap();
            let mut head = self.free_list_head.lock().unwrap();

            // Pop from free list; if empty, bulk-extend first
            let stream_id = match self.pop_free_descriptor(&mut streams_desc, &mut head)? {
                Some(id) => id,
                None => {
                    self.bulk_extend_free_list(&mut streams_desc, &mut head)?;
                    // Must succeed now — we just added slots
                    self.pop_free_descriptor(&mut streams_desc, &mut head)?
                        .expect("bulk_extend_free_list added slots but free list is still empty")
                }
            };

            let flags = if compressed {
                STREAM_FLAG_COMPRESSED
            } else {
                0
            };
            let new_desc = StreamDescriptor {
                size: 0,
                top_block: 0,
                reserved: 0,
                flags,
            };
            self.write_descriptor(&mut streams_desc, stream_id, &new_desc)?;

            self.persist_l3_header(&streams_desc)?;

            Ok(stream_id)
        })();

        self.release_lock(STREAMS_STREAM_ID, OpenMode::Write);
        result
    }

    fn stream_exists(&self, id: u64) -> bool {
        if self
            .acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true)
            .is_err()
        {
            return false;
        }
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let offset = descriptor_offset(id);
            if offset + DESCRIPTOR_SIZE > streams_desc.size {
                false
            } else {
                match self.read_descriptor(&streams_desc, id) {
                    Ok(desc) => !desc.is_free(),
                    Err(_) => false,
                }
            }
        };
        self.release_lock(STREAMS_STREAM_ID, OpenMode::Read);
        result
    }

    fn stream_count(&self) -> Result<u64, YakError> {
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true)?;
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let num_slots = num_descriptor_slots(streams_desc.size);
            let mut count = 0u64;
            for i in 0..num_slots {
                let desc = self.read_descriptor(&streams_desc, i)?;
                if !desc.is_free() {
                    count += 1;
                }
            }
            Ok(count)
        };
        self.release_lock(STREAMS_STREAM_ID, OpenMode::Read);
        result
    }

    fn stream_ids(&self) -> Result<Vec<u64>, YakError> {
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true)?;
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let num_slots = num_descriptor_slots(streams_desc.size);
            let mut ids = Vec::new();
            for i in 0..num_slots {
                let desc = self.read_descriptor(&streams_desc, i)?;
                if !desc.is_free() {
                    ids.push(i);
                }
            }
            Ok(ids)
        };
        self.release_lock(STREAMS_STREAM_ID, OpenMode::Read);
        result
    }

    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError> {
        self.open_stream_inner(id, mode, false)
    }

    fn open_stream_blocking(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError> {
        self.open_stream_inner(id, mode, true)
    }

    fn close_stream(&self, handle: Self::Handle) -> Result<(), YakError> {
        let info = {
            let mut state = self.state.lock().unwrap();
            state
                .open_handles
                .remove(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?
        };

        // If writer, flush cached descriptor back to Streams stream.
        // Locks must be released even if the flush fails, to avoid deadlocking
        // other threads waiting on STREAMS_STREAM_ID or this stream.
        let flush_result = if info.mode == OpenMode::Write {
            self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)
                .and_then(|()| {
                    let result = {
                        let mut streams_desc = self.streams_descriptor.lock().unwrap();
                        self.write_descriptor(
                            &mut streams_desc,
                            info.stream_id,
                            &info.cached_descriptor,
                        )
                        .and_then(|()| self.persist_l3_header(&streams_desc))
                    };
                    self.release_lock(STREAMS_STREAM_ID, OpenMode::Write);
                    result
                })
        } else {
            Ok(())
        };

        // Always release per-stream lock, regardless of flush outcome
        self.release_lock(info.stream_id, info.mode);

        flush_result
    }

    fn delete_stream(&self, id: u64) -> Result<(), YakError> {
        // Clear stale thread-local cache before traversing the stream's
        // pyramid tree — blocks may have been recycled since our last access.
        self.layer2.invalidate_thread_local_cache();

        // Check that stream is not currently open
        {
            let state = self.state.lock().unwrap();
            if let Some(lock) = state.locks.get(&id) {
                if lock.has_writer || lock.readers > 0 {
                    return Err(YakError::LockConflict(format!(
                        "cannot delete open stream {}",
                        id
                    )));
                }
            }
        }

        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;

        let result = (|| -> Result<(), YakError> {
            let mut streams_desc = self.streams_descriptor.lock().unwrap();
            let desc = self.read_descriptor(&streams_desc, id)?;
            if desc.is_free() {
                return Err(YakError::NotFound(format!("stream {}", id)));
            }

            // Deallocate all blocks belonging to the stream
            let capacity = desc.reserved.max(desc.size);
            if capacity > 0 && desc.top_block != 0 {
                let bs = self.block_size();
                let fo = self.fan_out();
                let biw = self.block_index_width_val();
                if desc.is_compressed() {
                    let cbs = 1usize << self.compressed_block_size_shift;
                    let ops = CompressedOps {
                        block_size: bs,
                        compressed_block_size: cbs,
                        block_index_width: biw,
                    };
                    let num_cblocks = compressed_blocks_needed(capacity, cbs);
                    let depth = pyramid_depth(num_cblocks, fo);
                    deallocate_tree(
                        &self.layer2,
                        desc.top_block,
                        depth,
                        biw,
                        &ops,
                        CacheMode::ThreadLocal,
                    )?;
                } else {
                    let ops = UncompressedOps { block_size: bs };
                    let num_blocks = data_blocks_needed(capacity, bs);
                    let depth = pyramid_depth(num_blocks, fo);
                    deallocate_tree(
                        &self.layer2,
                        desc.top_block,
                        depth,
                        biw,
                        &ops,
                        CacheMode::ThreadLocal,
                    )?;
                }
            }

            // Push descriptor onto free list
            let mut head = self.free_list_head.lock().unwrap();
            let free_desc = StreamDescriptor {
                size: *head, // next-free pointer
                top_block: FREE_DESCRIPTOR_MARKER,
                reserved: 0,
                flags: 0,
            };
            self.write_descriptor(&mut streams_desc, id, &free_desc)?;
            *head = id;
            self.write_free_list_head(&mut streams_desc, id)?;
            self.persist_l3_header(&streams_desc)?;

            Ok(())
        })();

        self.release_lock(STREAMS_STREAM_ID, OpenMode::Write);
        result
    }

    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, YakError> {
        let desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.cached_descriptor
        };

        let bs = self.block_size();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_read_compressed(
                &self.layer2,
                &desc,
                pos,
                buf,
                cbs,
                biw,
                CacheMode::ThreadLocal,
            )
        } else {
            pyramid_read(
                &self.layer2,
                &desc,
                pos,
                buf,
                bs,
                biw,
                CacheMode::ThreadLocal,
            )
        }
    }

    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, YakError> {
        let (stream_id, mut desc, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(YakError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            (info.stream_id, info.cached_descriptor, info.mode)
        };

        let _ = mode; // used for the check above
        let _ = stream_id; // might be useful for debugging

        let bs = self.block_size();
        let biw = self.block_index_width_val();
        let n = if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_write_compressed(
                &self.layer2,
                &mut desc,
                pos,
                buf,
                cbs,
                biw,
                CacheMode::ThreadLocal,
            )?
        } else {
            pyramid_write(
                &self.layer2,
                &mut desc,
                pos,
                buf,
                bs,
                biw,
                CacheMode::ThreadLocal,
            )?
        };

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(n)
    }

    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, YakError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.size)
    }

    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), YakError> {
        let mut desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(YakError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.cached_descriptor
        };

        let bs = self.block_size();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            let ops = CompressedOps {
                block_size: bs,
                compressed_block_size: cbs,
                block_index_width: biw,
            };
            pyramid_truncate(
                &self.layer2,
                &mut desc,
                new_len,
                biw,
                &ops,
                CacheMode::ThreadLocal,
            )?;
        } else {
            let ops = UncompressedOps { block_size: bs };
            pyramid_truncate(
                &self.layer2,
                &mut desc,
                new_len,
                biw,
                &ops,
                CacheMode::ThreadLocal,
            )?;
        }

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(())
    }

    fn reserve(&self, handle: &Self::Handle, n_bytes: u64) -> Result<(), YakError> {
        let mut desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(YakError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.cached_descriptor
        };

        let bs = self.block_size();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            let ops = CompressedOps {
                block_size: bs,
                compressed_block_size: cbs,
                block_index_width: biw,
            };
            pyramid_reserve(
                &self.layer2,
                &mut desc,
                n_bytes,
                bs,
                biw,
                &ops,
                CacheMode::ThreadLocal,
            )?;
        } else {
            let ops = UncompressedOps { block_size: bs };
            pyramid_reserve(
                &self.layer2,
                &mut desc,
                n_bytes,
                bs,
                biw,
                &ops,
                CacheMode::ThreadLocal,
            )?;
        }

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(())
    }

    fn stream_reserved(&self, handle: &Self::Handle) -> Result<u64, YakError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.reserved)
    }

    fn is_stream_compressed(&self, handle: &Self::Handle) -> Result<bool, YakError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.is_compressed())
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Slot 0 = L3's own, so upper layer index 0 → slot 1 (L4), etc.
        self.layer2.header_slot_for_upper(index + 1)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError> {
        self.layer2.write_header_slot(slot, data)
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError> {
        self.layer2.read_header_slot(slot)
    }

    fn verify(&self, claimed_streams: &[u64]) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();
        let mut all_claimed_blocks: Vec<u64> = Vec::new();

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();

        // 1. Clone the Streams stream descriptor (out-of-band)
        let streams_desc = *self.streams_descriptor.lock().unwrap();

        // 2. Collect blocks belonging to the Streams stream itself
        if streams_desc.size > 0 {
            let uc_ops = UncompressedOps { block_size: bs };
            let num_blocks = data_blocks_needed(streams_desc.size, bs);
            let depth = pyramid_depth(num_blocks, fo);
            let mut collector = TreeCollector {
                blocks: &mut all_claimed_blocks,
                issues: &mut issues,
                label: "Streams-stream",
            };
            collect_tree_blocks(
                &self.layer2,
                streams_desc.top_block,
                depth,
                biw,
                &mut collector,
                &uc_ops,
                CacheMode::Shared,
            );
        }

        // 3. Read all stream descriptors, build set of active stream IDs
        let num_slots = num_descriptor_slots(streams_desc.size);
        let claimed_set: std::collections::HashSet<u64> = claimed_streams.iter().cloned().collect();
        let mut active_on_disk = std::collections::HashSet::new();

        for i in 0..num_slots {
            match self.read_descriptor(&streams_desc, i) {
                Ok(desc) => {
                    if !desc.is_free() {
                        active_on_disk.insert(i);

                        // Validate descriptor consistency
                        if desc.size == 0 && desc.reserved == 0 && desc.top_block != 0 {
                            issues.push(format!(
                                "L3: stream {}: size is 0 but top_block is {} (expected 0)",
                                i, desc.top_block
                            ));
                        }
                        if desc.reserved < desc.size {
                            issues.push(format!(
                                "L3: stream {}: reserved ({}) < size ({}) — invariant violation",
                                i, desc.reserved, desc.size
                            ));
                        }

                        // Collect blocks for this stream's pyramid
                        let capacity = desc.reserved.max(desc.size);
                        if capacity > 0 {
                            let stream_label = i.to_string();
                            let mut collector = TreeCollector {
                                blocks: &mut all_claimed_blocks,
                                issues: &mut issues,
                                label: &stream_label,
                            };
                            if desc.is_compressed() {
                                let cbs = 1usize << self.compressed_block_size_shift;
                                let ops = CompressedOps {
                                    block_size: bs,
                                    compressed_block_size: cbs,
                                    block_index_width: biw,
                                };
                                let num_cblocks = compressed_blocks_needed(capacity, cbs);
                                let depth = pyramid_depth(num_cblocks, fo);
                                collect_tree_blocks(
                                    &self.layer2,
                                    desc.top_block,
                                    depth,
                                    biw,
                                    &mut collector,
                                    &ops,
                                    CacheMode::ThreadLocal,
                                );
                            } else {
                                let uc_ops = UncompressedOps { block_size: bs };
                                let num_blocks = data_blocks_needed(capacity, bs);
                                let depth = pyramid_depth(num_blocks, fo);
                                collect_tree_blocks(
                                    &self.layer2,
                                    desc.top_block,
                                    depth,
                                    biw,
                                    &mut collector,
                                    &uc_ops,
                                    CacheMode::ThreadLocal,
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    issues.push(format!(
                        "L3: error reading descriptor for stream {}: {}",
                        i, e
                    ));
                }
            }
        }

        // 4. Cross-check: streams claimed by L4 that don't exist at L3
        for &stream_id in claimed_streams {
            if !active_on_disk.contains(&stream_id) {
                issues.push(format!(
                    "L3: stream {} claimed by L4 but not found in stream descriptors",
                    stream_id
                ));
            }
        }

        // 5. Cross-check: active streams at L3 that L4 doesn't claim
        for &stream_id in &active_on_disk {
            if !claimed_set.contains(&stream_id) {
                issues.push(format!(
                    "L3: stream {} exists in stream descriptors but is not claimed by L4 (orphaned stream)",
                    stream_id
                ));
            }
        }

        // 6. Verify free-list integrity
        {
            let mut free_on_list = std::collections::HashSet::new();
            let mut free_by_scan = std::collections::HashSet::new();

            // Collect all descriptors marked free by the linear scan above
            for i in 0..num_slots {
                if !active_on_disk.contains(&i) {
                    // Must be free (if read succeeded; errors already reported above)
                    if let Ok(desc) = self.read_descriptor(&streams_desc, i) {
                        if desc.is_free() {
                            free_by_scan.insert(i);
                        }
                    }
                }
            }

            // Walk the free list chain from the on-disk head
            if streams_desc.size >= STREAMS_HEADER_SIZE {
                let mut head_buf = [0u8; STREAMS_HEADER_SIZE as usize];
                if pyramid_read(
                    &self.layer2,
                    &streams_desc,
                    0,
                    &mut head_buf,
                    bs,
                    biw,
                    CacheMode::Shared,
                )
                .is_ok()
                {
                    let mut cursor = u64::from_le_bytes(head_buf);
                    while cursor != FREE_LIST_EMPTY {
                        if cursor >= num_slots {
                            issues.push(format!(
                                "L3: free-list entry {} is out of range (num_slots={})",
                                cursor, num_slots
                            ));
                            break;
                        }
                        if !free_on_list.insert(cursor) {
                            issues.push(format!("L3: free-list cycle detected at slot {}", cursor));
                            break;
                        }
                        match self.read_descriptor(&streams_desc, cursor) {
                            Ok(desc) => {
                                if !desc.is_free() {
                                    issues.push(format!(
                                        "L3: slot {} is on free list but top_block is not FREE_DESCRIPTOR_MARKER",
                                        cursor
                                    ));
                                    break;
                                }
                                cursor = desc.size; // next-free pointer
                            }
                            Err(e) => {
                                issues.push(format!(
                                    "L3: error reading free-list entry {}: {}",
                                    cursor, e
                                ));
                                break;
                            }
                        }
                    }
                }
            }

            // Cross-check: every free descriptor must be on the chain
            for &slot in &free_by_scan {
                if !free_on_list.contains(&slot) {
                    issues.push(format!(
                        "L3: slot {} is marked free but not reachable from free list",
                        slot
                    ));
                }
            }
            for &slot in &free_on_list {
                if !free_by_scan.contains(&slot) {
                    issues.push(format!(
                        "L3: slot {} is on free list but not marked free in descriptor",
                        slot
                    ));
                }
            }

            // Count check
            let total = active_on_disk.len() + free_by_scan.len();
            if total as u64 != num_slots {
                issues.push(format!(
                    "L3: active ({}) + free ({}) = {} but num_slots = {}",
                    active_on_disk.len(),
                    free_by_scan.len(),
                    total,
                    num_slots
                ));
            }
        }

        // 7. Pass all collected blocks to L2 for verification
        issues.extend(self.layer2.verify(&all_claimed_blocks)?);

        Ok(issues)
    }
}
