use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

use crate::stream_layer::StreamLayer;

/// L4 payload size: "filing"(6) + version(1) + root_dir_stream_id(8) = 15 bytes.
const L4_PAYLOAD_SIZE: u16 = 15;

/// Current L4 format version. Bumped when the on-disk directory entry format changes.
/// Version 2: sorted name table with O(log n) binary search lookup.
/// Version 3: footer shrunk from 8 to 4 bytes (entry_count derived from name_table_offset).
const L4_FORMAT_VERSION: u8 = 3;

/// Size of one name table slot: hash (u32) + offset (u32) = 8 bytes.
const NAME_TABLE_SLOT_SIZE: usize = 8;

/// Size of the directory stream footer: name_table_offset (u32) = 4 bytes.
const FOOTER_SIZE: usize = 4;

/// Size of the copy buffer used when compacting entry data on delete.
const COMPACT_COPY_BUF_SIZE: usize = 64 * 1024;

/// FNV-1a hash of a byte slice, producing a 32-bit hash.
/// Used to accelerate directory entry lookups by comparing a cheap integer
/// before falling back to a full name comparison.
fn fnv1a_hash(bytes: &[u8]) -> u32 {
    const FNV_OFFSET: u32 = 2166136261;
    const FNV_PRIME: u32 = 16777619;
    let mut hash = FNV_OFFSET;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Derive the entry count from the stream length and name_table_offset.
/// The name table sits between name_table_offset and the footer, so its
/// byte size divided by NAME_TABLE_SLOT_SIZE gives the number of entries.
fn entry_count_from_footer(stream_len: usize, name_table_offset: u32) -> u32 {
    ((stream_len - FOOTER_SIZE - name_table_offset as usize) / NAME_TABLE_SLOT_SIZE) as u32
}

#[derive(Debug)]
pub enum YakError {
    NotFound(String),
    AlreadyExists(String),
    NotEmpty(String),
    InvalidPath(String),
    LockConflict(String),
    SeekOutOfBounds(String),
    IoError(String),
    ReadOnly(String),
    WrongPassword(String),
    EncryptionRequired(String),
}

impl fmt::Display for YakError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            YakError::NotFound(msg) => write!(f, "not found: {}", msg),
            YakError::AlreadyExists(msg) => write!(f, "already exists: {}", msg),
            YakError::NotEmpty(msg) => write!(f, "not empty: {}", msg),
            YakError::InvalidPath(msg) => write!(f, "invalid path: {}", msg),
            YakError::LockConflict(msg) => write!(f, "lock conflict: {}", msg),
            YakError::SeekOutOfBounds(msg) => write!(f, "seek out of bounds: {}", msg),
            YakError::IoError(msg) => write!(f, "I/O error: {}", msg),
            YakError::ReadOnly(msg) => write!(f, "read-only: {}", msg),
            YakError::WrongPassword(msg) => write!(f, "wrong password: {}", msg),
            YakError::EncryptionRequired(msg) => write!(f, "encryption required: {}", msg),
        }
    }
}

impl std::error::Error for YakError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Stream,
    Directory,
}

#[derive(Debug)]
pub struct DirEntry {
    pub name: String,
    pub entry_type: EntryType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Read,
    Write,
}

/// Opaque handle to an open stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamHandle(u64);

impl StreamHandle {
    pub fn id(&self) -> u64 {
        self.0
    }

    pub fn from_id(id: u64) -> StreamHandle {
        StreamHandle(id)
    }
}

// ---------------------------------------------------------------------------
// Directory stream entry serialization
// ---------------------------------------------------------------------------

/// An entry inside a directory stream.
struct StreamEntry {
    id: u64,
    name: String,
}

/// Serialize a single entry's data region bytes (no name table or footer).
/// Format: | id: biw bytes | name_len: u16 | name: [u8] |
fn serialize_entry_data(entry: &StreamEntry, block_index_width: u8) -> Vec<u8> {
    let biw = block_index_width as usize;
    let name_bytes = entry.name.as_bytes();
    let name_len = name_bytes.len() as u16;
    let total = biw + 2 + name_bytes.len();
    let mut buf = Vec::with_capacity(total);
    let id_bytes = entry.id.to_le_bytes();
    buf.extend_from_slice(&id_bytes[..biw]);
    buf.extend_from_slice(&name_len.to_le_bytes());
    buf.extend_from_slice(name_bytes);
    buf
}

/// Build a complete directory stream (entry data + name table + footer).
/// Returns empty Vec for an empty entry list (empty directory = zero-length stream).
fn build_directory_stream(entries: &[StreamEntry], block_index_width: u8) -> Vec<u8> {
    if entries.is_empty() {
        return Vec::new();
    }

    // Write entry data region, recording each entry's offset and hash
    let mut entry_data = Vec::new();
    let mut slots: Vec<(u32, u32)> = Vec::with_capacity(entries.len());
    for entry in entries {
        let offset = entry_data.len() as u32;
        let hash = fnv1a_hash(entry.name.as_bytes());
        entry_data.extend_from_slice(&serialize_entry_data(entry, block_index_width));
        slots.push((hash, offset));
    }

    // Sort name table slots by hash for binary search
    slots.sort_by_key(|&(hash, _)| hash);

    let name_table_offset = entry_data.len() as u32;

    // Assemble: entry_data + name_table + footer
    let total = entry_data.len() + slots.len() * NAME_TABLE_SLOT_SIZE + FOOTER_SIZE;
    let mut buf = Vec::with_capacity(total);
    buf.extend_from_slice(&entry_data);
    for &(hash, offset) in &slots {
        buf.extend_from_slice(&hash.to_le_bytes());
        buf.extend_from_slice(&offset.to_le_bytes());
    }
    buf.extend_from_slice(&name_table_offset.to_le_bytes());
    buf
}

/// Parse the footer from the last FOOTER_SIZE bytes of a directory stream buffer.
/// Returns name_table_offset.
fn parse_footer(data: &[u8]) -> Result<u32, YakError> {
    if data.len() < FOOTER_SIZE {
        return Err(YakError::IoError(
            "directory stream too short for footer".to_string(),
        ));
    }
    let footer_start = data.len() - FOOTER_SIZE;
    let name_table_offset =
        u32::from_le_bytes(data[footer_start..footer_start + 4].try_into().unwrap());
    Ok(name_table_offset)
}

