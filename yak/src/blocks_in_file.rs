use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use thread_local::ThreadLocal;

use crate::block_layer::{BlockLayer, CacheMode};
use crate::encryption::{self, BlockCipher, EncryptionConfig};
use crate::file_layer::FileLayer;
use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

// L2 header payload layout (offsets within the payload, WITHOUT the 2-byte length prefix):
// | "blocks": [u8;6] | version: u8 | bss: u8 | biw: u8 | free_list_head: u64 | encrypted: u8 |
// If encrypted == 1, the remaining bytes are the encryption config (129 bytes).
// total_blocks is derived at runtime from file size: (file_len - data_offset) / block_size
const P_ID_OFFSET: usize = 0;
const P_VERSION_OFFSET: usize = P_ID_OFFSET + 6;
const P_BSS_OFFSET: usize = P_VERSION_OFFSET + 1;
const P_BIW_OFFSET: usize = P_BSS_OFFSET + 1;
const P_FREE_LIST_OFFSET: usize = P_BIW_OFFSET + 1;
const P_ENCRYPTED_FLAG_OFFSET: usize = P_FREE_LIST_OFFSET + 8;
const P_ENCRYPTION_CONFIG_OFFSET: usize = P_ENCRYPTED_FLAG_OFFSET + 1;

/// L2 payload size for unencrypted files (base fields + encrypted_flag byte).
const L2_PAYLOAD_SIZE_PLAIN: u16 = P_ENCRYPTION_CONFIG_OFFSET as u16; // = 18

/// L2 payload size for encrypted files (base fields + encrypted_flag + encryption config).
const L2_PAYLOAD_SIZE_ENCRYPTED: u16 =
    L2_PAYLOAD_SIZE_PLAIN + encryption::ENCRYPTION_CONFIG_SIZE as u16; // = 147

/// L2 identifier in the header section.
const L2_IDENTIFIER: &[u8; 6] = b"blocks";

