use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

#[derive(Debug)]
pub enum SfsError {
    NotFound(String),
    AlreadyExists(String),
    NotEmpty(String),
    InvalidPath(String),
    LockConflict(String),
    SeekOutOfBounds(String),
    IoError(String),
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

/// Internal state for an open stream.
struct OpenStream {
    path: String,
    file: fs::File,
    position: u64,
    mode: OpenMode,
}

/// Per-stream lock state tracking.
#[derive(Default)]
struct LockState {
    readers: u32,
    has_writer: bool,
}

/// Represents an open SFS file (L4 mock backed by real filesystem).
pub struct Sfs {
    root: PathBuf,
    next_handle_id: u64,
    open_streams: HashMap<u64, OpenStream>,
    locks: HashMap<String, LockState>,
}

impl Sfs {
    /// Create a new SFS file. The path is used directly as the directory name.
    /// For example, `create("mydata.sfs")` creates a directory called `mydata.sfs/`.
    pub fn create(path: &str) -> Result<Sfs, SfsError> {
        let sfs_path = PathBuf::from(path);
        if sfs_path.exists() {
            return Err(SfsError::AlreadyExists(sfs_path.display().to_string()));
        }
        fs::create_dir(&sfs_path).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(Sfs {
            root: sfs_path,
            next_handle_id: 0,
            open_streams: HashMap::new(),
            locks: HashMap::new(),
        })
    }

    /// Open an existing SFS file.
    pub fn open(path: &str) -> Result<Sfs, SfsError> {
        let sfs_path = PathBuf::from(path);
        if !sfs_path.is_dir() {
            return Err(SfsError::NotFound(sfs_path.display().to_string()));
        }
        Ok(Sfs {
            root: sfs_path,
            next_handle_id: 0,
            open_streams: HashMap::new(),
            locks: HashMap::new(),
        })
    }

    /// Close the SFS file. Consumes self.
    pub fn close(self) {
        // Mock: dropping self closes all file handles via HashMap drop.
    }

    // -----------------------------------------------------------------------
    // Directory operations
    // -----------------------------------------------------------------------

