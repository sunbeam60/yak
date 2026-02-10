use std::sync::Mutex;

use crate::block_layer::BlockLayer;
use crate::file_layer::FileLayer;
use crate::{OpenMode, SfsError};

// L2 header section layout (offsets chained so inserting a field auto-adjusts all successors):
// | length: u16 | "blocks": [u8;6] | version: u8 | bss: u8 | biw: u8 | total_blocks: u64 | free_list_head: u64 |
const L2_ID_OFFSET: usize = 2;
const L2_VERSION_OFFSET: usize = L2_ID_OFFSET + 6;
const L2_BSS_OFFSET: usize = L2_VERSION_OFFSET + 1;
const L2_BIW_OFFSET: usize = L2_BSS_OFFSET + 1;
const L2_TOTAL_BLOCKS_OFFSET: usize = L2_BIW_OFFSET + 1;
const L2_FREE_LIST_OFFSET: usize = L2_TOTAL_BLOCKS_OFFSET + 8;
const L2_SECTION_SIZE: usize = L2_FREE_LIST_OFFSET + 8;

/// L2 identifier in the header section.
const L2_IDENTIFIER: &[u8; 6] = b"blocks";

/// L2 header version.
const L2_VERSION: u8 = 0;

/// Bookkeeping state protected by a Mutex.
struct BlocksInFileState {
    total_blocks: u64,
    free_list_head: u64,
}

/// Real L2 implementation that stores blocks within a single file managed by L1.
///
/// Block `n` is at file offset `data_offset + n * block_size`. New blocks are
/// zeroed on allocation. Freed blocks form a singly-linked free list: the first
/// `block_index_width` bytes of a free block contain the next free block ID
/// (or sentinel for end of list). `free_list_head` in the L2 header section
/// points to the first free block.
///
/// Thread-safe: bookkeeping state is behind a `Mutex`. File I/O goes through
/// L1 which has its own internal mutex.
pub struct BlocksInFile<L1: FileLayer> {
    file: L1,
    block_size_shift: u8,
    block_index_width: u8,
    state: Mutex<BlocksInFileState>,
    /// Cached file offset where L2's header section starts.
    l2_section_offset: u64,
}

impl<L1: FileLayer> BlocksInFile<L1> {
    /// Compute the sentinel value for the configured block_index_width.
    fn sentinel(&self) -> u64 {
        block_sentinel(self.block_index_width)
    }

    /// File offset of block `n`.
    fn block_offset(&self, index: u64) -> u64 {
        self.file.data_offset() + index * self.block_size() as u64
    }

    /// Build the L2 header section bytes.
    fn build_l2_section(
        block_size_shift: u8,
        block_index_width: u8,
        total_blocks: u64,
        free_list_head: u64,
    ) -> Vec<u8> {
        let total_len: u16 = L2_SECTION_SIZE as u16;
        let mut buf = Vec::with_capacity(L2_SECTION_SIZE);
        buf.extend_from_slice(&total_len.to_le_bytes());
        buf.extend_from_slice(L2_IDENTIFIER);
        buf.push(L2_VERSION);
        buf.push(block_size_shift);
        buf.push(block_index_width);
        buf.extend_from_slice(&total_blocks.to_le_bytes());
        buf.extend_from_slice(&free_list_head.to_le_bytes());
        buf
    }

    /// Write total_blocks directly to the header at the known file offset.
    fn persist_total_blocks(&self, total_blocks: u64) -> Result<(), SfsError> {
        let offset = self.l2_section_offset + L2_TOTAL_BLOCKS_OFFSET as u64;
        self.file.write(offset, &total_blocks.to_le_bytes())
    }

    /// Write free_list_head directly to the header at the known file offset.
    fn persist_free_list_head(&self, free_list_head: u64) -> Result<(), SfsError> {
        let offset = self.l2_section_offset + L2_FREE_LIST_OFFSET as u64;
        self.file.write(offset, &free_list_head.to_le_bytes())
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_shift
    }
}

/// Compute the sentinel value for a given block_index_width.
fn block_sentinel(block_index_width: u8) -> u64 {
    let w = block_index_width as u32;
    if w >= 8 {
        u64::MAX
    } else {
        (1u64 << (w * 8)) - 1
    }
}

