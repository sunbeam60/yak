use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use crate::stream_layer::StreamLayer;
use crate::{HeaderSlotId, OpenMode, YakError};

/// L3 mock identifier in the header section.
const L3_MOCK_IDENTIFIER: &[u8; 6] = b"strfil";

/// L3 mock header version.
const L3_MOCK_VERSION: u8 = 0;

/// L3 mock payload size: "strfil"(6) + version(1) + next_stream_id(8) = 15 bytes.
const L3_MOCK_PAYLOAD_SIZE: u16 = 15;

/// Size of the magic prefix: "yakyakyak"(9) + version(1) + total_header_length(2).
const MAGIC_SIZE: usize = 12;

/// Information about one header slot in the slot registry.
struct SlotInfo {
    /// Absolute byte offset in the header file where this slot's length prefix begins.
    offset: u64,
    /// Expected payload length (NOT including the 2-byte length prefix).
    payload_len: u16,
}

/// Handle for an open stream in StreamsFromFiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileStreamHandle(u64);

/// Per-stream lock state.
#[derive(Default)]
struct LockState {
    readers: u32,
    has_writer: bool,
}

/// Metadata for an open stream handle (no stored fs::File).
struct HandleInfo {
    stream_id: u64,
    mode: OpenMode,
    /// Tracked reservation (in-memory only, no real pre-allocation for mock).
    reserved: u64,
}

/// Bookkeeping state protected by a Mutex.
struct StreamsState {
    next_stream_id: u64,
    next_handle_id: u64,
    locks: HashMap<u64, LockState>,
    open_handles: HashMap<u64, HandleInfo>,
}

/// L3 implementation that stores each stream as a numbered file on disk.
///
/// When created, makes a directory at the given path. Each stream is stored
/// as `{id}.stream` inside this directory. A `meta` file tracks the next
/// available stream ID along with `block_index_width` and `block_size_shift`.
/// A `header` file stores the Yak header with slot registry.
///
/// Thread-safe: all bookkeeping state is behind a `Mutex`. File I/O uses
/// ephemeral file handles (opened and closed on each operation) so no
/// file descriptors are stored in state.
///
/// This implementation is intended to be kept permanently as a debugging and
/// testing tool, even after `StreamsFromBlocks` is implemented.
pub struct StreamsFromFiles {
    root: PathBuf,
    block_index_width: u8,
    block_size_shift: u8,
    state: Mutex<StreamsState>,
    lock_released: Condvar,
    slots: Vec<SlotInfo>,
}

impl StreamsFromFiles {
    fn stream_path(&self, id: u64) -> PathBuf {
        self.root.join(format!("{}.stream", id))
    }

    fn meta_path(&self) -> PathBuf {
        self.root.join("meta")
    }

    fn header_path(&self) -> PathBuf {
        self.root.join("header")
    }

    /// Meta format: | block_index_width: u8 | block_size_shift: u8 | next_stream_id: u64 |
    fn persist_meta(&self, next_id: u64) -> Result<(), YakError> {
        let mut buf = Vec::with_capacity(10);
        buf.push(self.block_index_width);
        buf.push(self.block_size_shift);
        buf.extend_from_slice(&next_id.to_le_bytes());
        fs::write(self.meta_path(), buf)
            .map_err(|e| YakError::IoError(format!("failed to write meta: {}", e)))
    }

    fn acquire_lock(&self, id: u64, mode: OpenMode, blocking: bool) -> Result<(), YakError> {
        let mut state = self.state.lock().unwrap();
        loop {
            let lock = state.locks.entry(id).or_default();
            match mode {
                OpenMode::Read => {
                    if !lock.has_writer {
                        lock.readers += 1;
                        return Ok(());
                    }
                }
                OpenMode::Write => {
                    if !lock.has_writer && lock.readers == 0 {
                        lock.has_writer = true;
                        return Ok(());
                    }
                }
            }
            if !blocking {
                let lock = state.locks.get(&id).unwrap();
                if lock.readers == 0 && !lock.has_writer {
                    state.locks.remove(&id);
                }
                return Err(YakError::LockConflict(format!(
                    "lock conflict on stream {}",
                    id
                )));
            }
            state = self.lock_released.wait(state).unwrap();
        }
    }

