use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use thread_local::ThreadLocal;

use crate::block_layer::BlockLayer;
use crate::file_layer::FileLayer;
use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, SfsError};

// L2 header payload layout (offsets within the payload, WITHOUT the 2-byte length prefix):
// | "blocks": [u8;6] | version: u8 | bss: u8 | biw: u8 | total_blocks: u64 | free_list_head: u64 |
const P_ID_OFFSET: usize = 0;
const P_VERSION_OFFSET: usize = P_ID_OFFSET + 6;
const P_BSS_OFFSET: usize = P_VERSION_OFFSET + 1;
const P_BIW_OFFSET: usize = P_BSS_OFFSET + 1;
const P_TOTAL_BLOCKS_OFFSET: usize = P_BIW_OFFSET + 1;
const P_FREE_LIST_OFFSET: usize = P_TOTAL_BLOCKS_OFFSET + 8;
const L2_PAYLOAD_SIZE: u16 = (P_FREE_LIST_OFFSET + 8) as u16; // = 25

/// L2 identifier in the header section.
const L2_IDENTIFIER: &[u8; 6] = b"blocks";

/// L2 header version.
const L2_VERSION: u8 = 0;

/// Default memory budget (in bytes) for the per-thread block cache.
/// Each thread gets its own independent LRU cache up to this size.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 2 * 1024 * 1024;

/// Maximum entry count for the per-thread block cache.
/// Prevents excessive LRU overhead when block sizes are very small.
const BLOCK_CACHE_MAX_ENTRIES: usize = 4096;

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
/// Includes a per-thread write-through LRU block cache. Reads check the cache
/// first; misses read a full block from L1 and cache it. Writes go to L1
/// immediately and update the cached copy if present.
///
/// Thread-safe: bookkeeping state is behind a `Mutex`. File I/O goes through
/// L1 which has its own internal mutex. The block cache is thread-local
/// (no cross-thread synchronization needed).
pub struct BlocksInFile<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> {
    layer1: L1,
    block_size_shift: u8,
    block_index_width: u8,
    state: Mutex<BlocksInFileState>,
    /// L2's own header slot ID.
    my_slot: HeaderSlotId,
    /// Per-thread LRU cache of full block contents.
    /// Key: block index. Value: full block as Vec<u8>.
    block_cache: ThreadLocal<RefCell<LruCache<u64, Vec<u8>>>>,
    /// Precomputed cache capacity (entry count) for this instance's block size.
    cache_capacity: NonZeroUsize,
}

impl<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> BlocksInFile<L1, CACHE_BUDGET_BYTES> {
    /// Compute the sentinel value for the configured block_index_width.
    fn sentinel(&self) -> u64 {
        block_sentinel(self.block_index_width)
    }

    /// File offset of block `n`.
    fn block_offset(&self, index: u64) -> u64 {
        self.layer1.data_offset() + index * self.block_size() as u64
    }

    /// Serialize the L2 header (no length prefix).
    fn serialize_header(
        block_size_shift: u8,
        block_index_width: u8,
        total_blocks: u64,
        free_list_head: u64,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(L2_PAYLOAD_SIZE as usize);
        buf.extend_from_slice(L2_IDENTIFIER);
        buf.push(L2_VERSION);
        buf.push(block_size_shift);
        buf.push(block_index_width);
        buf.extend_from_slice(&total_blocks.to_le_bytes());
        buf.extend_from_slice(&free_list_head.to_le_bytes());
        buf
    }

    /// Persist the full L2 header payload via its slot.
    fn persist_l2_header(&self, total_blocks: u64, free_list_head: u64) -> Result<(), SfsError> {
        let payload = Self::serialize_header(
            self.block_size_shift,
            self.block_index_width,
            total_blocks,
            free_list_head,
        );
        self.layer1.write_header_slot(self.my_slot, &payload)
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_shift
    }

    /// Get or create the calling thread's block cache.
    fn thread_cache(&self) -> &RefCell<LruCache<u64, Vec<u8>>> {
        self.block_cache
            .get_or(|| RefCell::new(LruCache::new(self.cache_capacity)))
    }

