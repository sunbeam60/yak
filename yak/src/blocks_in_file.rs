use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use rayon::prelude::*;
use thread_local::ThreadLocal;

use crate::block_layer::{BlockLayer, CacheMode};
use crate::encryption::{self, BlockCipher, EncryptionConfig};
use crate::file_layer::FileLayer;
use std::collections::VecDeque;

use crate::{HeaderSlotId, OpenMode, YakError};

// -------------------------------------------------------------------------
// Bootstrap header layout (written at MAGIC_SIZE, immutable after create)
// -------------------------------------------------------------------------
// | block_size_shift: u8 | block_index_width: u8 | encrypted: u8 |
// If encrypted == 1, encryption config follows (129 bytes).

/// Offset within the file where L2 bootstrap fields begin (after the 12-byte magic prefix).
const BOOTSTRAP_OFFSET: usize = 12; // MAGIC_SIZE

/// Bootstrap field offsets (relative to BOOTSTRAP_OFFSET).
const B_BSS_OFFSET: usize = 0;
const B_BIW_OFFSET: usize = B_BSS_OFFSET + 1;
const B_ENCRYPTED_FLAG_OFFSET: usize = B_BIW_OFFSET + 1;
const B_ENCRYPTION_CONFIG_OFFSET: usize = B_ENCRYPTED_FLAG_OFFSET + 1;

/// Bootstrap size for unencrypted files.
const BOOTSTRAP_SIZE_PLAIN: usize = B_ENCRYPTION_CONFIG_OFFSET; // = 3

/// Bootstrap size for encrypted files.
const BOOTSTRAP_SIZE_ENCRYPTED: usize = BOOTSTRAP_SIZE_PLAIN + encryption::ENCRYPTION_CONFIG_SIZE; // = 132

// -------------------------------------------------------------------------
// Block 0 (superblock) layout — encrypted like all other blocks
// -------------------------------------------------------------------------
// | "blk0": [u8;4] | version: u8 | free_list_head: u64 |
// | slot_count: u8 |
// For each slot: | payload_len: u16 | payload: [u8; payload_len] |
// ... zero-padded to block_size ...

/// Block 0 magic identifier.
const BLOCK0_MAGIC: &[u8; 4] = b"blk0";

/// Block 0 format version.
const BLOCK0_VERSION: u8 = 0;

/// Offset of free_list_head within block 0.
const B0_FREE_LIST_OFFSET: usize = 5; // after "blk0"(4) + version(1)

/// Offset of slot_count within block 0.
const B0_SLOT_COUNT_OFFSET: usize = B0_FREE_LIST_OFFSET + 8; // = 13

/// Offset where slot data begins within block 0.
const B0_SLOTS_OFFSET: usize = B0_SLOT_COUNT_OFFSET + 1; // = 14

/// Minimum block_size_shift enforced at create time.
/// 512 bytes minimum gives comfortable room for the superblock.
const MIN_BLOCK_SIZE_SHIFT: u8 = 9;

/// Default memory budget (in bytes) for the per-thread block cache.
/// Each thread gets its own independent LRU cache up to this size.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 2 * 1024 * 1024;

/// Maximum entry count for the per-thread block cache.
/// Prevents excessive LRU overhead when block sizes are very small.
const BLOCK_CACHE_MAX_ENTRIES: usize = 4096;

/// Information about one slot within block 0.
struct Block0SlotInfo {
    /// Byte offset within block 0 where this slot's payload begins.
    offset: usize,
    /// Expected payload length.
    payload_len: u16,
}

/// Bookkeeping state protected by a Mutex.
struct BlocksInFileState {
    total_blocks: u64,
    free_list_head: u64,
    /// In-memory plaintext copy of block 0.
    block0_cache: Vec<u8>,
}