/// Parse all entries from a complete directory stream buffer.
/// Reads the footer to find name_table_offset, then scans the entry data region.
fn parse_entries(data: &[u8], block_index_width: u8) -> Result<Vec<StreamEntry>, YakError> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let name_table_offset = parse_footer(data)?;
    let nto = name_table_offset as usize;
    let biw = block_index_width as usize;
    let entry_count = entry_count_from_footer(data.len(), name_table_offset);
    // Minimum entry size: biw (id) + 2 (name_len) + 0 (empty name not valid, but parsing allows it)
    let min_entry_size = biw + 2;

    let mut entries = Vec::with_capacity(entry_count as usize);
    let mut pos = 0;
    while pos < nto {
        if pos + min_entry_size > nto {
            return Err(YakError::IoError(
                "truncated entry in directory stream".to_string(),
            ));
        }
        let mut id_bytes = [0u8; 8];
        id_bytes[..biw].copy_from_slice(&data[pos..pos + biw]);
        let id = u64::from_le_bytes(id_bytes);

        let name_len = u16::from_le_bytes([data[pos + biw], data[pos + biw + 1]]) as usize;
        let name_end = pos + biw + 2 + name_len;
        if name_end > nto {
            return Err(YakError::IoError(
                "truncated entry name in directory stream".to_string(),
            ));
        }
        let name = std::str::from_utf8(&data[pos + biw + 2..name_end])
            .map_err(|e| YakError::IoError(format!("invalid UTF-8 in entry name: {}", e)))?
            .to_string();
        entries.push(StreamEntry { id, name });
        pos = name_end;
    }

    Ok(entries)
}

/// Binary search the name table for slots matching `target_hash`.
/// The name table is a sorted array of (hash: u32, offset: u32) pairs.
/// Returns the range of matching slot indices, or an empty range if not found.
fn find_hash_range(
    name_table: &[u8],
    entry_count: u32,
    target_hash: u32,
) -> std::ops::Range<usize> {
    let count = entry_count as usize;
    if count == 0 {
        return 0..0;
    }

    // Read hash from slot index
    let hash_at = |i: usize| -> u32 {
        let base = i * NAME_TABLE_SLOT_SIZE;
        u32::from_le_bytes([
            name_table[base],
            name_table[base + 1],
            name_table[base + 2],
            name_table[base + 3],
        ])
    };

    // Binary search for any occurrence of target_hash
    let mut lo = 0usize;
    let mut hi = count;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if hash_at(mid) < target_hash {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    // lo is the first slot >= target_hash
    if lo >= count || hash_at(lo) != target_hash {
        return lo..lo; // not found
    }
    let start = lo;
    // Find the end of the run of matching hashes
    let mut end = start + 1;
    while end < count && hash_at(end) == target_hash {
        end += 1;
    }
    start..end
}

/// Read offset from a name table slot.
fn slot_offset(name_table: &[u8], slot_index: usize) -> u32 {
    let base = slot_index * NAME_TABLE_SLOT_SIZE + 4;
    u32::from_le_bytes([
        name_table[base],
        name_table[base + 1],
        name_table[base + 2],
        name_table[base + 3],
    ])
}

/// Result of a name-table lookup: everything callers might need.
struct FoundEntry {
    slot_idx: usize,
    offset: u32,
    id: u64,
    entry_size: usize,
}

/// Parse the name table bytes into a Vec of (hash, offset) pairs.
fn parse_name_table_slots(name_table: &[u8], entry_count: u32) -> Vec<(u32, u32)> {
    let count = entry_count as usize;
    let mut slots = Vec::with_capacity(count);
    for i in 0..count {
        let base = i * NAME_TABLE_SLOT_SIZE;
        let hash = u32::from_le_bytes(name_table[base..base + 4].try_into().unwrap());
        let offset = u32::from_le_bytes(name_table[base + 4..base + 8].try_into().unwrap());
        slots.push((hash, offset));
    }
    slots
}

/// Serialize name table slots and footer into a contiguous byte buffer.
fn serialize_table_and_footer(slots: &[(u32, u32)], name_table_offset: u32) -> Vec<u8> {
    let total = slots.len() * NAME_TABLE_SLOT_SIZE + FOOTER_SIZE;
    let mut buf = Vec::with_capacity(total);
    for &(hash, offset) in slots {
        buf.extend_from_slice(&hash.to_le_bytes());
        buf.extend_from_slice(&offset.to_le_bytes());
    }
    buf.extend_from_slice(&name_table_offset.to_le_bytes());
    buf
}

// ---------------------------------------------------------------------------
// Path utilities
// ---------------------------------------------------------------------------

fn validate_path(path: &str) -> Result<(), YakError> {
    if path.starts_with('/') || path.ends_with('/') {
        return Err(YakError::InvalidPath(
            "path must not start or end with /".to_string(),
        ));
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(YakError::InvalidPath(
                "path contains empty component".to_string(),
            ));
        }
        if component == "." || component == ".." {
            return Err(YakError::InvalidPath(
                "path cannot contain . or ..".to_string(),
            ));
        }
    }
    Ok(())
}

/// Split "a/b/c" into ("a/b", "c"). Split "c" into ("", "c").
/// Returns borrowed slices — no heap allocations.
fn split_parent_leaf(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (parent, leaf),
        None => ("", path),
    }
}

// ---------------------------------------------------------------------------
// L4 internal state
// ---------------------------------------------------------------------------

/// Internal state for an open stream at the L4 level.
struct OpenStreamInfo<H: Copy> {
    _path: String,
    stream_id: u64,
    l3_handle: H,
    position: u64,
    mode: OpenMode,
}

/// L4 bookkeeping state protected by a Mutex.
struct YakState<H: Copy> {
    next_handle_id: u64,
    open_streams: HashMap<u64, OpenStreamInfo<H>>,
}

/// Represents an open Yak file. Generic over the L3 stream layer.
///
/// Thread-safe: L3 is accessed via `&self`, and L4 bookkeeping is behind
/// a `Mutex`. All public methods take `&self`.
pub struct Yak<L3: StreamLayer> {
    layer3: L3,
    root_dir_stream_id: u64,
    mode: OpenMode,
    state: Mutex<YakState<L3::Handle>>,
}

/// Options for creating a new Yak file.
///
/// Defaults: 4-byte block indices, 4 KB blocks (shift 12),
/// 32 KB compressed blocks (shift 15), no encryption.
pub struct CreateOptions<'a> {
    /// Number of bytes used for block indices on disk (e.g. 2, 4, or 8).
    pub block_index_width: u8,
    /// Power-of-2 exponent for block size (e.g. 12 → 4096 bytes).
    pub block_size_shift: u8,
    /// Power-of-2 exponent for compressed block size (e.g. 15 → 32768 bytes).
    /// Must be >= `block_size_shift` and should be 3 exponents higher for
    /// good compression efficiency.
    pub compressed_block_size_shift: u8,
    /// Optional password for AES-XTS encryption. `None` = no encryption.
    pub password: Option<&'a [u8]>,
}

