use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use fs2::FileExt;

use crate::file_layer::FileLayer;
use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

/// Magic bytes at the start of every Yak file.
const MAGIC: &[u8; 9] = b"yakyakyak";

/// Header layout format version.
const HEADER_FORMAT_VERSION: u8 = 0;

/// L1 identifier in the header section.
const L1_IDENTIFIER: &[u8; 6] = b"ondisk";

/// L1 header version.
const L1_VERSION: u8 = 0;

/// Size of the magic prefix: "yakyakyak"(9) + version(1) + total_header_length(2).
const MAGIC_SIZE: usize = 9 + 1 + 2; // = 12

/// Size of the L1 header section: | length: u16 | "ondisk" | version: u8 |
const L1_SECTION_SIZE: usize = 2 + 6 + 1; // = 9

/// Information about one header slot in the slot registry.
struct SlotInfo {
    /// Absolute byte offset in the file where this slot's length prefix begins.
    offset: u64,
    /// Expected payload length (NOT including the 2-byte length prefix).
    payload_len: u16,
}

/// L1 implementation that wraps a real file on disk.
///
/// Creates/opens a single file, acquires an exclusive process-level lock,
/// and provides random-access I/O. Header-aware: reads/writes the Yak
/// header format and tracks `data_offset` (from the magic section's
/// `total_header_length` field).
pub struct FileOnDisk {
    state: Mutex<fs::File>,
    data_offset: u64,
    slots: Vec<SlotInfo>,
}

impl FileOnDisk {
    /// Build the magic prefix: "yakyakyak"(9) + version(1) + total_header_length(2).
    fn serialize_magic_header(total_header_length: u16) -> [u8; MAGIC_SIZE] {
        let mut buf = [0u8; MAGIC_SIZE];
        buf[0..9].copy_from_slice(MAGIC);
        buf[9] = HEADER_FORMAT_VERSION;
        buf[10..12].copy_from_slice(&total_header_length.to_le_bytes());
        buf
    }

    /// Build the L1 header section bytes: | length: u16 | "ondisk" | version: u8 |
    fn serialize_header() -> [u8; L1_SECTION_SIZE] {
        let mut buf = [0u8; L1_SECTION_SIZE];
        let total_len = L1_SECTION_SIZE as u16;
        buf[0..2].copy_from_slice(&total_len.to_le_bytes());
        buf[2..8].copy_from_slice(L1_IDENTIFIER);
        buf[8] = L1_VERSION;
        buf
    }

    /// Read and validate the magic prefix: "yakyakyak"(9) + version(1) + total_header_length(2).
    /// Returns `data_offset` (= total_header_length).
    fn deserialize_magic_header(file: &mut fs::File) -> Result<u64, YakError> {
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

        let total_header_length = u16::from_le_bytes([buf[10], buf[11]]);
        Ok(total_header_length as u64)
    }

    /// Validate the L1 header payload: | "ondisk" | version: u8 |.
    /// `payload` is the section bytes WITHOUT the 2-byte length prefix.
    fn deserialize_header(payload: &[u8]) -> Result<(), YakError> {
        let expected_payload = L1_SECTION_SIZE - 2; // identifier(6) + version(1)
        if payload.len() < expected_payload {
            return Err(YakError::IoError(format!(
                "L1 header payload too short: {} < {}",
                payload.len(),
                expected_payload
            )));
        }
        if &payload[0..6] != L1_IDENTIFIER {
            return Err(YakError::IoError(format!(
                "expected L1 identifier 'ondisk', got '{}'",
                String::from_utf8_lossy(&payload[0..6])
            )));
        }
        Ok(())
    }

    /// Read magic and walk all layer sections into the slot registry.
    /// Returns (data_offset, slot_registry). Does NOT validate any layer headers.
    fn parse_header(file: &mut fs::File) -> Result<(u64, Vec<SlotInfo>), YakError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|e| YakError::IoError(e.to_string()))?;

        let data_offset = Self::deserialize_magic_header(file)?;

        // Walk all layer sections (including L1) into the slot registry
        let mut slots = Vec::new();
        let mut current_offset = MAGIC_SIZE as u64;
        while current_offset < data_offset {
            let mut len_buf = [0u8; 2];
            file.read_exact(&mut len_buf).map_err(|e| {
                YakError::IoError(format!(
                    "failed to read section length at offset {}: {}",
                    current_offset, e
                ))
            })?;
            let section_len = u16::from_le_bytes(len_buf);
            if section_len < 2 {
                return Err(YakError::IoError(format!(
                    "invalid section length {} at offset {}",
                    section_len, current_offset
                )));
            }
            let payload_len = section_len - 2;
            slots.push(SlotInfo {
                offset: current_offset,
                payload_len,
            });
            // Skip over the payload to reach the next section
            let skip = payload_len as i64;
            file.seek(SeekFrom::Current(skip))
                .map_err(|e| YakError::IoError(format!("failed to skip section payload: {}", e)))?;
            current_offset += section_len as u64;
        }

        Ok((data_offset, slots))
    }
}