    fn release_lock(&self, id: u64, mode: OpenMode) {
        let mut state = self.state.lock().unwrap();
        if let Some(lock) = state.locks.get_mut(&id) {
            match mode {
                OpenMode::Read => lock.readers = lock.readers.saturating_sub(1),
                OpenMode::Write => lock.has_writer = false,
            }
            if lock.readers == 0 && !lock.has_writer {
                state.locks.remove(&id);
            }
        }
        drop(state);
        self.lock_released.notify_all();
    }

    fn open_stream_inner(
        &self,
        id: u64,
        mode: OpenMode,
        blocking: bool,
    ) -> Result<FileStreamHandle, YakError> {
        let path = self.stream_path(id);
        if !path.exists() {
            return Err(YakError::NotFound(format!("stream {}", id)));
        }

        self.acquire_lock(id, mode, blocking)?;

        let mut state = self.state.lock().unwrap();
        let handle_id = state.next_handle_id;
        state.next_handle_id += 1;
        state.open_handles.insert(
            handle_id,
            HandleInfo {
                stream_id: id,
                mode,
                reserved: 0,
            },
        );

        Ok(FileStreamHandle(handle_id))
    }

    fn read_meta(root: &Path) -> Result<(u8, u8, u64), YakError> {
        let bytes = fs::read(root.join("meta"))
            .map_err(|e| YakError::IoError(format!("failed to read meta: {}", e)))?;
        if bytes.len() < 10 {
            return Err(YakError::IoError("meta file too short".to_string()));
        }
        let block_index_width = bytes[0];
        let block_size_shift = bytes[1];
        let next_stream_id = u64::from_le_bytes([
            bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
        ]);
        Ok((block_index_width, block_size_shift, next_stream_id))
    }

    /// Serialize the L3 mock header (no length prefix).
    /// Format: | "strfil": [u8;6] | version: u8 | next_stream_id: u64 |
    fn serialize_header(next_stream_id: u64) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L3_MOCK_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(L3_MOCK_IDENTIFIER);
        buf.push(L3_MOCK_VERSION);
        buf.extend_from_slice(&next_stream_id.to_le_bytes());
        buf
    }
}

impl StreamLayer for StreamsFromFiles {
    type Handle = FileStreamHandle;

    fn create(
        path: &str,
        block_index_width: u8,
        block_size_shift: u8,
        _compressed_block_size_shift: u8,
        mut slot_sizes: VecDeque<u16>,
        _password: Option<&[u8]>,
    ) -> Result<Self, YakError> {
        let root = PathBuf::from(path);
        if root.exists() {
            return Err(YakError::AlreadyExists(root.display().to_string()));
        }
        fs::create_dir(&root).map_err(|e| YakError::IoError(e.to_string()))?;

        // Push L3's own size to front (on-disk order: L3 first)
        slot_sizes.push_front(L3_MOCK_PAYLOAD_SIZE);

        // Compute header layout: magic + all sections
        let sections_total: usize = slot_sizes.iter().map(|&s| 2 + s as usize).sum();
        let total_header_length = (MAGIC_SIZE + sections_total) as u16;

        // Build header bytes
        let mut header = Vec::with_capacity(total_header_length as usize);
        // Magic: "yakyakyak" + version + total_header_length
        header.extend_from_slice(b"yakyakyak");
        header.push(0); // layout version 0
        header.extend_from_slice(&total_header_length.to_le_bytes());

        // Build slot registry and section placeholders (slot 0 = L3's own)
        let mut slots = Vec::with_capacity(slot_sizes.len());
        let mut current_offset = MAGIC_SIZE as u64;
        for &payload_len in &slot_sizes {
            slots.push(SlotInfo {
                offset: current_offset,
                payload_len,
            });
            let section_len = payload_len + 2;
            header.extend_from_slice(&section_len.to_le_bytes());
            header.extend_from_slice(&vec![0u8; payload_len as usize]);
            current_offset += section_len as u64;
        }

        // Write header file
        fs::write(root.join("header"), &header)
            .map_err(|e| YakError::IoError(format!("failed to write header: {}", e)))?;

        let instance = StreamsFromFiles {
            root,
            block_index_width,
            block_size_shift,
            state: Mutex::new(StreamsState {
                next_stream_id: 0,
                next_handle_id: 0,
                locks: HashMap::new(),
                open_handles: HashMap::new(),
            }),
            lock_released: Condvar::new(),
            slots,
        };

        // Write L3 payload into slot 0
        let l3_payload = Self::serialize_header(0);
        instance.write_header_slot(HeaderSlotId(0), &l3_payload)?;

        instance.persist_meta(0)?;

        Ok(instance)
    }

