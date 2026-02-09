use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::stream_layer::StreamLayer;
use crate::{OpenMode, SfsError};

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
}

impl StreamsFromFiles {
    fn stream_path(&self, id: u64) -> PathBuf {
        self.root.join(format!("{}.stream", id))
    }

    fn meta_path(&self) -> PathBuf {
        self.root.join("meta")
    }

    /// Meta format: | block_index_width: u8 | block_size_shift: u8 | next_stream_id: u64 |
    fn persist_meta(&self, next_id: u64) -> Result<(), SfsError> {
        let mut buf = Vec::with_capacity(10);
        buf.push(self.block_index_width);
        buf.push(self.block_size_shift);
        buf.extend_from_slice(&next_id.to_le_bytes());
        fs::write(self.meta_path(), buf)
            .map_err(|e| SfsError::IoError(format!("failed to write meta: {}", e)))
    }

    fn read_meta(root: &PathBuf) -> Result<(u8, u8, u64), SfsError> {
        let bytes = fs::read(root.join("meta"))
            .map_err(|e| SfsError::IoError(format!("failed to read meta: {}", e)))?;
        if bytes.len() < 10 {
            return Err(SfsError::IoError("meta file too short".to_string()));
        }
        let block_index_width = bytes[0];
        let block_size_shift = bytes[1];
        let next_stream_id = u64::from_le_bytes([
            bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8], bytes[9],
        ]);
        Ok((block_index_width, block_size_shift, next_stream_id))
    }
}

impl StreamLayer for StreamsFromFiles {
    type Handle = FileStreamHandle;

    fn create(path: &str, block_index_width: u8, block_size_shift: u8) -> Result<Self, SfsError> {
        let root = PathBuf::from(path);
        if root.exists() {
            return Err(SfsError::AlreadyExists(root.display().to_string()));
        }
        fs::create_dir(&root).map_err(|e| SfsError::IoError(e.to_string()))?;

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
        };
        instance.persist_meta(0)?;
        Ok(instance)
    }

    fn open(path: &str) -> Result<Self, SfsError> {
        let root = PathBuf::from(path);
        if !root.is_dir() {
            return Err(SfsError::NotFound(root.display().to_string()));
        }
        let (block_index_width, block_size_shift, next_stream_id) = Self::read_meta(&root)?;
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
        })
    }

    fn block_index_width(&self) -> u8 {
        self.block_index_width
    }

    fn block_size_shift(&self) -> u8 {
        self.block_size_shift
    }

    fn create_stream(&self) -> Result<u64, SfsError> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_stream_id;
        let stream_path = self.stream_path(id);

        fs::File::create(&stream_path).map_err(|e| {
            SfsError::IoError(format!("failed to create stream {}: {}", id, e))
        })?;

        state.next_stream_id += 1;
        let next_id = state.next_stream_id;
        drop(state);
        self.persist_meta(next_id)?;
        Ok(id)
    }

    fn stream_exists(&self, id: u64) -> bool {
        self.stream_path(id).exists()
    }

    fn open_stream(&self, id: u64, mode: OpenMode) -> Result<Self::Handle, SfsError> {
        let path = self.stream_path(id);
        if !path.exists() {
            return Err(SfsError::NotFound(format!("stream {}", id)));
        }

        let mut state = self.state.lock().unwrap();

        let lock = state.locks.entry(id).or_default();
        match mode {
            OpenMode::Read => {
                if lock.has_writer {
                    return Err(SfsError::LockConflict(format!(
                        "stream {} is opened for writing",
                        id
                    )));
                }
                lock.readers += 1;
            }
            OpenMode::Write => {
                if lock.has_writer {
                    return Err(SfsError::LockConflict(format!(
                        "stream {} is already opened for writing",
                        id
                    )));
                }
                if lock.readers > 0 {
                    return Err(SfsError::LockConflict(format!(
                        "stream {} has active readers",
                        id
                    )));
                }
                lock.has_writer = true;
            }
        }

        let handle_id = state.next_handle_id;
        state.next_handle_id += 1;
        state.open_handles.insert(
            handle_id,
            HandleInfo {
                stream_id: id,
                mode,
            },
        );

        Ok(FileStreamHandle(handle_id))
    }

    fn close_stream(&self, handle: Self::Handle) -> Result<(), SfsError> {
        let mut state = self.state.lock().unwrap();

        let info = state
            .open_handles
            .remove(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        if let Some(lock) = state.locks.get_mut(&info.stream_id) {
            match info.mode {
                OpenMode::Read => {
                    lock.readers = lock.readers.saturating_sub(1);
                }
                OpenMode::Write => {
                    lock.has_writer = false;
                }
            }
            if lock.readers == 0 && !lock.has_writer {
                state.locks.remove(&info.stream_id);
            }
        }

        Ok(())
    }

    fn delete_stream(&self, id: u64) -> Result<(), SfsError> {
        let path = self.stream_path(id);
        if !path.exists() {
            return Err(SfsError::NotFound(format!("stream {}", id)));
        }

        let state = self.state.lock().unwrap();
        if let Some(lock) = state.locks.get(&id) {
            if lock.has_writer || lock.readers > 0 {
                return Err(SfsError::LockConflict(format!(
                    "cannot delete open stream {}",
                    id
                )));
            }
        }
        drop(state);

        fs::remove_file(&path).map_err(|e| {
            SfsError::IoError(format!("failed to delete stream {}: {}", id, e))
        })?;
        Ok(())
    }

    fn read(&self, handle: &Self::Handle, pos: u64, buf: &mut [u8]) -> Result<usize, SfsError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.stream_id
        };

        let mut file = fs::File::open(self.stream_path(stream_id))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write(&self, handle: &Self::Handle, pos: u64, buf: &[u8]) -> Result<usize, SfsError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(SfsError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.stream_id
        };

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.stream_path(stream_id))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.seek(SeekFrom::Start(pos))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let n = file
            .write(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn stream_length(&self, handle: &Self::Handle) -> Result<u64, SfsError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            info.stream_id
        };

        let meta = fs::metadata(self.stream_path(stream_id))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(meta.len())
    }

    fn truncate(&self, handle: &Self::Handle, new_len: u64) -> Result<(), SfsError> {
        let stream_id = {
            let state = self.state.lock().unwrap();
            let info = state
                .open_handles
                .get(&handle.0)
                .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
            if info.mode != OpenMode::Write {
                return Err(SfsError::LockConflict(
                    "stream is not opened for writing".to_string(),
                ));
            }
            info.stream_id
        };

        let file = fs::OpenOptions::new()
            .write(true)
            .open(self.stream_path(stream_id))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.set_len(new_len)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }
}
