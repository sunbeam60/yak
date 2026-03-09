use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use rayon::prelude::*;
use thread_local::ThreadLocal;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::block_layer::{BlockLayer, CacheMode, BLOCK_INDEX_SENTINEL};
use crate::encryption::{self, BlockCipher, EncryptionConfig};
use crate::file_layer::FileLayer;
use std::collections::VecDeque;

use crate::{AdminSlotId, OpenMode, YakError};

// -------------------------------------------------------------------------
// Bootstrap header layout (written at MAGIC_SIZE, immutable after create)
// -------------------------------------------------------------------------
// | block_size_shift: u8 | encrypted: u8 |
// If encrypted == 1, encryption config follows (129 bytes).

/// Offset within the file where L2 bootstrap fields begin (after the 12-byte magic prefix).
const BOOTSTRAP_OFFSET: usize = 12; // MAGIC_SIZE

/// Bootstrap field offsets (relative to BOOTSTRAP_OFFSET).
const B_BSS_OFFSET: usize = 0;
const B_ENCRYPTED_FLAG_OFFSET: usize = B_BSS_OFFSET + 1;
const B_ENCRYPTION_CONFIG_OFFSET: usize = B_ENCRYPTED_FLAG_OFFSET + 1;

/// Bootstrap size for unencrypted files.
const BOOTSTRAP_SIZE_PLAIN: usize = B_ENCRYPTION_CONFIG_OFFSET; // = 2

/// Bootstrap size for encrypted files.
const BOOTSTRAP_SIZE_ENCRYPTED: usize = BOOTSTRAP_SIZE_PLAIN + encryption::ENCRYPTION_CONFIG_SIZE; // = 131

// -------------------------------------------------------------------------
// Block 0 (superblock) layout — encrypted like all other blocks
// -------------------------------------------------------------------------
// Defined by the Block0Header zerocopy struct below.
// After the header: for each slot: | payload_len: u16 | payload: [u8] |
// ... zero-padded to block_size ...

/// Block 0 magic identifier.
const BLOCK0_MAGIC: &[u8; 4] = b"blk0";

/// Block 0 format version.
const BLOCK0_VERSION: u8 = 1;

/// Block 0 fixed header (zerocopy packed struct).
/// Total: 4 + 1 + 4 + 1 = 10 bytes.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C, packed)]
struct Block0Header {
    magic: [u8; 4],
    version: u8,
    free_list_head: U32,
    slot_count: u8,
}

const _: () = assert!(size_of::<Block0Header>() == 10);

/// Offset where slot data begins within block 0.
const B0_SLOTS_OFFSET: usize = size_of::<Block0Header>();

// -------------------------------------------------------------------------
// Trunk-and-leaf free list structures
// -------------------------------------------------------------------------
// Trunk layout: [next_trunk: u32][TrunkExtent; N]
// where N = (block_size - 4) / 8.

/// A single extent in a trunk block: (start block, length in blocks).
/// Unused slots have start == BLOCK_INDEX_SENTINEL.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Clone, Copy)]
#[repr(C, packed)]
struct TrunkExtent {
    start: U32,
    length: U32,
}

const _: () = assert!(size_of::<TrunkExtent>() == 8);

/// Size of the next-trunk pointer at the start of each trunk block.
const TRUNK_NEXT_SIZE: usize = 4;

/// Minimum block_size_shift enforced at create time.
/// 512 bytes minimum gives comfortable room for the superblock.
const MIN_BLOCK_SIZE_SHIFT: u8 = 9;

/// Default memory budget (in bytes) for the per-thread block cache.
/// Each thread gets its own independent LRU cache up to this size.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 2 * 1024 * 1024;

/// Maximum entry count for the per-thread block cache.
/// Prevents excessive LRU overhead when block sizes are very small.
const BLOCK_CACHE_MAX_ENTRIES: usize = 4096;

/// Information about one admin slot in the block 0 slot registry.
struct Block0SlotInfo {
    /// Byte offset within the decrypted block 0 data where this slot's payload begins.
    offset: usize,
    /// Expected payload length.
    payload_len: u16,
}

/// Mutable bookkeeping state (behind a Mutex).
struct BlocksInFileState {
    /// Total number of blocks (including block 0).
    total_blocks: u64,
    /// Head of the trunk-based free list (BLOCK_INDEX_SENTINEL = empty).
    free_list_head: u32,
    /// In-memory copy of block 0, always decrypted.
    block0_cache: Vec<u8>,
}

