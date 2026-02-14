use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

use crate::stream_layer::StreamLayer;

/// L4 payload size: "filing"(6) + version(1) + root_dir_stream_id(8) = 15 bytes.
const L4_PAYLOAD_SIZE: u16 = 15;

#[derive(Debug)]
pub enum SfsError {
    NotFound(String),
    AlreadyExists(String),
    NotEmpty(String),
    InvalidPath(String),
    LockConflict(String),
    SeekOutOfBounds(String),
    IoError(String),
    ReadOnly(String),
}

impl fmt::Display for SfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SfsError::NotFound(msg) => write!(f, "not found: {}", msg),
            SfsError::AlreadyExists(msg) => write!(f, "already exists: {}", msg),
            SfsError::NotEmpty(msg) => write!(f, "not empty: {}", msg),
            SfsError::InvalidPath(msg) => write!(f, "invalid path: {}", msg),
            SfsError::LockConflict(msg) => write!(f, "lock conflict: {}", msg),
            SfsError::SeekOutOfBounds(msg) => write!(f, "seek out of bounds: {}", msg),
            SfsError::IoError(msg) => write!(f, "I/O error: {}", msg),
            SfsError::ReadOnly(msg) => write!(f, "read-only: {}", msg),
        }
    }
}

impl std::error::Error for SfsError {}

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

/// Serialize a single stream entry to bytes.
/// Format: | length: u16 | identifier: [u8; block_index_width] | name: [u8] |
fn serialize_entry(entry: &StreamEntry, block_index_width: u8) -> Vec<u8> {
    let biw = block_index_width as usize;
    let name_bytes = entry.name.as_bytes();
    let length: u16 = (2 + biw + name_bytes.len()) as u16;
    let mut buf = Vec::with_capacity(length as usize);
    buf.extend_from_slice(&length.to_le_bytes());
    let id_bytes = entry.id.to_le_bytes();
    buf.extend_from_slice(&id_bytes[..biw]);
    buf.extend_from_slice(name_bytes);
    buf
}

/// Serialize a list of stream entries to bytes.
fn serialize_entries(entries: &[StreamEntry], block_index_width: u8) -> Vec<u8> {
    let mut buf = Vec::new();
    for entry in entries {
        buf.extend_from_slice(&serialize_entry(entry, block_index_width));
    }
    buf
}

/// Parse stream entries from a byte buffer.
fn parse_entries(data: &[u8], block_index_width: u8) -> Result<Vec<StreamEntry>, SfsError> {
    let biw = block_index_width as usize;
    let min_entry_len = 2 + biw;
    let mut entries = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if pos + 2 > data.len() {
            return Err(SfsError::IoError(
                "truncated entry length in directory stream".to_string(),
            ));
        }
        let length = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        if length < min_entry_len {
            return Err(SfsError::IoError(
                "invalid entry length in directory stream".to_string(),
            ));
        }
        if pos + length > data.len() {
            return Err(SfsError::IoError(
                "truncated entry in directory stream".to_string(),
            ));
        }
        // Read identifier as u64, zero-extending from block_index_width bytes
        let mut id_bytes = [0u8; 8];
        id_bytes[..biw].copy_from_slice(&data[pos + 2..pos + 2 + biw]);
        let id = u64::from_le_bytes(id_bytes);
        let name = std::str::from_utf8(&data[pos + 2 + biw..pos + length])
            .map_err(|e| SfsError::IoError(format!("invalid UTF-8 in entry name: {}", e)))?
            .to_string();
        entries.push(StreamEntry { id, name });
        pos += length;
    }
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Path utilities
// ---------------------------------------------------------------------------

fn validate_path(path: &str) -> Result<(), SfsError> {
    if path.starts_with('/') || path.ends_with('/') {
        return Err(SfsError::InvalidPath(
            "path must not start or end with /".to_string(),
        ));
    }
    for component in path.split('/') {
        if component.is_empty() {
            return Err(SfsError::InvalidPath(
                "path contains empty component".to_string(),
            ));
        }
        if component == "." || component == ".." {
            return Err(SfsError::InvalidPath(
                "path cannot contain . or ..".to_string(),
            ));
        }
    }
    Ok(())
}

