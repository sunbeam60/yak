use crate::{OpenMode, SfsError};

/// L1 trait: File system abstraction.
///
/// Wraps OS file I/O and provides random-access reads and writes to a single
/// file. L1 is header-aware: it understands the SFS header format (magic +
/// length-prefixed layer sections) and knows where block data starts.
///
/// All methods take `&self` (not `&mut self`) so that a single L1 instance
/// can be safely shared across threads. Implementations use interior
/// mutability (e.g. `Mutex`) to protect file state.
pub trait FileLayer: Send + Sync {
    /// Create a new SFS file at the given path.
    ///
    /// `upper_layers` contains the accumulated header sections from L2+L3+L4
    /// (possibly with placeholder values — correct sizes, uninitialised data).
    ///
    /// L1 writes: magic (including `total_header_length`) + L1 section +
    /// upper_layers to the file, then acquires an exclusive process lock.
    /// After this call, `data_offset()` returns the byte offset where block
    /// data begins (equal to `total_header_length`).
    fn create(path: &str, upper_layers: &[u8]) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Open an existing SFS file at the given path.
    ///
    /// L1 reads the magic, validates the header format version, reads all
    /// length-prefixed header sections, pops its own (L1) section, validates
    /// it, and caches the remainder for `load_header()`.
    ///
    /// Acquires a shared process lock for `Read` mode or an exclusive lock
    /// for `Write` mode.
    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Byte offset in the file where upper layer header data begins
    /// (immediately after magic + L1 section).
    ///
    /// L2 uses this to know where its header section lives in the file,
    /// enabling efficient in-place updates of bookkeeping fields.
    fn upper_layers_offset(&self) -> u64;

    /// Byte offset in the file where block data begins
    /// (immediately after ALL header sections).
    ///
    /// Equal to `total_header_length` from the magic section.
    /// This value is fixed at create time and never changes.
    fn data_offset(&self) -> u64;

    /// Read bytes at the given absolute file offset.
    /// Returns the number of bytes actually read.
    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SfsError>;

    /// Write bytes at the given absolute file offset.
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), SfsError>;

    /// Get current file length in bytes.
    fn len(&self) -> Result<u64, SfsError>;

    /// Check whether the file is empty (length == 0).
    fn is_empty(&self) -> Result<bool, SfsError> {
        Ok(self.len()? == 0)
    }

    /// Set file length. Used to grow the file when allocating new blocks,
    /// or to shrink it.
    fn set_len(&self, len: u64) -> Result<(), SfsError>;

    /// Flush all writes to disk.
    fn flush(&self) -> Result<(), SfsError>;

    /// Rewrite the upper layer header sections in-place.
    ///
    /// L1 rewrites: magic + L1 section + upper_layers.
    /// The length of `upper_layers` must equal the original length passed
    /// during create (section sizes are fixed — only values change).
    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError>;

    /// Read and return the upper layer header sections
    /// (everything after magic + L1 section).
    fn load_header(&self) -> Result<Vec<u8>, SfsError>;
}