    fn open(path: &str, _mode: OpenMode, _password: Option<&[u8]>) -> Result<Self, YakError> {
        let root = PathBuf::from(path);
        if !root.is_dir() {
            return Err(YakError::NotFound(root.display().to_string()));
        }
        let (block_index_width, block_size_shift, next_stream_id) = Self::read_meta(&root)?;

        // Read header file and rebuild slot registry
        let header_data = fs::read(root.join("header"))
            .map_err(|e| YakError::IoError(format!("failed to read header: {}", e)))?;

        if header_data.len() < MAGIC_SIZE
            || &header_data[0..9] != b"yakyakyak"
            || header_data[9] != 0
        {
            return Err(YakError::IoError("invalid Yak header magic".to_string()));
        }

        let total_header_length = u16::from_le_bytes([header_data[10], header_data[11]]) as usize;

        // Walk sections to rebuild slot registry
        let mut slots = Vec::new();
        let mut pos = MAGIC_SIZE;
        while pos < total_header_length {
            if pos + 2 > header_data.len() {
                return Err(YakError::IoError(
                    "header truncated at section length".to_string(),
                ));
            }
            let section_len = u16::from_le_bytes([header_data[pos], header_data[pos + 1]]) as usize;
            if section_len < 2 {
                return Err(YakError::IoError(format!(
                    "invalid section length {} at offset {}",
                    section_len, pos
                )));
            }
            let payload_len = (section_len - 2) as u16;
            slots.push(SlotInfo {
                offset: pos as u64,
                payload_len,
            });
            pos += section_len;
        }

        Ok(StreamsFromFiles {
            root,
            block_index_width,
            block_size_shift,
            state: Mutex::new(StreamsState {
                next_stream_id,
                next_handle_id: 0,
                locks: HashMap::new(),
                open_handles: HashMap::new(),
            }),
            lock_released: Condvar::new(),
            slots,
        })
    }

    fn block_index_width(&self) -> u8 {
        self.block_index_width
    }

    fn block_size_shift(&self) -> u8 {
        self.block_size_shift
    }

    fn compressed_block_size_shift(&self) -> u8 {
        0 // File-backed mock does not support compression
    }

    fn create_stream(&self, _compressed: bool) -> Result<u64, YakError> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_stream_id;
        let stream_path = self.stream_path(id);

        fs::File::create(&stream_path)
            .map_err(|e| YakError::IoError(format!("failed to create stream {}: {}", id, e)))?;

