use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

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
    /// `compressed_block_size_shift` is the power-of-2 exponent for the
    /// compressed block size used by compressed streams (0 = compression not
    /// configured). Must be >= `block_size_shift` when non-zero.
    /// `slot_sizes` accumulates payload sizes (NOT including the
    /// 2-byte length prefix) as they flow down through the layer chain.
    /// L3 push_front's its own payload size and passes the deque down to L2.
    fn create(
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
        compressed_block_size_shift: u8,
        slot_sizes: VecDeque<u16>,
        password: Option<&[u8]>,
    ) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Open an existing L3 storage at the given path.
    /// Reads `block_index_width` and `block_size_shift` from the stored metadata.
    /// `mode` is forwarded to L2/L1 for file-level locking.
    /// `password` is forwarded to L2 for encryption support.
    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Returns true if the underlying storage has encryption enabled.
    fn is_encrypted(&self) -> bool {
        false
    }

    /// The number of bytes used for block indices on disk.
    fn block_index_width(&self) -> u8;

    /// Block size as a power of 2 (e.g. 12 → 4096 bytes).
    fn block_size_shift(&self) -> u8;

    /// Compressed block size shift (0 = compression not configured).
    fn compressed_block_size_shift(&self) -> u8;

    /// Create a new stream. Returns the stream identifier.
    /// If `compressed` is true, the stream will use leaf-level compression.
    /// Requires compressed_block_size_shift > 0 when `compressed` is true.
    fn create_stream(&self, compressed: bool) -> Result<u64, YakError>;

    /// Check whether a stream with the given identifier exists.
    fn stream_exists(&self, id: u64) -> bool;

    /// Return the number of active (non-free) streams.
    fn stream_count(&self) -> Result<u64, YakError>;

    /// Return the identifiers of all active (non-free) streams.
    fn stream_ids(&self) -> Result<Vec<u64>, YakError>;

    /// Open an existing stream by identifier.
    /// Enforces locking: one writer OR many readers per stream.
    /// Returns `LockConflict` immediately if the stream is already locked
    /// in a conflicting way.
    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError>;

    /// Open an existing stream by identifier, blocking until the lock is
    /// available. Used by L4 for short-lived internal directory operations
    /// that should wait rather than fail on contention.
    fn open_stream_blocking(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError> {
        self.open_stream(id, mode)
    }

    /// Close a stream handle.
    fn close_stream(&self, handle: Self::Handle) -> Result<(), YakError>;

    /// Delete a stream by identifier. Fails if the stream is currently open.
    fn delete_stream(&self, id: u64) -> Result<(), YakError>;

    /// Read from a stream at the given position.
    /// Returns the number of bytes actually read.
    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, YakError>;

    /// Write to a stream at the given position.
    /// Extends the stream if writing past the end.
    /// Returns the number of bytes written.
    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, YakError>;

    /// Get the total length of a stream in bytes.
    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, YakError>;

    /// Truncate a stream to the given length.
    /// Also frees excess reserved blocks and resets reserved capacity.
    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), YakError>;

    /// Pre-allocate storage so that at least `n_bytes` worth of data capacity
    /// is available for the stream. Does not change the stream's logical size.
    /// The actual reserved capacity may be larger than `n_bytes` (rounded up to
    /// block boundaries). Errors if `n_bytes < stream length`.
    /// Requires the stream to be open for writing.
    fn reserve(&self, handle: &Self::Handle, n_bytes: u64) -> Result<(), YakError>;

    /// Return the current reserved capacity (allocated block capacity in bytes).
    /// This is always >= stream_length and always a multiple of the block size (or 0).
    fn stream_reserved(&self, handle: &Self::Handle) -> Result<u64, YakError>;

    /// Check whether a stream is compressed.
    fn is_stream_compressed(&self, handle: &Self::Handle) -> Result<bool, YakError> {
        let _ = handle;
        Ok(false)
    }

    /// Get the `HeaderSlotId` for the `index`-th upper layer section.
    ///
    /// Index 0 = first upper section (L4), etc.
    /// Implemented as `self.l2.header_slot_for_upper(index + 1)` for the real L3.
    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId;

    /// Write a header section by slot ID (pass-through to L2/L1).
    ///
    /// `data` is the section payload: `identifier[6] + version[1] + payload`.
    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError>;

    /// Read a header section by slot ID (pass-through to L2/L1).
    ///
    /// Returns the section payload (without the 2-byte length prefix).
    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError>;

    /// Run L3 integrity checks. `claimed_streams` are stream IDs that L4
    /// asserts should exist (directory streams + data streams).
    /// L3 validates stream descriptors, walks pyramid structures to collect
    /// all block IDs, then passes them down to L2 for verification.
    /// Returns a list of issues found (including L2 and L1 issues).
    fn verify(&self, claimed_streams: &[u64]) -> Result<Vec<String>, YakError>;
}