impl Default for CreateOptions<'_> {
    fn default() -> Self {
        Self {
            block_index_width: 4,
            block_size_shift: 12,
            compressed_block_size_shift: 15,
            password: None,
        }
    }
}

impl<L3: StreamLayer> Yak<L3> {
    // -------------------------------------------------------------------
    // Yak file lifecycle
    // -------------------------------------------------------------------

    /// Create a new Yak file.
    ///
    /// See [`CreateOptions`] for available configuration. Use
    /// `CreateOptions::default()` for 4-byte indices, 4 KB blocks,
    /// 32 KB compressed blocks, no encryption.
    pub fn create(path: &str, opts: CreateOptions) -> Result<Self, YakError> {
        if opts.compressed_block_size_shift < opts.block_size_shift {
            return Err(YakError::IoError(format!(
                "compressed_block_size_shift ({}) must be >= block_size_shift ({})",
                opts.compressed_block_size_shift, opts.block_size_shift
            )));
        }
        Self::create_inner(
            path,
            opts.block_index_width,
            opts.block_size_shift,
            opts.compressed_block_size_shift,
            opts.password,
        )
    }

    fn create_inner(
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
        compressed_block_size_shift: u8,
        password: Option<&[u8]>,
    ) -> Result<Self, YakError> {
        // Pass L4 payload size down through the layer chain so L1 can calculate data_offset
        let layer3 = L3::create(
            path,
            block_index_width,
            block_size_shift,
            compressed_block_size_shift,
            VecDeque::from([L4_PAYLOAD_SIZE]),
            password,
        )?;
        let root_id = layer3.create_stream(false)?;

        // Write L4 header payload via slot
        let l4_slot = layer3.header_slot_for_upper(0);
        let l4_payload = serialize_header(root_id);
        layer3.write_header_slot(l4_slot, &l4_payload)?;

        Ok(Yak {
            layer3,
            root_dir_stream_id: root_id,
            mode: OpenMode::Write,
            state: Mutex::new(YakState {
                next_handle_id: 0,
                open_streams: HashMap::new(),
            }),
        })
    }

    /// Open an existing Yak file.
    ///
    /// `mode` controls file-level access:
    /// - `OpenMode::Read`: shared process lock, write operations are rejected
    /// - `OpenMode::Write`: exclusive process lock, all operations allowed
    pub fn open(path: &str, mode: OpenMode) -> Result<Self, YakError> {
        Self::open_inner(path, mode, None)
    }