/// Real L2 implementation that stores blocks within a single file managed by L1.
///
/// Block `n` is at file offset `data_offset + n * block_size`. Block 0 is
/// the superblock: it holds all mutable metadata (free_list_head, L3/L4
/// headers). The bootstrap header before data_offset is immutable after
/// creation and stores only block_size_shift, block_index_width, and
/// encryption config.
///
/// New blocks are zeroed on allocation. Freed blocks form a singly-linked
/// free list: the first `block_index_width` bytes of a free block contain
/// the next free block ID (or sentinel for end of list). `free_list_head`
/// is stored in block 0.
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
    /// Slot registry for block 0 (offsets and lengths within the superblock).
    /// Slot 0 is reserved for L2's own mutable state (free_list_head).
    /// Upper layer slots start at index 1.
    block0_slots: Vec<Block0SlotInfo>,
    /// Per-thread LRU cache of full **decrypted** block contents (user streams).
    block_cache: ThreadLocal<RefCell<LruCache<u64, Vec<u8>>>>,
    /// Shared LRU cache of full **decrypted** block contents (Streams stream).
    shared_cache: Mutex<LruCache<u64, Vec<u8>>>,
    /// Precomputed cache capacity (entry count).
    cache_capacity: NonZeroUsize,
    /// Runtime cipher for block encryption/decryption. None = unencrypted.
    cipher: Option<BlockCipher>,
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

    fn block_size(&self) -> usize {
        1 << self.block_size_shift
    }

    /// Compute the data_offset (byte offset where blocks begin) from block_size_shift
    /// and encryption config presence.
    fn compute_data_offset(_block_size_shift: u8, encrypted: bool) -> u64 {
        let bootstrap_size = if encrypted {
            BOOTSTRAP_SIZE_ENCRYPTED
        } else {
            BOOTSTRAP_SIZE_PLAIN
        };
        // data_offset = MAGIC_SIZE + bootstrap_size, but rounded up to block alignment
        // for clean block 0 positioning. Actually, no need to align — block 0 starts
        // at data_offset regardless. Keep it tight.
        (BOOTSTRAP_OFFSET + bootstrap_size) as u64
    }

    /// Serialize the bootstrap fields (written once at create, never modified).
    fn serialize_bootstrap(
        block_size_shift: u8,
        block_index_width: u8,
        encryption_config: Option<&EncryptionConfig>,
    ) -> Vec<u8> {
        let size = if encryption_config.is_some() {
            BOOTSTRAP_SIZE_ENCRYPTED
        } else {
            BOOTSTRAP_SIZE_PLAIN
        };
        let mut buf = Vec::with_capacity(size);
        buf.push(block_size_shift);
        buf.push(block_index_width);
        if let Some(config) = encryption_config {
            buf.push(1); // encrypted_flag = 1
            buf.extend_from_slice(&encryption::serialize_config(config));
        } else {
            buf.push(0); // encrypted_flag = 0
        }
        buf
    }

    /// Create the initial block 0 content with sentinel free_list_head and
    /// zeroed upper-layer slots.
    fn init_block0(
        block_size: usize,
        sentinel: u64,
        upper_slot_sizes: &[u16],
    ) -> (Vec<u8>, Vec<Block0SlotInfo>) {
        let mut block0 = vec![0u8; block_size];

        // Magic and version
        block0[0..4].copy_from_slice(BLOCK0_MAGIC);
        block0[4] = BLOCK0_VERSION;

        // free_list_head = sentinel (empty free list)
        block0[B0_FREE_LIST_OFFSET..B0_FREE_LIST_OFFSET + 8]
            .copy_from_slice(&sentinel.to_le_bytes());

        // Slot count (upper layer slots only — free_list_head is at a fixed offset)
        let slot_count = upper_slot_sizes.len() as u8;
        block0[B0_SLOT_COUNT_OFFSET] = slot_count;

        // Build slot registry and write length prefixes
        let mut slots = Vec::with_capacity(upper_slot_sizes.len());
        let mut offset = B0_SLOTS_OFFSET;
        for &payload_len in upper_slot_sizes {
            // Write length prefix
            block0[offset..offset + 2].copy_from_slice(&payload_len.to_le_bytes());
            offset += 2;

            // Record slot info (payload starts after length prefix)
            slots.push(Block0SlotInfo {
                offset,
                payload_len,
            });

            // Payload is zeroed (Vec is zero-initialized)
            offset += payload_len as usize;
        }

        (block0, slots)
    }

    /// Parse block 0 content, extracting free_list_head and building the slot registry.
    fn parse_block0(block0: &[u8]) -> Result<(u64, Vec<Block0SlotInfo>), YakError> {
        if block0.len() < B0_SLOTS_OFFSET {
            return Err(YakError::IoError("block 0 too small".to_string()));
        }

        // Validate magic
        if &block0[0..4] != BLOCK0_MAGIC {
            return Err(YakError::IoError(format!(
                "block 0 magic mismatch: expected 'blk0', got '{}'",
                String::from_utf8_lossy(&block0[0..4])
            )));
        }

        // Validate version
        if block0[4] != BLOCK0_VERSION {
            return Err(YakError::IoError(format!(
                "unsupported block 0 version: {}",
                block0[4]
            )));
        }

        // Extract free_list_head
        let free_list_head = u64::from_le_bytes(
            block0[B0_FREE_LIST_OFFSET..B0_FREE_LIST_OFFSET + 8]
                .try_into()
                .unwrap(),
        );

        // Parse slot registry
        let slot_count = block0[B0_SLOT_COUNT_OFFSET] as usize;
        let mut slots = Vec::with_capacity(slot_count);
        let mut offset = B0_SLOTS_OFFSET;

        for _ in 0..slot_count {
            if offset + 2 > block0.len() {
                return Err(YakError::IoError(
                    "block 0 slot registry truncated".to_string(),
                ));
            }
            let payload_len = u16::from_le_bytes([block0[offset], block0[offset + 1]]);
            offset += 2;

            if offset + payload_len as usize > block0.len() {
                return Err(YakError::IoError(
                    "block 0 slot payload extends beyond block".to_string(),
                ));
            }

            slots.push(Block0SlotInfo {
                offset,
                payload_len,
            });
            offset += payload_len as usize;
        }

        Ok((free_list_head, slots))
    }

    /// Flush block 0 from in-memory cache to disk (encrypting if needed).
    /// Must be called with the state mutex held (caller passes the cache contents).
    fn flush_block0(&self, block0: &[u8]) -> Result<(), YakError> {
        let file_offset = self.layer1.data_offset(); // block 0 is at data_offset

        if let Some(cipher) = &self.cipher {
            let mut encrypted = block0.to_vec();
            encryption::encrypt_block(cipher, 0, &mut encrypted);
            self.layer1.write(file_offset, &encrypted)
        } else {
            self.layer1.write(file_offset, block0)
        }
    }

    /// Persist free_list_head into the in-memory block 0 cache and flush to disk.
    /// Called while state mutex is held.
    fn persist_l2_header(&self, state: &mut BlocksInFileState) -> Result<(), YakError> {
        // Update free_list_head in the cached block 0
        state.block0_cache[B0_FREE_LIST_OFFSET..B0_FREE_LIST_OFFSET + 8]
            .copy_from_slice(&state.free_list_head.to_le_bytes());

        self.flush_block0(&state.block0_cache)
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
        slot_sizes: VecDeque<u16>,
        password: Option<&[u8]>,
    ) -> Result<Self, YakError> {
        // Enforce minimum block size for superblock
        if block_size_shift < MIN_BLOCK_SIZE_SHIFT {
            return Err(YakError::IoError(format!(
                "block_size_shift must be >= {} (minimum {} bytes for superblock)",
                MIN_BLOCK_SIZE_SHIFT,
                1u64 << MIN_BLOCK_SIZE_SHIFT
            )));
        }

        let sentinel = block_sentinel(block_index_width);

        // Set up encryption if a password is provided
        let (cipher, encryption_config) = if let Some(pw) = password {
            let (config, cipher) = encryption::create_encryption(pw)?;
            (Some(cipher), Some(config))
        } else {
            (None, None)
        };

        // Compute data_offset and create L1
        let data_offset = Self::compute_data_offset(block_size_shift, encryption_config.is_some());
        let layer1 = L1::create(path, data_offset)?;

        // Write bootstrap fields after magic prefix
        let bootstrap = Self::serialize_bootstrap(
            block_size_shift,
            block_index_width,
            encryption_config.as_ref(),
        );
        layer1.write(BOOTSTRAP_OFFSET as u64, &bootstrap)?;

        // Consume slot_sizes from upper layers for block 0 layout
        let upper_slot_sizes: Vec<u16> = slot_sizes.into_iter().collect();
        let block_size = 1usize << block_size_shift;

        // Initialize block 0 (superblock)
        let (block0, block0_slots) = Self::init_block0(block_size, sentinel, &upper_slot_sizes);

        // Grow file to include block 0
        let file_len = data_offset + block_size as u64;
        layer1.set_len(file_len)?;

        // Write block 0 to disk (encrypting if needed)
        let file_offset = data_offset;
        if let Some(ref cipher) = cipher {
            let mut encrypted = block0.clone();
            encryption::encrypt_block(cipher, 0, &mut encrypted);
            layer1.write(file_offset, &encrypted)?;
        } else {
            layer1.write(file_offset, &block0)?;
        }

        let cache_capacity = Self::compute_cache_capacity(block_size_shift);
        Ok(BlocksInFile {
            layer1,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks: 1, // block 0 exists
                free_list_head: sentinel,
                block0_cache: block0,
            }),
            block0_slots,
            block_cache: ThreadLocal::new(),
            shared_cache: Mutex::new(LruCache::new(cache_capacity)),
            cache_capacity,
            cipher,
        })
    }

    fn open(path: &str, mode: OpenMode, password: Option<&[u8]>) -> Result<Self, YakError> {
        let layer1 = L1::open(path, mode)?;

        // Read bootstrap fields from after magic prefix
        let mut bootstrap_buf = [0u8; BOOTSTRAP_SIZE_ENCRYPTED]; // max size
        let bootstrap_read_len = BOOTSTRAP_SIZE_PLAIN; // read minimum first
        layer1.read(
            BOOTSTRAP_OFFSET as u64,
            &mut bootstrap_buf[..bootstrap_read_len],
        )?;

        let block_size_shift = bootstrap_buf[B_BSS_OFFSET];
        let block_index_width = bootstrap_buf[B_BIW_OFFSET];
        let encrypted_flag = bootstrap_buf[B_ENCRYPTED_FLAG_OFFSET];

        // If encrypted, read the encryption config too
        let cipher = if encrypted_flag == 1 {
            // Read the rest of the bootstrap (encryption config)
            layer1.read(
                (BOOTSTRAP_OFFSET + B_ENCRYPTION_CONFIG_OFFSET) as u64,
                &mut bootstrap_buf[B_ENCRYPTION_CONFIG_OFFSET
                    ..B_ENCRYPTION_CONFIG_OFFSET + encryption::ENCRYPTION_CONFIG_SIZE],
            )?;

            let config = encryption::deserialize_config(
                &bootstrap_buf[B_ENCRYPTION_CONFIG_OFFSET
                    ..B_ENCRYPTION_CONFIG_OFFSET + encryption::ENCRYPTION_CONFIG_SIZE],
            )?;
            let pw = password.ok_or_else(|| {
                YakError::EncryptionRequired(
                    "file is encrypted but no password was provided".to_string(),
                )
            })?;
            Some(encryption::open_encryption(&config, pw)?)
        } else {
            if password.is_some() {
                return Err(YakError::IoError(
                    "password provided but file is not encrypted".to_string(),
                ));
            }
            None
        };

        // Derive total_blocks from file size
        let block_size = 1u64 << block_size_shift;
        let total_blocks = (layer1.len()? - layer1.data_offset()) / block_size;

        // Read and parse block 0
        let mut block0 = vec![0u8; block_size as usize];
        layer1.read(layer1.data_offset(), &mut block0)?;

        if let Some(ref cipher) = cipher {
            encryption::decrypt_block(cipher, 0, &mut block0);
        }

        let (free_list_head, block0_slots) = Self::parse_block0(&block0)?;

        let cache_capacity = Self::compute_cache_capacity(block_size_shift);
        Ok(BlocksInFile {
            layer1,
            block_size_shift,
            block_index_width,
            state: Mutex::new(BlocksInFileState {
                total_blocks,
                free_list_head,
                block0_cache: block0,
            }),
            block0_slots,
            block_cache: ThreadLocal::new(),
            shared_cache: Mutex::new(LruCache::new(cache_capacity)),
            cache_capacity,
            cipher,
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
                // Encrypted: encrypt zeros in parallel, then write sequentially to L1
                let encrypted_zeros: Vec<Vec<u8>> = result[..free_list_count]
                    .par_iter()
                    .map(|&block_id| {
                        let mut zeros = vec![0u8; block_size];
                        encryption::encrypt_block(cipher, block_id, &mut zeros);
                        zeros
                    })
                    .collect();
                for (&block_id, zeros) in result[..free_list_count].iter().zip(encrypted_zeros) {
                    self.layer1.write(self.block_offset(block_id), &zeros)?;
                }
            } else {
                let zeros = vec![0u8; block_size];
                for &block_id in &result[..free_list_count] {
                    self.layer1.write(self.block_offset(block_id), &zeros)?;
                }
            }
        }

        // Phase 4: persist L2 header (block 0) before releasing the mutex
        self.persist_l2_header(&mut state)?;
        drop(state);

        // Phase 5: evict allocated blocks from both caches.
        for &block_id in &result {
            self.evict_from_all_caches(block_id);
        }

        Ok(result)
    }

    fn deallocate_block(&self, index: u64) -> Result<(), YakError> {
        // Block 0 (superblock) must never be deallocated
        if index == 0 {
            return Err(YakError::IoError(
                "cannot deallocate block 0 (superblock)".to_string(),
            ));
        }

        let mut state = self.state.lock().unwrap();

        // Write the current free_list_head into the first block_index_width bytes of this block
        let biw = self.block_index_width as usize;
        let head_bytes = state.free_list_head.to_le_bytes();
        if let Some(cipher) = &self.cipher {
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

        // Persist updated block 0 before releasing the mutex
        self.persist_l2_header(&mut state)?;
        drop(state);

        // Evict deallocated block from both caches.
        self.evict_from_all_caches(index);

        Ok(())
    }

    fn deallocate_blocks(&self, indices: &mut Vec<u64>) -> Result<(), YakError> {
        if indices.is_empty() {
            return Ok(());
        }

        // Block 0 must never be deallocated
        if indices.contains(&0) {
            return Err(YakError::IoError(
                "cannot deallocate block 0 (superblock)".to_string(),
            ));
        }

        // Sort ascending so the free-list chain is written in block order.
        indices.sort_unstable();

        let mut state = self.state.lock().unwrap();
        let biw = self.block_index_width as usize;

        // Chain: indices[0] → indices[1] → … → indices[n-1] → old free_list_head
        if let Some(cipher) = &self.cipher {
            let block_size = self.block_size();
            let mut blocks: Vec<(u64, Vec<u8>)> = Vec::with_capacity(indices.len());
            for i in 0..indices.len() - 1 {
                let mut block = vec![0u8; block_size];
                let next = indices[i + 1].to_le_bytes();
                block[..biw].copy_from_slice(&next[..biw]);
                blocks.push((indices[i], block));
            }
            // Last freed block points to the previous head
            let mut last_block = vec![0u8; block_size];
            let head_bytes = state.free_list_head.to_le_bytes();
            last_block[..biw].copy_from_slice(&head_bytes[..biw]);
            blocks.push((*indices.last().unwrap(), last_block));

            // Encrypt all blocks in parallel
            blocks.par_iter_mut().for_each(|(block_id, block)| {
                encryption::encrypt_block(cipher, *block_id, block);
            });

            // Write sequentially to L1
            for (block_id, block) in &blocks {
                self.layer1.write(self.block_offset(*block_id), block)?;
            }
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

        // Single header persist
        self.persist_l2_header(&mut state)?;
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

        // Cache miss: read full block from L1
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
            let mut full_block = if let Some(cached) = self.try_cache_peek_full_block(index, cache)
            {
                cached
            } else {
                let mut block = vec![0u8; block_size];
                let file_offset = self.block_offset(index);
                self.layer1.read(file_offset, &mut block)?;
                encryption::decrypt_block(cipher, index, &mut block);
                block
            };

            full_block[offset..offset + buf.len()].copy_from_slice(buf);

            let mut encrypted = full_block.clone();
            encryption::encrypt_block(cipher, index, &mut encrypted);
            let file_offset = self.block_offset(index);
            self.layer1.write(file_offset, &encrypted)?;

            self.cache_put(index, full_block, cache);
        } else {
            let file_offset = self.block_offset(index) + offset as u64;
            self.layer1.write(file_offset, buf)?;

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
            let file_offset = self.block_offset(start_index) + offset as u64;
            return self.layer1.read(file_offset, buf);
        }

        let bs = self.block_size();
        let last_byte = offset + buf.len();
        let blocks_needed = last_byte.div_ceil(bs);

        let mut raw = vec![0u8; blocks_needed * bs];
        let file_offset = self.block_offset(start_index);
        self.layer1.read(file_offset, &mut raw)?;

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
            let file_offset = self.block_offset(start_index) + offset as u64;
            self.layer1.write(file_offset, buf)?;
            return Ok(buf.len());
        }

        let bs = self.block_size();
        let cipher = self.cipher.as_ref().unwrap();
        let last_byte = offset + buf.len();
        let blocks_needed = last_byte.div_ceil(bs);

        let mut raw = vec![0u8; blocks_needed * bs];

        let is_aligned = offset == 0 && buf.len().is_multiple_of(bs);
        if !is_aligned {
            let file_offset = self.block_offset(start_index);
            self.layer1.read(file_offset, &mut raw)?;
            encryption::decrypt_blocks(cipher, &mut raw, bs, start_index);
        }

        raw[offset..offset + buf.len()].copy_from_slice(buf);

        encryption::encrypt_blocks(cipher, &mut raw, bs, start_index);

        let file_offset = self.block_offset(start_index);
        self.layer1.write(file_offset, &raw)?;
        Ok(buf.len())
    }

    fn header_slot_for_upper(&self, index: u8) -> HeaderSlotId {
        // Upper layer index 0 = block0_slots[0] (L3), index 1 = block0_slots[1] (L4), etc.
        HeaderSlotId(index)
    }

    fn write_header_slot(&self, slot: HeaderSlotId, data: &[u8]) -> Result<(), YakError> {
        let idx = slot.0 as usize;
        if idx >= self.block0_slots.len() {
            return Err(YakError::IoError(format!(
                "header slot index {} out of range (have {} slots in block 0)",
                idx,
                self.block0_slots.len()
            )));
        }
        let info = &self.block0_slots[idx];
        if data.len() != info.payload_len as usize {
            return Err(YakError::IoError(format!(
                "header slot {}: expected {} bytes, got {}",
                idx,
                info.payload_len,
                data.len()
            )));
        }

        let mut state = self.state.lock().unwrap();
        // Write into the in-memory block 0 cache
        state.block0_cache[info.offset..info.offset + data.len()].copy_from_slice(data);
        // Flush to disk
        self.flush_block0(&state.block0_cache)
    }

    fn read_header_slot(&self, slot: HeaderSlotId) -> Result<Vec<u8>, YakError> {
        let idx = slot.0 as usize;
        if idx >= self.block0_slots.len() {
            return Err(YakError::IoError(format!(
                "header slot index {} out of range (have {} slots in block 0)",
                idx,
                self.block0_slots.len()
            )));
        }
        let info = &self.block0_slots[idx];

        let state = self.state.lock().unwrap();
        Ok(state.block0_cache[info.offset..info.offset + info.payload_len as usize].to_vec())
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
            if current == 0 {
                issues.push("L2: block 0 (superblock) found on free list".to_string());
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

        // 5. Build claimed set — include block 0 (superblock) as always claimed
        let mut claimed_set: std::collections::HashSet<u64> =
            claimed_blocks.iter().cloned().collect();
        claimed_set.insert(0); // block 0 is always reserved

        for &block_id in claimed_blocks {
            if block_id >= total_blocks {
                issues.push(format!(
                    "L2: claimed block {} is >= total_blocks ({})",
                    block_id, total_blocks
                ));
            }
        }

        // 6. Check for duplicate claims (a block claimed by two streams)
        if claimed_set.len() != claimed_blocks.len() + 1 {
            // +1 for block 0 we inserted
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

        // 9. Verify block 0 internal consistency
        {
            let state = self.state.lock().unwrap();
            if let Err(e) = Self::parse_block0(&state.block0_cache) {
                issues.push(format!("L2: block 0 parse error: {}", e));
            }
        }

        Ok(issues)
    }
}