    /// Compute cache entry count from block size.
    fn compute_cache_capacity(block_size_shift: u8) -> NonZeroUsize {
        let block_size = 1usize << block_size_shift;
        let by_budget = CACHE_BUDGET_BYTES / block_size;
        let entries = by_budget.clamp(1, BLOCK_CACHE_MAX_ENTRIES);
        NonZeroUsize::new(entries).unwrap()
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

impl<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> BlockLayer
    for BlocksInFile<L1, CACHE_BUDGET_BYTES>
{
    fn create(
        path: &str,
        block_size_shift: u8,
        block_index_width: u8,
        mut slot_sizes: VecDeque<u16>,
    ) -> Result<Self, SfsError> {
        let sentinel = block_sentinel(block_index_width);

        // Push L2 payload size to front (on-disk order: L2 first)
        slot_sizes.push_front(L2_PAYLOAD_SIZE);

        let layer1 = L1::create(path, slot_sizes)?;
        let my_slot = layer1.header_slot_for_upper(0);

        // Write initial L2 payload via slot
        let l2_payload = Self::serialize_header(
            block_size_shift,
            block_index_width,
            0,        // total_blocks
            sentinel, // free_list_head (empty list)
        );
        layer1.write_header_slot(my_slot, &l2_payload)?;

        Ok(BlocksInFile {
            layer1,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks: 0,
                free_list_head: sentinel,
            }),
            my_slot,
            block_cache: ThreadLocal::new(),
            cache_capacity: Self::compute_cache_capacity(block_size_shift),
        })
    }

    fn open(path: &str, mode: OpenMode) -> Result<Self, SfsError> {
        let layer1 = L1::open(path, mode)?;
        let my_slot = layer1.header_slot_for_upper(0);

        // Read L2 payload via slot (no length prefix)
        let header_buffer = layer1.read_header_slot(my_slot)?;

        if header_buffer.len() < L2_PAYLOAD_SIZE as usize {
            return Err(SfsError::IoError(format!(
                "L2 payload too short: {} < {}",
                header_buffer.len(),
                L2_PAYLOAD_SIZE
            )));
        }

        if &header_buffer[P_ID_OFFSET..P_VERSION_OFFSET] != L2_IDENTIFIER {
            return Err(SfsError::IoError(format!(
                "expected L2 identifier 'blocks', got '{}'",
                String::from_utf8_lossy(&header_buffer[P_ID_OFFSET..P_VERSION_OFFSET])
            )));
        }

        let block_size_shift = header_buffer[P_BSS_OFFSET];
        let block_index_width = header_buffer[P_BIW_OFFSET];
        let total_blocks = u64::from_le_bytes(
            header_buffer[P_TOTAL_BLOCKS_OFFSET..P_FREE_LIST_OFFSET]
                .try_into()
                .unwrap(),
        );
        let free_list_head = u64::from_le_bytes(
            header_buffer[P_FREE_LIST_OFFSET..L2_PAYLOAD_SIZE as usize]
                .try_into()
                .unwrap(),
        );

        Ok(BlocksInFile {
            layer1,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks,
                free_list_head,
            }),
            my_slot,
            block_cache: ThreadLocal::new(),
            cache_capacity: Self::compute_cache_capacity(block_size_shift),
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
        let blocks = self.allocate_blocks(1)?;
        Ok(blocks[0])
    }

    fn allocate_blocks(&self, count: u64) -> Result<Vec<u64>, SfsError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let mut state = self.state.lock().unwrap();
        let block_size = self.block_size();
        let sentinel = self.sentinel();
        let biw = self.block_index_width as usize;
        let mut result = Vec::with_capacity(count as usize);

        // Phase 1: drain free list
        while (result.len() as u64) < count && state.free_list_head != sentinel {
            let block_id = state.free_list_head;
            let mut next_buf = [0u8; 8];
            self.layer1
                .read(self.block_offset(block_id), &mut next_buf[..biw])?;
            state.free_list_head = u64::from_le_bytes(next_buf);
            result.push(block_id);
        }
        let free_list_count = result.len();

        // Phase 2: grow file once for all remaining blocks
        let remaining = count as usize - result.len();
        if remaining > 0 {
            let first_new_id = state.total_blocks;

            // Index overflow protection
            if first_new_id + remaining as u64 > sentinel {
                return Err(SfsError::IoError(format!(
                    "block index overflow: need {} blocks but only {} available before sentinel {}",
                    remaining,
                    sentinel - first_new_id,
                    sentinel
                )));
            }

            // Single set_len to grow file for all new blocks at once
            let last_new_id = first_new_id + remaining as u64;
            let new_file_len = self.block_offset(last_new_id);
            self.layer1.set_len(new_file_len)?;

            for i in 0..remaining as u64 {
                result.push(first_new_id + i);
            }
            state.total_blocks += remaining as u64;
        }

        // Phase 3: zero free-list blocks only (file-growth blocks are already
        // zero from set_len — guaranteed on Linux, macOS, and Windows)
        if free_list_count > 0 {
            let zeros = vec![0u8; block_size];
            for &block_id in &result[..free_list_count] {
                self.layer1.write(self.block_offset(block_id), &zeros)?;
            }
        }

        // Phase 4: persist L2 header before releasing the mutex, so other
        // threads cannot observe the updated in-memory state until the header
        // is written.
        self.persist_l2_header(state.total_blocks, state.free_list_head)?;
        drop(state);

