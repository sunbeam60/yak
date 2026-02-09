use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::block_layer::BlockLayer;
use crate::SfsError;

/// Bookkeeping state protected by a Mutex.
struct BlocksState {
    next_block_id: u64,
}

/// L2 mock implementation that stores each block as a numbered file on disk.
///
/// When created, makes a directory at the given path. Each block is stored
/// as `{id}.block` inside this directory. A `meta` file tracks the next
/// available block ID. A `header` file stores the full SFS header.
///
/// Block files are exactly `block_size` bytes, zero-padded.
///
/// Thread-safe: all bookkeeping state is behind a `Mutex`. File I/O uses
/// ephemeral file handles (opened and closed on each operation).
pub struct BlocksFromFiles {
    root: PathBuf,
    block_size_shift: u8,
    block_index_width: u8,
    state: Mutex<BlocksState>,
}

impl BlocksFromFiles {
    fn block_path(&self, index: u64) -> PathBuf {
        self.root.join(format!("{}.block", index))
    }

    fn meta_path(&self) -> PathBuf {
        self.root.join("meta")
    }

    fn header_path(&self) -> PathBuf {
        self.root.join("header")
    }

    /// Meta format: | next_block_id: u64 | (8 bytes, little-endian)
    fn persist_meta(&self, next_block_id: u64) -> Result<(), SfsError> {
        fs::write(self.meta_path(), &next_block_id.to_le_bytes())
            .map_err(|e| SfsError::IoError(format!("failed to write meta: {}", e)))
    }

    fn read_meta(root: &PathBuf) -> Result<u64, SfsError> {
        let bytes = fs::read(root.join("meta"))
            .map_err(|e| SfsError::IoError(format!("failed to read meta: {}", e)))?;
        if bytes.len() < 8 {
            return Err(SfsError::IoError("meta file too short".to_string()));
        }
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Sentinel value for the configured block_index_width.
    /// All 0xFF bytes in `block_index_width` bytes. This value is reserved
    /// and must never be allocated.
    fn sentinel(&self) -> u64 {
        let w = self.block_index_width as u32;
        if w >= 8 {
            u64::MAX
        } else {
            (1u64 << (w * 8)) - 1
        }
    }

    /// Build the L2 header section: | length: u16 | "blkfil" | version: u8 | block_size_shift | block_index_width |
    fn build_l2_section(&self) -> Vec<u8> {
        let data_len: u16 = 1 + 1; // block_size_shift + block_index_width
        let total_len: u16 = 2 + 6 + 1 + data_len; // = 11
        let mut buf = Vec::with_capacity(total_len as usize);
        buf.extend_from_slice(&total_len.to_le_bytes());
        buf.extend_from_slice(b"blkfil");
        buf.push(0); // version
        buf.push(self.block_size_shift);
        buf.push(self.block_index_width);
        buf
    }
}

impl BlockLayer for BlocksFromFiles {
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        upper_layers: &[u8],
    ) -> Result<Self, SfsError> {
        let root = PathBuf::from(path);
        if root.exists() {
            return Err(SfsError::AlreadyExists(root.display().to_string()));
        }
        fs::create_dir(&root).map_err(|e| SfsError::IoError(e.to_string()))?;

        let instance = BlocksFromFiles {
            root,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksState { next_block_id: 0 }),
        };
        instance.persist_meta(0)?;

        // Write the initial header (L2 section + upper_layers)
        instance.store_header(upper_layers)?;

