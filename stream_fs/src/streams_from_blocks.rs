use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::block_layer::BlockLayer;
use crate::stream_layer::StreamLayer;
use crate::{HeaderSlotId, OpenMode, SfsError};

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
) -> Result<u64, SfsError> {
    let biw = block_index_width as usize;
    let offset = (slot as usize) * biw;
    let mut buf = [0u8; 8];
    layer2.read_block(redirector_block, offset, &mut buf[..biw], true)?;
    Ok(u64::from_le_bytes(buf))
}

/// Write a block index to a redirector block at the given slot.
fn write_block_index<L2: BlockLayer>(
    layer2: &L2,
    redirector_block: u64,
    slot: u64,
    index: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let biw = block_index_width as usize;
    let offset = (slot as usize) * biw;
    let bytes = index.to_le_bytes();
    layer2.write_block(redirector_block, offset, &bytes[..biw], true)?;
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
) -> Result<usize, SfsError> {
    let _ = block_index_width; // sentinel is always all-0xFF regardless of width
    let buf = vec![0xFFu8; block_size];
    layer2.write_block(block, 0, &buf, true)
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
) -> Result<u64, SfsError> {
    if depth <= 1 {
        return Ok(descriptor.top_block);
    }

    let mut current_block = descriptor.top_block;
    let mut remaining_idx = data_block_idx;

    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;

        let child = read_block_index(layer2, current_block, slot, block_index_width)?;
        if child == block_sentinel(block_index_width) {
            return Err(SfsError::IoError(format!(
                "navigate_to_leaf: invalid block at depth {}, slot {}",
                level, slot
            )));
        }
        current_block = child;
    }

    Ok(current_block)
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
) -> Result<(u64, u64), SfsError> {
    let biw = block_index_width as usize;
    let offset = start_slot as usize * biw;
    let sentinel = block_sentinel(block_index_width);

    // Decode the first index
    let mut idx_buf = [0u8; 8];
    idx_buf[..biw].copy_from_slice(&leaf_buf[offset..offset + biw]);
    let first_block = u64::from_le_bytes(idx_buf);

    if first_block == sentinel {
        return Err(SfsError::IoError(format!(
            "scan_run_from_buffer: sentinel at slot {}",
            start_slot
        )));
    }

    // Scan forward for consecutive physical block indices
    let mut run_length: u64 = 1;
    let slots_to_scan = max_slots as usize;
    for i in 1..slots_to_scan {
        let pos = offset + i * biw;
        idx_buf = [0u8; 8];
        idx_buf[..biw].copy_from_slice(&leaf_buf[pos..pos + biw]);
        let idx = u64::from_le_bytes(idx_buf);
        if idx != first_block + run_length || idx == sentinel {
            break;
        }
        run_length += 1;
    }

    Ok((first_block, run_length))
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
    fan_out: u64,
    block_index_width: u8,
    depth: u32,
) -> Result<(), SfsError> {
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

            let mut child = read_block_index(layer2, current_block, slot, block_index_width)?;
            if child == sentinel {
                // Allocate new intermediate redirector, fill with sentinels
                child = layer2.allocate_block()?;
                fill_sentinel(layer2, child, block_size, block_index_width)?;
                write_block_index(layer2, current_block, slot, child, block_index_width)?;
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
        layer2.write_block(current_block, offset, &buf, true)?;

        i += batch_size;
    }

    Ok(())
}

/// Ensure the pyramid has enough blocks allocated to cover `target_data_blocks`.
/// Grows the pyramid as needed (increasing depth, allocating redirectors and data blocks).
/// Returns the (possibly updated) descriptor.
fn ensure_capacity<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    target_data_blocks: u64,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if target_data_blocks == 0 {
        return Ok(());
    }

    let current_data_blocks = data_blocks_needed(descriptor.reserved, block_size);
    let current_depth = pyramid_depth(current_data_blocks, fan_out);
    let target_depth = pyramid_depth(target_data_blocks, fan_out);

    // Handle empty stream: allocate first block
    if descriptor.reserved == 0 && descriptor.top_block == 0 {
        if target_depth == 0 {
            // Just need a single data block
            let block = layer2.allocate_block()?;
            descriptor.top_block = block;
            return Ok(());
        } else {
            // Need a redirector tree; start with one data block, then grow depth
            let block = layer2.allocate_block()?;
            descriptor.top_block = block;
            // current state: depth 0, 1 data block. Grow depth below.
        }
    }

    // Grow depth if needed: wrap current top in new redirector layers
    let effective_current_depth = if descriptor.reserved == 0 && current_data_blocks == 0 {
        // We just allocated a top block above, so we're at depth 0
        0
    } else {
        current_depth
    };

    let mut current_top = descriptor.top_block;
    for _ in effective_current_depth..target_depth {
        let new_redirector = layer2.allocate_block()?;
        // Fill entire redirector with sentinels, then set slot 0 to old top
        fill_sentinel(layer2, new_redirector, block_size, block_index_width)?;
        write_block_index(layer2, new_redirector, 0, current_top, block_index_width)?;
        current_top = new_redirector;
    }
    descriptor.top_block = current_top;

    // Allocate missing data blocks (fill pyramid slots)
    let effective_current = if current_data_blocks == 0 {
        1
    } else {
        current_data_blocks
    };
    let blocks_needed = target_data_blocks - effective_current;
    if blocks_needed > 0 {
        let new_blocks = layer2.allocate_blocks(blocks_needed)?;
        batch_fill_leaf_redirectors(
            layer2,
            descriptor,
            &new_blocks,
            effective_current,
            fan_out,
            block_index_width,
            target_depth,
        )?;
    }

    Ok(())
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
    fan_out: u64,
    block_index_width: u8,
) -> Result<usize, SfsError> {
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
            )?;
            layer2.read_block(leaf_block, 0, &mut leaf_buf, true)?;
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
    fan_out: u64,
    block_index_width: u8,
) -> Result<usize, SfsError> {
    if buf.is_empty() {
        return Ok(0);
    }

    let end_pos = pos + buf.len() as u64;

    // Ensure we have enough blocks for the write endpoint (or current size, whichever is larger)
    pyramid_reserve(
        layer2,
        descriptor,
        end_pos.max(descriptor.size),
        block_size,
        fan_out,
        block_index_width,
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
            )?;
            layer2.read_block(leaf_block, 0, &mut leaf_buf, true)?;
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

/// Truncate a stream to `new_len` bytes. Deallocates unneeded blocks.
fn pyramid_truncate<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    new_len: u64,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if new_len >= descriptor.size {
        return Ok(());
    }

    if new_len == 0 {
        // Deallocate everything
        let capacity = descriptor.reserved.max(descriptor.size);
        if capacity > 0 && descriptor.top_block != 0 {
            let old_blocks = data_blocks_needed(capacity, block_size);
            let old_depth = pyramid_depth(old_blocks, fan_out);
            deallocate_tree(
                layer2,
                descriptor.top_block,
                old_depth,
                fan_out,
                block_index_width,
            )?;
        }
        descriptor.size = 0;
        descriptor.top_block = 0;
        descriptor.reserved = 0;
        return Ok(());
    }

    let capacity = descriptor.reserved.max(descriptor.size);
    let old_blocks = data_blocks_needed(capacity, block_size);
    let new_blocks = data_blocks_needed(new_len, block_size);
    let old_depth = pyramid_depth(old_blocks, fan_out);
    let new_depth = pyramid_depth(new_blocks, fan_out);

    // Deallocate excess data blocks (and their parent redirectors if empty)
    if new_blocks < old_blocks {
        deallocate_excess_blocks(
            layer2,
            descriptor.top_block,
            old_depth,
            new_blocks,
            old_blocks,
            fan_out,
            block_index_width,
        )?;
    }

    // Collapse depth if needed
    let mut current_top = descriptor.top_block;
    for _ in new_depth..old_depth {
        // The current top is a redirector with only one used child (slot 0)
        let child = read_block_index(layer2, current_top, 0, block_index_width)?;
        layer2.deallocate_block(current_top)?;
        current_top = child;
    }
    descriptor.top_block = current_top;
    descriptor.size = new_len;
    descriptor.reserved = new_blocks * block_size as u64;

    Ok(())
}

