use crate::{OpenMode, SfsError};

/// L3 trait: Data stream abstraction.
///
/// Provides numbered stream management to L4. L3 implementations handle
/// stream creation, deletion, reading, writing, and per-stream locking
/// (one writer OR many readers per stream).
///
/// All methods take `&self` (not `&mut self`) so that a single L3 instance
/// can be safely shared across threads. Implementations use interior
/// mutability (e.g. `Mutex`) to protect bookkeeping state.
///
/// `block_index_width` and `block_size_shift` are runtime values stored in
/// the file header. Internally all stream IDs use u64; on-disk serialization
/// uses only `block_index_width` bytes.
pub trait StreamLayer: Send + Sync {
    /// Handle type for open streams. Must be Copy so L4 can extract handles
    /// from its internal maps without borrow conflicts.
    type Handle: Copy;

    /// Create a new L3 storage at the given path.
    ///
    /// `block_index_width` is the number of bytes used for block indices on
    /// disk (e.g. 2, 4, or 8).
    /// `block_size_shift` is the power-of-2 exponent for block size
    /// (e.g. 12 → 4096 bytes).
    fn create(path: &str, block_index_width: u8, block_size_shift: u8) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// Open an existing L3 storage at the given path.
    /// Reads `block_index_width` and `block_size_shift` from the stored metadata.
    fn open(path: &str) -> Result<Self, SfsError>
    where
        Self: Sized;

    /// The number of bytes used for block indices on disk.
    fn block_index_width(&self) -> u8;

    /// Block size as a power of 2 (e.g. 12 → 4096 bytes).
    fn block_size_shift(&self) -> u8;

    /// Create a new stream. Returns the stream identifier.
    fn create_stream(&self) -> Result<u64, SfsError>;

    /// Check whether a stream with the given identifier exists.
    fn stream_exists(&self, id: u64) -> bool;

    /// Open an existing stream by identifier.
    /// Enforces locking: one writer OR many readers per stream.
    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, SfsError>;

    /// Close a stream handle.
    fn close_stream(&self, handle: Self::Handle) -> Result<(), SfsError>;

    /// Delete a stream by identifier. Fails if the stream is currently open.
    fn delete_stream(&self, id: u64) -> Result<(), SfsError>;

    /// Read from a stream at the given position.
    /// Returns the number of bytes actually read.
    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError>;

    /// Write to a stream at the given position.
    /// Extends the stream if writing past the end.
    /// Returns the number of bytes written.
    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, SfsError>;

    /// Get the total length of a stream in bytes.
    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, SfsError>;

    /// Truncate a stream to the given length.
    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), SfsError>;
}
