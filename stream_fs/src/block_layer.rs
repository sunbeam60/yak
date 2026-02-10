use crate::{OpenMode, SfsError};

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
    /// `upper_layers` contains the accumulated header sections from L3+L4
    /// (possibly with placeholder values). L2 prepends its own section
    /// and passes everything down to L1 (or writes to disk).
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        upper_layers: &[u8],
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
