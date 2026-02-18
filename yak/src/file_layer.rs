use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

/// L1 trait: File system abstraction.
///
/// Wraps OS file I/O and provides random-access reads and writes to a single
/// file. L1 is header-aware: it understands the Yak header format (magic +
/// length-prefixed layer sections) and knows where block data starts.
///
/// Header sections for upper layers are managed via a slot system. During
/// `create()`, payload sizes flow down and L1 allocates fixed-size slots.
/// Each layer can then independently read/write its own section using
/// `read_header_slot` / `write_header_slot` with its `HeaderSlotId`.
///
/// All methods take `&self` (not `&mut self`) so that a single L1 instance
/// can be safely shared across threads. Implementations use interior
/// mutability (e.g. `Mutex`) to protect file state.
pub trait FileLayer: Send + Sync {
    /// Create a new Yak file at the given path.
    ///
    /// `slot_sizes` contains the payload size (in bytes, NOT including
    /// the 2-byte length prefix) for each upper layer's header section,
    /// in on-disk order (inner-to-outer): `[L2_payload, L3_payload, L4_payload]`.
    /// Each layer push_front's its own size as the deque flows down.
    ///
    /// L1 writes: magic + L1 section + empty slots (length-prefixed, zeroed
    /// payloads). After this call, each layer can fill its slot via
    /// `write_header_slot`.
    fn create(path: &str, slot_sizes: VecDeque<u16>) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Open an existing Yak file at the given path.
    ///
    /// L1 reads the magic, validates the header format version, reads the
    /// L1 section, and rebuilds the slot registry from the remaining
    /// length-prefixed sections.
    ///
    /// Acquires a shared process lock for `Read` mode or an exclusive lock
    /// for `Write` mode.
    fn open(path: &str, mode: OpenMode) -> Result<Self, YakError>
    where
        Self: Sized;

    /// Byte offset in the file where block data begins
    /// (immediately after ALL header sections).
    ///
    /// Equal to `total_header_length` from the magic section.
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

    /// Get the `HeaderSlotId` for the `index`-th upper layer section.
    ///
    /// Index 0 = first upper section (L2), index 1 = next (L3), etc.
    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId;

    /// Write a header section by slot ID.
    ///
    /// `data` is the section payload: `identifier[6] + version[1] + payload`.
    /// L1 validates that `data.len()` matches the slot's expected payload size,
    /// then writes `[length_prefix: u16] + data` at the slot's file offset.
    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError>;

    /// Read a header section by slot ID.
    ///
    /// Returns the section payload (without the 2-byte length prefix):
    /// `identifier[6] + version[1] + payload`.
    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError>;

    /// Run L1 integrity checks. Returns a list of issues found.
    /// Default implementation returns empty (no checks).
    fn verify(&self) -> Result<Vec<String>, YakError> {
        Ok(Vec::new())
    }
}