    fn open_inner(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError> {
        let layer3 = L3::open(path, mode, password)?;

        // Read L4 header payload via slot
        let l4_slot = layer3.header_slot_for_upper(0);
        let l4_data = layer3.read_header_slot(l4_slot)?;
        let root_id = deserialize_header(&l4_data)?;

        Ok(Yak {
            layer3,
            root_dir_stream_id: root_id,
            mode,
            state: Mutex::new(YakState {
                next_handle_id: 0,
                open_streams: HashMap::new(),
            }),
        })
    }

    /// Check that the Yak file is opened for writing; return `ReadOnly` error if not.
    fn require_write(&self) -> Result<(), YakError> {
        if self.mode == OpenMode::Read {
            return Err(YakError::ReadOnly(
                "Yak file is opened for reading".to_string(),
            ));
        }
        Ok(())
    }

    /// Drain all open stream handles and close them via L3.
    /// Attempts to close ALL handles even if individual closes fail.
    /// Returns the first error encountered, if any.
    fn flush_open_streams(&self) -> Result<(), YakError> {
        let handles: Vec<(u64, OpenStreamInfo<L3::Handle>)> = {
            let mut state = self.state.lock().unwrap();
            state.open_streams.drain().collect()
        };

        let mut first_error: Option<YakError> = None;
        for (_handle_id, info) in handles {
            if let Err(e) = self.layer3.close_stream(info.l3_handle) {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }

        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Close the Yak file. Consumes self.
    ///
    /// Flushes all open stream handles before dropping. If any stream
    /// handle fails to close, all remaining handles are still closed
    /// and the first error is returned.
    pub fn close(self) -> Result<(), YakError> {
        self.flush_open_streams()
        // Drop runs immediately after, but the HashMap is now empty.
    }

    /// Run integrity verification across all layers.
    ///
    /// L4 walks the directory tree to collect all stream IDs (both directory
    /// streams and data streams), then passes them to L3 for further
    /// validation. Returns a list of issues found across all layers.
    ///
    /// This is a read-only operation that works regardless of open mode.
    pub fn verify(&self) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();

        // Collect all stream IDs by walking the directory tree
        let mut all_stream_ids = Vec::new();
        self.collect_stream_ids_recursive(
            self.root_dir_stream_id,
            &mut all_stream_ids,
            &mut issues,
        )?;

        // Check for duplicate stream IDs (a stream referenced from multiple dirs)
        {
            let mut seen = std::collections::HashSet::new();
            for &id in &all_stream_ids {
                if !seen.insert(id) {
                    issues.push(format!(
                        "L4: stream ID {} appears in multiple directory entries",
                        id
                    ));
                }
            }
        }

        // Pass claimed streams to L3 for verification
        issues.extend(self.layer3.verify(&all_stream_ids)?);

        Ok(issues)
    }

    /// Recursively walk directory streams to collect all stream IDs.
    /// Adds both directory stream IDs and data stream IDs.
    fn collect_stream_ids_recursive(
        &self,
        dir_stream_id: u64,
        stream_ids: &mut Vec<u64>,
        issues: &mut Vec<String>,
    ) -> Result<(), YakError> {
        // Include this directory stream itself
        stream_ids.push(dir_stream_id);

        // Open and read directory entries
        let handle = match self
            .layer3
            .open_stream_blocking(dir_stream_id, OpenMode::Read)
        {
            Ok(h) => h,
            Err(e) => {
                issues.push(format!(
                    "L4: failed to open directory stream {} for verification: {}",
                    dir_stream_id, e
                ));
                return Ok(());
            }
        };

        let entries = match self.read_entries_from_handle(&handle) {
            Ok(e) => e,
            Err(e) => {
                issues.push(format!(
                    "L4: failed to read entries from directory stream {}: {}",
                    dir_stream_id, e
                ));
                let _ = self.layer3.close_stream(handle);
                return Ok(());
            }
        };
        self.layer3.close_stream(handle)?;

        for entry in &entries {
            if entry.name.ends_with('/') {
                // Directory entry — recurse
                self.collect_stream_ids_recursive(entry.id, stream_ids, issues)?;
            } else {
                // Data stream entry
                stream_ids.push(entry.id);
            }
        }

        Ok(())
    }

    /// The number of bytes used for block indices on disk.
    pub fn block_index_width(&self) -> u8 {
        self.layer3.block_index_width()
    }

    /// Block size as a power of 2 (e.g. 12 → 4096 bytes).
    pub fn block_size_shift(&self) -> u8 {
        self.layer3.block_size_shift()
    }

    /// Compressed block size shift (0 = compression not configured).
    pub fn compressed_block_size_shift(&self) -> u8 {
        self.layer3.compressed_block_size_shift()
    }

    /// Returns true if the underlying storage has encryption enabled.
    pub fn is_encrypted(&self) -> bool {
        self.layer3.is_encrypted()
    }

    /// Open an existing encrypted Yak file.
    ///
    /// Returns `EncryptionRequired` if the file is encrypted but no password
    /// is provided. Returns `WrongPassword` if the password is incorrect.
    pub fn open_encrypted(path: &str, mode: OpenMode, password: &[u8]) -> Result<Self, YakError> {
        Self::open_inner(path, mode, Some(password))
    }

    // -------------------------------------------------------------------
    // Optimize (compaction + defragmentation)
    // -------------------------------------------------------------------

    /// Size of the buffer used when copying stream data during optimize.
    const OPTIMIZE_COPY_BUF_SIZE: usize = 256 * 1024;

    /// Optimize a Yak file by rewriting it without free blocks.
    ///
    /// Creates a new file containing only the active directory structure and
    /// stream data, then atomically replaces the original. All excess reserved
    /// capacity is stripped; stream data is written contiguously for maximum
    /// locality.
    ///
    /// For encrypted files, the password must be provided so the new file can
    /// be re-encrypted. Returns the number of bytes reclaimed.
    ///
    /// The file must not be open elsewhere (an exclusive lock is acquired).
    pub fn optimize(path: &str, password: Option<&[u8]>) -> Result<u64, YakError> {
        let source = Self::open_inner(path, OpenMode::Write, password)?;

        let opts = CreateOptions {
            block_index_width: source.block_index_width(),
            block_size_shift: source.block_size_shift(),
            compressed_block_size_shift: source.compressed_block_size_shift(),
            password,
        };

        let tmp_path = format!("{}.optimize.tmp", path);
        let dest = match Self::create(&tmp_path, opts) {
            Ok(d) => d,
            Err(e) => {
                let _ = source.close();
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
        };

        if let Err(e) = Self::copy_tree_recursive(&source, &dest, "") {
            let _ = source.close();
            let _ = dest.close();
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }

        let old_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        source.close()?;
        dest.close()?;

        let new_size = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);

        std::fs::rename(&tmp_path, path)
            .map_err(|e| YakError::IoError(format!("failed to rename optimized file: {}", e)))?;

        Ok(old_size.saturating_sub(new_size))
    }

    /// Recursively copy the directory tree from `source` to `dest`.
    fn copy_tree_recursive(source: &Self, dest: &Self, dir_path: &str) -> Result<(), YakError> {
        let entries = source.list(dir_path)?;
        for entry in entries {
            let full_path = if dir_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", dir_path, entry.name)
            };
            match entry.entry_type {
                EntryType::Directory => {
                    dest.mkdir(&full_path)?;
                    Self::copy_tree_recursive(source, dest, &full_path)?;
                }
                EntryType::Stream => {
                    Self::copy_stream(source, dest, &full_path)?;
                }
            }
        }
        Ok(())
    }

    /// Copy a single stream's data from `source` to `dest`.
    fn copy_stream(source: &Self, dest: &Self, stream_path: &str) -> Result<(), YakError> {
        let src_h = source.open_stream(stream_path, OpenMode::Read)?;
        let compressed = source.is_stream_compressed(&src_h)?;
        let length = source.stream_length(&src_h)?;

        let dst_h = dest.create_stream(stream_path, compressed)?;

        let mut buf = vec![0u8; Self::OPTIMIZE_COPY_BUF_SIZE];
        let mut remaining = length;
        while remaining > 0 {
            let to_read = std::cmp::min(remaining as usize, Self::OPTIMIZE_COPY_BUF_SIZE);
            let n = source.read(&src_h, &mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            dest.write(&dst_h, &buf[..n])?;
            remaining -= n as u64;
        }

        source.close_stream(src_h)?;
        dest.close_stream(dst_h)?;
        Ok(())
    }

    // -------------------------------------------------------------------
    // Directory operations
    // -------------------------------------------------------------------

    /// Create a directory. Parent directories must already exist.
    pub fn mkdir(&self, path: &str) -> Result<(), YakError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(YakError::InvalidPath(
                "cannot create root directory".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(parent_path)?;
        let dir_entry_name = format!("{}/", leaf);

        // Create new empty directory stream
        let new_dir_id = self.layer3.create_stream(false)?;

        // Open parent dir stream for writing (blocking — waits for contention)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;

        let result =
            self.add_entry_to_dir_handle(&parent_handle, &dir_entry_name, new_dir_id, path);
        if result.is_err() {
            self.layer3.close_stream(parent_handle)?;
            // Clean up the stream we just created
            let _ = self.layer3.delete_stream(new_dir_id);
            return result;
        }
        self.layer3.close_stream(parent_handle)?;
        Ok(())
    }

    /// Delete an empty directory. Fails if not empty or not found.
    pub fn rmdir(&self, path: &str) -> Result<(), YakError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(YakError::InvalidPath(
                "cannot remove root directory".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(parent_path)?;
        let dir_entry_name = format!("{}/", leaf);

        // Open parent for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;

        // Find the directory entry to get its stream ID
        let len = self.layer3.stream_length(&parent_handle)?;
        let biw = self.layer3.block_index_width() as usize;
        let dir_stream_id =
            match self.find_name_in_handle(&parent_handle, len, &dir_entry_name, biw)? {
                Some(id) => id,
                None => {
                    self.layer3.close_stream(parent_handle)?;
                    return Err(YakError::NotFound(path.to_string()));
                }
            };

        // Check that directory is empty (blocking)
        let child_handle = self
            .layer3
            .open_stream_blocking(dir_stream_id, OpenMode::Read)?;
        let child_len = self.layer3.stream_length(&child_handle)?;
        self.layer3.close_stream(child_handle)?;

        if child_len > 0 {
            self.layer3.close_stream(parent_handle)?;
            return Err(YakError::NotEmpty(path.to_string()));
        }

        // Remove entry from parent using in-place compaction
        self.remove_entry_from_dir_handle(&parent_handle, &dir_entry_name, path)?;
        self.layer3.close_stream(parent_handle)?;

        // Delete the directory stream
        self.layer3.delete_stream(dir_stream_id)?;
        Ok(())
    }

    /// List the contents of a directory. Use "" for root.
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, YakError> {
        let dir_id = if path.is_empty() {
            self.root_dir_stream_id
        } else {
            validate_path(path)?;
            self.resolve_dir_stream_id(path)?
        };

        let handle = self.layer3.open_stream_blocking(dir_id, OpenMode::Read)?;
        let entries = self.read_entries_from_handle(&handle)?;
        self.layer3.close_stream(handle)?;

        let mut result: Vec<DirEntry> = entries
            .iter()
            .map(|e| {
                if e.name.ends_with('/') {
                    DirEntry {
                        name: e.name[..e.name.len() - 1].to_string(),
                        entry_type: EntryType::Directory,
                    }
                } else {
                    DirEntry {
                        name: e.name.clone(),
                        entry_type: EntryType::Stream,
                    }
                }
            })
            .collect();

        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    /// Rename/move a directory. Fails if destination already exists.
    pub fn rename_dir(&self, old_path: &str, new_path: &str) -> Result<(), YakError> {
        self.require_write()?;
        if old_path.is_empty() || new_path.is_empty() {
            return Err(YakError::InvalidPath(
                "cannot rename root directory".to_string(),
            ));
        }
        validate_path(old_path)?;
        validate_path(new_path)?;

        let (old_parent_path, old_leaf) = split_parent_leaf(old_path);
        let (new_parent_path, new_leaf) = split_parent_leaf(new_path);
        let old_dir_name = format!("{}/", old_leaf);
        let new_dir_name = format!("{}/", new_leaf);

        let old_parent_id = self.resolve_dir_stream_id(old_parent_path)?;
        let new_parent_id = self.resolve_dir_stream_id(new_parent_path)?;

        if old_parent_id == new_parent_id {
            // Same parent: remove old entry, add new entry (both on same handle)
            let handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;

            let stream_id =
                match self.remove_entry_from_dir_handle(&handle, &old_dir_name, old_path) {
                    Ok(id) => id,
                    Err(e) => {
                        self.layer3.close_stream(handle)?;
                        return Err(e);
                    }
                };

            if let Err(e) =
                self.add_entry_to_dir_handle(&handle, &new_dir_name, stream_id, new_path)
            {
                // Re-add the old entry to restore consistency
                let _ = self.add_entry_to_dir_handle(&handle, &old_dir_name, stream_id, old_path);
                self.layer3.close_stream(handle)?;
                return Err(e);
            }
            self.layer3.close_stream(handle)?;
        } else {
            // Different parents: remove from old, add to new
            let old_handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let stream_id =
                match self.remove_entry_from_dir_handle(&old_handle, &old_dir_name, old_path) {
                    Ok(id) => id,
                    Err(e) => {
                        self.layer3.close_stream(old_handle)?;
                        return Err(e);
                    }
                };
            self.layer3.close_stream(old_handle)?;

            // Add to new parent
            let new_handle = self
                .layer3
                .open_stream_blocking(new_parent_id, OpenMode::Write)?;
            if let Err(e) =
                self.add_entry_to_dir_handle(&new_handle, &new_dir_name, stream_id, new_path)
            {
                self.layer3.close_stream(new_handle)?;
                // Re-add to old parent to restore consistency
                let old_handle = self
                    .layer3
                    .open_stream_blocking(old_parent_id, OpenMode::Write)?;
                let _ =
                    self.add_entry_to_dir_handle(&old_handle, &old_dir_name, stream_id, old_path);
                self.layer3.close_stream(old_handle)?;
                return Err(e);
            }
            self.layer3.close_stream(new_handle)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Stream lifecycle
    // -------------------------------------------------------------------

    /// Create a new stream and open it for writing.
    /// Returns a handle positioned at byte 0.
    /// If `compressed` is true, the stream uses leaf-level compression
    /// (requires the Yak file to have been created with compression support).
    pub fn create_stream(&self, path: &str, compressed: bool) -> Result<StreamHandle, YakError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(YakError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(parent_path)?;

        // Create new data stream in L3
        let new_stream_id = self.layer3.create_stream(compressed)?;

        // Open parent dir stream for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;

        // Add entry (checks for duplicates via name table binary search)
        let result = self.add_entry_to_dir_handle(&parent_handle, leaf, new_stream_id, path);
        if result.is_err() {
            self.layer3.close_stream(parent_handle)?;
            let _ = self.layer3.delete_stream(new_stream_id);
            return result.map(|_| StreamHandle(0)); // unreachable but keeps types happy
        }
        self.layer3.close_stream(parent_handle)?;

        // Open the new data stream for writing
        let l3_handle = self.layer3.open_stream(new_stream_id, OpenMode::Write)?;

        let mut state = self.state.lock().unwrap();
        let handle_id = state.next_handle_id;
        state.next_handle_id += 1;
        state.open_streams.insert(
            handle_id,
            OpenStreamInfo {
                _path: path.to_string(),
                stream_id: new_stream_id,
                l3_handle,
                position: 0,
                mode: OpenMode::Write,
            },
        );

        Ok(StreamHandle(handle_id))
    }

    /// Open an existing stream for reading or writing.
    /// Returns a handle positioned at byte 0.
    pub fn open_stream(&self, path: &str, mode: OpenMode) -> Result<StreamHandle, YakError> {
        if mode == OpenMode::Write {
            self.require_write()?;
        }
        if path.is_empty() {
            return Err(YakError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(parent_path)?;

        // Look up stream ID by name in parent directory (hash-accelerated)
        let stream_id = self
            .find_entry_in_dir(parent_id, leaf)?
            .ok_or_else(|| YakError::NotFound(path.to_string()))?;

        // Open via L3 (L3 handles locking)
        let l3_handle = self.layer3.open_stream(stream_id, mode)?;

        let mut state = self.state.lock().unwrap();
        let handle_id = state.next_handle_id;
        state.next_handle_id += 1;
        state.open_streams.insert(
            handle_id,
            OpenStreamInfo {
                _path: path.to_string(),
                stream_id,
                l3_handle,
                position: 0,
                mode,
            },
        );

        Ok(StreamHandle(handle_id))
    }

    /// Close a stream handle.
    pub fn close_stream(&self, handle: StreamHandle) -> Result<(), YakError> {
        let l3_handle = {
            let mut state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .remove(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };

        self.layer3.close_stream(l3_handle)?;
        Ok(())
    }

    /// Delete a stream. Must not be currently open.
    pub fn delete_stream(&self, path: &str) -> Result<(), YakError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(YakError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(parent_path)?;

        // Open parent for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;

        // Find the entry first so we can check the open-streams lock
        let biw = self.layer3.block_index_width() as usize;
        let len = self.layer3.stream_length(&parent_handle)?;
        let stream_id = match self.find_name_in_handle(&parent_handle, len, leaf, biw)? {
            Some(id) => id,
            None => {
                self.layer3.close_stream(parent_handle)?;
                return Err(YakError::NotFound(path.to_string()));
            }
        };

        // Check if stream is currently open at L4 level
        {
            let state = self.state.lock().unwrap();
            if state
                .open_streams
                .values()
                .any(|s| s.stream_id == stream_id)
            {
                drop(state);
                self.layer3.close_stream(parent_handle)?;
                return Err(YakError::LockConflict(
                    "cannot delete an open stream".to_string(),
                ));
            }
        }

        // Remove entry from parent using in-place compaction
        self.remove_entry_from_dir_handle(&parent_handle, leaf, path)?;
        self.layer3.close_stream(parent_handle)?;

        // Delete the stream in L3
        self.layer3.delete_stream(stream_id)?;
        Ok(())
    }

    /// Rename/move a stream. Must not be currently open.
    pub fn rename_stream(&self, old_path: &str, new_path: &str) -> Result<(), YakError> {
        self.require_write()?;
        if old_path.is_empty() || new_path.is_empty() {
            return Err(YakError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(old_path)?;
        validate_path(new_path)?;

        let (old_parent_path, old_leaf) = split_parent_leaf(old_path);
        let (new_parent_path, new_leaf) = split_parent_leaf(new_path);

        let old_parent_id = self.resolve_dir_stream_id(old_parent_path)?;
        let new_parent_id = self.resolve_dir_stream_id(new_parent_path)?;

        // Look up the stream ID and check it's not open before mutating anything
        let stream_id = self
            .find_entry_in_dir(old_parent_id, old_leaf)?
            .ok_or_else(|| YakError::NotFound(old_path.to_string()))?;
        {
            let state = self.state.lock().unwrap();
            if state
                .open_streams
                .values()
                .any(|s| s.stream_id == stream_id)
            {
                return Err(YakError::LockConflict(
                    "cannot rename an open stream".to_string(),
                ));
            }
        }

        if old_parent_id == new_parent_id {
            // Same parent: remove old entry, add new entry
            let handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;

            let removed_id = match self.remove_entry_from_dir_handle(&handle, old_leaf, old_path) {
                Ok(id) => id,
                Err(e) => {
                    self.layer3.close_stream(handle)?;
                    return Err(e);
                }
            };

            if let Err(e) = self.add_entry_to_dir_handle(&handle, new_leaf, removed_id, new_path) {
                // Restore old entry
                let _ = self.add_entry_to_dir_handle(&handle, old_leaf, removed_id, old_path);
                self.layer3.close_stream(handle)?;
                return Err(e);
            }
            self.layer3.close_stream(handle)?;
        } else {
            // Different parents: remove from old, add to new
            let old_handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let removed_id =
                match self.remove_entry_from_dir_handle(&old_handle, old_leaf, old_path) {
                    Ok(id) => id,
                    Err(e) => {
                        self.layer3.close_stream(old_handle)?;
                        return Err(e);
                    }
                };
            self.layer3.close_stream(old_handle)?;

            let new_handle = self
                .layer3
                .open_stream_blocking(new_parent_id, OpenMode::Write)?;
            if let Err(e) =
                self.add_entry_to_dir_handle(&new_handle, new_leaf, removed_id, new_path)
            {
                self.layer3.close_stream(new_handle)?;
                // Restore old entry
                let old_handle = self
                    .layer3
                    .open_stream_blocking(old_parent_id, OpenMode::Write)?;
                let _ = self.add_entry_to_dir_handle(&old_handle, old_leaf, removed_id, old_path);
                self.layer3.close_stream(old_handle)?;
                return Err(e);
            }
            self.layer3.close_stream(new_handle)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Stream I/O
    // -------------------------------------------------------------------

    /// Read up to buf.len() bytes from the current head position.
    /// Advances the head position by the number of bytes read.
    pub fn read(&self, handle: &StreamHandle, buf: &mut [u8]) -> Result<usize, YakError> {
        let (l3_handle, pos) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.position)
        };

        let n = self.layer3.read(&l3_handle, pos, buf)?;

        {
            let mut state = self.state.lock().unwrap();
            state.open_streams.get_mut(&handle.0).unwrap().position += n as u64;
        }
        Ok(n)
    }

    /// Write buf to the stream at the current head position.
    /// Extends the stream if writing past the current end.
    pub fn write(&self, handle: &StreamHandle, buf: &[u8]) -> Result<usize, YakError> {
        let (l3_handle, pos, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.position, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(YakError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        let n = self.layer3.write(&l3_handle, pos, buf)?;

        {
            let mut state = self.state.lock().unwrap();
            state.open_streams.get_mut(&handle.0).unwrap().position += n as u64;
        }
        Ok(n)
    }

    /// Set the head position. Fails if pos > stream length.
    pub fn seek(&self, handle: &StreamHandle, pos: u64) -> Result<(), YakError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };

        let len = self.layer3.stream_length(&l3_handle)?;
        if pos > len {
            return Err(YakError::SeekOutOfBounds(format!(
                "position {} exceeds stream length {}",
                pos, len
            )));
        }

        {
            let mut state = self.state.lock().unwrap();
            state.open_streams.get_mut(&handle.0).unwrap().position = pos;
        }
        Ok(())
    }

    /// Get the current head position.
    pub fn tell(&self, handle: &StreamHandle) -> Result<u64, YakError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_streams
            .get(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.position)
    }

    /// Get the total length of the stream in bytes.
    pub fn stream_length(&self, handle: &StreamHandle) -> Result<u64, YakError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };
        self.layer3.stream_length(&l3_handle)
    }

    /// Truncate the stream to the given length.
    /// If head position > new_len, the position is moved to new_len.
    pub fn truncate(&self, handle: &StreamHandle, new_len: u64) -> Result<(), YakError> {
        let (l3_handle, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(YakError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        self.layer3.truncate(&l3_handle, new_len)?;

        {
            let mut state = self.state.lock().unwrap();
            let info = state.open_streams.get_mut(&handle.0).unwrap();
            if info.position > new_len {
                info.position = new_len;
            }
        }
        Ok(())
    }

    /// Pre-allocate storage so that at least `n_bytes` of data capacity is available.
    /// Does not change the stream's logical size or head position.
    /// Errors if `n_bytes` is less than the current stream size.
    pub fn reserve(&self, handle: &StreamHandle, n_bytes: u64) -> Result<(), YakError> {
        let (l3_handle, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(YakError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        self.layer3.reserve(&l3_handle, n_bytes)
    }

    /// Return the current reserved capacity (allocated block capacity in bytes).
    /// Always >= stream_length and always a multiple of the block size (or 0).
    pub fn stream_reserved(&self, handle: &StreamHandle) -> Result<u64, YakError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };
        self.layer3.stream_reserved(&l3_handle)
    }

    /// Check whether a stream is compressed.
    pub fn is_stream_compressed(&self, handle: &StreamHandle) -> Result<bool, YakError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };
        self.layer3.is_stream_compressed(&l3_handle)
    }

    // -------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------

    /// Walk directory streams to resolve a directory path to its stream ID.
    /// Returns root_dir_stream_id for empty path.
    fn resolve_dir_stream_id(&self, path: &str) -> Result<u64, YakError> {
        if path.is_empty() {
            return Ok(self.root_dir_stream_id);
        }

        let mut current_id = self.root_dir_stream_id;
        for component in path.split('/') {
            let dir_name = format!("{}/", component);
            current_id = self
                .find_entry_in_dir(current_id, &dir_name)?
                .ok_or_else(|| YakError::NotFound("parent directory does not exist".to_string()))?;
        }

        Ok(current_id)
    }

    /// Read and parse all directory stream entries from an already-open handle.
    /// Reads the full stream; used by list() and verify().
    fn read_entries_from_handle(&self, handle: &L3::Handle) -> Result<Vec<StreamEntry>, YakError> {
        let len = self.layer3.stream_length(handle)?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; len as usize];
        self.layer3.read(handle, 0, &mut buf)?;
        parse_entries(&buf, self.layer3.block_index_width())
    }

    /// Look up a single entry by name in a directory stream.
    /// O(log n) via binary search on the name table.
    /// Reads only footer + name table + candidate entries (not full stream).
    /// Opens and closes the directory stream handle internally.
    fn find_entry_in_dir(&self, dir_stream_id: u64, name: &str) -> Result<Option<u64>, YakError> {
        let handle = self
            .layer3
            .open_stream_blocking(dir_stream_id, OpenMode::Read)?;
        let len = self.layer3.stream_length(&handle)?;
        if len == 0 {
            self.layer3.close_stream(handle)?;
            return Ok(None);
        }

        let biw = self.layer3.block_index_width() as usize;
        let result = self.find_name_in_handle(&handle, len, name, biw);
        self.layer3.close_stream(handle)?;
        result
    }

    /// Search for a name in an open directory stream using the name table.
    /// Returns Some(id) if found, None otherwise.
    fn find_name_in_handle(
        &self,
        handle: &L3::Handle,
        stream_len: u64,
        name: &str,
        biw: usize,
    ) -> Result<Option<u64>, YakError> {
        if stream_len == 0 {
            return Ok(None);
        }
        let (name_table, entry_count, _) = self.read_name_table(handle, stream_len)?;
        let found = self.find_entry_in_name_table(handle, &name_table, entry_count, name, biw)?;
        Ok(found.map(|e| e.id))
    }

    /// Read footer + name table from an open directory stream.
    /// Returns (name_table_bytes, entry_count, name_table_offset).
    fn read_name_table(
        &self,
        handle: &L3::Handle,
        stream_len: u64,
    ) -> Result<(Vec<u8>, u32, u32), YakError> {
        let mut footer_buf = [0u8; FOOTER_SIZE];
        self.layer3
            .read(handle, stream_len - FOOTER_SIZE as u64, &mut footer_buf)?;
        let name_table_offset = u32::from_le_bytes(footer_buf[0..4].try_into().unwrap());
        let entry_count = entry_count_from_footer(stream_len as usize, name_table_offset);

        let table_size = entry_count as usize * NAME_TABLE_SLOT_SIZE;
        let mut name_table = vec![0u8; table_size];
        self.layer3
            .read(handle, name_table_offset as u64, &mut name_table)?;
        Ok((name_table, entry_count, name_table_offset))
    }

    /// Search for a name in an already-loaded name table.
    /// Returns full entry info on match, or None.
    fn find_entry_in_name_table(
        &self,
        handle: &L3::Handle,
        name_table: &[u8],
        entry_count: u32,
        name: &str,
        biw: usize,
    ) -> Result<Option<FoundEntry>, YakError> {
        let name_bytes = name.as_bytes();
        let target_hash = fnv1a_hash(name_bytes);
        let range = find_hash_range(name_table, entry_count, target_hash);
        let header_size = biw + 2;

        for slot_idx in range {
            let offset = slot_offset(name_table, slot_idx);
            let mut header_buf = vec![0u8; header_size];
            self.layer3.read(handle, offset as u64, &mut header_buf)?;

            let name_len = u16::from_le_bytes([header_buf[biw], header_buf[biw + 1]]) as usize;
            if name_len == name_bytes.len() {
                let mut name_buf = vec![0u8; name_len];
                self.layer3.read(
                    handle,
                    (offset as usize + header_size) as u64,
                    &mut name_buf,
                )?;
                if name_buf == name_bytes {
                    let mut id_bytes = [0u8; 8];
                    id_bytes[..biw].copy_from_slice(&header_buf[..biw]);
                    return Ok(Some(FoundEntry {
                        slot_idx,
                        offset,
                        id: u64::from_le_bytes(id_bytes),
                        entry_size: header_size + name_len,
                    }));
                }
            }
        }
        Ok(None)
    }

    /// Add an entry to a directory stream that is already open for writing.
    /// Checks for duplicates (both `entry_name` and the dir/stream alternative).
    /// On success the entry has been written; the caller must still close the handle.
    fn add_entry_to_dir_handle(
        &self,
        handle: &L3::Handle,
        entry_name: &str,
        entry_id: u64,
        error_path: &str,
    ) -> Result<(), YakError> {
        let biw = self.layer3.block_index_width();
        let biw_usize = biw as usize;
        let len = self.layer3.stream_length(handle)?;

        // Determine the alternative name for duplicate checking
        let alt_name = if let Some(stripped) = entry_name.strip_suffix('/') {
            stripped.to_string()
        } else {
            format!("{}/", entry_name)
        };

        if len == 0 {
            // Empty directory — write single entry + 1-slot table + footer
            let entry = StreamEntry {
                id: entry_id,
                name: entry_name.to_string(),
            };
            let buf = build_directory_stream(&[entry], biw);
            self.layer3.write(handle, 0, &buf)?;
            return Ok(());
        }

        let (name_table, entry_count, name_table_offset) = self.read_name_table(handle, len)?;

        // Check for duplicates against both the entry name and its alternative
        for check_name in [entry_name, alt_name.as_str()] {
            if self
                .find_entry_in_name_table(handle, &name_table, entry_count, check_name, biw_usize)?
                .is_some()
            {
                return Err(YakError::AlreadyExists(error_path.to_string()));
            }
        }

        // Build the new entry bytes
        let entry = StreamEntry {
            id: entry_id,
            name: entry_name.to_string(),
        };
        let entry_bytes = serialize_entry_data(&entry, biw);

        // Parse name table into slots and insert the new slot in sorted order
        let mut slots = parse_name_table_slots(&name_table, entry_count);
        let new_hash = fnv1a_hash(entry_name.as_bytes());
        // New entry goes at current name_table_offset (appended to entry data region)
        let insert_pos = slots.partition_point(|&(h, _)| h < new_hash);
        slots.insert(insert_pos, (new_hash, name_table_offset));

        let new_nto = name_table_offset + entry_bytes.len() as u32;
        let table_and_footer = serialize_table_and_footer(&slots, new_nto);

        // Single contiguous write: entry_bytes + table + footer at old name_table_offset
        let mut write_buf = Vec::with_capacity(entry_bytes.len() + table_and_footer.len());
        write_buf.extend_from_slice(&entry_bytes);
        write_buf.extend_from_slice(&table_and_footer);
        self.layer3
            .write(handle, name_table_offset as u64, &write_buf)?;

        Ok(())
    }

    /// Remove an entry from a directory stream using in-place compaction.
    /// Returns the stream ID of the removed entry.
    /// The caller must still close the handle.
    fn remove_entry_from_dir_handle(
        &self,
        handle: &L3::Handle,
        entry_name: &str,
        error_path: &str,
    ) -> Result<u64, YakError> {
        let biw = self.layer3.block_index_width() as usize;
        let len = self.layer3.stream_length(handle)?;
        if len == 0 {
            return Err(YakError::NotFound(error_path.to_string()));
        }

        let (name_table, entry_count, name_table_offset) = self.read_name_table(handle, len)?;

        let found = self
            .find_entry_in_name_table(handle, &name_table, entry_count, entry_name, biw)?
            .ok_or_else(|| YakError::NotFound(error_path.to_string()))?;

        let slot_idx = found.slot_idx;
        let found_id = found.id;
        let found_offset = found.offset;
        let found_entry_size = found.entry_size;
        let nto = name_table_offset as usize;
        let gap_start = found_offset as usize;
        let gap_end = gap_start + found_entry_size;

        // Chunk-copy to close the gap in the entry data region
        if gap_end < nto {
            let mut copy_buf = vec![0u8; COMPACT_COPY_BUF_SIZE];
            let mut src = gap_end;
            let mut dst = gap_start;
            while src < nto {
                let chunk = (nto - src).min(COMPACT_COPY_BUF_SIZE);
                self.layer3
                    .read(handle, src as u64, &mut copy_buf[..chunk])?;
                self.layer3.write(handle, dst as u64, &copy_buf[..chunk])?;
                src += chunk;
                dst += chunk;
            }
        }

        // Update name table: remove the deleted slot and adjust offsets
        let mut slots = parse_name_table_slots(&name_table, entry_count);
        slots.remove(slot_idx);
        for slot in &mut slots {
            if slot.1 > found_offset {
                slot.1 -= found_entry_size as u32;
            }
        }

        let new_nto = (nto - found_entry_size) as u32;

        if slots.is_empty() {
            // Directory is now empty — truncate to zero
            self.layer3.truncate(handle, 0)?;
        } else {
            // Write updated table + footer at the new name_table_offset
            let table_and_footer = serialize_table_and_footer(&slots, new_nto);
            self.layer3
                .write(handle, new_nto as u64, &table_and_footer)?;

            // Truncate to remove the old tail
            let new_len =
                new_nto as u64 + (slots.len() * NAME_TABLE_SLOT_SIZE) as u64 + FOOTER_SIZE as u64;
            self.layer3.truncate(handle, new_len)?;
        }

        Ok(found_id)
    }
}

impl<L3: StreamLayer> Drop for Yak<L3> {
    fn drop(&mut self) {
        // Safety net: flush any stream handles that were not explicitly closed.
        // If close() was already called, the HashMap is empty and this is a no-op.
        if let Err(e) = self.flush_open_streams() {
            eprintln!("Yak Drop: error flushing open streams: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// L4 header section helpers
// ---------------------------------------------------------------------------

/// Serialize the L4 header (no length prefix).
/// Format: | "filing": [u8;6] | version: u8 | root_dir_stream_id: u64 |
fn serialize_header(root_dir_stream_id: u64) -> Vec<u8> {
    let mut buf = Vec::with_capacity(L4_PAYLOAD_SIZE as usize);
    buf.extend_from_slice(b"filing");
    buf.push(L4_FORMAT_VERSION);
    buf.extend_from_slice(&root_dir_stream_id.to_le_bytes());
    buf
}

/// Deserialize the L4 header (no length prefix), returning root_dir_stream_id.
/// Input: 15 bytes — identifier starts at byte 0.
fn deserialize_header(data: &[u8]) -> Result<u64, YakError> {
    if data.len() < L4_PAYLOAD_SIZE as usize {
        return Err(YakError::IoError(format!(
            "L4 payload too short: {} < {}",
            data.len(),
            L4_PAYLOAD_SIZE
        )));
    }
    // Verify identifier
    if &data[0..6] != b"filing" {
        return Err(YakError::IoError(format!(
            "expected L4 identifier 'filing', got '{}'",
            String::from_utf8_lossy(&data[0..6])
        )));
    }
    // Verify version
    let version = data[6];
    if version != L4_FORMAT_VERSION {
        return Err(YakError::IoError(format!(
            "unsupported L4 format version: {} (expected {})",
            version, L4_FORMAT_VERSION
        )));
    }
    // payload[6] = version, payload[7..15] = root_dir_stream_id
    let root_id = u64::from_le_bytes(
        data[7..15]
            .try_into()
            .map_err(|_| YakError::IoError("failed to parse root_dir_stream_id".to_string()))?,
    );
    Ok(root_id)
}