/// Split "a/b/c" into ("a/b", "c"). Split "c" into ("", "c").
fn split_parent_leaf(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (parent.to_string(), leaf.to_string()),
        None => (String::new(), path.to_string()),
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
struct SfsState<H: Copy> {
    next_handle_id: u64,
    open_streams: HashMap<u64, OpenStreamInfo<H>>,
}

/// Represents an open SFS file. Generic over the L3 stream layer.
///
/// Thread-safe: L3 is accessed via `&self`, and L4 bookkeeping is behind
/// a `Mutex`. All public methods take `&self`.
pub struct Sfs<L3: StreamLayer> {
    layer3: L3,
    root_dir_stream_id: u64,
    mode: OpenMode,
    state: Mutex<SfsState<L3::Handle>>,
}

impl<L3: StreamLayer> Sfs<L3> {
    // -------------------------------------------------------------------
    // SFS file lifecycle
    // -------------------------------------------------------------------

    /// Create a new SFS file. The path is used directly as the storage name.
    ///
    /// `block_index_width` is the number of bytes used for block indices
    /// on disk (e.g. 2, 4, or 8).
    /// `block_size_shift` is the power-of-2 exponent for block size
    /// (e.g. 12 → 4096 bytes).
    pub fn create(
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
    ) -> Result<Self, SfsError> {
        // Pass L4 payload size down through the layer chain so L1 can calculate data_offset
        let layer3 = L3::create(
            path,
            block_index_width,
            block_size_shift,
            VecDeque::from([L4_PAYLOAD_SIZE]),
        )?;
        let root_id = layer3.create_stream()?;

        // Write L4 header payload via slot
        let l4_slot = layer3.header_slot_for_upper(0);
        let l4_payload = serialize_header(root_id);
        layer3.write_header_slot(l4_slot, &l4_payload)?;

        Ok(Sfs {
            layer3,
            root_dir_stream_id: root_id,
            mode: OpenMode::Write,
            state: Mutex::new(SfsState {
                next_handle_id: 0,
                open_streams: HashMap::new(),
            }),
        })
    }

    /// Open an existing SFS file.
    ///
    /// `mode` controls file-level access:
    /// - `OpenMode::Read`: shared process lock, write operations are rejected
    /// - `OpenMode::Write`: exclusive process lock, all operations allowed
    pub fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError> {
        let layer3 = L3::open(path, mode)?;

        // Read L4 header payload via slot
        let l4_slot = layer3.header_slot_for_upper(0);
        let l4_data = layer3.read_header_slot(l4_slot)?;
        let root_id = deserialize_header(&l4_data)?;

        Ok(Sfs {
            layer3,
            root_dir_stream_id: root_id,
            mode,
            state: Mutex::new(SfsState {
                next_handle_id: 0,
                open_streams: HashMap::new(),
            }),
        })
    }

    /// Check that the SFS file is opened for writing; return `ReadOnly` error if not.
    fn require_write(&self) -> Result<(), SfsError> {
        if self.mode == OpenMode::Read {
            return Err(SfsError::ReadOnly(
                "SFS file is opened for reading".to_string(),
            ));
        }
        Ok(())
    }

    /// Drain all open stream handles and close them via L3.
    /// Attempts to close ALL handles even if individual closes fail.
    /// Returns the first error encountered, if any.
    fn flush_open_streams(&self) -> Result<(), SfsError> {
        let handles: Vec<(u64, OpenStreamInfo<L3::Handle>)> = {
            let mut state = self.state.lock().unwrap();
            state.open_streams.drain().collect()
        };

        let mut first_error: Option<SfsError> = None;
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

    /// Close the SFS file. Consumes self.
    ///
    /// Flushes all open stream handles before dropping. If any stream
    /// handle fails to close, all remaining handles are still closed
    /// and the first error is returned.
    pub fn close(self) -> Result<(), SfsError> {
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
    pub fn verify(&self) -> Result<Vec<String>, SfsError> {
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
    ) -> Result<(), SfsError> {
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

    // -------------------------------------------------------------------
    // Directory operations
    // -------------------------------------------------------------------

    /// Create a directory. Parent directories must already exist.
    pub fn mkdir(&self, path: &str) -> Result<(), SfsError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot create root directory".to_string(),
            ));
        }
        validate_path(path)?;

        let biw = self.layer3.block_index_width();
        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(&parent_path)?;
        let dir_entry_name = format!("{}/", leaf);

        // Open parent dir stream for writing (blocking — waits for contention)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;

        // Read existing entries and check for duplicates
        let entries = self.read_entries_from_handle(&parent_handle)?;
        if entries
            .iter()
            .any(|e| e.name == dir_entry_name || e.name == leaf)
        {
            self.layer3.close_stream(parent_handle)?;
            return Err(SfsError::AlreadyExists(path.to_string()));
        }

        // Create new empty directory stream
        let new_dir_id = self.layer3.create_stream()?;

        // Append entry to parent
        let entry = StreamEntry {
            id: new_dir_id,
            name: dir_entry_name,
        };
        let entry_buf = serialize_entry(&entry, biw);
        let len = self.layer3.stream_length(&parent_handle)?;
        self.layer3.write(&parent_handle, len, &entry_buf)?;

        self.layer3.close_stream(parent_handle)?;
        Ok(())
    }

    /// Delete an empty directory. Fails if not empty or not found.
    pub fn rmdir(&self, path: &str) -> Result<(), SfsError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot remove root directory".to_string(),
            ));
        }
        validate_path(path)?;

        let biw = self.layer3.block_index_width();
        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(&parent_path)?;
        let dir_entry_name = format!("{}/", leaf);

        // Open parent for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;
        let entries = self.read_entries_from_handle(&parent_handle)?;

        let entry = entries.iter().find(|e| e.name == dir_entry_name);
        if entry.is_none() {
            self.layer3.close_stream(parent_handle)?;
            return Err(SfsError::NotFound(path.to_string()));
        }
        let dir_stream_id = entry.unwrap().id;

        // Check that directory is empty (blocking)
        let child_handle = self
            .layer3
            .open_stream_blocking(dir_stream_id, OpenMode::Read)?;
        let child_len = self.layer3.stream_length(&child_handle)?;
        self.layer3.close_stream(child_handle)?;

        if child_len > 0 {
            self.layer3.close_stream(parent_handle)?;
            return Err(SfsError::NotEmpty(path.to_string()));
        }

        // Remove entry from parent
        let remaining: Vec<StreamEntry> = entries
            .into_iter()
            .filter(|e| e.name != dir_entry_name)
            .collect();
        let new_buf = serialize_entries(&remaining, biw);
        self.layer3.truncate(&parent_handle, 0)?;
        if !new_buf.is_empty() {
            self.layer3.write(&parent_handle, 0, &new_buf)?;
        }
        self.layer3.close_stream(parent_handle)?;

        // Delete the directory stream
        self.layer3.delete_stream(dir_stream_id)?;
        Ok(())
    }

    /// List the contents of a directory. Use "" for root.
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, SfsError> {
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
    pub fn rename_dir(&self, old_path: &str, new_path: &str) -> Result<(), SfsError> {
        self.require_write()?;
        if old_path.is_empty() || new_path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot rename root directory".to_string(),
            ));
        }
        validate_path(old_path)?;
        validate_path(new_path)?;

        let biw = self.layer3.block_index_width();
        let (old_parent_path, old_leaf) = split_parent_leaf(old_path);
        let (new_parent_path, new_leaf) = split_parent_leaf(new_path);
        let old_dir_name = format!("{}/", old_leaf);
        let new_dir_name = format!("{}/", new_leaf);

        let old_parent_id = self.resolve_dir_stream_id(&old_parent_path)?;
        let new_parent_id = self.resolve_dir_stream_id(&new_parent_path)?;

        if old_parent_id == new_parent_id {
            // Same parent: rename entry in place (blocking)
            let handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let entries = self.read_entries_from_handle(&handle)?;

            let old_entry = entries.iter().find(|e| e.name == old_dir_name);
            if old_entry.is_none() {
                self.layer3.close_stream(handle)?;
                return Err(SfsError::NotFound(old_path.to_string()));
            }
            let stream_id = old_entry.unwrap().id;

            if entries
                .iter()
                .any(|e| e.name == new_dir_name || e.name == new_leaf)
            {
                self.layer3.close_stream(handle)?;
                return Err(SfsError::AlreadyExists(new_path.to_string()));
            }

            // Per architecture: rename = delete old + re-add at end
            let mut remaining: Vec<StreamEntry> = entries
                .into_iter()
                .filter(|e| e.name != old_dir_name)
                .collect();
            remaining.push(StreamEntry {
                id: stream_id,
                name: new_dir_name,
            });

            let buf = serialize_entries(&remaining, biw);
            self.layer3.truncate(&handle, 0)?;
            if !buf.is_empty() {
                self.layer3.write(&handle, 0, &buf)?;
            }
            self.layer3.close_stream(handle)?;
        } else {
            // Different parents: remove from old, add to new (blocking)
            let old_handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let old_entries = self.read_entries_from_handle(&old_handle)?;

            let old_entry = old_entries.iter().find(|e| e.name == old_dir_name);
            if old_entry.is_none() {
                self.layer3.close_stream(old_handle)?;
                return Err(SfsError::NotFound(old_path.to_string()));
            }
            let stream_id = old_entry.unwrap().id;

            let remaining: Vec<StreamEntry> = old_entries
                .into_iter()
                .filter(|e| e.name != old_dir_name)
                .collect();
            let buf = serialize_entries(&remaining, biw);
            self.layer3.truncate(&old_handle, 0)?;
            if !buf.is_empty() {
                self.layer3.write(&old_handle, 0, &buf)?;
            }
            self.layer3.close_stream(old_handle)?;

            // Add to new parent (blocking)
            let new_handle = self
                .layer3
                .open_stream_blocking(new_parent_id, OpenMode::Write)?;
            let new_entries = self.read_entries_from_handle(&new_handle)?;

            if new_entries
                .iter()
                .any(|e| e.name == new_dir_name || e.name == new_leaf)
            {
                self.layer3.close_stream(new_handle)?;
                return Err(SfsError::AlreadyExists(new_path.to_string()));
            }

            let entry = StreamEntry {
                id: stream_id,
                name: new_dir_name,
            };
            let entry_buf = serialize_entry(&entry, biw);
            let len = self.layer3.stream_length(&new_handle)?;
            self.layer3.write(&new_handle, len, &entry_buf)?;
            self.layer3.close_stream(new_handle)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Stream lifecycle
    // -------------------------------------------------------------------

    /// Create a new stream and open it for writing.
    /// Returns a handle positioned at byte 0.
    pub fn create_stream(&self, path: &str) -> Result<StreamHandle, SfsError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let biw = self.layer3.block_index_width();
        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(&parent_path)?;
        let dir_entry_name = format!("{}/", leaf);

        // Open parent dir stream for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;
        let entries = self.read_entries_from_handle(&parent_handle)?;

        // Check for duplicates (stream or directory with same name)
        if entries
            .iter()
            .any(|e| e.name == leaf || e.name == dir_entry_name)
        {
            self.layer3.close_stream(parent_handle)?;
            return Err(SfsError::AlreadyExists(path.to_string()));
        }

        // Create new data stream in L3
        let new_stream_id = self.layer3.create_stream()?;

        // Add entry to parent directory stream
        let entry = StreamEntry {
            id: new_stream_id,
            name: leaf.to_string(),
        };
        let entry_buf = serialize_entry(&entry, biw);
        let len = self.layer3.stream_length(&parent_handle)?;
        self.layer3.write(&parent_handle, len, &entry_buf)?;
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
    pub fn open_stream(&self, path: &str, mode: OpenMode) -> Result<StreamHandle, SfsError> {
        if mode == OpenMode::Write {
            self.require_write()?;
        }
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(&parent_path)?;

        // Read parent dir to find the stream ID (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Read)?;
        let entries = self.read_entries_from_handle(&parent_handle)?;
        self.layer3.close_stream(parent_handle)?;

        let entry = entries
            .iter()
            .find(|e| e.name == leaf)
            .ok_or_else(|| SfsError::NotFound(path.to_string()))?;
        let stream_id = entry.id;

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
    pub fn close_stream(&self, handle: StreamHandle) -> Result<(), SfsError> {
        let l3_handle = {
            let mut state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .remove(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };

        self.layer3.close_stream(l3_handle)?;
        Ok(())
    }

    /// Delete a stream. Must not be currently open.
    pub fn delete_stream(&self, path: &str) -> Result<(), SfsError> {
        self.require_write()?;
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(path)?;

        let biw = self.layer3.block_index_width();
        let (parent_path, leaf) = split_parent_leaf(path);
        let parent_id = self.resolve_dir_stream_id(&parent_path)?;

        // Open parent for writing (blocking)
        let parent_handle = self
            .layer3
            .open_stream_blocking(parent_id, OpenMode::Write)?;
        let entries = self.read_entries_from_handle(&parent_handle)?;

        let entry = entries.iter().find(|e| e.name == leaf);
        if entry.is_none() {
            self.layer3.close_stream(parent_handle)?;
            return Err(SfsError::NotFound(path.to_string()));
        }
        let stream_id = entry.unwrap().id;

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
                return Err(SfsError::LockConflict(
                    "cannot delete an open stream".to_string(),
                ));
            }
        }

        // Remove entry from parent
        let remaining: Vec<StreamEntry> = entries.into_iter().filter(|e| e.name != leaf).collect();
        let buf = serialize_entries(&remaining, biw);
        self.layer3.truncate(&parent_handle, 0)?;
        if !buf.is_empty() {
            self.layer3.write(&parent_handle, 0, &buf)?;
        }
        self.layer3.close_stream(parent_handle)?;

        // Delete the stream in L3
        self.layer3.delete_stream(stream_id)?;
        Ok(())
    }

    /// Rename/move a stream. Must not be currently open.
    pub fn rename_stream(&self, old_path: &str, new_path: &str) -> Result<(), SfsError> {
        self.require_write()?;
        if old_path.is_empty() || new_path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        validate_path(old_path)?;
        validate_path(new_path)?;

        let biw = self.layer3.block_index_width();
        let (old_parent_path, old_leaf) = split_parent_leaf(old_path);
        let (new_parent_path, new_leaf) = split_parent_leaf(new_path);

        let old_parent_id = self.resolve_dir_stream_id(&old_parent_path)?;
        let new_parent_id = self.resolve_dir_stream_id(&new_parent_path)?;

        if old_parent_id == new_parent_id {
            // Same parent (blocking)
            let handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let entries = self.read_entries_from_handle(&handle)?;

            let old_entry = entries.iter().find(|e| e.name == old_leaf);
            if old_entry.is_none() {
                self.layer3.close_stream(handle)?;
                return Err(SfsError::NotFound(old_path.to_string()));
            }
            let stream_id = old_entry.unwrap().id;

            // Check if stream is currently open
            {
                let state = self.state.lock().unwrap();
                if state
                    .open_streams
                    .values()
                    .any(|s| s.stream_id == stream_id)
                {
                    drop(state);
                    self.layer3.close_stream(handle)?;
                    return Err(SfsError::LockConflict(
                        "cannot rename an open stream".to_string(),
                    ));
                }
            }

            let new_dir_name = format!("{}/", new_leaf);
            if entries
                .iter()
                .any(|e| e.name == new_leaf || e.name == new_dir_name)
            {
                self.layer3.close_stream(handle)?;
                return Err(SfsError::AlreadyExists(new_path.to_string()));
            }

            let mut remaining: Vec<StreamEntry> =
                entries.into_iter().filter(|e| e.name != old_leaf).collect();
            remaining.push(StreamEntry {
                id: stream_id,
                name: new_leaf.to_string(),
            });

            let buf = serialize_entries(&remaining, biw);
            self.layer3.truncate(&handle, 0)?;
            if !buf.is_empty() {
                self.layer3.write(&handle, 0, &buf)?;
            }
            self.layer3.close_stream(handle)?;
        } else {
            // Different parents (blocking)
            let old_handle = self
                .layer3
                .open_stream_blocking(old_parent_id, OpenMode::Write)?;
            let old_entries = self.read_entries_from_handle(&old_handle)?;

            let old_entry = old_entries.iter().find(|e| e.name == old_leaf);
            if old_entry.is_none() {
                self.layer3.close_stream(old_handle)?;
                return Err(SfsError::NotFound(old_path.to_string()));
            }
            let stream_id = old_entry.unwrap().id;

            {
                let state = self.state.lock().unwrap();
                if state
                    .open_streams
                    .values()
                    .any(|s| s.stream_id == stream_id)
                {
                    drop(state);
                    self.layer3.close_stream(old_handle)?;
                    return Err(SfsError::LockConflict(
                        "cannot rename an open stream".to_string(),
                    ));
                }
            }

            let remaining: Vec<StreamEntry> = old_entries
                .into_iter()
                .filter(|e| e.name != old_leaf)
                .collect();
            let buf = serialize_entries(&remaining, biw);
            self.layer3.truncate(&old_handle, 0)?;
            if !buf.is_empty() {
                self.layer3.write(&old_handle, 0, &buf)?;
            }
            self.layer3.close_stream(old_handle)?;

            let new_handle = self
                .layer3
                .open_stream_blocking(new_parent_id, OpenMode::Write)?;
            let new_entries = self.read_entries_from_handle(&new_handle)?;

            let new_dir_name = format!("{}/", new_leaf);
            if new_entries
                .iter()
                .any(|e| e.name == new_leaf || e.name == new_dir_name)
            {
                self.layer3.close_stream(new_handle)?;
                return Err(SfsError::AlreadyExists(new_path.to_string()));
            }

            let entry = StreamEntry {
                id: stream_id,
                name: new_leaf.to_string(),
            };
            let entry_buf = serialize_entry(&entry, biw);
            let len = self.layer3.stream_length(&new_handle)?;
            self.layer3.write(&new_handle, len, &entry_buf)?;
            self.layer3.close_stream(new_handle)?;
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // Stream I/O
    // -------------------------------------------------------------------

    /// Read up to buf.len() bytes from the current head position.
    /// Advances the head position by the number of bytes read.
    pub fn read(&self, handle: &StreamHandle, buf: &mut [u8]) -> Result<usize, SfsError> {
        let (l3_handle, pos) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
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
    pub fn write(&self, handle: &StreamHandle, buf: &[u8]) -> Result<usize, SfsError> {
        let (l3_handle, pos, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.position, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(SfsError::LockConflict(
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
    pub fn seek(&self, handle: &StreamHandle, pos: u64) -> Result<(), SfsError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };

        let len = self.layer3.stream_length(&l3_handle)?;
        if pos > len {
            return Err(SfsError::SeekOutOfBounds(format!(
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
    pub fn tell(&self, handle: &StreamHandle) -> Result<u64, SfsError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_streams
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.position)
    }

    /// Get the total length of the stream in bytes.
    pub fn stream_length(&self, handle: &StreamHandle) -> Result<u64, SfsError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };
        self.layer3.stream_length(&l3_handle)
    }

    /// Truncate the stream to the given length.
    /// If head position > new_len, the position is moved to new_len.
    pub fn truncate(&self, handle: &StreamHandle, new_len: u64) -> Result<(), SfsError> {
        let (l3_handle, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(SfsError::LockConflict(
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
    pub fn reserve(&self, handle: &StreamHandle, n_bytes: u64) -> Result<(), SfsError> {
        let (l3_handle, mode) = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            (info.l3_handle, info.mode)
        };

        if mode != OpenMode::Write {
            return Err(SfsError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        self.layer3.reserve(&l3_handle, n_bytes)
    }

    /// Return the current reserved capacity (allocated block capacity in bytes).
    /// Always >= stream_length and always a multiple of the block size (or 0).
    pub fn stream_reserved(&self, handle: &StreamHandle) -> Result<u64, SfsError> {
        let l3_handle = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_streams
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.l3_handle
        };
        self.layer3.stream_reserved(&l3_handle)
    }

    // -------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------

    /// Walk directory streams to resolve a directory path to its stream ID.
    /// Returns root_dir_stream_id for empty path.
    fn resolve_dir_stream_id(&self, path: &str) -> Result<u64, SfsError> {
        if path.is_empty() {
            return Ok(self.root_dir_stream_id);
        }

        let mut current_id = self.root_dir_stream_id;
        for component in path.split('/') {
            let dir_name = format!("{}/", component);
            let handle = self
                .layer3
                .open_stream_blocking(current_id, OpenMode::Read)?;
            let entries = self.read_entries_from_handle(&handle)?;
            self.layer3.close_stream(handle)?;

            let entry = entries
                .iter()
                .find(|e| e.name == dir_name)
                .ok_or_else(|| SfsError::NotFound("parent directory does not exist".to_string()))?;
            current_id = entry.id;
        }

        Ok(current_id)
    }

    /// Read and parse all directory stream entries from an already-open handle.
    fn read_entries_from_handle(&self, handle: &L3::Handle) -> Result<Vec<StreamEntry>, SfsError> {
        let len = self.layer3.stream_length(handle)?;
        if len == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; len as usize];
        self.layer3.read(handle, 0, &mut buf)?;
        parse_entries(&buf, self.layer3.block_index_width())
    }
}

impl<L3: StreamLayer> Drop for Sfs<L3> {
    fn drop(&mut self) {
        // Safety net: flush any stream handles that were not explicitly closed.
        // If close() was already called, the HashMap is empty and this is a no-op.
        if let Err(e) = self.flush_open_streams() {
            eprintln!("SFS Drop: error flushing open streams: {}", e);
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
    buf.push(0); // version
    buf.extend_from_slice(&root_dir_stream_id.to_le_bytes());
    buf
}

/// Deserialize the L4 header (no length prefix), returning root_dir_stream_id.
/// Input: 15 bytes — identifier starts at byte 0.
fn deserialize_header(data: &[u8]) -> Result<u64, SfsError> {
    if data.len() < L4_PAYLOAD_SIZE as usize {
        return Err(SfsError::IoError(format!(
            "L4 payload too short: {} < {}",
            data.len(),
            L4_PAYLOAD_SIZE
        )));
    }
    // Verify identifier
    if &data[0..6] != b"filing" {
        return Err(SfsError::IoError(format!(
            "expected L4 identifier 'filing', got '{}'",
            String::from_utf8_lossy(&data[0..6])
        )));
    }
    // payload[6] = version, payload[7..15] = root_dir_stream_id
    let root_id = u64::from_le_bytes(
        data[7..15]
            .try_into()
            .map_err(|_| SfsError::IoError("failed to parse root_dir_stream_id".to_string()))?,
    );
    Ok(root_id)
}
