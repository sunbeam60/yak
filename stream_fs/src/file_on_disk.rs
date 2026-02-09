use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

use fs2::FileExt;

use crate::file_layer::FileLayer;
use crate::{OpenMode, SfsError};

/// Magic bytes at the start of every SFS file.
const MAGIC: &[u8; 9] = b"stream_fs";

/// Header layout format version.
const HEADER_FORMAT_VERSION: u8 = 0;

/// L1 identifier in the header section.
const L1_IDENTIFIER: &[u8; 6] = b"ondisk";

/// L1 header version.
const L1_VERSION: u8 = 0;

/// Size of the magic + format version prefix.
const MAGIC_SIZE: usize = 9 + 1; // "stream_fs" + version byte

/// Size of the L1 header section: | length: u16 | "ondisk" | version: u8 | data_offset: u64 |
const L1_SECTION_SIZE: usize = 2 + 6 + 1 + 8; // = 17

/// L1 implementation that wraps a real file on disk.
///
/// Creates/opens a single file, acquires an exclusive process-level lock,
/// and provides random-access I/O. Header-aware: reads/writes the SFS
/// header format and tracks `data_offset`.
pub struct FileOnDisk {
    state: Mutex<fs::File>,
    upper_layers_offset: u64,
    data_offset: u64,
    upper_layers_len: usize,
}

impl FileOnDisk {
    /// Build the L1 header section bytes.
    fn build_l1_section(data_offset: u64) -> Vec<u8> {
        let total_len: u16 = L1_SECTION_SIZE as u16;
        let mut buf = Vec::with_capacity(L1_SECTION_SIZE);
        buf.extend_from_slice(&total_len.to_le_bytes());
        buf.extend_from_slice(L1_IDENTIFIER);
        buf.push(L1_VERSION);
        buf.extend_from_slice(&data_offset.to_le_bytes());
        buf
    }

    /// Parse the full header from a file, validating magic and L1 section.
    /// Returns (upper_layers_offset, data_offset, upper_layers_data).
    fn parse_header(file: &mut fs::File) -> Result<(u64, u64, Vec<u8>), SfsError> {
        file.seek(SeekFrom::Start(0))
            .map_err(|e| SfsError::IoError(e.to_string()))?;

        // Read magic + format version
        let mut magic_buf = [0u8; MAGIC_SIZE];
        file.read_exact(&mut magic_buf)
            .map_err(|e| SfsError::IoError(format!("failed to read SFS header magic: {}", e)))?;

        if &magic_buf[0..9] != MAGIC {
            return Err(SfsError::IoError("not an SFS file (bad magic)".to_string()));
        }
        if magic_buf[9] != HEADER_FORMAT_VERSION {
            return Err(SfsError::IoError(format!(
                "unsupported header format version: {}",
                magic_buf[9]
            )));
        }

        // Read L1 section
        let mut l1_len_buf = [0u8; 2];
        file.read_exact(&mut l1_len_buf)
            .map_err(|e| SfsError::IoError(format!("failed to read L1 section length: {}", e)))?;
        let l1_len = u16::from_le_bytes(l1_len_buf) as usize;

        if l1_len < L1_SECTION_SIZE {
            return Err(SfsError::IoError(format!(
                "L1 section too short: {} < {}",
                l1_len, L1_SECTION_SIZE
            )));
        }

        // Read the rest of the L1 section (identifier + version + any extra data)
        let l1_remaining = l1_len - 2; // we already read the length
        let mut l1_body = vec![0u8; l1_remaining];
        file.read_exact(&mut l1_body)
            .map_err(|e| SfsError::IoError(format!("failed to read L1 section body: {}", e)))?;

        // Validate L1 identifier
        if &l1_body[0..6] != L1_IDENTIFIER {
            return Err(SfsError::IoError(format!(
                "expected L1 identifier 'ondisk', got '{}'",
                String::from_utf8_lossy(&l1_body[0..6])
            )));
        }

        // Read data_offset from the L1 section (bytes 7..15, after identifier + version)
        if l1_body.len() < 15 {
            return Err(SfsError::IoError(
                "L1 section missing data_offset field".to_string(),
            ));
        }
        let data_offset = u64::from_le_bytes(l1_body[7..15].try_into().unwrap());

        let upper_layers_offset = (MAGIC_SIZE + l1_len) as u64;

        // Read all upper layer sections between upper_layers_offset and data_offset.
        let upper_layers_len = (data_offset - upper_layers_offset) as usize;
        let mut sections_data = vec![0u8; upper_layers_len];
        file.read_exact(&mut sections_data).map_err(|e| {
            SfsError::IoError(format!("failed to read upper layer sections: {}", e))
        })?;

        Ok((upper_layers_offset, data_offset, sections_data))
    }
}