/// Pre-allocate blocks so the stream can hold at least `n_bytes` without further allocation.
/// Does not change the stream's logical size. Errors if `n_bytes < descriptor.size`.
fn pyramid_reserve<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    n_bytes: u64,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if n_bytes < descriptor.size {
        return Err(SfsError::IoError(format!(
            "cannot reserve {} bytes: stream size is already {}",
            n_bytes, descriptor.size
        )));
    }
    if n_bytes <= descriptor.reserved {
        return Ok(()); // Already have enough capacity
    }
    let target_blocks = data_blocks_needed(n_bytes, block_size);
    ensure_capacity(
        layer2,
        descriptor,
        target_blocks,
        block_size,
        fan_out,
        block_index_width,
    )?;
    descriptor.reserved = target_blocks * block_size as u64;
    Ok(())
}

/// Recursively collect all block IDs (data + redirector) in a pyramid tree.
/// Used by `deallocate_tree` to batch-free all blocks in one call.
fn collect_tree_blocks_for_dealloc<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
    blocks: &mut Vec<u64>,
) -> Result<(), SfsError> {
    blocks.push(block);
    if depth == 0 {
        return Ok(());
    }
    let sentinel = block_sentinel(block_index_width);
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width)?;
        if child != sentinel {
            collect_tree_blocks_for_dealloc(
                layer2,
                child,
                depth - 1,
                fan_out,
                block_index_width,
                blocks,
            )?;
        }
    }
    Ok(())
}

/// Deallocate all blocks in a tree rooted at `block` with given `depth`.
/// Collects all block IDs first, then batch-deallocates them so the free list
/// is written in sorted block order (maximising contiguous runs on re-allocation)
/// with a single header persist.
fn deallocate_tree<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let mut blocks = Vec::new();
    collect_tree_blocks_for_dealloc(
        layer2,
        block,
        depth,
        fan_out,
        block_index_width,
        &mut blocks,
    )?;
    layer2.deallocate_blocks(&mut blocks)?;
    Ok(())
}

/// Accumulator for collecting block IDs and issues during pyramid tree walks.
struct TreeCollector<'a> {
    blocks: &'a mut Vec<u64>,
    issues: &'a mut Vec<String>,
    label: &'a str,
}

