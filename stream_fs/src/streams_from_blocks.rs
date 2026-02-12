use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};

use crate::block_layer::BlockLayer;
use crate::stream_layer::StreamLayer;
use crate::{HeaderSlotId, OpenMode, SfsError};

/// L3 payload size: "pyra  "(6) + version(1) + size(8) + top_block(8) + reserved(8) = 31 bytes.
const L3_PAYLOAD_SIZE: u16 = 31;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Sentinel value for stream descriptors (full u64 fields).
/// A top_block of u64::MAX means the descriptor is free.
const FREE_DESCRIPTOR_MARKER: u64 = u64::MAX;

/// Size of a stream descriptor in bytes: u64 size + u64 top_block + u64 reserved.
const DESCRIPTOR_SIZE: u64 = 24;

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
    /// Allocated capacity in bytes (always a multiple of block_size, or 0).
    /// Invariant: reserved >= size.
    reserved: u64,
}

impl StreamDescriptor {
    fn is_free(&self) -> bool {
        self.top_block == FREE_DESCRIPTOR_MARKER
    }

    fn to_bytes(self) -> [u8; 24] {
        let mut buf = [0u8; 24];
        buf[0..8].copy_from_slice(&self.size.to_le_bytes());
        buf[8..16].copy_from_slice(&self.top_block.to_le_bytes());
        buf[16..24].copy_from_slice(&self.reserved.to_le_bytes());
        buf
    }

    fn from_bytes(data: &[u8; 24]) -> Self {
        StreamDescriptor {
            size: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            top_block: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            reserved: u64::from_le_bytes(data[16..24].try_into().unwrap()),
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
    layer2.read_block(redirector_block, offset, &mut buf[..biw])?;
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
    layer2.write_block(redirector_block, offset, &bytes[..biw])?;
    Ok(())
}

/// Navigate the pyramid to find the data block and offset for a byte position.
/// Returns (data_block_index, offset_within_block).
fn navigate_to_position<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    pos: u64,
    block_size: usize,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(u64, usize), SfsError> {
    let capacity = descriptor.reserved.max(descriptor.size);
    let num_data_blocks = data_blocks_needed(capacity, block_size);
    let depth = pyramid_depth(num_data_blocks, fan_out);

    let data_block_idx = pos / block_size as u64;
    let offset_in_block = (pos % block_size as u64) as usize;

    let mut current_block = descriptor.top_block;
    let mut remaining_idx = data_block_idx;

    for level in (0..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;

        let child = read_block_index(layer2, current_block, slot, block_index_width)?;
        if child == block_sentinel(block_index_width) {
            return Err(SfsError::IoError(format!(
                "navigating pyramid: found invalid block at depth {}, slot {}",
                level, slot
            )));
        }
        current_block = child;
    }

    Ok((current_block, offset_in_block))
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
        // The new redirector's first slot points to the old top
        write_block_index(layer2, new_redirector, 0, current_top, block_index_width)?;
        // Fill remaining slots with block_sentinel(block_index_width) (already zeroed, but we need 0xFF)
        let biw = block_index_width as usize;
        let invalid_bytes = block_sentinel(block_index_width).to_le_bytes();
        for slot in 1..fan_out {
            let offset = (slot as usize) * biw;
            layer2.write_block(new_redirector, offset, &invalid_bytes[..biw])?;
        }
        current_top = new_redirector;
    }
    descriptor.top_block = current_top;

    // Allocate missing data blocks (fill pyramid slots)
    let effective_current = if current_data_blocks == 0 {
        1
    } else {
        current_data_blocks
    };
    for block_idx in effective_current..target_data_blocks {
        let new_data_block = layer2.allocate_block()?;
        // Navigate to the correct slot and write the new block index
        write_block_at_index(
            layer2,
            descriptor,
            block_idx,
            new_data_block,
            fan_out,
            block_index_width,
            target_depth,
        )?;
    }

    Ok(())
}

/// Write a data block index into the correct pyramid slot for `data_block_idx`.
fn write_block_at_index<L2: BlockLayer>(
    layer2: &L2,
    descriptor: &StreamDescriptor,
    data_block_idx: u64,
    new_block: u64,
    fan_out: u64,
    block_index_width: u8,
    depth: u32,
) -> Result<(), SfsError> {
    if depth == 0 {
        // Top is the data block itself; nothing to write in a redirector
        return Ok(());
    }

    let mut current_block = descriptor.top_block;
    let mut remaining_idx = data_block_idx;

    // Navigate down to the parent redirector of the data block
    for level in (1..depth).rev() {
        let span = fan_out.pow(level);
        let slot = remaining_idx / span;
        remaining_idx %= span;

        let mut child = read_block_index(layer2, current_block, slot, block_index_width)?;
        if child == block_sentinel(block_index_width) {
            // Need to allocate a new redirector at this level
            child = layer2.allocate_block()?;
            // Fill with block_sentinel(block_index_width)
            let biw = block_index_width as usize;
            let invalid_bytes = block_sentinel(block_index_width).to_le_bytes();
            for s in 0..fan_out {
                let offset = (s as usize) * biw;
                layer2.write_block(child, offset, &invalid_bytes[..biw])?;
            }
            write_block_index(layer2, current_block, slot, child, block_index_width)?;
        }
        current_block = child;
    }

    // We're at the bottom redirector; write the data block index
    let slot = remaining_idx;
    write_block_index(layer2, current_block, slot, new_block, block_index_width)?;
    Ok(())
}

/// Read bytes from a stream described by `descriptor`, starting at `pos`.
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