/// L2 implementation: blocks stored in a single contiguous file.
///
/// Uses a trunk-and-leaf free list: each trunk block stores a next-trunk
/// pointer (u32) followed by an array of (start, length) extents describing
/// free blocks. Trunk blocks themselves are implicitly free — reclaimed
/// when all their extents are consumed.
///
/// Block indices are always u32 (4 bytes). Sentinel = u32::MAX.
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
    state: Mutex<BlocksInFileState>,
    /// Slot registry for block 0 (offsets and lengths within the superblock).
    block0_slots: Vec<Block0SlotInfo>,
    /// Per-thread LRU cache of full **decrypted** block contents (user streams).
    block_cache: ThreadLocal<RefCell<LruCache<u32, Vec<u8>>>>,
    /// Shared LRU cache of full **decrypted** block contents (Streams stream).
    shared_cache: Mutex<LruCache<u32, Vec<u8>>>,
    /// Precomputed cache capacity (entry count).
    cache_capacity: NonZeroUsize,
    /// Runtime cipher for block encryption/decryption. None = unencrypted.
    cipher: Option<BlockCipher>,
}

impl<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> BlocksInFile<L1, CACHE_BUDGET_BYTES> {
    /// File offset of block `n`.
    fn block_offset(&self, index: u32) -> u64 {
        self.layer1.data_offset() + (index as u64) * self.block_size() as u64
    }

    /// File offset of block `n` (u64 variant for total_blocks).
    fn block_offset_u64(&self, index: u64) -> u64 {
        self.layer1.data_offset() + index * self.block_size() as u64
    }

    fn block_size(&self) -> usize {
        1 << self.block_size_shift
    }

    // -----------------------------------------------------------------
    // Trunk-and-leaf free list helpers
    // -----------------------------------------------------------------

    /// Number of extent slots in one trunk block.
    fn trunk_extent_count(&self) -> usize {
        (self.block_size() - TRUNK_NEXT_SIZE) / size_of::<TrunkExtent>()
    }

    /// Read the next-trunk pointer from a trunk block buffer.
    fn read_trunk_next(trunk: &[u8]) -> u32 {
        u32::from_le_bytes(trunk[..4].try_into().unwrap())
    }

    /// Write the next-trunk pointer into a trunk block buffer.
    fn write_trunk_next(trunk: &mut [u8], next: u32) {
        trunk[..4].copy_from_slice(&next.to_le_bytes());
    }

    /// Get a reference to the extent at slot `i` in a trunk block buffer.
    fn trunk_extent(trunk: &[u8], i: usize) -> TrunkExtent {
        let offset = TRUNK_NEXT_SIZE + i * size_of::<TrunkExtent>();
        *TrunkExtent::ref_from_bytes(&trunk[offset..offset + size_of::<TrunkExtent>()])
            .expect("trunk extent alignment")
    }

    /// Write an extent at slot `i` in a trunk block buffer.
    fn write_trunk_extent(trunk: &mut [u8], i: usize, extent: TrunkExtent) {
        let offset = TRUNK_NEXT_SIZE + i * size_of::<TrunkExtent>();
        trunk[offset..offset + size_of::<TrunkExtent>()].copy_from_slice(extent.as_bytes());
    }

    /// Read a full block from L1, decrypting if needed. No caching.
    fn read_raw_block(&self, index: u32) -> Result<Vec<u8>, YakError> {
        let block_size = self.block_size();
        let mut block = vec![0u8; block_size];
        self.layer1.read(self.block_offset(index), &mut block)?;
        if let Some(cipher) = &self.cipher {
            encryption::decrypt_block(cipher, index as u64, &mut block);
        }
        Ok(block)
    }

    /// Write a full block to L1, encrypting if needed. No caching.
    fn write_raw_block(&self, index: u32, data: &[u8]) -> Result<(), YakError> {
        if let Some(cipher) = &self.cipher {
            let mut encrypted = data.to_vec();
            encryption::encrypt_block(cipher, index as u64, &mut encrypted);
            self.layer1.write(self.block_offset(index), &encrypted)
        } else {
            self.layer1.write(self.block_offset(index), data)
        }
    }

