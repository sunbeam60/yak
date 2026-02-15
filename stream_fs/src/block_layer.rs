use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, SfsError};

/// L2 trait: Block storage abstraction.
///
/// Provides numbered fixed-size block management to L3. L2 implementations
/// handle block allocation, deallocation, reading, writing, and header
/// storage.
///
/// All methods take `&self` (not `&mut self`) so that a single L2 instance
/// can be safely shared across threads. Implementations use interior
/// mutability (e.g. `Mutex`) to protect bookkeeping state.
///
/// `block_size_shift` is the power-of-2 exponent for block size.
/// `block_index_width` is the number of bytes used for block indices on disk.
pub trait BlockLayer: Send + Sync {
    /// Create a new L2 storage at the given path.
    ///
    /// `block_size_shift` is the power-of-2 exponent for block size
    /// (e.g. 12 -> 4096 bytes).
    /// `block_index_width` is the number of bytes used for block indices
    /// on disk (e.g. 2, 4, or 8).
    /// `slot_sizes` accumulates payload sizes (NOT including the
    /// 2-byte length prefix) as they flow down through the layer chain.
    /// L2 push_front's its own payload size and passes the deque down to L1.
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        slot_sizes: VecDeque<u16>,
    ) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Open an existing L2 storage at the given path.
    /// `mode` is forwarded to L1 for file-level locking.
    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError>
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

    /// Allocate multiple blocks at once. Returns a vector of block indices.
    /// All block contents are zeroed. The default implementation calls
    /// `allocate_block` in a loop; implementations may override to batch
    /// file growth and header persistence for better performance.
    fn allocate_blocks(&self, count: u64) -> Result<Vec<u64>, SfsError> {
        let mut result = Vec::with_capacity(count as usize);
        for _ in 0..count {
            result.push(self.allocate_block()?);
        }
        Ok(result)
    }

    /// Deallocate a block, returning it for future reuse.
    fn deallocate_block(&self, index: u64) -> Result<(), SfsError>;

    /// Read from a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// When `cache` is true, the block may be served from and stored in the
    /// block cache. When false, the cache is bypassed entirely.
    /// Returns the number of bytes actually read.
    fn read_block(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        cache: bool,
    ) -> Result<usize, SfsError>;

    /// Write to a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// When `cache` is true, any cached copy of this block is kept coherent.
    /// When false, the cache is not consulted or updated.
    /// Returns the number of bytes actually written.
    fn write_block(
        &self,
        index: u64,
        offset: usize,
        buf: &[u8],
        cache: bool,
    ) -> Result<usize, SfsError>;

    /// Read across a run of contiguous block indices in a single operation.
    ///
    /// Reads `buf.len()` bytes starting at `offset` within block `start_index`,
    /// continuing through consecutive blocks as needed. The caller must ensure
    /// that blocks `start_index` through
    /// `start_index + (offset + buf.len() - 1) / block_size` are contiguous
    /// on the underlying storage.
    ///
    /// The default implementation splits into per-block `read_block()` calls.
    /// `BlocksInFile` overrides this with a single L1 read.
    fn read_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, SfsError> {
        let bs = self.block_size();
        let mut bytes_read = 0;
        let mut block = start_index;
        let mut off = offset;
        while bytes_read < buf.len() {
            let chunk = (buf.len() - bytes_read).min(bs - off);
            let n = self.read_block(block, off, &mut buf[bytes_read..bytes_read + chunk], false)?;
            bytes_read += n;
            block += 1;
            off = 0;
        }
        Ok(bytes_read)
    }

    /// Write across a run of contiguous block indices in a single operation.
    ///
    /// Writes `buf.len()` bytes starting at `offset` within block `start_index`,
    /// continuing through consecutive blocks as needed. The caller must ensure
    /// that blocks `start_index` through
    /// `start_index + (offset + buf.len() - 1) / block_size` are contiguous
    /// on the underlying storage.
    ///
    /// The default implementation splits into per-block `write_block()` calls.
    /// `BlocksInFile` overrides this with a single L1 write.
    fn write_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &[u8],
    ) -> Result<usize, SfsError> {
        let bs = self.block_size();
        let mut bytes_written = 0;
        let mut block = start_index;
        let mut off = offset;
        while bytes_written < buf.len() {
            let chunk = (buf.len() - bytes_written).min(bs - off);
            let n = self.write_block(
                block,
                off,
                &buf[bytes_written..bytes_written + chunk],
                false,
            )?;
            bytes_written += n;
            block += 1;
            off = 0;
        }
        Ok(bytes_written)
    }

    /// Get the `HeaderSlotId` for the `index`-th upper layer section.
    ///
    /// Index 0 = first upper section (L3), index 1 = next (L4), etc.
    /// Implemented as `self.l1.header_slot_for_upper(index + 1)` for the real L2.
    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId;

    /// Write a header section by slot ID (pass-through to L1).
    ///
    /// `data` is the section payload: `identifier[6] + version[1] + payload`.
    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), SfsError>;

    /// Read a header section by slot ID (pass-through to L1).
    ///
    /// Returns the section payload (without the 2-byte length prefix).
    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, SfsError>;

    /// Invalidate the calling thread's block cache.
    ///
    /// Used by L3 to ensure cross-thread coherency for shared data
    /// structures (e.g., the Streams stream). Implementations that
    /// maintain a per-thread cache should clear it so subsequent reads
    /// go to the backing store. The default implementation is a no-op.
    fn invalidate_block_cache(&self) {}

    /// Run L2 integrity checks. `claimed_blocks` are block IDs that upper
    /// layers assert are in use. L2 validates that claimed + free == all blocks,
    /// with no overlaps, no orphans, and no free-list cycles.
    /// Returns a list of issues found (including any L1 issues).
    /// Default implementation returns empty (no checks).
    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, SfsError> {
        let _ = claimed_blocks;
        Ok(Vec::new())
    }
}
