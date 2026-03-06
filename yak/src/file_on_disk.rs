use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use fs2::FileExt;

use crate::file_layer::FileLayer;

use crate::{OpenMode, YakError};

/// Magic bytes at the start of every Yak file.
const MAGIC: &[u8; 9] = b"yakyakyak";

/// Header layout format version.
/// Bumped to 1 for the block-0 superblock format (no slot registry).
const HEADER_FORMAT_VERSION: u8 = 1;

/// Size of the magic prefix: "yakyakyak"(9) + version(1) + data_offset(2).
const MAGIC_SIZE: usize = 9 + 1 + 2; // = 12

/// L1 implementation that wraps a real file on disk.
///
/// Creates/opens a single file, acquires an exclusive process-level lock,
/// and provides random-access I/O. Writes a minimal magic prefix and
/// tracks `data_offset` (from the magic section's `data_offset` field).
/// All mutable metadata is managed by L2 in block 0.
pub struct FileOnDisk {
    state: Mutex<fs::File>,
    data_offset: u64,
}

impl FileOnDisk {
    /// Build the magic prefix: "yakyakyak"(9) + version(1) + data_offset(2).
    fn serialize_magic(data_offset: u16) -> [u8; MAGIC_SIZE] {
        let mut buf = [0u8; MAGIC_SIZE];
        buf[0..9].copy_from_slice(MAGIC);
        buf[9] = HEADER_FORMAT_VERSION;
        buf[10..12].copy_from_slice(&data_offset.to_le_bytes());
        buf
    }

    /// Read and validate the magic prefix.
    /// Returns `data_offset`.
    fn deserialize_magic(file: &mut fs::File) -> Result<u64, YakError> {
        let mut buf = [0u8; MAGIC_SIZE];
        file.read_exact(&mut buf)
            .map_err(|e| YakError::IoError(format!("failed to read Yak header magic: {}", e)))?;

        if &buf[0..9] != MAGIC {
            return Err(YakError::IoError("not a Yak file (bad magic)".to_string()));
        }
        if buf[9] != HEADER_FORMAT_VERSION {
            return Err(YakError::IoError(format!(
                "unsupported header format version: {}",
                buf[9]
            )));
        }

        let data_offset = u16::from_le_bytes([buf[10], buf[11]]);
        Ok(data_offset as u64)
    }
}

impl FileLayer for FileOnDisk {
    fn create(path: &str, data_offset: u64) -> Result<Self, YakError>
    where
        Self: Sized,
    {
        if std::path::Path::new(path).exists() {
            return Err(YakError::AlreadyExists(path.to_string()));
        }

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| YakError::IoError(format!("failed to create Yak file: {}", e)))?;

        // Acquire exclusive process lock
        file.try_lock_exclusive().map_err(|e| {
            YakError::IoError(format!("Yak file is locked by another process: {}", e))
        })?;

        // Write magic prefix and zero-fill bootstrap region
        let magic = Self::serialize_magic(data_offset as u16);
        let mut header = vec![0u8; data_offset as usize];
        header[..MAGIC_SIZE].copy_from_slice(&magic);

        file.write_all(&header)
            .map_err(|e| YakError::IoError(format!("failed to write Yak header: {}", e)))?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            data_offset,
        })
    }

    fn open(path: &str, mode: OpenMode) -> Result<Self, YakError>
    where
        Self: Sized,
    {
        let mut file = match mode {
            OpenMode::Read => fs::OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|e| YakError::IoError(format!("failed to open Yak file: {}", e)))?,
            OpenMode::Write => fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| YakError::IoError(format!("failed to open Yak file: {}", e)))?,
        };

        // Acquire process lock: shared for readers, exclusive for writers
        match mode {
            OpenMode::Read => file.try_lock_shared().map_err(|e| {
                YakError::IoError(format!("Yak file is locked by another process: {}", e))
            })?,
            OpenMode::Write => file.try_lock_exclusive().map_err(|e| {
                YakError::IoError(format!("Yak file is locked by another process: {}", e))
            })?,
        };

        // Read and validate magic prefix
        file.seek(SeekFrom::Start(0))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let data_offset = Self::deserialize_magic(&mut file)?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            data_offset,
        })
    }

    fn data_offset(&self) -> u64 {
        self.data_offset
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, YakError> {
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), YakError> {
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        file.write_all(data)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(())
    }

    fn len(&self) -> Result<u64, YakError> {
        let file = self.state.lock().unwrap();
        let len = file
            .metadata()
            .map_err(|e| YakError::IoError(e.to_string()))?
            .len();
        Ok(len)
    }

    fn set_len(&self, len: u64) -> Result<(), YakError> {
        let file = self.state.lock().unwrap();
        file.set_len(len)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(())
    }

    fn verify(&self) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();

        // Check file size >= data_offset
        let file_len = self.len()?;
        if file_len < self.data_offset {
            issues.push(format!(
                "L1: file size ({}) is less than data_offset ({})",
                file_len, self.data_offset
            ));
        }

        // Re-read and validate magic header
        let mut magic_buf = [0u8; MAGIC_SIZE];
        self.read(0, &mut magic_buf)?;
        if &magic_buf[0..9] != MAGIC {
            issues.push("L1: file magic 'yakyakyak' is corrupted".to_string());
        }
        if magic_buf[9] != HEADER_FORMAT_VERSION {
            issues.push(format!(
                "L1: header format version {} does not match expected {}",
                magic_buf[9], HEADER_FORMAT_VERSION
            ));
        }
        let stored_offset = u16::from_le_bytes([magic_buf[10], magic_buf[11]]);
        if stored_offset as u64 != self.data_offset {
            issues.push(format!(
                "L1: data_offset in magic ({}) does not match stored data_offset ({})",
                stored_offset, self.data_offset
            ));
        }

        Ok(issues)
    }
}