impl FileLayer for FileOnDisk {
    fn create(path: &str, slot_sizes: VecDeque<u16>) -> Result<Self, YakError>
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

        // Acquire exclusive process lock as we're about to write to this file
        file.try_lock_exclusive().map_err(|e| {
            YakError::IoError(format!("Yak file is locked by another process: {}", e))
        })?;

        // Sizes arrive in on-disk order [L2, L3, L4] (each layer push_front'd)
        // Compute offsets
        // now we write our own header (magic + L1) and reserve space for upper layers' sections (length prefix + zeros)

        // compute the total header lefth
        let upper_layers_offset = (MAGIC_SIZE + L1_SECTION_SIZE) as u64;
        let upper_total: usize = slot_sizes
            .iter()
            .map(|&s| 2 + s as usize) //+2 to create 2 byte space for the length prefix of each section
            .sum();
        let data_offset = upper_layers_offset + upper_total as u64;
        let total_header_length = data_offset as u16;

        // build magic and L1 header
        let magic_header = Self::serialize_magic_header(total_header_length);
        let l1_header = Self::serialize_header();

        // Build header: magic + L1 + slot placeholders (length prefix + zeros)
        let mut header = Vec::with_capacity(data_offset as usize);
        header.extend_from_slice(&magic_header);
        header.extend_from_slice(&l1_header);

        // Build slot registry: slot 0 = L1, then upper layers
        let mut slots = Vec::with_capacity(1 + slot_sizes.len());
        slots.push(SlotInfo {
            offset: MAGIC_SIZE as u64,
            payload_len: (L1_SECTION_SIZE - 2) as u16,
        });
        let mut current_offset = upper_layers_offset;
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

        // write the header (at this point layer headers are blank, the layers will fill them in later via write_header_slot)
        file.write_all(&header)
            .map_err(|e| YakError::IoError(format!("failed to write Yak header: {}", e)))?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            data_offset,
            slots,
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

        // pull out layer header information
        let (data_offset, slots) = Self::parse_header(&mut file)?;

        // Validate L1's own header (slot 0)
        if slots.is_empty() {
            return Err(YakError::IoError(
                "no header sections found after magic".to_string(),
            ));
        }
        let l1_slot = &slots[0];
        file.seek(SeekFrom::Start(l1_slot.offset + 2))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let mut l1_payload = vec![0u8; l1_slot.payload_len as usize];
        file.read_exact(&mut l1_payload)
            .map_err(|e| YakError::IoError(format!("failed to read L1 header: {}", e)))?;
        Self::deserialize_header(&l1_payload)?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            data_offset,
            slots,
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

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        HeaderSlotId(index + 1) // slot 0 is L1's own header; upper layers start at slot 1
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

        // Write length prefix + payload at the slot offset
        let section_len = info.payload_len + 2; // total section length including the 2-byte length prefix
        let mut buf = Vec::with_capacity(section_len as usize);
        buf.extend_from_slice(&section_len.to_le_bytes());
        buf.extend_from_slice(data);

        let mut file = self.state.lock().unwrap();
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

        // Read payload bytes (skip the 2-byte length prefix)
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(info.offset + 2))
            .map_err(|e| YakError::IoError(e.to_string()))?;
        let mut buf = vec![0u8; info.payload_len as usize];
        file.read_exact(&mut buf)
            .map_err(|e| YakError::IoError(format!("failed to read header slot: {}", e)))?;
        Ok(buf)
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
        let stored_header_len = u16::from_le_bytes([magic_buf[10], magic_buf[11]]);
        if stored_header_len as u64 != self.data_offset {
            issues.push(format!(
                "L1: total_header_length in magic ({}) does not match data_offset ({})",
                stored_header_len, self.data_offset
            ));
        }

        Ok(issues)
    }
}