impl<L1: FileLayer> BlockLayer for BlocksInFile<L1> {
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        upper_layers: &[u8],
    ) -> Result<Self, SfsError> {
        let sentinel = block_sentinel(block_index_width);

        // Build L2 section with initial values
        let l2_section = Self::build_l2_section(
            block_size_shift,
            block_index_width,
            0,        // total_blocks
            sentinel, // free_list_head (empty list)
        );

        // Prepend L2 section to upper_layers and pass down to L1
        let mut combined = l2_section;
        combined.extend_from_slice(upper_layers);

        let file = L1::create(path, &combined)?;
        let l2_section_offset = file.upper_layers_offset();

        Ok(BlocksInFile {
            file,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks: 0,
                free_list_head: sentinel,
            }),
            l2_section_offset,
        })
    }

    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError> {
        let file = L1::open(path, mode)?;
        let l2_section_offset = file.upper_layers_offset();

        // Read the upper layers header data from L1
        let header_data = file.load_header()?;

        // Parse L2 section
        if header_data.len() < L2_SECTION_SIZE {
            return Err(SfsError::IoError(
                "header too short for L2 section".to_string(),
            ));
        }

        let l2_len = u16::from_le_bytes([header_data[0], header_data[1]]) as usize;
        if l2_len != L2_SECTION_SIZE {
            return Err(SfsError::IoError(format!(
                "unexpected L2 section length: {} (expected {})",
                l2_len, L2_SECTION_SIZE
            )));
        }

        if &header_data[L2_ID_OFFSET..L2_VERSION_OFFSET] != L2_IDENTIFIER {
            return Err(SfsError::IoError(format!(
                "expected L2 identifier 'blocks', got '{}'",
                String::from_utf8_lossy(&header_data[L2_ID_OFFSET..L2_VERSION_OFFSET])
            )));
        }

        let block_size_shift = header_data[L2_BSS_OFFSET];
        let block_index_width = header_data[L2_BIW_OFFSET];
        let total_blocks = u64::from_le_bytes(
            header_data[L2_TOTAL_BLOCKS_OFFSET..L2_FREE_LIST_OFFSET]
                .try_into()
                .unwrap(),
        );
        let free_list_head = u64::from_le_bytes(
            header_data[L2_FREE_LIST_OFFSET..L2_SECTION_SIZE]
                .try_into()
                .unwrap(),
        );

        Ok(BlocksInFile {
            file,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks,
                free_list_head,
            }),
            l2_section_offset,
        })
    }

    fn block_size(&self) -> usize {
        self.block_size()
    }

    fn block_size_shift(&self) -> u8 {
        self.block_size_shift
    }

    fn block_index_width(&self) -> u8 {
        self.block_index_width
    }

    fn allocate_block(&self) -> Result<u64, SfsError> {
        let mut state = self.state.lock().unwrap();
        let block_size = self.block_size();
        let sentinel = self.sentinel();

        if state.free_list_head != sentinel {
            // Pop from free list
            let block_id = state.free_list_head;

            // Read the next-free pointer from this block
            let biw = self.block_index_width as usize;
            let mut next_buf = [0u8; 8];
            self.file
                .read(self.block_offset(block_id), &mut next_buf[..biw])?;
            let next_free = u64::from_le_bytes(next_buf);

            state.free_list_head = next_free;

            // Zero the entire block
            let zeros = vec![0u8; block_size];
            self.file.write(self.block_offset(block_id), &zeros)?;

            // Persist updated free_list_head
            drop(state);
            self.persist_free_list_head(next_free)?;

            Ok(block_id)
        } else {
            // Extend file with a new block
            let block_id = state.total_blocks;

            // Index overflow protection
            if block_id >= sentinel {
                return Err(SfsError::IoError(format!(
                    "block index overflow: next block {} >= sentinel {} for block_index_width={}",
                    block_id, sentinel, self.block_index_width
                )));
            }

            // Grow the file to accommodate the new block
            let new_file_len = self.block_offset(block_id) + block_size as u64;
            self.file.set_len(new_file_len)?;

            // Zero the new block (file set_len may not zero on all platforms)
            let zeros = vec![0u8; block_size];
            self.file.write(self.block_offset(block_id), &zeros)?;

            state.total_blocks += 1;
            let total = state.total_blocks;
            drop(state);

            // Persist updated total_blocks
            self.persist_total_blocks(total)?;

            Ok(block_id)
        }
    }

    fn deallocate_block(&self, index: u64) -> Result<(), SfsError> {
        let mut state = self.state.lock().unwrap();

        // Write the current free_list_head into the first block_index_width bytes of this block
        let biw = self.block_index_width as usize;
        let head_bytes = state.free_list_head.to_le_bytes();
        self.file
            .write(self.block_offset(index), &head_bytes[..biw])?;

        // Update free_list_head to point to this block
        state.free_list_head = index;
        let new_head = state.free_list_head;
        drop(state);

        // Persist updated free_list_head
        self.persist_free_list_head(new_head)?;

        Ok(())
    }

    fn read_block(&self, index: u64, offset: usize, buf: &mut [u8]) -> Result<usize, SfsError> {
        if index >= self.sentinel() {
            return Err(SfsError::IoError(format!(
                "read_block: block index {} is >= sentinel {} (block_index_width={})",
                index,
                self.sentinel(),
                self.block_index_width
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

        let file_offset = self.block_offset(index) + offset as u64;
        self.file.read(file_offset, buf)
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

        let file_offset = self.block_offset(index) + offset as u64;
        self.file.write(file_offset, buf)?;
        Ok(buf.len())
    }

    fn store_header(&self, upper_layers: &[u8]) -> Result<(), SfsError> {
        let state = self.state.lock().unwrap();
        let l2_section = Self::build_l2_section(
            self.block_size_shift,
            self.block_index_width,
            state.total_blocks,
            state.free_list_head,
        );
        drop(state);

        let mut combined = l2_section;
        combined.extend_from_slice(upper_layers);

        self.file.store_header(&combined)
    }

    fn load_header(&self) -> Result<Vec<u8>, SfsError> {
        let header_data = self.file.load_header()?;

        // Skip L2 section, return remainder (L3 + L4 sections)
        if header_data.len() < L2_SECTION_SIZE {
            return Err(SfsError::IoError(
                "header too short for L2 section".to_string(),
            ));
        }

        let l2_len = u16::from_le_bytes([header_data[0], header_data[1]]) as usize;
        if header_data.len() < l2_len {
            return Err(SfsError::IoError(
                "header too short for L2 data".to_string(),
            ));
        }

        Ok(header_data[l2_len..].to_vec())
    }

    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, SfsError> {
        let mut issues = Vec::new();

        // 1. Run L1 verification
        issues.extend(self.file.verify()?);

        // 2. Read current state
        let state = self.state.lock().unwrap();
        let total_blocks = state.total_blocks;
        let free_list_head = state.free_list_head;
        drop(state);

        let sentinel = self.sentinel();
        let block_size = self.block_size() as u64;

        // 3. Check file size consistency
        let file_len = self.file.len()?;
        let expected_len = self.file.data_offset() + total_blocks * block_size;
        if file_len != expected_len {
            issues.push(format!(
                "L2: file size ({}) does not match expected ({}) for {} blocks",
                file_len, expected_len, total_blocks
            ));
        }

        // 4. Walk the free list, collecting free block IDs and detecting cycles
        let mut free_blocks = std::collections::HashSet::new();
        let mut current = free_list_head;
        let mut steps = 0u64;
        while current != sentinel {
            if current >= total_blocks {
                issues.push(format!(
                    "L2: free list references block {} which is >= total_blocks ({})",
                    current, total_blocks
                ));
                break;
            }
            if !free_blocks.insert(current) {
                issues.push(format!(
                    "L2: free list cycle detected at block {} after {} steps",
                    current, steps
                ));
                break;
            }
            steps += 1;
            if steps > total_blocks {
                issues.push("L2: free list longer than total_blocks (cycle likely)".to_string());
                break;
            }
            // Read next pointer from block
            let biw = self.block_index_width as usize;
            let mut next_buf = [0u8; 8];
            self.file
                .read(self.block_offset(current), &mut next_buf[..biw])?;
            current = u64::from_le_bytes(next_buf);
        }

        // 5. Build claimed set and check for out-of-range
        let claimed_set: std::collections::HashSet<u64> = claimed_blocks.iter().cloned().collect();
        for &block_id in claimed_blocks {
            if block_id >= total_blocks {
                issues.push(format!(
                    "L2: claimed block {} is >= total_blocks ({})",
                    block_id, total_blocks
                ));
            }
        }

        // 6. Check for duplicate claims (a block claimed by two streams)
        if claimed_set.len() != claimed_blocks.len() {
            let mut seen = std::collections::HashSet::new();
            for &block_id in claimed_blocks {
                if !seen.insert(block_id) {
                    issues.push(format!(
                        "L2: block {} claimed by multiple streams",
                        block_id
                    ));
                }
            }
        }

        // 7. Check overlap between claimed and free
        for &block_id in &claimed_set {
            if free_blocks.contains(&block_id) {
                issues.push(format!(
                    "L2: block {} is both claimed by a stream and on the free list",
                    block_id
                ));
            }
        }

        // 8. Check for orphaned blocks (not claimed, not free)
        for block_id in 0..total_blocks {
            if !claimed_set.contains(&block_id) && !free_blocks.contains(&block_id) {
                issues.push(format!(
                    "L2: block {} is orphaned (not claimed by any stream, not on free list)",
                    block_id
                ));
            }
        }

        Ok(issues)
    }
}