/// Recursively collect all block IDs in a pyramid tree (data + redirector blocks).
/// Used by `verify` to enumerate all blocks belonging to a stream.
fn collect_tree_blocks<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
    collector: &mut TreeCollector<'_>,
) {
    collector.blocks.push(block);

    if depth == 0 {
        return; // Data block, already collected
    }

    // Redirector block: recurse into children
    for slot in 0..fan_out {
        match read_block_index(layer2, block, slot, block_index_width) {
            Ok(child) => {
                if child != block_sentinel(block_index_width) {
                    collect_tree_blocks(
                        layer2,
                        child,
                        depth - 1,
                        fan_out,
                        block_index_width,
                        collector,
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

/// Deallocate data blocks from `keep_blocks` to `total_blocks - 1`,
/// and any redirector blocks that become empty.
fn deallocate_excess_blocks<L2: BlockLayer>(
    layer2: &L2,
    top_block: u64,
    depth: u32,
    keep_blocks: u64,
    total_blocks: u64,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    for block_idx in keep_blocks..total_blocks {
        // Navigate to the block and deallocate it
        deallocate_data_block_at(
            layer2,
            top_block,
            block_idx,
            depth,
            fan_out,
            block_index_width,
        )?;
    }
    Ok(())
}

/// Deallocate the data block at `data_block_idx` and mark its slot as INVALID.
/// Also deallocates empty redirector blocks on the way back up.
fn deallocate_data_block_at<L2: BlockLayer>(
    layer2: &L2,
    top_block: u64,
    data_block_idx: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if depth == 0 {
        // Top is the data block itself - don't deallocate here, caller handles
        return Ok(());
    }

    // Navigate to find the data block
    let mut path: Vec<(u64, u64)> = Vec::new(); // (block, slot)
    let mut current_block = top_block;
    let mut remaining_idx = data_block_idx;

    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;
        path.push((current_block, slot));

        let child = read_block_index(layer2, current_block, slot, block_index_width)?;
        if child == block_sentinel(block_index_width) {
            return Ok(()); // Already deallocated
        }
        current_block = child;
    }

    // current_block is the bottom redirector, remaining_idx is the slot
    let slot = remaining_idx;
    let data_block = read_block_index(layer2, current_block, slot, block_index_width)?;
    if data_block == block_sentinel(block_index_width) {
        return Ok(());
    }

    // Deallocate data block and mark slot
    layer2.deallocate_block(data_block)?;
    write_block_index(
        layer2,
        current_block,
        slot,
        block_sentinel(block_index_width),
        block_index_width,
    )?;

    // Check if bottom redirector is now empty; if so, deallocate it and mark in parent
    // We check all slots
    if is_redirector_empty(layer2, current_block, fan_out, block_index_width)? {
        layer2.deallocate_block(current_block)?;
        if let Some(&(parent_block, parent_slot)) = path.last() {
            write_block_index(
                layer2,
                parent_block,
                parent_slot,
                block_sentinel(block_index_width),
                block_index_width,
            )?;
        }
    }

    Ok(())
}

/// Check if all slots in a redirector block are block_sentinel(block_index_width).
fn is_redirector_empty<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    fan_out: u64,
    block_index_width: u8,
) -> Result<bool, SfsError> {
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width)?;
        if child != block_sentinel(block_index_width) {
            return Ok(false);
        }
    }
    Ok(true)
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
/// Reads the entire redirector in a single `read_block` call, mirroring
/// `write_comp_redir` which writes the whole redirector in one call.
fn read_comp_redir<L2: BlockLayer>(
    layer2: &L2,
    redir_block: u64,
    block_size: usize,
    block_index_width: u8,
) -> Result<CompRedir, SfsError> {
    // First read: get the compressed length (4 bytes) so we know how large
    // the redirector is.
    let mut len_buf = [0u8; 4];
    layer2.read_block(redir_block, 0, &mut len_buf, true)?;
    let compressed_length = u32::from_le_bytes(len_buf);

    if compressed_length == 0 {
        return Ok(CompRedir {
            compressed_length: 0,
            data_blocks: Vec::new(),
        });
    }

    let biw = block_index_width as usize;
    let n_blocks = (compressed_length as usize).div_ceil(block_size);

    // Second read: read the entire redirector (length + all block pointers)
    // in a single call, then parse block pointers from the buffer.
    let buf_size = 4 + n_blocks * biw;
    let mut buf = vec![0u8; buf_size];
    layer2.read_block(redir_block, 0, &mut buf, true)?;

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

/// Write a compression redirector to the given block.
fn write_comp_redir<L2: BlockLayer>(
    layer2: &L2,
    redir_block: u64,
    redir: &CompRedir,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let biw = block_index_width as usize;

    // Build the redirector content in a single buffer for one write
    let buf_size = 4 + redir.data_blocks.len() * biw;
    let mut buf = vec![0u8; buf_size];
    buf[0..4].copy_from_slice(&redir.compressed_length.to_le_bytes());
    for (i, &block_ptr) in redir.data_blocks.iter().enumerate() {
        let offset = 4 + i * biw;
        let bytes = block_ptr.to_le_bytes();
        buf[offset..offset + biw].copy_from_slice(&bytes[..biw]);
    }

    layer2.write_block(redir_block, 0, &buf, true)?;
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
) -> Result<(), SfsError> {
    let redir = read_comp_redir(layer2, redir_block, block_size, block_index_width)?;

    if redir.compressed_length == 0 {
        out_buf.fill(0);
        return Ok(());
    }

    // Read compressed data from physical blocks into reusable buffer
    let total_compressed = redir.compressed_length as usize;
    compressed_buf.resize(total_compressed, 0);
    let mut bytes_remaining = total_compressed;
    let mut buf_offset = 0;

    for &data_block in &redir.data_blocks {
        let to_read = bytes_remaining.min(block_size);
        layer2.read_block(
            data_block,
            0,
            &mut compressed_buf[buf_offset..buf_offset + to_read],
            true,
        )?;
        buf_offset += to_read;
        bytes_remaining -= to_read;
    }

    // Decompress
    let decompressed = lz4_flex::decompress_size_prepended(compressed_buf)
        .map_err(|e| SfsError::IoError(format!("lz4 decompression failed: {}", e)))?;

    // Copy into out_buf, zero-padding if the valid extent is smaller than compressed_block_size
    let dec_len = decompressed.len();
    out_buf[..dec_len].copy_from_slice(&decompressed);
    out_buf[dec_len..].fill(0);
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
    fan_out: u64,
    block_index_width: u8,
) -> Result<usize, SfsError> {
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

    while bytes_read < to_read {
        let cblock_idx = current_pos / compressed_block_size as u64;
        let offset_in_cblock = (current_pos % compressed_block_size as u64) as usize;

        // Navigate pyramid to get the compression redirector block
        let redir_block = if depth == 0 {
            descriptor.top_block
        } else {
            let leaf = navigate_to_leaf(
                layer2,
                descriptor,
                cblock_idx,
                depth,
                fan_out,
                block_index_width,
            )?;
            let slot = cblock_idx % fan_out;
            read_block_index(layer2, leaf, slot, block_index_width)?
        };

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
    fan_out: u64,
    block_index_width: u8,
) -> Result<usize, SfsError> {
    if buf.is_empty() {
        return Ok(0);
    }

    let block_size = layer2.block_size();
    let end_pos = pos + buf.len() as u64;

    // Ensure tree has enough capacity for the write endpoint
    pyramid_reserve_compressed(
        layer2,
        descriptor,
        end_pos.max(descriptor.size),
        block_size,
        compressed_block_size,
        fan_out,
        block_index_width,
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
    let mut new_data_blocks: Vec<u64> = Vec::new();
    let mut excess_blocks: Vec<u64> = Vec::new();
    let mut redir_write_buf: Vec<u8> = Vec::new();

    while bytes_written < buf.len() {
        let cblock_idx = current_pos / compressed_block_size as u64;
        let offset_in_cblock = (current_pos % compressed_block_size as u64) as usize;

        // Find the compression redirector block for this compressed block
        let redir_block = if depth == 0 {
            descriptor.top_block
        } else {
            let leaf = navigate_to_leaf(layer2, descriptor, cblock_idx, depth, fan_out, biw)?;
            let slot = cblock_idx % fan_out;
            read_block_index(layer2, leaf, slot, biw)?
        };

        if redir_block == sentinel {
            return Err(SfsError::IoError(format!(
                "compressed write: no redirector at cblock {}",
                cblock_idx
            )));
        }

        // Read existing compressed data for this compressed block
        let old_redir = read_comp_redir(layer2, redir_block, block_size, biw)?;

        if old_redir.compressed_length == 0 {
            cblock_buf.fill(0);
        } else {
            // Read compressed data from physical blocks into reusable buffer
            let total_compressed = old_redir.compressed_length as usize;
            compressed_read_buf.resize(total_compressed, 0);
            let mut remaining = total_compressed;
            let mut off = 0;
            for &db in &old_redir.data_blocks {
                let to_read = remaining.min(block_size);
                layer2.read_block(db, 0, &mut compressed_read_buf[off..off + to_read], true)?;
                off += to_read;
                remaining -= to_read;
            }
            let dec = lz4_flex::decompress_size_prepended(&compressed_read_buf)
                .map_err(|e| SfsError::IoError(format!("lz4 decompression failed: {}", e)))?;
            let dec_len = dec.len();
            cblock_buf[..dec_len].copy_from_slice(&dec);
            cblock_buf[dec_len..].fill(0);
        }

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

        // Compress the valid extent
        let compressed = lz4_flex::compress_prepend_size(&cblock_buf[..valid_extent]);
        let new_compressed_len = compressed.len();
        let new_block_count = new_compressed_len.div_ceil(block_size);
        let old_block_count = old_redir.data_blocks.len();

        // Reconcile physical blocks: reuse existing, allocate/free the delta
        new_data_blocks.clear();

        // Reuse as many existing blocks as we can
        let reuse_count = old_block_count.min(new_block_count);
        new_data_blocks.extend_from_slice(&old_redir.data_blocks[..reuse_count]);

        if new_block_count > old_block_count {
            // Need more blocks
            let extra = (new_block_count - old_block_count) as u64;
            let allocated = layer2.allocate_blocks(extra)?;
            new_data_blocks.extend_from_slice(&allocated);
        } else if old_block_count > new_block_count {
            // Free excess blocks
            excess_blocks.clear();
            excess_blocks.extend_from_slice(&old_redir.data_blocks[new_block_count..]);
            layer2.deallocate_blocks(&mut excess_blocks)?;
        }

        // Write compressed data to physical blocks
        let mut comp_remaining = new_compressed_len;
        let mut comp_offset = 0;
        for &db in &new_data_blocks {
            let to_write = comp_remaining.min(block_size);
            layer2.write_block(
                db,
                0,
                &compressed[comp_offset..comp_offset + to_write],
                true,
            )?;
            comp_offset += to_write;
            comp_remaining -= to_write;
        }

        // Update the compression redirector (build inline to avoid cloning
        // the reusable new_data_blocks vec)
        let biw_sz = biw as usize;
        let redir_buf_size = 4 + new_data_blocks.len() * biw_sz;
        redir_write_buf.resize(redir_buf_size, 0);
        redir_write_buf[0..4].copy_from_slice(&(new_compressed_len as u32).to_le_bytes());
        for (i, &block_ptr) in new_data_blocks.iter().enumerate() {
            let off = 4 + i * biw_sz;
            let bytes = block_ptr.to_le_bytes();
            redir_write_buf[off..off + biw_sz].copy_from_slice(&bytes[..biw_sz]);
        }
        layer2.write_block(redir_block, 0, &redir_write_buf[..redir_buf_size], true)?;

        bytes_written += chunk;
        current_pos += chunk as u64;
    }

    // Update size
    let actual_end = pos + bytes_written as u64;
    descriptor.size = actual_end.max(old_size);

    Ok(bytes_written)
}

/// Pre-allocate compressed stream capacity. Grows the tree and allocates
/// compression redirector blocks (initialized with compressed_length=0).
fn pyramid_reserve_compressed<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    n_bytes: u64,
    block_size: usize,
    compressed_block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if n_bytes < descriptor.size {
        return Err(SfsError::IoError(format!(
            "cannot reserve {} bytes: stream size is already {}",
            n_bytes, descriptor.size
        )));
    }
    if n_bytes <= descriptor.reserved {
        return Ok(());
    }

    let target_cblocks = compressed_blocks_needed(n_bytes, compressed_block_size);

    ensure_capacity_compressed(
        layer2,
        descriptor,
        target_cblocks,
        block_size,
        compressed_block_size,
        fan_out,
        block_index_width,
    )?;
    descriptor.reserved = target_cblocks * compressed_block_size as u64;
    Ok(())
}

/// Grow the compressed pyramid to support `target_cblocks` compressed blocks.
/// Allocates compression redirector blocks (one per new compressed block),
/// each initialized with compressed_length=0.
fn ensure_capacity_compressed<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    target_cblocks: u64,
    block_size: usize,
    compressed_block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if target_cblocks == 0 {
        return Ok(());
    }

    let current_cblocks = compressed_blocks_needed(descriptor.reserved, compressed_block_size);
    let current_depth = pyramid_depth(current_cblocks, fan_out);
    let target_depth = pyramid_depth(target_cblocks, fan_out);

    // Handle empty stream: allocate first compression redirector block
    if descriptor.reserved == 0 && descriptor.top_block == 0 {
        let redir_block = layer2.allocate_block()?;
        // Initialize with compressed_length=0
        let zero_redir = CompRedir {
            compressed_length: 0,
            data_blocks: Vec::new(),
        };
        write_comp_redir(layer2, redir_block, &zero_redir, block_index_width)?;
        descriptor.top_block = redir_block;

        if target_cblocks <= 1 {
            return Ok(());
        }
    }

    // Grow depth if needed: wrap current top in new redirector layers
    let effective_current_depth = if descriptor.reserved == 0 && current_cblocks == 0 {
        0
    } else {
        current_depth
    };

    let mut current_top = descriptor.top_block;
    for _ in effective_current_depth..target_depth {
        let new_redirector = layer2.allocate_block()?;
        fill_sentinel(layer2, new_redirector, block_size, block_index_width)?;
        write_block_index(layer2, new_redirector, 0, current_top, block_index_width)?;
        current_top = new_redirector;
    }
    descriptor.top_block = current_top;

    // Allocate missing compression redirector blocks
    let effective_current = if current_cblocks == 0 {
        1
    } else {
        current_cblocks
    };
    let redirs_needed = target_cblocks - effective_current;
    if redirs_needed > 0 {
        let new_redir_blocks = layer2.allocate_blocks(redirs_needed)?;

        // Initialize each compression redirector with compressed_length=0
        // (4 zero bytes at offset 0 is sufficient)
        let zero_buf = [0u8; 4];
        for &rb in &new_redir_blocks {
            layer2.write_block(rb, 0, &zero_buf, true)?;
        }

        // Fill leaf slots in the pyramid tree
        batch_fill_leaf_redirectors(
            layer2,
            descriptor,
            &new_redir_blocks,
            effective_current,
            fan_out,
            block_index_width,
            target_depth,
        )?;
    }

    Ok(())
}

/// Truncate a compressed stream. Deallocates compressed blocks beyond `new_len`,
/// including their compression redirectors and physical data blocks.
fn pyramid_truncate_compressed<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &mut StreamDescriptor,
    new_len: u64,
    block_size: usize,
    compressed_block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if new_len >= descriptor.size {
        return Ok(());
    }

    if new_len == 0 {
        // Deallocate the entire compressed tree
        let capacity = descriptor.reserved.max(descriptor.size);
        if capacity > 0 && descriptor.top_block != 0 {
            let old_cblocks = compressed_blocks_needed(capacity, compressed_block_size);
            let old_depth = pyramid_depth(old_cblocks, fan_out);
            deallocate_compressed_tree(
                layer2,
                descriptor.top_block,
                old_depth,
                block_size,
                fan_out,
                block_index_width,
            )?;
        }
        descriptor.size = 0;
        descriptor.top_block = 0;
        descriptor.reserved = 0;
        return Ok(());
    }

    let capacity = descriptor.reserved.max(descriptor.size);
    let old_cblocks = compressed_blocks_needed(capacity, compressed_block_size);
    let new_cblocks = compressed_blocks_needed(new_len, compressed_block_size);
    let old_depth = pyramid_depth(old_cblocks, fan_out);
    let new_depth = pyramid_depth(new_cblocks, fan_out);

    // Deallocate excess compressed blocks (their redirectors + data blocks)
    if new_cblocks < old_cblocks {
        deallocate_excess_compressed_blocks(
            layer2,
            descriptor.top_block,
            old_depth,
            new_cblocks,
            old_cblocks,
            fan_out,
            block_index_width,
        )?;
    }

    // Collapse depth if needed
    let mut current_top = descriptor.top_block;
    for _ in new_depth..old_depth {
        let child = read_block_index(layer2, current_top, 0, block_index_width)?;
        layer2.deallocate_block(current_top)?;
        current_top = child;
    }
    descriptor.top_block = current_top;
    descriptor.size = new_len;
    descriptor.reserved = new_cblocks * compressed_block_size as u64;

    Ok(())
}

/// Deallocate all blocks in a compressed tree (redirectors + their physical data blocks).
fn deallocate_compressed_tree<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let mut blocks = Vec::new();
    collect_compressed_tree_blocks_for_dealloc(
        layer2,
        block,
        depth,
        block_size,
        fan_out,
        block_index_width,
        &mut blocks,
    )?;
    layer2.deallocate_blocks(&mut blocks)?;
    Ok(())
}

/// Recursively collect all block IDs in a compressed pyramid tree:
/// redirector blocks, compression redirector blocks, and their physical data blocks.
fn collect_compressed_tree_blocks_for_dealloc<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
    blocks: &mut Vec<u64>,
) -> Result<(), SfsError> {
    if depth == 0 {
        // This is a compression redirector block at the leaf level.
        // Collect its data blocks, then the redirector itself.
        let redir = read_comp_redir(layer2, block, block_size, block_index_width)?;
        for &db in &redir.data_blocks {
            blocks.push(db);
        }
        blocks.push(block);
        return Ok(());
    }

    // Pyramid redirector: recurse into children
    let sentinel = block_sentinel(block_index_width);
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width)?;
        if child != sentinel {
            collect_compressed_tree_blocks_for_dealloc(
                layer2,
                child,
                depth - 1,
                block_size,
                fan_out,
                block_index_width,
                blocks,
            )?;
        }
    }
    blocks.push(block);
    Ok(())
}