/// L2 header version. Bumped from 0 to 1 to indicate the new format with encryption support.
const L2_VERSION: u8 = 1;

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
/// Dual-cache design:
/// - **Per-thread LRU** (`block_cache`): user stream redirector blocks.
///   Zero cross-thread contention; survives Streams stream lock acquisitions.
/// - **Shared LRU** (`shared_cache`): Streams stream redirector blocks.
///   Mutex-guarded for cross-thread coherency; write-through semantics
///   ensure all threads see each other's Streams stream writes immediately.
///
/// Thread-safe: bookkeeping state is behind a `Mutex`. File I/O goes through
/// L1 which has its own internal mutex.
pub struct BlocksInFile<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> {
    layer1: L1,
    block_size_shift: u8,
    block_index_width: u8,
    state: Mutex<BlocksInFileState>,
    /// L2's own header slot ID.
    my_slot: HeaderSlotId,
    /// Per-thread LRU cache of full **decrypted** block contents (user streams).
    block_cache: ThreadLocal<RefCell<LruCache<u64, Vec<u8>>>>,
    /// Shared LRU cache of full **decrypted** block contents (Streams stream).
    shared_cache: Mutex<LruCache<u64, Vec<u8>>>,
    /// Precomputed cache capacity (entry count).
    cache_capacity: NonZeroUsize,
    /// Runtime cipher for block encryption/decryption. None = unencrypted.
    cipher: Option<BlockCipher>,
    /// On-disk encryption config. Stored so we can re-serialize the full L2
    /// header on every `persist_l2_header` call (which happens on every
    /// allocate/deallocate).
    encryption_config: Option<EncryptionConfig>,
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
        free_list_head: u64,
        encryption_config: Option<&EncryptionConfig>,
    ) -> Vec<u8> {
        let payload_size = if encryption_config.is_some() {
            L2_PAYLOAD_SIZE_ENCRYPTED
        } else {
            L2_PAYLOAD_SIZE_PLAIN
        };
        let mut buf = Vec::with_capacity(payload_size as usize);
        buf.extend_from_slice(L2_IDENTIFIER);
        buf.push(L2_VERSION);
        buf.push(block_size_shift);
        buf.push(block_index_width);
        buf.extend_from_slice(&free_list_head.to_le_bytes());
        if let Some(config) = encryption_config {
            buf.push(1); // encrypted_flag = 1
            buf.extend_from_slice(&encryption::serialize_config(config));
        } else {
            buf.push(0); // encrypted_flag = 0
        }
        buf
    }

    /// Persist the full L2 header payload via its slot.
    fn persist_l2_header(&self, free_list_head: u64) -> Result<(), YakError> {
        let payload = Self::serialize_header(
            self.block_size_shift,
            self.block_index_width,
            free_list_head,
            self.encryption_config.as_ref(),
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

    // -----------------------------------------------------------------
    // Cache helpers — route to the appropriate cache based on CacheMode
    // -----------------------------------------------------------------

    /// Try a cache read. Returns `Some(())` if data was served from cache.
    fn try_cache_read(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        cache: CacheMode,
    ) -> Option<()> {
        if CACHE_BUDGET_BYTES == 0 {
            return None;
        }
        match cache {
            CacheMode::None => None,
            CacheMode::ThreadLocal => {
                let mut lru = self.thread_cache().borrow_mut();
                let cached = lru.get(&index)?;
                buf.copy_from_slice(&cached[offset..offset + buf.len()]);
                Some(())
            }
            CacheMode::Shared => {
                let mut lru = self.shared_cache.lock().unwrap();
                let cached = lru.get(&index)?;
                buf.copy_from_slice(&cached[offset..offset + buf.len()]);
                Some(())
            }
        }
    }

    /// Store a full block in the appropriate cache.
    fn cache_put(&self, index: u64, full_block: Vec<u8>, cache: CacheMode) {
        if CACHE_BUDGET_BYTES == 0 {
            return;
        }
        match cache {
            CacheMode::None => {}
            CacheMode::ThreadLocal => {
                self.thread_cache().borrow_mut().put(index, full_block);
            }
            CacheMode::Shared => {
                self.shared_cache.lock().unwrap().put(index, full_block);
            }
        }
    }

    /// Update a cached block in-place (unencrypted write-through).
    fn cache_update_in_place(&self, index: u64, offset: usize, buf: &[u8], cache: CacheMode) {
        if CACHE_BUDGET_BYTES == 0 {
            return;
        }
        match cache {
            CacheMode::None => {}
            CacheMode::ThreadLocal => {
                let mut lru = self.thread_cache().borrow_mut();
                if let Some(cached) = lru.get_mut(&index) {
                    cached[offset..offset + buf.len()].copy_from_slice(buf);
                }
            }
            CacheMode::Shared => {
                let mut lru = self.shared_cache.lock().unwrap();
                if let Some(cached) = lru.get_mut(&index) {
                    cached[offset..offset + buf.len()].copy_from_slice(buf);
                }
            }
        }
    }

    /// Peek a full plaintext block from cache (for encrypted RMW).
    fn try_cache_peek_full_block(&self, index: u64, cache: CacheMode) -> Option<Vec<u8>> {
        if CACHE_BUDGET_BYTES == 0 {
            return None;
        }
        match cache {
            CacheMode::None => None,
            CacheMode::ThreadLocal => {
                let lru = self.thread_cache().borrow();
                lru.peek(&index).cloned()
            }
            CacheMode::Shared => {
                let lru = self.shared_cache.lock().unwrap();
                lru.peek(&index).cloned()
            }
        }
    }

    /// Evict a block from **both** caches. Called on allocate/deallocate.
    fn evict_from_all_caches(&self, index: u64) {
        if CACHE_BUDGET_BYTES == 0 {
            return;
        }
        if let Some(tc) = self.block_cache.get() {
            tc.borrow_mut().pop(&index);
        }
        self.shared_cache.lock().unwrap().pop(&index);
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
        password: Option<&[u8]>,
    ) -> Result<Self, YakError> {
        let sentinel = block_sentinel(block_index_width);

        // Set up encryption if a password is provided
        let (cipher, encryption_config) = if let Some(pw) = password {
            // AES-XTS requires at least 16 bytes per block
            if block_size_shift < 4 {
                return Err(YakError::IoError(
                    "encrypted files require block_size_shift >= 4 (16 bytes minimum)".to_string(),
                ));
            }
            let (config, cipher) = encryption::create_encryption(pw)?;
            (Some(cipher), Some(config))
        } else {
            (None, None)
        };

        // Push L2 payload size to front (on-disk order: L2 first)
        let payload_size = if encryption_config.is_some() {
            L2_PAYLOAD_SIZE_ENCRYPTED
        } else {
            L2_PAYLOAD_SIZE_PLAIN
        };
        slot_sizes.push_front(payload_size);

        let layer1 = L1::create(path, slot_sizes)?;
        let my_slot = layer1.header_slot_for_upper(0);

        // Write initial L2 payload via slot
        let l2_payload = Self::serialize_header(
            block_size_shift,
            block_index_width,
            sentinel, // free_list_head (empty list)
            encryption_config.as_ref(),
        );
        layer1.write_header_slot(my_slot, &l2_payload)?;

        let cache_capacity = Self::compute_cache_capacity(block_size_shift);
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
            shared_cache: Mutex::new(LruCache::new(cache_capacity)),
            cache_capacity,
            cipher,
            encryption_config,
        })
    }

    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError> {
        let layer1 = L1::open(path, mode)?;
        let my_slot = layer1.header_slot_for_upper(0);

        // Read L2 payload via slot (no length prefix)
        let header_buffer = layer1.read_header_slot(my_slot)?;

        // Minimum size: the base fields before the encrypted_flag
        let min_size = P_ENCRYPTED_FLAG_OFFSET;
        if header_buffer.len() < min_size {
            return Err(YakError::IoError(format!(
                "L2 payload too short: {} < {}",
                header_buffer.len(),
                min_size
            )));
        }

        if &header_buffer[P_ID_OFFSET..P_VERSION_OFFSET] != L2_IDENTIFIER {
            return Err(YakError::IoError(format!(
                "expected L2 identifier 'blocks', got '{}'",
                String::from_utf8_lossy(&header_buffer[P_ID_OFFSET..P_VERSION_OFFSET])
            )));
        }

        let version = header_buffer[P_VERSION_OFFSET];
        let block_size_shift = header_buffer[P_BSS_OFFSET];
        let block_index_width = header_buffer[P_BIW_OFFSET];
        let free_list_head = u64::from_le_bytes(
            header_buffer[P_FREE_LIST_OFFSET..P_ENCRYPTED_FLAG_OFFSET]
                .try_into()
                .unwrap(),
        );

        // Handle encryption based on version and encrypted_flag
        let (cipher, encryption_config) = if version >= 1
            && header_buffer.len() > P_ENCRYPTED_FLAG_OFFSET
            && header_buffer[P_ENCRYPTED_FLAG_OFFSET] == 1
        {
            // File is encrypted — deserialize config and verify password
            let config =
                encryption::deserialize_config(&header_buffer[P_ENCRYPTION_CONFIG_OFFSET..])?;
            let pw = password.ok_or_else(|| {
                YakError::EncryptionRequired(
                    "file is encrypted but no password was provided".to_string(),
                )
            })?;
            let cipher = encryption::open_encryption(&config, pw)?;
            (Some(cipher), Some(config))
        } else {
            // File is not encrypted
            if password.is_some() {
                return Err(YakError::IoError(
                    "password provided but file is not encrypted".to_string(),
                ));
            }
            (None, None)
        };

        // Derive total_blocks from file size rather than storing it in the header
        let block_size = 1u64 << block_size_shift;
        let total_blocks = (layer1.len()? - layer1.data_offset()) / block_size;

        let cache_capacity = Self::compute_cache_capacity(block_size_shift);
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
            shared_cache: Mutex::new(LruCache::new(cache_capacity)),
            cache_capacity,
            cipher,
            encryption_config,
        })
    }

    fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
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

    fn allocate_block(&self) -> Result<u64, YakError> {
        let blocks = self.allocate_blocks(1)?;
        Ok(blocks[0])
    }

    fn allocate_blocks(&self, count: u64) -> Result<Vec<u64>, YakError> {
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
            if let Some(cipher) = &self.cipher {
                // Encrypted: read full block, decrypt, extract free-list pointer
                let mut full_block = vec![0u8; block_size];
                self.layer1
                    .read(self.block_offset(block_id), &mut full_block)?;
                encryption::decrypt_block(cipher, block_id, &mut full_block);
                next_buf[..biw].copy_from_slice(&full_block[..biw]);
            } else {
                self.layer1
                    .read(self.block_offset(block_id), &mut next_buf[..biw])?;
            }
            state.free_list_head = u64::from_le_bytes(next_buf);
            result.push(block_id);
        }
        let free_list_count = result.len();

        // Sort free-list blocks by index so that originally-contiguous blocks
        // are returned in ascending order, maximising contiguous runs for the
        // run detector to batch into single I/O operations.
        if free_list_count > 1 {
            result[..free_list_count].sort_unstable();
        }

        // Phase 2: grow file once for all remaining blocks
        let remaining = count as usize - result.len();
        if remaining > 0 {
            let first_new_id = state.total_blocks;

            // Index overflow protection
            // This is a non-overflowing way of ensuring we don't allocate more
            // blocks than the block index width can represent
            if remaining as u64 > sentinel - first_new_id {
                return Err(YakError::IoError(format!(
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
            if let Some(cipher) = &self.cipher {
                // Encrypted: encrypt zeros per-block (each gets its own tweak)
                for &block_id in &result[..free_list_count] {
                    let mut zeros = vec![0u8; block_size];
                    encryption::encrypt_block(cipher, block_id, &mut zeros);
                    self.layer1.write(self.block_offset(block_id), &zeros)?;
                }
            } else {
                let zeros = vec![0u8; block_size];
                for &block_id in &result[..free_list_count] {
                    self.layer1.write(self.block_offset(block_id), &zeros)?;
                }
            }
        }

        // Phase 4: persist L2 header before releasing the mutex, so other
        // threads cannot observe the updated in-memory state until the header
        // is written.
        self.persist_l2_header(state.free_list_head)?;
        drop(state);

        // Phase 5: evict allocated blocks from both caches.
        // These blocks may have stale data from a previous incarnation
        // (recycled from the free list).
        for &block_id in &result {
            self.evict_from_all_caches(block_id);
        }

        Ok(result)
    }

    fn deallocate_block(&self, index: u64) -> Result<(), YakError> {
        let mut state = self.state.lock().unwrap();

        // Write the current free_list_head into the first block_index_width bytes of this block
        let biw = self.block_index_width as usize;
        let head_bytes = state.free_list_head.to_le_bytes();
        if let Some(cipher) = &self.cipher {
            // Encrypted: zero-fill block, write pointer, encrypt, then write full block.
            // This also securely zeroes old data in deallocated blocks.
            let block_size = self.block_size();
            let mut block = vec![0u8; block_size];
            block[..biw].copy_from_slice(&head_bytes[..biw]);
            encryption::encrypt_block(cipher, index, &mut block);
            self.layer1.write(self.block_offset(index), &block)?;
        } else {
            self.layer1
                .write(self.block_offset(index), &head_bytes[..biw])?;
        }

        // Update free_list_head to point to this block
        state.free_list_head = index;

        // Persist updated L2 header before releasing the mutex, so other
        // threads cannot observe the updated in-memory state until the header
        // is written.
        self.persist_l2_header(state.free_list_head)?;
        drop(state);

        // Evict deallocated block from both caches.
        self.evict_from_all_caches(index);

        Ok(())
    }

    fn deallocate_blocks(&self, indices: &mut Vec<u64>) -> Result<(), YakError> {
        if indices.is_empty() {
            return Ok(());
        }

        // Sort ascending so the free-list chain is written in block order.
        // When these blocks are later re-allocated, they come off the free list
        // in ascending order, maximising contiguous runs for batched I/O.
        indices.sort_unstable();

        let mut state = self.state.lock().unwrap();
        let biw = self.block_index_width as usize;

        // Chain: indices[0] → indices[1] → … → indices[n-1] → old free_list_head
        if let Some(cipher) = &self.cipher {
            // Encrypted: zero-fill + pointer per block, encrypt, write full blocks.
            // Securely zeroes old data in deallocated blocks.
            let block_size = self.block_size();
            for i in 0..indices.len() - 1 {
                let mut block = vec![0u8; block_size];
                let next = indices[i + 1].to_le_bytes();
                block[..biw].copy_from_slice(&next[..biw]);
                encryption::encrypt_block(cipher, indices[i], &mut block);
                self.layer1.write(self.block_offset(indices[i]), &block)?;
            }
            // Last freed block points to the previous head
            let mut block = vec![0u8; block_size];
            let head_bytes = state.free_list_head.to_le_bytes();
            block[..biw].copy_from_slice(&head_bytes[..biw]);
            let last_idx = *indices.last().unwrap();
            encryption::encrypt_block(cipher, last_idx, &mut block);
            self.layer1.write(self.block_offset(last_idx), &block)?;
        } else {
            for i in 0..indices.len() - 1 {
                let next = indices[i + 1].to_le_bytes();
                self.layer1
                    .write(self.block_offset(indices[i]), &next[..biw])?;
            }
            // Last freed block points to the previous head
            let head_bytes = state.free_list_head.to_le_bytes();
            self.layer1.write(
                self.block_offset(*indices.last().unwrap()),
                &head_bytes[..biw],
            )?;
        }

        // New head is the lowest-numbered freed block
        state.free_list_head = indices[0];

        // Single header persist (saves n-1 header writes vs individual calls)
        self.persist_l2_header(state.free_list_head)?;
        drop(state);

        // Evict freed blocks from both caches
        for &id in indices.iter() {
            self.evict_from_all_caches(id);
        }

        Ok(())
    }

    fn read_block(
        &self,
        index: u64,
        offset: usize,
        buf: &mut [u8],
        cache: CacheMode,
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

        // Cache hit: serve from the appropriate cache (already decrypted)
        if self.try_cache_read(index, offset, buf, cache).is_some() {
            return Ok(buf.len());
        }

        // Cache miss (or cache disabled/bypassed): read full block from L1
        let mut full_block = vec![0u8; block_size];
        let file_offset = self.block_offset(index);
        let n = self.layer1.read(file_offset, &mut full_block)?;

        // Decrypt if encrypted
        if let Some(cipher) = &self.cipher {
            encryption::decrypt_block(cipher, index, &mut full_block);
        }

        // Copy requested sub-region to caller's buffer
        buf.copy_from_slice(&full_block[offset..offset + buf.len()]);

        // Cache the full block if we got a complete read
        if n == block_size {
            self.cache_put(index, full_block, cache);
        }

        Ok(buf.len())
    }

    fn write_block(
        &self,
        index: u64,
        offset: usize,
        buf: &[u8],
        cache: CacheMode,
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

        if let Some(cipher) = &self.cipher {
            // Encrypted path: XTS operates on full blocks, so partial writes
            // require read-modify-write.
            let mut full_block = if let Some(cached) = self.try_cache_peek_full_block(index, cache)
            {
                cached
            } else {
                // Read from L1 and decrypt
                let mut block = vec![0u8; block_size];
                let file_offset = self.block_offset(index);
                self.layer1.read(file_offset, &mut block)?;
                encryption::decrypt_block(cipher, index, &mut block);
                block
            };

            // Apply the partial write to plaintext
            full_block[offset..offset + buf.len()].copy_from_slice(buf);

            // Encrypt a copy and write to L1
            let mut encrypted = full_block.clone();
            encryption::encrypt_block(cipher, index, &mut encrypted);
            let file_offset = self.block_offset(index);
            self.layer1.write(file_offset, &encrypted)?;

            // Update cache with plaintext
            self.cache_put(index, full_block, cache);
        } else {
            // Unencrypted path: write partial data directly to L1
            let file_offset = self.block_offset(index) + offset as u64;
            self.layer1.write(file_offset, buf)?;

            // Update cache if block is already cached
            self.cache_update_in_place(index, offset, buf, cache);
        }

        Ok(buf.len())
    }

    fn read_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<usize, YakError> {
        if self.cipher.is_none() {
            // Unencrypted: single L1 read
            let file_offset = self.block_offset(start_index) + offset as u64;
            return self.layer1.read(file_offset, buf);
        }

        // Encrypted: read full blocks from L1, decrypt each, copy requested region
        let bs = self.block_size();
        let last_byte = offset + buf.len();
        let blocks_needed = last_byte.div_ceil(bs);

        let mut raw = vec![0u8; blocks_needed * bs];
        let file_offset = self.block_offset(start_index);
        self.layer1.read(file_offset, &mut raw)?;

        // Decrypt each block in place
        let cipher = self.cipher.as_ref().unwrap();
        encryption::decrypt_blocks(cipher, &mut raw, bs, start_index);

        buf.copy_from_slice(&raw[offset..offset + buf.len()]);
        Ok(buf.len())
    }

    fn write_contiguous_blocks(
        &self,
        start_index: u64,
        offset: usize,
        buf: &[u8],
    ) -> Result<usize, YakError> {
        if self.cipher.is_none() {
            // Unencrypted: single L1 write
            let file_offset = self.block_offset(start_index) + offset as u64;
            self.layer1.write(file_offset, buf)?;
            return Ok(buf.len());
        }

        let bs = self.block_size();
        let cipher = self.cipher.as_ref().unwrap();
        let last_byte = offset + buf.len();
        let blocks_needed = last_byte.div_ceil(bs);

        let mut raw = vec![0u8; blocks_needed * bs];

        // If partial first block or partial last block, read-modify-write
        let is_aligned = offset == 0 && buf.len().is_multiple_of(bs);
        if !is_aligned {
            let file_offset = self.block_offset(start_index);
            self.layer1.read(file_offset, &mut raw)?;
            encryption::decrypt_blocks(cipher, &mut raw, bs, start_index);
        }

        // Apply the write
        raw[offset..offset + buf.len()].copy_from_slice(buf);

        // Encrypt each block
        encryption::encrypt_blocks(cipher, &mut raw, bs, start_index);

        // Single L1 write
        let file_offset = self.block_offset(start_index);
        self.layer1.write(file_offset, &raw)?;
        Ok(buf.len())
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Map upper layer index to L1 slot index (+1 because slot 0 is L2's own)
        self.layer1.header_slot_for_upper(index + 1)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError> {
        self.layer1.write_header_slot(slot, data)
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError> {
        self.layer1.read_header_slot(slot)
    }

    fn invalidate_thread_local_cache(&self) {
        if CACHE_BUDGET_BYTES == 0 {
            return;
        }
        if let Some(tc) = self.block_cache.get() {
            tc.borrow_mut().clear();
        }
    }

    fn verify(&self, claimed_blocks: &[u64]) -> Result<Vec<String>, YakError> {
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
            if let Some(cipher) = &self.cipher {
                // Encrypted: read full block, decrypt, extract free-list pointer
                let mut full_block = vec![0u8; block_size as usize];
                self.layer1
                    .read(self.block_offset(current), &mut full_block)?;
                encryption::decrypt_block(cipher, current, &mut full_block);
                next_buf[..biw].copy_from_slice(&full_block[..biw]);
            } else {
                self.layer1
                    .read(self.block_offset(current), &mut next_buf[..biw])?;
            }
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