        state.next_stream_id += 1;
        let next_id = state.next_stream_id;
        drop(state);
        self.persist_meta(next_id)?;
        Ok(id)
    }

    fn stream_exists(&self, id: u64) -> bool {
        self.stream_path(id).exists()
    }

    fn stream_count(&self) -> Result<u64, YakError> {
        let state = self.state.lock().unwrap();
        let mut count = 0u64;
        for id in 0..state.next_stream_id {
            if self.stream_path(id).exists() {
                count += 1;
            }
        }
        Ok(count)
    }

    fn stream_ids(&self) -> Result<Vec<u64>, YakError> {
        let state = self.state.lock().unwrap();
        let mut ids = Vec::new();
        for id in 0..state.next_stream_id {
            if self.stream_path(id).exists() {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError> {
        self.open_stream_inner(id, mode, false)
    }

    fn open_stream_blocking(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, YakError> {
        self.open_stream_inner(id, mode, true)
    }

    fn close_stream(&self, handle: Self::Handle) -> Result<(), YakError> {
        let info = {
            let mut state = self.state.lock().unwrap();
            state
                .open_handles
                .remove(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?
        };

        self.release_lock(info.stream_id, info.mode);
        Ok(())
    }

    fn delete_stream(&self, id: u64) -> Result<(), YakError> {
        let path = self.stream_path(id);
        if !path.exists() {
            return Err(YakError::NotFound(format!("stream {}", id)));
        }

        let state = self.state.lock().unwrap();
        if let Some(lock) = state.locks.get(&id) {
            if lock.has_writer || lock.readers > 0 {
                return Err(YakError::LockConflict(format!(
                    "cannot delete open stream {}",
                    id
                )));
            }
        }
        drop(state);

        fs::remove_file(&path)
            .map_err(|e| YakError::IoError(format!("failed to delete stream {}: {}", id, e)))?;
        Ok(())
    }

    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, YakError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.stream_id
        };

        let mut file = fs::File::open(self.stream_path(stream_id))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, YakError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(YakError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.stream_id
        };

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.stream_path(stream_id))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let n = file
            .write(buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, YakError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            info.stream_id
        };

        let meta = fs::metadata(self.stream_path(stream_id))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(meta.len())
    }

    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), YakError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(YakError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.stream_id
        };

        let file = fs::OpenOptions::new()
            .write(true)
            .open(self.stream_path(stream_id))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        file.set_len(new_len)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(())
    }

    fn reserve(&self, handle: &Self::Handle, n_bytes: u64) -> Result<(), YakError> {
        let mut state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get_mut(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        if info.mode != OpenMode::Write {
            return Err(YakError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }
        // Check invariant: n_bytes >= current file size
        let file_len = std::fs::metadata(self.stream_path(info.stream_id))
            .map(|m| m.len())
            .unwrap_or(0);
        if n_bytes < file_len {
            return Err(YakError::IoError(format!(
                "cannot reserve {} bytes: stream size is already {}",
                n_bytes, file_len
            )));
        }
        // Track reservation (no real allocation in mock)
        if n_bytes > info.reserved {
            info.reserved = n_bytes;
        }
        Ok(())
    }

    fn stream_reserved(&self, handle: &Self::Handle) -> Result<u64, YakError> {
        let state = self.state.lock().unwrap();
        let info = state
            .open_handles
            .get(&handle.0)
            .ok_or_else(|| YakError::NotFound("invalid stream handle".to_string()))?;
        Ok(info.reserved)
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Slot 0 = L3's own section, so upper layer index 0 → slot 1 (L4), etc.
        HeaderSlotId(index + 1)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError> {
        let idx = slot.0 as usize;
        if idx >= self.slots.len() {
            return Err(YakError::IoError(format!(
                "header slot index {} out of range (have {} slots)",
                idx,
                self.slots.len()
            )));
        }
        let info = &self.slots[idx];
        if data.len() != info.payload_len as usize {
            return Err(YakError::IoError(format!(
                "header slot {}: expected {} bytes, got {}",
                idx,
                info.payload_len,
                data.len()
            )));
        }

        let section_len = info.payload_len + 2;
        let mut buf = Vec::with_capacity(section_len as usize);
        buf.extend_from_slice(&section_len.to_le_bytes());
        buf.extend_from_slice(data);

        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(self.header_path())
            .map_err(|e| YakError::IoError(format!("failed to open header for write: {}", e)))?;
        file.seek(SeekFrom::Start(info.offset))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        file.write_all(&buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(())
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError> {
        let idx = slot.0 as usize;
        if idx >= self.slots.len() {
            return Err(YakError::IoError(format!(
                "header slot index {} out of range (have {} slots)",
                idx,
                self.slots.len()
            )));
        }
        let info = &self.slots[idx];

        let mut file = fs::File::open(self.header_path())
            .map_err(|e| YakError::IoError(format!("failed to open header for read: {}", e)))?;
        file.seek(SeekFrom::Start(info.offset + 2))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let mut buf = vec![0u8; info.payload_len as usize];
        file.read_exact(&mut buf)
            .map_err(|e| YakError::IoError(format!("failed to read header slot: {}", e)))?;
        Ok(buf)
    }

    fn verify(&self, claimed_streams: &[u64]) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();

        // Collect existing .stream files on disk
        let state = self.state.lock().unwrap();
        let next_id = state.next_stream_id;
        drop(state);

        let mut disk_streams = std::collections::HashSet::new();
        for id in 0..next_id {
            if self.stream_path(id).exists() {
                disk_streams.insert(id);
            }
        }

        let claimed_set: std::collections::HashSet<u64> = claimed_streams.iter().cloned().collect();

        // Streams on disk but not claimed by L4
        for &id in &disk_streams {
            if !claimed_set.contains(&id) {
                issues.push(format!(
                    "L3-mock: stream {} exists on disk but is not claimed by L4",
                    id
                ));
            }
        }

        // Streams claimed by L4 but not on disk
        for &id in &claimed_set {
            if !disk_streams.contains(&id) {
                issues.push(format!(
                    "L3-mock: stream {} is claimed by L4 but does not exist on disk",
                    id
                ));
            }
        }

        // No blocks to pass down (L3 mock has no L2)
        Ok(issues)
    }
}