impl FileLayer for FileOnDisk {
    fn create(path: &str, upper_layers: &[u8]) -> Result<Self, SfsError>
    where
        Self: Sized,
    {
        if std::path::Path::new(path).exists() {
            return Err(SfsError::AlreadyExists(path.to_string()));
        }

        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| SfsError::IoError(format!("failed to create SFS file: {}", e)))?;

        // Acquire exclusive process lock
        file.try_lock_exclusive().map_err(|e| {
            SfsError::IoError(format!("SFS file is locked by another process: {}", e))
        })?;

        // Compute offsets first, then build L1 section with data_offset
        let upper_layers_offset = (MAGIC_SIZE + L1_SECTION_SIZE) as u64;
        let data_offset = upper_layers_offset + upper_layers.len() as u64;
        let l1_section = Self::build_l1_section(data_offset);

        let mut header = Vec::with_capacity(data_offset as usize);
        header.extend_from_slice(MAGIC);
        header.push(HEADER_FORMAT_VERSION);
        header.extend_from_slice(&l1_section);
        header.extend_from_slice(upper_layers);

        file.write_all(&header)
            .map_err(|e| SfsError::IoError(format!("failed to write SFS header: {}", e)))?;
        file.flush()
            .map_err(|e| SfsError::IoError(format!("failed to flush SFS header: {}", e)))?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            upper_layers_offset,
            data_offset,
            upper_layers_len: upper_layers.len(),
        })
    }

    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError>
    where
        Self: Sized,
    {
        let mut file = match mode {
            OpenMode::Read => fs::OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|e| SfsError::IoError(format!("failed to open SFS file: {}", e)))?,
            OpenMode::Write => fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| SfsError::IoError(format!("failed to open SFS file: {}", e)))?,
        };

        // Acquire process lock: shared for readers, exclusive for writers
        match mode {
            OpenMode::Read => file.try_lock_shared().map_err(|e| {
                SfsError::IoError(format!("SFS file is locked by another process: {}", e))
            })?,
            OpenMode::Write => file.try_lock_exclusive().map_err(|e| {
                SfsError::IoError(format!("SFS file is locked by another process: {}", e))
            })?,
        };

        let (upper_layers_offset, data_offset, upper_layers_data) =
            Self::parse_header(&mut file)?;

        Ok(FileOnDisk {
            state: Mutex::new(file),
            upper_layers_offset,
            data_offset,
            upper_layers_len: upper_layers_data.len(),
        })
    }

    fn upper_layers_offset(&self) -> u64 {
        self.upper_layers_offset
    }

    fn data_offset(&self) -> u64 {
        self.data_offset
    }

    fn read(&self, offset: u64, buf: &mut [u8]) -> Result<usize, SfsError> {
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let n = file
            .read(buf)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(n)
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), SfsError> {
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.write_all(data)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    fn len(&self) -> Result<u64, SfsError> {
        let file = self.state.lock().unwrap();
        let len = file
            .metadata()
            .map_err(|e| SfsError::IoError(e.to_string()))?
            .len();
        Ok(len)
    }

    fn set_len(&self, len: u64) -> Result<(), SfsError> {
        let file = self.state.lock().unwrap();
        file.set_len(len)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    fn flush(&self) -> Result<(), SfsError> {
        let mut file = self.state.lock().unwrap();
        file.flush()
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
        if upper_layers.len() != self.upper_layers_len {
            return Err(SfsError::IoError(format!(
                "header size mismatch: expected {} bytes, got {}",
                self.upper_layers_len,
                upper_layers.len()
            )));
        }

        // Rewrite the full header in-place
        let l1_section = Self::build_l1_section(self.data_offset);
        let mut header = Vec::with_capacity(self.data_offset as usize);
        header.extend_from_slice(MAGIC);
        header.push(HEADER_FORMAT_VERSION);
        header.extend_from_slice(&l1_section);
        header.extend_from_slice(upper_layers);

        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(0))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.write_all(&header)
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        file.flush()
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        Ok(())
    }

    fn load_header(&self) -> Result<Vec<u8>, SfsError> {
        let mut file = self.state.lock().unwrap();
        file.seek(SeekFrom::Start(self.upper_layers_offset))
            .map_err(|e| SfsError::IoError(e.to_string()))?;
        let mut buf = vec![0u8; self.upper_layers_len];
        file.read_exact(&mut buf)
            .map_err(|e| SfsError::IoError(format!("failed to read header: {}", e)))?;
        Ok(buf)
    }
}