    /// Create a directory. Parent directories must already exist.
    pub fn mkdir(&self, path: &str) -> Result<(), SfsError> {
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot create root directory".to_string(),
            ));
        }
        let full_path = self.resolve_path(path)?;
        if full_path.exists() {
            return Err(SfsError::AlreadyExists(path.to_string()));
        }
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                return Err(SfsError::NotFound(
                    "parent directory does not exist".to_string(),
                ));
            }
        }
        fs::create_dir(&full_path).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Delete an empty directory. Fails if not empty or not found.
    pub fn rmdir(&self, path: &str) -> Result<(), SfsError> {
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot remove root directory".to_string(),
            ));
        }
        let full_path = self.resolve_path(path)?;
        if !full_path.is_dir() {
            return Err(SfsError::NotFound(path.to_string()));
        }
        let has_entries = fs::read_dir(&full_path)
            .map_err(|e| SfsError::IoError(e.to_string()))?
            .next()
            .is_some();
        if has_entries {
            return Err(SfsError::NotEmpty(path.to_string()));
        }
        fs::remove_dir(&full_path).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    /// List the contents of a directory. Use "" for root.
    pub fn list(&self, path: &str) -> Result<Vec<DirEntry>, SfsError> {
        let full_path = if path.is_empty() {
            self.root.clone()
        } else {
            self.resolve_path(path)?
        };

        if !full_path.is_dir() {
            return Err(SfsError::NotFound(
                if path.is_empty() {
                    "root".to_string()
                } else {
                    path.to_string()
                },
            ));
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&full_path).map_err(|e| SfsError::IoError(e.to_string()))? {
            let entry = entry.map_err(|e| SfsError::IoError(e.to_string()))?;
            let file_type = entry
                .file_type()
                .map_err(|e| SfsError::IoError(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            let entry_type = if file_type.is_dir() {
                EntryType::Directory
            } else {
                EntryType::Stream
            };
            entries.push(DirEntry { name, entry_type });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Rename/move a directory. Fails if destination already exists.
    pub fn rename_dir(&self, old_path: &str, new_path: &str) -> Result<(), SfsError> {
        if old_path.is_empty() || new_path.is_empty() {
            return Err(SfsError::InvalidPath(
                "cannot rename root directory".to_string(),
            ));
        }
        let old_full = self.resolve_path(old_path)?;
        let new_full = self.resolve_path(new_path)?;
        if !old_full.is_dir() {
            return Err(SfsError::NotFound(old_path.to_string()));
        }
        if new_full.exists() {
            return Err(SfsError::AlreadyExists(new_path.to_string()));
        }
        if let Some(parent) = new_full.parent() {
            if !parent.exists() {
                return Err(SfsError::NotFound(
                    "destination parent directory does not exist".to_string(),
                ));
            }
        }
        fs::rename(&old_full, &new_full).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Stream lifecycle
    // -----------------------------------------------------------------------

    /// Create a new stream and open it for writing.
    /// Returns a handle positioned at byte 0.
    pub fn create_stream(&mut self, path: &str) -> Result<StreamHandle, SfsError> {
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        let full_path = self.resolve_path(path)?;
        if full_path.exists() {
            return Err(SfsError::AlreadyExists(path.to_string()));
        }
        if let Some(parent) = full_path.parent() {
            if !parent.exists() {
                return Err(SfsError::NotFound(
                    "parent directory does not exist".to_string(),
                ));
            }
        }

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&full_path)
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        let handle_id = self.next_handle_id;
        self.next_handle_id += 1;

        let lock = self.locks.entry(path.to_string()).or_default();
        lock.has_writer = true;

        self.open_streams.insert(
            handle_id,
            OpenStream {
                path: path.to_string(),
                file,
                position: 0,
                mode: OpenMode::Write,
            },
        );

        Ok(StreamHandle(handle_id))
    }

    /// Open an existing stream for reading or writing.
    /// Returns a handle positioned at byte 0.
    pub fn open_stream(
        &mut self,
        path: &str,
        mode: OpenMode,
    ) -> Result<StreamHandle, SfsError> {
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        let full_path = self.resolve_path(path)?;
        if !full_path.is_file() {
            return Err(SfsError::NotFound(path.to_string()));
        }

        // Check locking
        let lock = self.locks.entry(path.to_string()).or_default();
        match mode {
            OpenMode::Read => {
                if lock.has_writer {
                    return Err(SfsError::LockConflict(
                        "stream is opened for writing".to_string(),
                    ));
                }
                lock.readers += 1;
            }
            OpenMode::Write => {
                if lock.has_writer {
                    return Err(SfsError::LockConflict(
                        "stream is already opened for writing".to_string(),
                    ));
                }
                if lock.readers > 0 {
                    return Err(SfsError::LockConflict(
                        "stream has active readers".to_string(),
                    ));
                }
                lock.has_writer = true;
            }
        }

        let file = match mode {
            OpenMode::Read => fs::File::open(&full_path)
                .map_err(|e| SfsError::IoError(e.to_string()))?,
            OpenMode::Write => fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&full_path)
                .map_err(|e| SfsError::IoError(e.to_string()))?,
        };

        let handle_id = self.next_handle_id;
        self.next_handle_id += 1;

        self.open_streams.insert(
            handle_id,
            OpenStream {
                path: path.to_string(),
                file,
                position: 0,
                mode,
            },
        );

        Ok(StreamHandle(handle_id))
    }

    /// Close a stream handle.
    pub fn close_stream(&mut self, handle: StreamHandle) -> Result<(), SfsError> {
        let stream = self
            .open_streams
            .remove(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        if let Some(lock) = self.locks.get_mut(&stream.path) {
            match stream.mode {
                OpenMode::Read => {
                    lock.readers = lock.readers.saturating_sub(1);
                }
                OpenMode::Write => {
                    lock.has_writer = false;
                }
            }
            if lock.readers == 0 && !lock.has_writer {
                self.locks.remove(&stream.path);
            }
        }

        Ok(())
    }

    /// Delete a stream. Must not be currently open.
    pub fn delete_stream(&mut self, path: &str) -> Result<(), SfsError> {
        if path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        let full_path = self.resolve_path(path)?;
        if !full_path.is_file() {
            return Err(SfsError::NotFound(path.to_string()));
        }

        if let Some(lock) = self.locks.get(path) {
            if lock.has_writer || lock.readers > 0 {
                return Err(SfsError::LockConflict(
                    "cannot delete an open stream".to_string(),
                ));
            }
        }

        fs::remove_file(&full_path).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    /// Rename/move a stream. Must not be currently open.
    pub fn rename_stream(&mut self, old_path: &str, new_path: &str) -> Result<(), SfsError> {
        if old_path.is_empty() || new_path.is_empty() {
            return Err(SfsError::InvalidPath(
                "stream path cannot be empty".to_string(),
            ));
        }
        let old_full = self.resolve_path(old_path)?;
        let new_full = self.resolve_path(new_path)?;
        if !old_full.is_file() {
            return Err(SfsError::NotFound(old_path.to_string()));
        }
        if new_full.exists() {
            return Err(SfsError::AlreadyExists(new_path.to_string()));
        }

        if let Some(lock) = self.locks.get(old_path) {
            if lock.has_writer || lock.readers > 0 {
                return Err(SfsError::LockConflict(
                    "cannot rename an open stream".to_string(),
                ));
            }
        }

        if let Some(parent) = new_full.parent() {
            if !parent.exists() {
                return Err(SfsError::NotFound(
                    "destination parent directory does not exist".to_string(),
                ));
            }
        }

        fs::rename(&old_full, &new_full).map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Stream I/O
    // -----------------------------------------------------------------------

    /// Read up to buf.len() bytes from the current head position.
    /// Advances the head position by the number of bytes read.
    pub fn read(&mut self, handle: &StreamHandle, buf: &mut [u8]) -> Result<usize, SfsError> {
        let stream = self
            .open_streams
            .get_mut(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        stream
            .file
            .seek(SeekFrom::Start(stream.position))
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        let n = stream
            .file
            .read(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        stream.position += n as u64;
        Ok(n)
    }

    /// Write buf to the stream at the current head position.
    /// Extends the stream if writing past the current end.
    pub fn write(&mut self, handle: &StreamHandle, buf: &[u8]) -> Result<usize, SfsError> {
        let stream = self
            .open_streams
            .get_mut(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        if stream.mode != OpenMode::Write {
            return Err(SfsError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        stream
            .file
            .seek(SeekFrom::Start(stream.position))
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        let n = stream
            .file
            .write(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        stream.position += n as u64;
        Ok(n)
    }

    /// Set the head position. Fails if pos > stream length.
    pub fn seek(&mut self, handle: &StreamHandle, pos: u64) -> Result<(), SfsError> {
        let stream = self
            .open_streams
            .get_mut(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        let len = stream
            .file
            .metadata()
            .map_err(|e| SfsError::IoError(e.to_string()))?
            .len();

        if pos > len {
            return Err(SfsError::SeekOutOfBounds(format!(
                "position {} exceeds stream length {}",
                pos, len
            )));
        }

        stream.position = pos;
        Ok(())
    }

    /// Get the current head position.
    pub fn tell(&self, handle: &StreamHandle) -> Result<u64, SfsError> {
        let stream = self
            .open_streams
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        Ok(stream.position)
    }

    /// Get the total length of the stream in bytes.
    pub fn stream_length(&self, handle: &StreamHandle) -> Result<u64, SfsError> {
        let stream = self
            .open_streams
            .get(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;
        let len = stream
            .file
            .metadata()
            .map_err(|e| SfsError::IoError(e.to_string()))?
            .len();
        Ok(len)
    }

    /// Truncate the stream to the given length.
    /// If head position > new_len, the position is moved to new_len.
    pub fn truncate(&mut self, handle: &StreamHandle, new_len: u64) -> Result<(), SfsError> {
        let stream = self
            .open_streams
            .get_mut(&handle.0)
            .ok_or_else(|| SfsError::NotFound("invalid stream handle".to_string()))?;

        if stream.mode != OpenMode::Write {
            return Err(SfsError::LockConflict(
                "stream is not opened for writing".to_string(),
            ));
        }

        stream
            .file
            .set_len(new_len)
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        if stream.position > new_len {
            stream.position = new_len;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn resolve_path(&self, path: &str) -> Result<PathBuf, SfsError> {
        if path.is_empty() {
            return Ok(self.root.clone());
        }
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
        Ok(self.root.join(path))
    }
}