        // Phase 5: evict allocated blocks from thread-local cache.
        // These blocks may have stale data from a previous incarnation
        // (recycled from the free list).
        if CACHE_BUDGET_BYTES > 0 {
            let mut cache = self.thread_cache().borrow_mut();
            for &block_id in &result {
                cache.pop(&block_id);
            }
        }

        Ok(result)
    }

    fn deallocate_block(&self, index: u64) -> Result<(), SfsError> {
        let mut state = self.state.lock().unwrap();

        // Write the current free_list_head into the first block_index_width bytes of this block
        let biw = self.block_index_width as usize;
        let head_bytes = state.free_list_head.to_le_bytes();
        self.layer1
            .write(self.block_offset(index), &head_bytes[..biw])?;

        // Update free_list_head to point to this block
        state.free_list_head = index;

        // Persist updated L2 header before releasing the mutex, so other
        // threads cannot observe the updated in-memory state until the header
        // is written.
        self.persist_l2_header(state.total_blocks, state.free_list_head)?;
        drop(state);

        // Evict deallocated block from thread-local cache.
        // Block content is now a free-list pointer, not useful data.
        if CACHE_BUDGET_BYTES > 0 {
            self.thread_cache().borrow_mut().pop(&index);
        }

        Ok(())
    }

    fn read_block(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        cache: bool,
    ) -> Result<usize, SfsError> {
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

        if cache && CACHE_BUDGET_BYTES > 0 {
            let mut lru = self.thread_cache().borrow_mut();

            // Cache hit: serve from cached full block
            if let Some(cached_block) = lru.get(&index) {
                buf.copy_from_slice(&cached_block[offset..offset + buf.len()]);
                return Ok(buf.len());
            }
        }

        // Cache miss (or cache disabled/bypassed): read full block from L1
        let mut full_block = vec![0u8; block_size];
        let file_offset = self.block_offset(index);
        let n = self.layer1.read(file_offset, &mut full_block)?;

        // Copy requested sub-region to caller's buffer
        buf.copy_from_slice(&full_block[offset..offset + buf.len()]);

        // Only cache if caller requested caching and we got a complete block read
        if cache && CACHE_BUDGET_BYTES > 0 && n == block_size {
            self.thread_cache().borrow_mut().put(index, full_block);
        }

        Ok(buf.len())
    }

    fn write_block(
        &self,
        index: u64,
        offset: usize,
        buf: &[u8],
        cache: bool,
    ) -> Result<usize, SfsError> {
        let block_size = self.block_size();
        if offset + buf.len() > block_size {
            return Err(SfsError::IoError(format!(
                "write_block: offset {} + len {} exceeds block_size {}",
                offset,
                buf.len(),
                block_size
            )));
        }

        // Write through to L1 first
        let file_offset = self.block_offset(index) + offset as u64;
        self.layer1.write(file_offset, buf)?;

        // Update cache if caller requested caching and block is already cached
        if cache && CACHE_BUDGET_BYTES > 0 {
            let mut lru = self.thread_cache().borrow_mut();
            if let Some(cached_block) = lru.get_mut(&index) {
                cached_block[offset..offset + buf.len()].copy_from_slice(buf);
            }
            // If not in cache, don't insert — we only have partial data
        }

        Ok(buf.len())
    }

    fn read_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, SfsError> {
        let file_offset = self.block_offset(start_index) + offset as u64;
        self.layer1.read(file_offset, buf)
    }

    fn write_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &[u8],
    ) -> Result<usize, SfsError> {
        let file_offset = self.block_offset(start_index) + offset as u64;
        self.layer1.write(file_offset, buf)?;
        Ok(buf.len())
    }

    fn invalidate_block_cache(&self) {
        if CACHE_BUDGET_BYTES > 0 {
            if let Some(cache_cell) = self.block_cache.get() {
                cache_cell.borrow_mut().clear();
            }
        }
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Map upper layer index to L1 slot index (+1 because slot 0 is L2's own)
        self.layer1.header_slot_for_upper(index + 1)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), SfsError> {
        self.layer1.write_header_slot(slot, data)
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, SfsError> {
        self.layer1.read_header_slot(slot)
    }

    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, SfsError> {
        let mut issues = Vec::new();

        // 1. Run L1 verification
        issues.extend(self.layer1.verify()?);

        // 2. Read current state
        let state = self.state.lock().unwrap();
        let total_blocks = state.total_blocks;
        let free_list_head = state.free_list_head;
        drop(state);

        let sentinel = self.sentinel();
        let block_size = self.block_size() as u64;

        // 3. Check file size consistency
        let file_len = self.layer1.len()?;
        let expected_len = self.layer1.data_offset() + total_blocks * block_size;
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
            self.layer1
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