    let available = (descriptor.size - pos) as usize;
    let to_read = buf.len().min(available);
    if to_read == 0 {
        return Ok(0);
    }

    let mut bytes_read = 0;
    let mut current_pos = pos;

    while bytes_read < to_read {
        let (data_block, offset) = navigate_to_position(
            layer2,
            descriptor,
            current_pos,
            block_size,
            fan_out,
            block_index_width,
        )?;

        let chunk_len = (to_read - bytes_read).min(block_size - offset);
        let n = layer2.read_block(
            data_block,
            offset,
            &mut buf[bytes_read..bytes_read + chunk_len],
        )?;
        bytes_read += n;
        current_pos += n as u64;

        if n < chunk_len {
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

    let mut bytes_written = 0;
    let mut current_pos = pos;

    // write the data, possibly over multiple blocks
    while bytes_written < buf.len() {
        let (data_block, offset) = navigate_to_position(
            layer2,
            descriptor,
            current_pos,
            block_size,
            fan_out,
            block_index_width,
        )?;

        let chunk_len = (buf.len() - bytes_written).min(block_size - offset);
        let n = layer2.write_block(
            data_block,
            offset,
            &buf[bytes_written..bytes_written + chunk_len],
        )?;
        bytes_written += n;
        current_pos += n as u64;

        if n < chunk_len {
            break;
        }
    }

    // Update size to reflect actual write extent
    let actual_end = pos + bytes_written as u64;
    if actual_end > old_size {
        descriptor.size = actual_end;
    } else {
        descriptor.size = old_size;
    }

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

/// Recursively deallocate all blocks in a tree rooted at `block` with given `depth`.
fn deallocate_tree<L2: BlockLayer>(
    layer2: &L2,
    block: u64,
    depth: u32,
    fan_out: u64,
    block_index_width: u8,
) -> Result<(), SfsError> {
    if depth == 0 {
        // Data block
        layer2.deallocate_block(block)?;
        return Ok(());
    }

    // Redirector block: recurse into children, then deallocate self
    for slot in 0..fan_out {
        let child = read_block_index(layer2, block, slot, block_index_width)?;
        if child != block_sentinel(block_index_width) {
            deallocate_tree(layer2, child, depth - 1, fan_out, block_index_width)?;
        }
    }
    layer2.deallocate_block(block)?;
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
        let mut buf = [0u8; 24];
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
    /// Format: | "pyra  ": [u8;6] | version: u8 | size: u64 | top_block: u64 | reserved: u64 |
    fn serialize_header(streams_desc: &StreamDescriptor) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L3_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(b"pyra  ");
        buf.push(1); // version 1: descriptors include reserved field
        buf.extend_from_slice(&streams_desc.size.to_le_bytes());
        buf.extend_from_slice(&streams_desc.top_block.to_le_bytes());
        buf.extend_from_slice(&streams_desc.reserved.to_le_bytes());
        buf
    }

    /// Deserialize the L3 header (no length prefix).
    /// Input: 31 bytes — identifier starts at byte 0.
    fn deserialize_header(data: &[u8]) -> Result<StreamDescriptor, SfsError> {
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
        if version != 1 {
            return Err(SfsError::IoError(format!(
                "unsupported L3 version: {} (expected 1; re-create the file)",
                version
            )));
        }
        // payload[7..15] = streams_size, [15..23] = streams_top_block, [23..31] = streams_reserved
        let streams_size = u64::from_le_bytes(data[7..15].try_into().unwrap());
        let streams_top_block = u64::from_le_bytes(data[15..23].try_into().unwrap());
        let streams_reserved = u64::from_le_bytes(data[23..31].try_into().unwrap());
        Ok(StreamDescriptor {
            size: streams_size,
            top_block: streams_top_block,
            reserved: streams_reserved,
        })
    }

    /// Persist the L3 header payload to L2 via its slot.
    /// Caller must hold the STREAMS_STREAM_ID lock.
    fn persist_l3_header(&self, streams_desc: &StreamDescriptor) -> Result<(), SfsError> {
        let payload = Self::serialize_header(streams_desc);
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
        mut slot_sizes: VecDeque<u16>,
    ) -> Result<Self, SfsError>
    where
        Self: Sized,
    {
        // Push L3 payload size to front (on-disk order: L3 before L4)
        slot_sizes.push_front(L3_PAYLOAD_SIZE);

        let layer2 = L2::create(path, block_size_shift, block_index_width, slot_sizes)?;
        let my_slot = layer2.header_slot_for_upper(0);

        // Write initial L3 payload
        let streams_desc = StreamDescriptor {
            size: 0,
            top_block: 0,
            reserved: 0,
        };
        let l3_payload = Self::serialize_header(&streams_desc);
        layer2.write_header_slot(my_slot, &l3_payload)?;

        Ok(StreamsFromBlocks {
            layer2,
            streams_descriptor: Mutex::new(streams_desc),
            my_slot,
            state: Mutex::new(StreamsFromBlocksState {
                next_handle_id: 0,
                locks: HashMap::new(),
                open_handles: HashMap::new(),
            }),
            lock_released: Condvar::new(),
        })
    }

    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError>
    where
        Self: Sized,
    {
        let layer2 = L2::open(path, mode)?;
        let my_header_slot = layer2.header_slot_for_upper(0);

        // Read and parse L3 payload
        let header_buffer = layer2.read_header_slot(my_header_slot)?;
        let streams_desc = Self::deserialize_header(&header_buffer)?;

        Ok(StreamsFromBlocks {
            layer2,
            streams_descriptor: Mutex::new(streams_desc),
            my_slot: my_header_slot,
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

    fn create_stream(&self) -> Result<u64, SfsError> {
        // we're about to create a new stream, so we need find an available stream descriptor from the Streams stream
        // therefore we need to acquire the STREAMS_STREAM_ID write lock to ensure exclusive access while we scan for a free descriptor and potentially extend the stream. This is the main reason for the STREAMS_STREAM_ID lock: it protects the integrity of the stream descriptors in the Streams stream.
        self.acquire_lock(STREAMS_STREAM_ID, OpenMode::Write, true)?;

        let result = (|| -> Result<u64, SfsError> {
            // we should never block on this call. The Mutex is just to satisfy Rust's safety guarantees around shared mutable access to the descriptor, but in practice we always hold the STREAMS_STREAM_ID write lock when accessing it, so there is no contention.
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

            // Did we find a free slot? If not, we need to extend the Streams stream by adding a new descriptor at the end
            let stream_id = match free_id {
                Some(id) => id,
                None => {
                    // Extend the Streams stream
                    // TODO: optimize by writing a zeroed block instead of individual descriptors (this is currently safe, but may not be in the future, so while possibly more efficient, it's also slightly riskier to just zero it out)
                    // TODO: Make it configurable how much to extend by
                    // new id would be at the end of the slots, so we just need to write a new descriptor there with size=0 and top_block=0
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
                    };
                    let offset = new_id * DESCRIPTOR_SIZE;
                    let buf = new_desc.to_bytes();
                    pyramid_write(&self.layer2, &mut streams_desc, offset, &buf, bs, fo, biw)?;
                    new_id
                }
            };

            // Write the new descriptor (size=0, top_block=0, reserved=0)
            let new_desc = StreamDescriptor {
                size: 0,
                top_block: 0,
                reserved: 0,
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
                let num_blocks = data_blocks_needed(capacity, bs);
                let depth = pyramid_depth(num_blocks, fo);
                deallocate_tree(&self.layer2, desc.top_block, depth, fo, biw)?;
            }

            // Mark descriptor as free
            let free_desc = StreamDescriptor {
                size: 0,
                top_block: FREE_DESCRIPTOR_MARKER,
                reserved: 0,
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
        pyramid_read(&self.layer2, &desc, pos, buf, bs, fo, biw)
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
        let n = pyramid_write(&self.layer2, &mut desc, pos, buf, bs, fo, biw)?;

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
        pyramid_truncate(&self.layer2, &mut desc, new_len, bs, fo, biw)?;

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
        pyramid_reserve(&self.layer2, &mut desc, n_bytes, bs, fo, biw)?;

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
                            let num_blocks = data_blocks_needed(capacity, bs);
                            let depth = pyramid_depth(num_blocks, fo);
                            let stream_label = i.to_string();
                            let mut collector = TreeCollector {
                                blocks: &mut all_claimed_blocks,
                                issues: &mut issues,
                                label: &stream_label,
                            };
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
