use crate::{OpenMode, YakError};

/// L1 trait: File system abstraction.
///
/// Wraps OS file I/O and provides random-access reads and writes to a single
/// file. L1 writes a minimal bootstrap header (magic prefix with `data_offset`)
/// and provides raw I/O. All mutable metadata is managed by L2 in block 0.
///
/// All methods take `&self` (not `&mut self`) so that a single L1 instance
/// can be safely shared across threads. Implementations use interior
/// mutability (e.g. `Mutex`) to protect file state.
pub trait FileLayer: Send + Sync {
    /// Create a new Yak file at the given path.
    ///
    /// `data_offset` is the byte offset where block data begins. L1 writes
    /// the magic prefix (including `data_offset`) and zero-fills the
    /// bootstrap region up to `data_offset`. The caller (L2) writes its
    /// bootstrap fields into this region after creation.
    fn create(path: &str, data_offset: u64) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Open an existing Yak file at the given path.
    ///
    /// L1 reads the magic prefix, validates the header format version, and
    /// extracts `data_offset`.
    ///
    /// Acquires a shared process lock for `Read` mode or an exclusive lock
    /// for `Write` mode.
    fn open(path: &str, mode: OpenMode) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Byte offset in the file where block data begins
    /// (immediately after the bootstrap header).
    ///
    /// This value is fixed at create time and never changes.
    fn data_offset(&self) -> u64;

    /// Read bytes at the given absolute file offset.
    /// Returns the number of bytes actually read.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, YakError>;

    /// Write bytes at the given absolute file offset.
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), YakError>;

    /// Get current file length in bytes.
    fn len(&self) -> Result<u64, YakError>;

    /// Check whether the file is empty (length == 0).
    fn is_empty(&self) -> Result<bool, YakError> {
        Ok(self.len()? == 0)
    }

    /// Set file length. Used to grow the file when allocating new blocks,
    /// or to shrink it.
    fn set_len(&self, len: u64) -> Result<(), YakError>;

    /// Run L1 integrity checks. Returns a list of issues found.
    /// Default implementation returns empty (no checks).
    fn verify(&self) -> Result<Vec<String>, YakError> {
        Ok(Vec::new())
    }
}