        Ok(instance)
    }

    fn open(path: &str) -> Result<Self, SfsError> {
        let root = PathBuf::from(path);
        if !root.is_dir() {
            return Err(SfsError::NotFound(root.display().to_string()));
        }

        // Read block params from the header file
        let header_data = fs::read(root.join("header"))
            .map_err(|e| SfsError::IoError(format!("failed to read header: {}", e)))?;

        // Verify magic
        if header_data.len() < 10 || &header_data[0..9] != b"stream_fs" || header_data[9] != 0 {
            return Err(SfsError::IoError("invalid SFS header magic".to_string()));
        }

        // Read L2 section: offset 10 = length, offset 12 = identifier
        if header_data.len() < 21 {
            return Err(SfsError::IoError(
                "header too short for L2 section".to_string(),
            ));
        }
        if &header_data[12..18] != b"blkfil" {
            return Err(SfsError::IoError(format!(
                "expected L2 identifier 'blkfil', got '{}'",
                String::from_utf8_lossy(&header_data[12..18])
            )));
        }
        // offset 18 = version (skip), 19 = block_size_shift, 20 = block_index_width
        let block_size_shift = header_data[19];
        let block_index_width = header_data[20];

        let next_block_id = Self::read_meta(&root)?;

        Ok(BlocksFromFiles {
            root,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksState { next_block_id }),
        })
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_shift
    }

    fn block_size_shift(&self) -> u8 {
        self.block_size_shift
    }

    fn block_index_width(&self) -> u8 {
        self.block_index_width
    }

    fn allocate_block(&self) -> Result<u64, SfsError> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_block_id;

        // Index overflow protection
        if id >= self.sentinel() {
            return Err(SfsError::IoError(format!(
                "block index overflow: next block {} >= sentinel {} for block_index_width={}",
                id,
                self.sentinel(),
                self.block_index_width
            )));
        }

        // Create zeroed block file
        let block_size = self.block_size();
        let block_path = self.block_path(id);
        fs::write(&block_path, &vec![0u8; block_size]).map_err(|e| {
            SfsError::IoError(format!("failed to create block {}: {}", id, e))
        })?;

        state.next_block_id += 1;
        let next_id = state.next_block_id;
        drop(state);
        self.persist_meta(next_id)?;
        Ok(id)
    }

    fn deallocate_block(&self, index: u64) -> Result<(), SfsError> {
        let path = self.block_path(index);
        if !path.exists() {
            return Err(SfsError::NotFound(format!("block {}", index)));
        }
        fs::remove_file(&path).map_err(|e| {
            SfsError::IoError(format!("failed to delete block {}: {}", index, e))
        })?;
        Ok(())
    }

    fn read_block(&self, index: u64, offset: usize, buf: &mut [u8]) -> Result<usize, SfsError> {
        if index >= self.sentinel() {
            return Err(SfsError::IoError(format!(
                "read_block: block index {} is >= sentinel {} (block_index_width={})",
                index, self.sentinel(), self.block_index_width
            )));
        }
        let block_size = self.block_size();
        if offset + buf.len() > block_size {
            return Err(SfsError::IoError(format!(
                "read_block: offset {} + len {} exceeds block_size {}",
                offset,
                buf.len(),
                block_size
            )));
        }

        let path = self.block_path(index);
        let mut file = fs::File::open(&path)
            .map_err(|e| SfsError::IoError(format!("failed to open block {}: {}", index, e)))?;
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write_block(&self, index: u64, offset: usize, buf: &[u8]) -> Result<usize, SfsError> {
        let block_size = self.block_size();
        if offset + buf.len() > block_size {
            return Err(SfsError::IoError(format!(
                "write_block: offset {} + len {} exceeds block_size {}",
                offset,
                buf.len(),
                block_size
            )));
        }

        let path = self.block_path(index);
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| SfsError::IoError(format!("failed to open block {}: {}", index, e)))?;
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let n = file
            .write(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
        let mut header = Vec::new();
        // Magic
        header.extend_from_slice(b"stream_fs");
        header.push(0); // layout version 0
        // L2 section
        header.extend_from_slice(&self.build_l2_section());
        // L3 + L4 sections (passed through from above)
        header.extend_from_slice(upper_layers);

        fs::write(self.header_path(), &header)
            .map_err(|e| SfsError::IoError(format!("failed to write header: {}", e)))
    }

    fn load_header(&self) -> Result<Vec<u8>, SfsError> {
        let data = fs::read(self.header_path())
            .map_err(|e| SfsError::IoError(format!("failed to read header: {}", e)))?;

        // Verify magic
        if data.len() < 10 || &data[0..9] != b"stream_fs" || data[9] != 0 {
            return Err(SfsError::IoError("invalid SFS header magic".to_string()));
        }

        // Read L2 section length at offset 10, skip L2 section
        if data.len() < 12 {
            return Err(SfsError::IoError(
                "header too short for L2 section".to_string(),
            ));
        }
        let l2_len = u16::from_le_bytes([data[10], data[11]]) as usize;
        if data.len() < 10 + l2_len {
            return Err(SfsError::IoError("header too short for L2 data".to_string()));
        }

        // Return remainder (L3 + L4 sections)
        Ok(data[10 + l2_len..].to_vec())
    }
}
