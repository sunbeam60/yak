use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

/// Controls which cache layer a block read/write should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    /// Bypass all caches; read from / write through to L1 directly.
    None,
    /// Per-thread LRU cache (no cross-thread synchronization).
    ThreadLocal,
    /// Shared Mutex-guarded LRU cache (cross-thread coherent).
    Shared,
}

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
        password: Option<&[u8]>,
    ) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Open an existing L2 storage at the given path.
    /// `mode` is forwarded to L1 for file-level locking.
    /// `password` is required if the file was created with encryption.
    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Returns true if this L2 instance has encryption enabled.
    fn is_encrypted(&self) -> bool {
        false
    }

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
    fn allocate_block(&self) -> Result<u64, YakError>;

    /// Allocate multiple blocks at once. Returns a vector of block indices.
    /// All block contents are zeroed. The default implementation calls
    /// `allocate_block` in a loop; implementations may override to batch
    /// file growth and header persistence for better performance.
    fn allocate_blocks(&self, count: u64) -> Result<Vec<u64>, YakError> {
        let mut result = Vec::with_capacity(count as usize);
        for _ in 0..count {
            result.push(self.allocate_block()?);
        }
        Ok(result)
    }

    /// Deallocate a block, returning it for future reuse.
    fn deallocate_block(&self, index: u64) -> Result<(), YakError>;

    /// Deallocate multiple blocks at once, returning them for future reuse.
    /// The default implementation calls `deallocate_block` in a loop.
    /// `BlocksInFile` overrides this to sort indices and write the free-list
    /// chain in block order with a single header persist.
    fn deallocate_blocks(&self, indices: &mut Vec<u64>) -> Result<(), YakError> {
        for &index in indices.iter() {
            self.deallocate_block(index)?;
        }
        Ok(())
    }

    /// Read from a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// `cache` selects which cache to consult/populate, or `None` to bypass.
    /// Returns the number of bytes actually read.
    fn read_block(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        cache: CacheMode,
    ) -> Result<usize, YakError>;

    /// Write to a block at the given offset within the block.
    /// `offset + buf.len()` must not exceed `block_size`.
    /// `cache` selects which cache to keep coherent, or `None` to bypass.
    /// Returns the number of bytes actually written.
    fn write_block(
        &self,
        index: u64,
        offset: usize,
        buf: &[u8],
        cache: CacheMode,
    ) -> Result<usize, YakError>;

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
    ) -> Result<usize, YakError> {
        let bs = self.block_size();
        let mut bytes_read = 0;
        let mut block = start_index;
        let mut off = offset;
        while bytes_read < buf.len() {
            let chunk = (buf.len() - bytes_read).min(bs - off);
            let n = self.read_block(
                block,
                off,
                &mut buf[bytes_read..bytes_read + chunk],
                CacheMode::None,
            )?;
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
    ) -> Result<usize, YakError> {
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
                CacheMode::None,
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
    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError>;

    /// Read a header section by slot ID (pass-through to L1).
    ///
    /// Returns the section payload (without the 2-byte length prefix).
    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError>;

    /// Clear the calling thread's per-thread block cache.
    ///
    /// Must be called before accessing a user stream that another thread may
    /// have modified (e.g. a shared directory stream). The shared cache is
    /// unaffected — it remains cross-thread coherent.
    fn invalidate_thread_local_cache(&self) {}

    /// Run L2 integrity checks. `claimed_blocks` are block IDs that upper
    /// layers assert are in use. L2 validates that claimed + free == all blocks,
    /// with no overlaps, no orphans, and no free-list cycles.
    /// Returns a list of issues found (including any L1 issues).
    /// Default implementation returns empty (no checks).
    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, YakError> {
        let _ = claimed_blocks;
        Ok(Vec::new())
    }
}