    /// Initialize a trunk block buffer: next_trunk pointer + all extent slots as sentinel.
    fn init_trunk_block(&self, next_trunk: u32) -> Vec<u8> {
        let block_size = self.block_size();
        let extent_count = self.trunk_extent_count();
        let mut buf = vec![0u8; block_size];
        Self::write_trunk_next(&mut buf, next_trunk);
        let empty_extent = TrunkExtent {
            start: U32::new(BLOCK_INDEX_SENTINEL),
            length: U32::new(0),
        };
        for i in 0..extent_count {
            Self::write_trunk_extent(&mut buf, i, empty_extent);
        }
        buf
    }

    /// Check whether all extent slots in a trunk are sentinel (empty trunk).
    fn trunk_is_empty(&self, trunk: &[u8]) -> bool {
        let extent_count = self.trunk_extent_count();
        (0..extent_count).all(|i| Self::trunk_extent(trunk, i).start.get() == BLOCK_INDEX_SENTINEL)
    }

    /// Coalesce sorted block indices into contiguous extents (start, length).
    fn coalesce_extents(sorted_indices: &[u32]) -> Vec<(u32, u32)> {
        if sorted_indices.is_empty() {
            return Vec::new();
        }
        let mut extents = Vec::with_capacity(sorted_indices.len());
        let mut start = sorted_indices[0];
        let mut len = 1u32;
        for &idx in &sorted_indices[1..] {
            if idx == start + len {
                len += 1;
            } else {
                extents.push((start, len));
                start = idx;
                len = 1;
            }
        }
        extents.push((start, len));
        extents
    }

    /// Compute the data_offset (byte offset where blocks begin) from encryption config presence.
    fn compute_data_offset(_block_size_shift: u8, encrypted: bool) -> u64 {
        let bootstrap_size = if encrypted {
            BOOTSTRAP_SIZE_ENCRYPTED
        } else {
            BOOTSTRAP_SIZE_PLAIN
        };
        (BOOTSTRAP_OFFSET + bootstrap_size) as u64
    }