/// Collect all block IDs in a compressed tree for verification purposes.
fn collect_compressed_tree_blocks<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
    collector: &mut TreeCollector<'_>,
) {
    if depth == 0 {
        // Compression redirector block
        collector.blocks.push(block);
        match read_comp_redir(layer2, block, block_size, block_index_width) {
            Ok(redir) => {
                for &db in &redir.data_blocks {
                    collector.blocks.push(db);
                }
            }
            Err(e) => {
                collector.issues.push(format!(
                    "L3: stream {}: error reading comp redirector block {}: {}",
                    collector.label, block, e
                ));
            }
        }
        return;
    }

    // Pyramid redirector: recurse into children
    collector.blocks.push(block);
    let sentinel = block_sentinel(block_index_width);
    for slot in 0..fan_out {
        match read_block_index(layer2, block, slot, block_index_width) {
            Ok(child) => {
                if child != sentinel {
                    collect_compressed_tree_blocks(
                        layer2,
                        child,
                        depth - 1,
                        block_size,
                        fan_out,
                        block_index_width,
                        collector,
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

/// Deallocate excess compressed compressed blocks from `keep_cblocks` to `total_cblocks - 1`.
fn deallocate_excess_compressed_blocks<L2: BlockLayer>(
    layer2: &L2,
    top_block: u64,
    depth: u32,
    keep_cblocks: u64,
    total_cblocks: u64,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let block_size = layer2.block_size();
    for cblock_idx in keep_cblocks..total_cblocks {
        deallocate_cblock_at(
            layer2,
            top_block,
            cblock_idx,
            depth,
            block_size,
            fan_out,
            block_index_width,
        )?;
    }
    Ok(())
}

/// Deallocate a single compressed compressed block: free its physical data blocks,
/// free the compression redirector block, and sentinel the leaf slot.
fn deallocate_cblock_at<L2: BlockLayer>(
    layer2: &L2,
    top_block: u64,
    cblock_idx: u64,
    depth: u32,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    let sentinel = block_sentinel(block_index_width);

    if depth == 0 {
        // top_block IS the comp redirector — don't deallocate here, caller handles
        return Ok(());
    }

    // Navigate to find the compression redirector block
    let mut path: Vec<(u64, u64)> = Vec::new(); // (block, slot)
    let mut current_block = top_block;
    let mut remaining_idx = cblock_idx;

    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;
        path.push((current_block, slot));

        let child = read_block_index(layer2, current_block, slot, block_index_width)?;
        if child == sentinel {
            return Ok(()); // Already deallocated
        }
        current_block = child;
    }

    // current_block is the bottom (leaf) redirector, remaining_idx is the slot
    let slot = remaining_idx;
    let redir_block = read_block_index(layer2, current_block, slot, block_index_width)?;
    if redir_block == sentinel {
        return Ok(());
    }

    // Read the compression redirector to find its physical data blocks
    let redir = read_comp_redir(layer2, redir_block, block_size, block_index_width)?;

    // Collect all blocks to deallocate: data blocks + the redirector itself
    let mut to_dealloc = redir.data_blocks;
    to_dealloc.push(redir_block);
    layer2.deallocate_blocks(&mut to_dealloc)?;

    // Mark the leaf slot as sentinel
    write_block_index(layer2, current_block, slot, sentinel, block_index_width)?;

    // Check if leaf redirector is now empty
    if is_redirector_empty(layer2, current_block, fan_out, block_index_width)? {
        layer2.deallocate_block(current_block)?;
        if let Some(&(parent_block, parent_slot)) = path.last() {
            write_block_index(
                layer2,
                parent_block,
                parent_slot,
                sentinel,
                block_index_width,
            )?;
        }
    }

    Ok(())
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
    fn acquire_lock(&self, id: u64, mode: OpenMode, blocking: bool) -> Result<(), SfsError> {
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
                return Err(SfsError::LockConflict(format!(
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
    ) -> Result<BlockStreamHandle, SfsError> {
        if id == STREAMS_STREAM_ID {
            return Err(SfsError::InvalidPath(
                "cannot open Streams stream directly".to_string(),
            ));
        }

        // 1. Acquire per-stream lock
        self.acquire_lock(id, mode, blocking)?;

        // 2. Read the stream's descriptor (acquire STREAMS read lock, always blocking)
        if let Err(e) = self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true) {
            self.release_lock(id, mode);
            return Err(e);
        }
        self.layer2.invalidate_block_cache();
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
            return Err(SfsError::NotFound(format!("stream {}", id)));
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
    ) -> Result<StreamDescriptor, SfsError> {
        let offset = stream_id * DESCRIPTOR_SIZE;
        if offset + DESCRIPTOR_SIZE > streams_desc.size {
            return Err(SfsError::NotFound(format!("stream {}", stream_id)));
        }
        let mut buf = [0u8; DESCRIPTOR_SIZE as usize];
        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        pyramid_read(&self.layer2, streams_desc, offset, &mut buf, bs, fo, biw)?;
        Ok(StreamDescriptor::from_bytes(&buf))
    }

    /// Write a stream descriptor to the Streams stream.
    /// Caller must hold the write lock on STREAMS_STREAM_ID.
    fn write_descriptor(
        &self,
        streams_desc: &mut StreamDescriptor,
        stream_id: u64,
        desc: &StreamDescriptor,
    ) -> Result<(), SfsError> {
        let offset = stream_id * DESCRIPTOR_SIZE;
        let buf = desc.to_bytes();
        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        pyramid_write(&self.layer2, streams_desc, offset, &buf, bs, fo, biw)?;
        Ok(())
    }

    /// Serialize the L3 header (no length prefix).
    /// Format: | "pyra  ": [u8;6] | version: u8 | size: u64 | top_block: u64 | reserved: u64 | cbss: u8 |
    fn serialize_header(streams_desc: &StreamDescriptor, cbss: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L3_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(b"pyra  ");
        buf.push(2); // version 2: adds cbss and per-stream compressed flags
        buf.extend_from_slice(&streams_desc.size.to_le_bytes());
        buf.extend_from_slice(&streams_desc.top_block.to_le_bytes());
        buf.extend_from_slice(&streams_desc.reserved.to_le_bytes());
        buf.push(cbss);
        buf
    }

    /// Deserialize the L3 header (no length prefix).
    /// Input: 32 bytes — identifier starts at byte 0.
    /// Returns (streams_descriptor, compressed_block_size_shift).
    fn deserialize_header(data: &[u8]) -> Result<(StreamDescriptor, u8), SfsError> {
        if data.len() < L3_PAYLOAD_SIZE as usize {
            return Err(SfsError::IoError(format!(
                "L3 payload too short: {} < {}",
                data.len(),
                L3_PAYLOAD_SIZE
            )));
        }
        if &data[0..6] != b"pyra  " {
            return Err(SfsError::IoError(format!(
                "expected L3 identifier 'pyra  ', got '{}'",
                String::from_utf8_lossy(&data[0..6])
            )));
        }
        let version = data[6];
        if version != 2 {
            return Err(SfsError::IoError(format!(
                "unsupported L3 version: {} (expected 2; re-create the file)",
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
    fn persist_l3_header(&self, streams_desc: &StreamDescriptor) -> Result<(), SfsError> {
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
    ) -> Result<Self, SfsError>
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
        let streams_desc = StreamDescriptor {
            size: 0,
            top_block: 0,
            reserved: 0,
            flags: 0,
        };
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
        })
    }

    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, SfsError>
    where
        Self: Sized,
    {
        let layer2 = L2::open(path, mode, password)?;
        let my_header_slot = layer2.header_slot_for_upper(0);

        // Read and parse L3 payload
        let header_buffer = layer2.read_header_slot(my_header_slot)?;
        let (streams_desc, cbss) = Self::deserialize_header(&header_buffer)?;

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

    fn create_stream(&self, compressed: bool) -> Result<u64, SfsError> {
        if compressed && self.compressed_block_size_shift == 0 {
            return Err(SfsError::IoError(
                "compression not configured for this filesystem".to_string(),
            ));
        }

        // We're about to create a new stream, so we need to find an available stream descriptor
        // from the Streams stream. We acquire the STREAMS_STREAM_ID write lock to ensure
        // exclusive access while we scan for a free descriptor and potentially extend the stream.
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;
        self.layer2.invalidate_block_cache();

        let result = (|| -> Result<u64, SfsError> {
            // We should never block on this call. The Mutex is just to satisfy Rust's safety
            // guarantees around shared mutable access to the descriptor, but in practice we
            // always hold the STREAMS_STREAM_ID write lock when accessing it, so there is no
            // contention.
            let mut streams_desc = self.streams_descriptor.lock().unwrap();
            let bs = self.block_size();
            let fo = self.fan_out();
            let biw = self.block_index_width_val();

            // Scan for a free descriptor slot
            let num_slots = streams_desc.size / DESCRIPTOR_SIZE;
            let mut free_id: Option<u64> = None;

            for i in 0..num_slots {
                let desc = self.read_descriptor(&streams_desc, i)?;
                if desc.is_free() {
                    free_id = Some(i);
                    break;
                }
            }

            // Did we find a free slot? If not, extend the Streams stream
            let stream_id = match free_id {
                Some(id) => id,
                None => {
                    // TODO: optimize by writing a zeroed block instead of individual descriptors
                    // TODO: Make it configurable how much to extend by
                    let new_id = num_slots;
                    if new_id >= self.sentinel() {
                        return Err(SfsError::IoError(format!(
                            "stream ID overflow: {} >= sentinel {} for block_index_width={}",
                            new_id,
                            self.sentinel(),
                            self.layer2.block_index_width()
                        )));
                    }
                    let new_desc = StreamDescriptor {
                        size: 0,
                        top_block: 0,
                        reserved: 0,
                        flags: 0,
                    };
                    let offset = new_id * DESCRIPTOR_SIZE;
                    let buf = new_desc.to_bytes();
                    pyramid_write(&self.layer2, &mut streams_desc, offset, &buf, bs, fo, biw)?;
                    new_id
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
        self.layer2.invalidate_block_cache();
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let offset = id * DESCRIPTOR_SIZE;
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

    fn stream_count(&self) -> Result<u64, SfsError> {
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true)?;
        self.layer2.invalidate_block_cache();
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let num_slots = streams_desc.size / DESCRIPTOR_SIZE;
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

    fn stream_ids(&self) -> Result<Vec<u64>, SfsError> {
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Read, true)?;
        self.layer2.invalidate_block_cache();
        let result = {
            let streams_desc = self.streams_descriptor.lock().unwrap();
            let num_slots = streams_desc.size / DESCRIPTOR_SIZE;
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

    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, SfsError> {
        self.open_stream_inner(id, mode, false)
    }

    fn open_stream_blocking(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, SfsError> {
        self.open_stream_inner(id, mode, true)
    }

    fn close_stream(&self, handle: Self::Handle) -> Result<(), SfsError> {
        let info = {
            let mut state = self.state.lock().unwrap();
            state
                .open_handles
                .remove(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?
        };

        // If writer, flush cached descriptor back to Streams stream
        if info.mode == OpenMode::Write {
            self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;
            self.layer2.invalidate_block_cache();
            let result = {
                let mut streams_desc = self.streams_descriptor.lock().unwrap();
                self.write_descriptor(&mut streams_desc, info.stream_id, &info.cached_descriptor)?;
                self.persist_l3_header(&streams_desc)
            };
            self.release_lock(STREAMS_STREAM_ID, OpenMode::Write);
            result?;
        }

        // Release per-stream lock
        self.release_lock(info.stream_id, info.mode);

        Ok(())
    }

    fn delete_stream(&self, id: u64) -> Result<(), SfsError> {
        // Check that stream is not currently open
        {
            let state = self.state.lock().unwrap();
            if let Some(lock) = state.locks.get(&id) {
                if lock.has_writer || lock.readers > 0 {
                    return Err(SfsError::LockConflict(format!(
                        "cannot delete open stream {}",
                        id
                    )));
                }
            }
        }

        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;
        self.layer2.invalidate_block_cache();

        let result = (|| -> Result<(), SfsError> {
            let mut streams_desc = self.streams_descriptor.lock().unwrap();
            let desc = self.read_descriptor(&streams_desc, id)?;
            if desc.is_free() {
                return Err(SfsError::NotFound(format!("stream {}", id)));
            }

            // Deallocate all blocks belonging to the stream
            let capacity = desc.reserved.max(desc.size);
            if capacity > 0 && desc.top_block != 0 {
                let bs = self.block_size();
                let fo = self.fan_out();
                let biw = self.block_index_width_val();
                if desc.is_compressed() {
                    let cbs = 1usize << self.compressed_block_size_shift;
                    let num_cblocks = compressed_blocks_needed(capacity, cbs);
                    let depth = pyramid_depth(num_cblocks, fo);
                    deallocate_compressed_tree(&self.layer2, desc.top_block, depth, bs, fo, biw)?;
                } else {
                    let num_blocks = data_blocks_needed(capacity, bs);
                    let depth = pyramid_depth(num_blocks, fo);
                    deallocate_tree(&self.layer2, desc.top_block, depth, fo, biw)?;
                }
            }

            // Mark descriptor as free
            let free_desc = StreamDescriptor {
                size: 0,
                top_block: FREE_DESCRIPTOR_MARKER,
                reserved: 0,
                flags: 0,
            };
            self.write_descriptor(&mut streams_desc, id, &free_desc)?;
            self.persist_l3_header(&streams_desc)?;

            Ok(())
        })();

        self.release_lock(STREAMS_STREAM_ID, OpenMode::Write);
        result
    }

    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError> {
        let desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.cached_descriptor
        };

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_read_compressed(&self.layer2, &desc, pos, buf, cbs, fo, biw)
        } else {
            pyramid_read(&self.layer2, &desc, pos, buf, bs, fo, biw)
        }
    }

    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, SfsError> {
        let (stream_id, mut desc, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(SfsError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            (info.stream_id, info.cached_descriptor, info.mode)
        };

        let _ = mode; // used for the check above
        let _ = stream_id; // might be useful for debugging

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        let n = if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_write_compressed(&self.layer2, &mut desc, pos, buf, cbs, fo, biw)?
        } else {
            pyramid_write(&self.layer2, &mut desc, pos, buf, bs, fo, biw)?
        };

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(n)
    }

    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, SfsError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.size)
    }

    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), SfsError> {
        let mut desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(SfsError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.cached_descriptor
        };

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_truncate_compressed(&self.layer2, &mut desc, new_len, bs, cbs, fo, biw)?;
        } else {
            pyramid_truncate(&self.layer2, &mut desc, new_len, bs, fo, biw)?;
        }

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(())
    }

    fn reserve(&self, handle: &Self::Handle, n_bytes: u64) -> Result<(), SfsError> {
        let mut desc = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(SfsError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.cached_descriptor
        };

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();
        if desc.is_compressed() {
            let cbs = 1usize << self.compressed_block_size_shift;
            pyramid_reserve_compressed(&self.layer2, &mut desc, n_bytes, bs, cbs, fo, biw)?;
        } else {
            pyramid_reserve(&self.layer2, &mut desc, n_bytes, bs, fo, biw)?;
        }

        // Update cached descriptor
        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_handles.get_mut(&handle.0).unwrap();
            info.cached_descriptor = desc;
        }

        Ok(())
    }

    fn stream_reserved(&self, handle: &Self::Handle) -> Result<u64, SfsError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.reserved)
    }

    fn is_stream_compressed(&self, handle: &Self::Handle) -> Result<bool, SfsError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.cached_descriptor.is_compressed())
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Slot 0 = L3's own, so upper layer index 0 → slot 1 (L4), etc.
        self.layer2.header_slot_for_upper(index + 1)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), SfsError> {
        self.layer2.write_header_slot(slot, data)
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, SfsError> {
        self.layer2.read_header_slot(slot)
    }

    fn verify(&self, claimed_streams: &[u64]) -> Result<Vec<String>, SfsError> {
        let mut issues = Vec::new();
        let mut all_claimed_blocks: Vec<u64> = Vec::new();

        let bs = self.block_size();
        let fo = self.fan_out();
        let biw = self.block_index_width_val();

        // 1. Clone the Streams stream descriptor (out-of-band)
        let streams_desc = *self.streams_descriptor.lock().unwrap();

        // 2. Collect blocks belonging to the Streams stream itself
        if streams_desc.size > 0 {
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
                fo,
                biw,
                &mut collector,
            );
        }

        // 3. Read all stream descriptors, build set of active stream IDs
        let num_slots = streams_desc.size / DESCRIPTOR_SIZE;
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
                                let num_cblocks = compressed_blocks_needed(capacity, cbs);
                                let depth = pyramid_depth(num_cblocks, fo);
                                collect_compressed_tree_blocks(
                                    &self.layer2,
                                    desc.top_block,
                                    depth,
                                    bs,
                                    fo,
                                    biw,
                                    &mut collector,
                                );
                            } else {
                                let num_blocks = data_blocks_needed(capacity, bs);
                                let depth = pyramid_depth(num_blocks, fo);
                                collect_tree_blocks(
                                    &self.layer2,
                                    desc.top_block,
                                    depth,
                                    fo,
                                    biw,
                                    &mut collector,
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

        // 6. Pass all collected blocks to L2 for verification
        issues.extend(self.layer2.verify(&all_claimed_blocks)?);

        Ok(issues)
    }
}
