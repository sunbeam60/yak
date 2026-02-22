use std::collections::VecDeque;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::block_layer::{BlockLayer, CacheMode};
use crate::{HeaderSlotId, OpenMode, YakError};

/// L2 mock identifier in the header section.
const L2_MOCK_IDENTIFIER: &[u8; 6] = b"blkfil";

/// L2 mock header version.
const L2_MOCK_VERSION: u8 = 0;

/// L2 mock payload size: "blkfil"(6) + version(1) + bss(1) + biw(1) = 9 bytes.
const L2_MOCK_PAYLOAD_SIZE: u16 = 9;

/// Size of the magic prefix: "yakyakyak"(9) + version(1) + total_header_length(2).
const MAGIC_SIZE: usize = 12;

/// Information about one header slot in the slot registry.
struct SlotInfo {
    /// Absolute byte offset in the header file where this slot's length prefix begins.
    offset: u64,
    /// Expected payload length (NOT including the 2-byte length prefix).
    payload_len: u16,
}

/// Bookkeeping state protected by a Mutex.
struct BlocksState {
    next_block_id: u64,
}

/// L2 mock implementation that stores each block as a numbered file on disk.
///
/// When created, makes a directory at the given path. Each block is stored
/// as `{id}.block` inside this directory. A `meta` file tracks the next
/// available block ID. A `header` file stores the Yak header with slot registry.
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
    slots: Vec<SlotInfo>,
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
    fn persist_meta(&self, next_block_id: u64) -> Result<(), YakError> {
        fs::write(self.meta_path(), next_block_id.to_le_bytes())
            .map_err(|e| YakError::IoError(format!("failed to write meta: {}", e)))
    }

    fn read_meta(root: &Path) -> Result<u64, YakError> {
        let bytes = fs::read(root.join("meta"))
            .map_err(|e| YakError::IoError(format!("failed to read meta: {}", e)))?;
        if bytes.len() < 8 {
            return Err(YakError::IoError("meta file too short".to_string()));
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

    /// Serialize the L2 mock header (no length prefix).
    /// Format: | "blkfil": [u8;6] | version: u8 | bss: u8 | biw: u8 |
    fn serialize_header(block_size_shift: u8, block_index_width: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L2_MOCK_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(L2_MOCK_IDENTIFIER);
        buf.push(L2_MOCK_VERSION);
        buf.push(block_size_shift);
        buf.push(block_index_width);
        buf
    }
}

impl BlockLayer for BlocksFromFiles {
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        mut slot_sizes: VecDeque<u16>,
        _password: Option<&[u8]>,
    ) -> Result<Self, YakError> {
        let root = PathBuf::from(path);
        if root.exists() {
            return Err(YakError::AlreadyExists(root.display().to_string()));
        }
        fs::create_dir(&root).map_err(|e| YakError::IoError(e.to_string()))?;

        // Push L2's own size to front (on-disk order: L2 first)
        slot_sizes.push_front(L2_MOCK_PAYLOAD_SIZE);

        // Compute header layout: magic + all sections
        let sections_total: usize = slot_sizes.iter().map(|&s| 2 + s as usize).sum();
        let total_header_length = (MAGIC_SIZE + sections_total) as u16;

        // Build header bytes
        let mut header = Vec::with_capacity(total_header_length as usize);
        // Magic: "yakyakyak" + version + total_header_length
        header.extend_from_slice(b"yakyakyak");
        header.push(0); // layout version 0
        header.extend_from_slice(&total_header_length.to_le_bytes());

        // Build slot registry and section placeholders (slot 0 = L2's own)
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

        let instance = BlocksFromFiles {
            root,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksState { next_block_id: 0 }),
            slots,
        };

        // Write L2 payload into slot 0
        let l2_payload = Self::serialize_header(block_size_shift, block_index_width);
        instance.write_header_slot(HeaderSlotId(0), &l2_payload)?;

        instance.persist_meta(0)?;

        Ok(instance)
    }

    fn open(path: &str, _mode: OpenMode, _password: Option<&[u8]>) -> Result<Self, YakError> {
        let root = PathBuf::from(path);
        if !root.is_dir() {
            return Err(YakError::NotFound(root.display().to_string()));
        }

        // Read header file
        let header_data = fs::read(root.join("header"))
            .map_err(|e| YakError::IoError(format!("failed to read header: {}", e)))?;

        // Verify magic (12 bytes: "yakyakyak" + version + total_header_length)
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

        // Read L2 section from slot 0
        if slots.is_empty() {
            return Err(YakError::IoError("no sections found in header".to_string()));
        }
        let l2_start = slots[0].offset as usize + 2; // skip length prefix
        let l2_end = l2_start + slots[0].payload_len as usize;
        if l2_end > header_data.len() {
            return Err(YakError::IoError(
                "L2 section extends beyond header".to_string(),
            ));
        }
        let l2_payload = &header_data[l2_start..l2_end];

        if l2_payload.len() < L2_MOCK_PAYLOAD_SIZE as usize {
            return Err(YakError::IoError("L2 payload too short".to_string()));
        }
        if &l2_payload[0..6] != L2_MOCK_IDENTIFIER {
            return Err(YakError::IoError(format!(
                "expected L2 identifier 'blkfil', got '{}'",
                String::from_utf8_lossy(&l2_payload[0..6])
            )));
        }

        // payload[6] = version (skip), payload[7] = bss, payload[8] = biw
        let block_size_shift = l2_payload[7];
        let block_index_width = l2_payload[8];

        let next_block_id = Self::read_meta(&root)?;

        Ok(BlocksFromFiles {
            root,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksState { next_block_id }),
            slots,
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

    fn allocate_block(&self) -> Result<u64, YakError> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_block_id;

        // Index overflow protection
        if id >= self.sentinel() {
            return Err(YakError::IoError(format!(
                "block index overflow: next block {} >= sentinel {} for block_index_width={}",
                id,
                self.sentinel(),
                self.block_index_width
            )));
        }

        // Create zeroed block file
        let block_size = self.block_size();
        let block_path = self.block_path(id);
        fs::write(&block_path, vec![0u8; block_size])
            .map_err(|e| YakError::IoError(format!("failed to create block {}: {}", id, e)))?;

        state.next_block_id += 1;
        // Persist before releasing the mutex, so other threads cannot observe
        // the updated in-memory state until the metadata is written.
        self.persist_meta(state.next_block_id)?;
        drop(state);
        Ok(id)
    }

    fn deallocate_block(&self, index: u64) -> Result<(), YakError> {
        let path = self.block_path(index);
        if !path.exists() {
            return Err(YakError::NotFound(format!("block {}", index)));
        }
        fs::remove_file(&path)
            .map_err(|e| YakError::IoError(format!("failed to delete block {}: {}", index, e)))?;
        Ok(())
    }

    fn read_block(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        _cache: CacheMode,
    ) -> Result<usize, YakError> {
        if index >= self.sentinel() {
            return Err(YakError::IoError(format!(
                "read_block: block index {} is >= sentinel {} (block_index_width={})",
                index,
                self.sentinel(),
                self.block_index_width
            )));
        }
        let block_size = self.block_size();
        if offset + buf.len() > block_size {
            return Err(YakError::IoError(format!(
                "read_block: offset {} + len {} exceeds block_size {}",
                offset,
                buf.len(),
                block_size
            )));
        }

        let path = self.block_path(index);
        let mut file = fs::File::open(&path)
            .map_err(|e| YakError::IoError(format!("failed to open block {}: {}", index, e)))?;
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write_block(
        &self,
        index: u64,
        offset: usize,
        buf: &[u8],
        _cache: CacheMode,
    ) -> Result<usize, YakError> {
        let block_size = self.block_size();
        if offset + buf.len() > block_size {
            return Err(YakError::IoError(format!(
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
            .map_err(|e| YakError::IoError(format!("failed to open block {}: {}", index, e)))?;
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let n = file
            .write(buf)
            .map_err(|e| YakError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Slot 0 = L2's own section, so upper layer index 0 → slot 1 (L3), etc.
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

    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();

        // Collect existing .block files on disk
        let state = self.state.lock().unwrap();
        let next_id = state.next_block_id;
        drop(state);

        let mut disk_blocks = std::collections::HashSet::new();
        for id in 0..next_id {
            if self.block_path(id).exists() {
                disk_blocks.insert(id);
            }
        }

        let claimed_set: std::collections::HashSet<u64> = claimed_blocks.iter().cloned().collect();

        // Blocks on disk but not claimed
        for &id in &disk_blocks {
            if !claimed_set.contains(&id) {
                issues.push(format!(
                    "L2-mock: block {} exists on disk but is not claimed",
                    id
                ));
            }
        }

        // Blocks claimed but not on disk
        for &id in &claimed_set {
            if !disk_blocks.contains(&id) {
                issues.push(format!(
                    "L2-mock: block {} is claimed but does not exist on disk",
                    id
                ));
            }
        }

        Ok(issues)
    }
}