    /// Serialize the bootstrap fields (written once at create, never modified).
    fn serialize_bootstrap(
        block_size_shift: u8,
        encryption_config: Option<&EncryptionConfig>,
    ) -> Vec<u8> {
        let size = if encryption_config.is_some() {
            BOOTSTRAP_SIZE_ENCRYPTED
        } else {
            BOOTSTRAP_SIZE_PLAIN
        };
        let mut buf = Vec::with_capacity(size);
        buf.push(block_size_shift);
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
    fn init_block0(block_size: usize, upper_slot_sizes: &[u16]) -> (Vec<u8>, Vec<Block0SlotInfo>) {
        let mut block0 = vec![0u8; block_size];

        // Write the fixed header via zerocopy struct
        let header = Block0Header {
            magic: *BLOCK0_MAGIC,
            version: BLOCK0_VERSION,
            free_list_head: U32::new(BLOCK_INDEX_SENTINEL),
            slot_count: upper_slot_sizes.len() as u8,
        };
        block0[..size_of::<Block0Header>()].copy_from_slice(header.as_bytes());

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
    fn parse_block0(block0: &[u8]) -> Result<(u32, Vec<Block0SlotInfo>), YakError> {
        if block0.len() < size_of::<Block0Header>() {
            return Err(YakError::IoError("block 0 too small".to_string()));
        }

        // Read fixed header via zerocopy
        let header = Block0Header::ref_from_bytes(&block0[..size_of::<Block0Header>()])
            .map_err(|e| YakError::IoError(format!("block 0 header parse error: {}", e)))?;

        // Validate magic
        if &header.magic != BLOCK0_MAGIC {
            return Err(YakError::IoError(format!(
                "block 0 magic mismatch: expected 'blk0', got '{}'",
                String::from_utf8_lossy(&header.magic)
            )));
        }

        // Validate version
        if header.version != BLOCK0_VERSION {
            return Err(YakError::IoError(format!(
                "unsupported block 0 version: {}",
                header.version
            )));
        }

        let free_list_head = header.free_list_head.get();

        // Parse slot registry
        let slot_count = header.slot_count as usize;
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
        // Update free_list_head in the cached block 0 via zerocopy header
        let header =
            Block0Header::mut_from_bytes(&mut state.block0_cache[..size_of::<Block0Header>()])
                .expect("block0 header size");
        header.free_list_head = U32::new(state.free_list_head);

        self.flush_block0(&state.block0_cache)
    }

    /// Get or create the calling thread's block cache.
    fn thread_cache(&self) -> &RefCell<LruCache<u32, Vec<u8>>> {
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
        index: u32,
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
    fn cache_put(&self, index: u32, full_block: Vec<u8>, cache: CacheMode) {
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
    fn cache_update_in_place(&self, index: u32, offset: usize, buf: &[u8], cache: CacheMode) {
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
    fn try_cache_peek_full_block(&self, index: u32, cache: CacheMode) -> Option<Vec<u8>> {
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
    fn evict_from_all_caches(&self, index: u32) {
        if CACHE_BUDGET_BYTES == 0 {
            return;
        }
        if let Some(tc) = self.block_cache.get() {
            tc.borrow_mut().pop(&index);
        }
        self.shared_cache.lock().unwrap().pop(&index);
    }
}

impl<L1: FileLayer, const CACHE_BUDGET_BYTES: usize> BlockLayer
    for BlocksInFile<L1, CACHE_BUDGET_BYTES>
{
    fn create(
        path: &str,
        block_size_shift: u8,
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
        let bootstrap = Self::serialize_bootstrap(block_size_shift, encryption_config.as_ref());
        layer1.write(BOOTSTRAP_OFFSET as u64, &bootstrap)?;

        // Consume slot_sizes from upper layers for block 0 layout
        let upper_slot_sizes: Vec<u16> = slot_sizes.into_iter().collect();
        let block_size = 1usize << block_size_shift;

        // Initialize block 0 (superblock)
        let (block0, block0_slots) = Self::init_block0(block_size, &upper_slot_sizes);

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
            state: Mutex::new(BlocksInFileState {
                total_blocks: 1, // block 0 exists
                free_list_head: BLOCK_INDEX_SENTINEL,
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

    fn allocate_block(&self) -> Result<u32, YakError> {
        let blocks = self.allocate_blocks(1)?;
        Ok(blocks[0])
    }

    fn allocate_blocks(&self, count: u32) -> Result<Vec<u32>, YakError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let mut state = self.state.lock().unwrap();
        let block_size = self.block_size();
        let extent_count = self.trunk_extent_count();
        let mut result = Vec::with_capacity(count as usize);

        // Phase 1: drain extents from trunk blocks on the free list.
        while (result.len() as u32) < count && state.free_list_head != BLOCK_INDEX_SENTINEL {
            let trunk_index = state.free_list_head;
            let mut trunk = self.read_raw_block(trunk_index)?;
            let next_trunk = Self::read_trunk_next(&trunk);
            let mut trunk_modified = false;

            for i in 0..extent_count {
                if (result.len() as u32) >= count {
                    break;
                }
                let ext = Self::trunk_extent(&trunk, i);
                let ext_start = ext.start.get();
                if ext_start == BLOCK_INDEX_SENTINEL {
                    continue;
                }

                let ext_len = ext.length.get();
                let needed = count - result.len() as u32;

                if ext_len <= needed {
                    // Take entire extent
                    for j in 0..ext_len {
                        result.push(ext_start + j);
                    }
                    Self::write_trunk_extent(
                        &mut trunk,
                        i,
                        TrunkExtent {
                            start: U32::new(BLOCK_INDEX_SENTINEL),
                            length: U32::new(0),
                        },
                    );
                } else {
                    // Take `needed` blocks from the end of the extent
                    let new_len = ext_len - needed;
                    for j in 0..needed {
                        result.push(ext_start + new_len + j);
                    }
                    Self::write_trunk_extent(
                        &mut trunk,
                        i,
                        TrunkExtent {
                            start: U32::new(ext_start),
                            length: U32::new(new_len),
                        },
                    );
                }
                trunk_modified = true;
            }

            if self.trunk_is_empty(&trunk) && (result.len() as u32) < count {
                // Trunk fully drained and we still need blocks — reclaim it
                state.free_list_head = next_trunk;
                result.push(trunk_index);
            } else if trunk_modified {
                // Trunk still has extents (or is empty but we have enough) — write back
                self.write_raw_block(trunk_index, &trunk)?;
            }
        }
        let free_list_count = result.len();

        // Sort free-list blocks by index so that originally-contiguous blocks
        // are returned in ascending order, maximising contiguous runs.
        if free_list_count > 1 {
            result[..free_list_count].sort_unstable();
        }

        // Phase 2: grow file once for all remaining blocks
        let remaining = count as usize - result.len();
        if remaining > 0 {
            let first_new_id = state.total_blocks;

            // Index overflow protection
            if first_new_id + remaining as u64 > BLOCK_INDEX_SENTINEL as u64 {
                return Err(YakError::IoError(format!(
                    "block index overflow: need {} blocks but only {} available before sentinel",
                    remaining,
                    BLOCK_INDEX_SENTINEL as u64 - first_new_id,
                )));
            }

            // Single set_len to grow file for all new blocks at once
            let last_new_id = first_new_id + remaining as u64;
            let new_file_len = self.block_offset_u64(last_new_id);
            self.layer1.set_len(new_file_len)?;

            for i in 0..remaining as u64 {
                result.push((first_new_id + i) as u32);
            }
            state.total_blocks += remaining as u64;
        }

        // Phase 3: zero free-list blocks only (file-growth blocks are already
        // zero from set_len — guaranteed on Linux, macOS, and Windows).
        if free_list_count > 0 {
            if let Some(cipher) = &self.cipher {
                let encrypted_zeros: Vec<Vec<u8>> = result[..free_list_count]
                    .par_iter()
                    .map(|&block_id| {
                        let mut zeros = vec![0u8; block_size];
                        encryption::encrypt_block(cipher, block_id as u64, &mut zeros);
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

    fn deallocate_block(&self, index: u32) -> Result<(), YakError> {
        // Block 0 (superblock) must never be deallocated
        if index == 0 {
            return Err(YakError::IoError(
                "cannot deallocate block 0 (superblock)".to_string(),
            ));
        }

        let mut state = self.state.lock().unwrap();
        let extent_count = self.trunk_extent_count();

        // Try to find an empty extent slot in the head trunk
        if state.free_list_head != BLOCK_INDEX_SENTINEL {
            let trunk_index = state.free_list_head;
            let mut trunk = self.read_raw_block(trunk_index)?;

            for i in 0..extent_count {
                if Self::trunk_extent(&trunk, i).start.get() == BLOCK_INDEX_SENTINEL {
                    // Empty slot — write extent (index, 1)
                    Self::write_trunk_extent(
                        &mut trunk,
                        i,
                        TrunkExtent {
                            start: U32::new(index),
                            length: U32::new(1),
                        },
                    );
                    self.write_raw_block(trunk_index, &trunk)?;

                    self.persist_l2_header(&mut state)?;
                    drop(state);
                    self.evict_from_all_caches(index);
                    return Ok(());
                }
            }
        }

        // No room in head trunk (or free list is empty) — the freed block
        // becomes a new trunk.
        let trunk_data = self.init_trunk_block(state.free_list_head);
        self.write_raw_block(index, &trunk_data)?;
        state.free_list_head = index;

        self.persist_l2_header(&mut state)?;
        drop(state);
        self.evict_from_all_caches(index);
        Ok(())
    }

    fn deallocate_blocks(&self, indices: &mut Vec<u32>) -> Result<(), YakError> {
        if indices.is_empty() {
            return Ok(());
        }

        // Block 0 must never be deallocated
        if indices.contains(&0) {
            return Err(YakError::IoError(
                "cannot deallocate block 0 (superblock)".to_string(),
            ));
        }

        // Sort ascending and coalesce contiguous runs into extents.
        indices.sort_unstable();
        let mut extents = Self::coalesce_extents(indices);
        let extent_count = self.trunk_extent_count();

        let mut state = self.state.lock().unwrap();
        let mut ext_idx = 0; // cursor into `extents`

        // Try to fill empty slots in the current head trunk
        if state.free_list_head != BLOCK_INDEX_SENTINEL {
            let trunk_index = state.free_list_head;
            let mut trunk = self.read_raw_block(trunk_index)?;
            let mut trunk_modified = false;

            for i in 0..extent_count {
                if ext_idx >= extents.len() {
                    break;
                }
                if Self::trunk_extent(&trunk, i).start.get() == BLOCK_INDEX_SENTINEL {
                    let (es, el) = extents[ext_idx];
                    Self::write_trunk_extent(
                        &mut trunk,
                        i,
                        TrunkExtent {
                            start: U32::new(es),
                            length: U32::new(el),
                        },
                    );
                    ext_idx += 1;
                    trunk_modified = true;
                }
            }

            if trunk_modified {
                self.write_raw_block(trunk_index, &trunk)?;
            }
        }

        // If extents remain, create new trunk blocks from the freed blocks themselves
        while ext_idx < extents.len() {
            // Take one block from the first remaining extent to serve as the trunk
            let trunk_block;
            let (es, el) = extents[ext_idx];
            if el > 1 {
                trunk_block = es;
                extents[ext_idx] = (es + 1, el - 1);
            } else {
                trunk_block = es;
                ext_idx += 1;
            }

            // Initialize new trunk pointing to the current head
            let mut trunk_data = self.init_trunk_block(state.free_list_head);

            // Fill extent slots with remaining extents
            let mut slot = 0;
            while ext_idx < extents.len() && slot < extent_count {
                let (es, el) = extents[ext_idx];
                Self::write_trunk_extent(
                    &mut trunk_data,
                    slot,
                    TrunkExtent {
                        start: U32::new(es),
                        length: U32::new(el),
                    },
                );
                ext_idx += 1;
                slot += 1;
            }

            self.write_raw_block(trunk_block, &trunk_data)?;
            state.free_list_head = trunk_block;
        }

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
        index: u32,
        offset: usize,
        buf: &mut [u8],
        cache: CacheMode,
    ) -> Result<usize, YakError> {
        if index == BLOCK_INDEX_SENTINEL {
            return Err(YakError::IoError(format!(
                "read_block: block index {} is sentinel",
                index,
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

        // Fast path: unencrypted with no caching — read sub-region directly
        // into the caller's buffer, avoiding a full-block allocation and copy.
        if self.cipher.is_none() && (cache == CacheMode::None || CACHE_BUDGET_BYTES == 0) {
            let file_offset = self.block_offset(index) + offset as u64;
            self.layer1.read(file_offset, buf)?;
            return Ok(buf.len());
        }

        // Slow path: need full block for decryption and/or cache population
        let mut full_block = vec![0u8; block_size];
        let file_offset = self.block_offset(index);
        let n = self.layer1.read(file_offset, &mut full_block)?;

        if let Some(cipher) = &self.cipher {
            encryption::decrypt_block(cipher, index as u64, &mut full_block);
        }

        buf.copy_from_slice(&full_block[offset..offset + buf.len()]);

        if n == block_size {
            self.cache_put(index, full_block, cache);
        }

        Ok(buf.len())
    }

    fn write_block(
        &self,
        index: u32,
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
                encryption::decrypt_block(cipher, index as u64, &mut block);
                block
            };

            full_block[offset..offset + buf.len()].copy_from_slice(buf);

            let mut encrypted = full_block.clone();
            encryption::encrypt_block(cipher, index as u64, &mut encrypted);
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
        start_index: u32,
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
        let aligned_len = blocks_needed * bs;

        let cipher = self.cipher.as_ref().unwrap();
        let file_offset = self.block_offset(start_index);

        // If the read is block-aligned, decrypt directly in buf (no extra allocation)
        if offset == 0 && buf.len() == aligned_len {
            self.layer1.read(file_offset, buf)?;
            encryption::decrypt_blocks(cipher, buf, bs, start_index as u64);
            return Ok(buf.len());
        }

        let mut raw = vec![0u8; aligned_len];
        self.layer1.read(file_offset, &mut raw)?;
        encryption::decrypt_blocks(cipher, &mut raw, bs, start_index as u64);

        buf.copy_from_slice(&raw[offset..offset + buf.len()]);
        Ok(buf.len())
    }

    fn write_contiguous_blocks(
        &self,
        start_index: u32,
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
            encryption::decrypt_blocks(cipher, &mut raw, bs, start_index as u64);
        }

        raw[offset..offset + buf.len()].copy_from_slice(buf);

        encryption::encrypt_blocks(cipher, &mut raw, bs, start_index as u64);

        let file_offset = self.block_offset(start_index);
        self.layer1.write(file_offset, &raw)?;
        Ok(buf.len())
    }

    fn admin_slot_for_upper(&self, index: u8) -> AdminSlotId {
        // Upper layer index 0 = block0_slots[0] (L3), index 1 = block0_slots[1] (L4), etc.
        AdminSlotId(index)
    }

    fn write_admin_slot(&self, slot: AdminSlotId, data: &[u8]) -> Result<(), YakError> {
        let idx = slot.0 as usize;
        if idx >= self.block0_slots.len() {
            return Err(YakError::IoError(format!(
                "admin slot index {} out of range (have {} slots in block 0)",
                idx,
                self.block0_slots.len()
            )));
        }
        let info = &self.block0_slots[idx];
        if data.len() != info.payload_len as usize {
            return Err(YakError::IoError(format!(
                "admin slot {}: expected {} bytes, got {}",
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

    fn read_admin_slot(&self, slot: AdminSlotId) -> Result<Vec<u8>, YakError> {
        let idx = slot.0 as usize;
        if idx >= self.block0_slots.len() {
            return Err(YakError::IoError(format!(
                "admin slot index {} out of range (have {} slots in block 0)",
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

    fn verify(&self, claimed_blocks: &[u32]) -> Result<Vec<String>, YakError> {
        let mut issues = Vec::new();

        // 1. Run L1 verification
        issues.extend(self.layer1.verify()?);

        // 2. Read current state
        let state = self.state.lock().unwrap();
        let total_blocks = state.total_blocks;
        let free_list_head = state.free_list_head;
        drop(state);

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

        // 4. Walk the trunk-based free list, collecting free block IDs and
        //    detecting cycles.
        let mut free_blocks = std::collections::HashSet::new();
        let mut current_trunk = free_list_head;
        let extent_count = self.trunk_extent_count();
        let mut trunk_steps = 0u64;
        while current_trunk != BLOCK_INDEX_SENTINEL {
            if (current_trunk as u64) >= total_blocks {
                issues.push(format!(
                    "L2: free list trunk {} is >= total_blocks ({})",
                    current_trunk, total_blocks
                ));
                break;
            }
            if current_trunk == 0 {
                issues.push("L2: block 0 (superblock) found as trunk on free list".to_string());
                break;
            }
            if !free_blocks.insert(current_trunk) {
                issues.push(format!(
                    "L2: trunk cycle detected at block {} after {} trunks",
                    current_trunk, trunk_steps
                ));
                break;
            }
            trunk_steps += 1;
            if trunk_steps > total_blocks {
                issues.push("L2: trunk chain longer than total_blocks (cycle likely)".to_string());
                break;
            }

            // Read trunk block
            let trunk = self.read_raw_block(current_trunk)?;
            let next_trunk = Self::read_trunk_next(&trunk);

            // Parse extents
            for i in 0..extent_count {
                let ext = Self::trunk_extent(&trunk, i);
                let ext_start = ext.start.get();
                if ext_start == BLOCK_INDEX_SENTINEL {
                    continue;
                }

                let ext_len = ext.length.get();
                if ext_len == 0 {
                    issues.push(format!(
                        "L2: trunk {}, extent slot {}: zero-length extent at start {}",
                        current_trunk, i, ext_start
                    ));
                    continue;
                }

                for j in 0..ext_len {
                    let block_id = ext_start + j;
                    if (block_id as u64) >= total_blocks {
                        issues.push(format!(
                            "L2: extent in trunk {} references block {} >= total_blocks ({})",
                            current_trunk, block_id, total_blocks
                        ));
                        break;
                    }
                    if block_id == 0 {
                        issues.push("L2: block 0 found in free list extent".to_string());
                    }
                    if !free_blocks.insert(block_id) {
                        issues.push(format!(
                            "L2: block {} appears multiple times in free list",
                            block_id
                        ));
                    }
                }
            }

            current_trunk = next_trunk;
        }

        // 5. Build claimed set — include block 0 (superblock) as always claimed
        let mut claimed_set: std::collections::HashSet<u32> =
            claimed_blocks.iter().cloned().collect();
        claimed_set.insert(0); // block 0 is always reserved

        for &block_id in claimed_blocks {
            if (block_id as u64) >= total_blocks {
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
        for block_id in 0..total_blocks as u32 {
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
