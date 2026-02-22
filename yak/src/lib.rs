mod block_layer;
mod blocks_from_files;
mod blocks_in_file;
pub(crate) mod encryption;
mod file_layer;
mod file_on_disk;
mod stream_layer;
mod streams_from_blocks;
mod streams_from_files;
mod yak;

pub use block_layer::{BlockLayer, CacheMode};
pub use blocks_from_files::BlocksFromFiles;
pub use blocks_in_file::{BlocksInFile, DEFAULT_CACHE_BUDGET_BYTES};
pub use file_layer::FileLayer;
pub use file_on_disk::FileOnDisk;
pub use stream_layer::StreamLayer;
pub use streams_from_blocks::StreamsFromBlocks;
pub use streams_from_files::StreamsFromFiles;
pub use yak::{CreateOptions, DirEntry, EntryType, OpenMode, StreamHandle, Yak, YakError};

/// Opaque token identifying a header section slot.
///
/// Each layer stores its own slot ID and uses it to read/write its header
/// section independently. Slot IDs are issued by L1 and passed through
/// upper layers via `header_slot_for_upper(index)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderSlotId(pub(crate) u8);

/// Default Yak configuration: single-file Yak with default L1, L2, L3, L4 layers.
pub type YakDefault = Yak<StreamsFromBlocks<BlocksInFile<FileOnDisk, DEFAULT_CACHE_BUDGET_BYTES>>>;

/// Block-file-backed streams (L2 mock, debugging/testing tool).
pub type YakBlockFileBacked = Yak<StreamsFromBlocks<BlocksFromFiles>>;

/// File-backed streams (L3 mock, debugging/testing tool).
pub type YakFileBacked = Yak<StreamsFromFiles>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reopen() {
        let path = std::env::temp_dir().join("yak_test_reopen.yak");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Create and write
        {
            let yk = YakDefault::create(path_str, CreateOptions::default()).unwrap();
            let sh = yk.create_stream("hello.txt", false).unwrap();
            yk.write(&sh, b"Hello, World!").unwrap();
            yk.close_stream(sh).unwrap();
            yk.close().unwrap();
        }

        // Reopen and verify
        {
            let yk = YakDefault::open(path_str, OpenMode::Write).unwrap();
            let sh = yk.open_stream("hello.txt", OpenMode::Read).unwrap();
            let mut buf = vec![0u8; 13];
            let n = yk.read(&sh, &mut buf).unwrap();
            assert_eq!(n, 13);
            assert_eq!(&buf, b"Hello, World!");
            yk.close_stream(sh).unwrap();
            yk.close().unwrap();
        }
    }

    #[test]
    fn test_stream_enumeration() {
        let path = std::env::temp_dir().join("yak_test_stream_enum.yak");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Test L3 directly via StreamsFromBlocks
        type L3 = StreamsFromBlocks<BlocksInFile<FileOnDisk, DEFAULT_CACHE_BUDGET_BYTES>>;
        let l3 = L3::create(path_str, 4, 12, 0, std::collections::VecDeque::new(), None).unwrap();

        // Initially no streams
        assert_eq!(l3.stream_count().unwrap(), 0);
        assert!(l3.stream_ids().unwrap().is_empty());

        // Create 3 streams
        let id0 = l3.create_stream(false).unwrap();
        let id1 = l3.create_stream(false).unwrap();
        let id2 = l3.create_stream(false).unwrap();

        assert_eq!(l3.stream_count().unwrap(), 3);
        let mut ids = l3.stream_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec![id0, id1, id2]);

        // Delete the middle stream
        l3.delete_stream(id1).unwrap();
        assert_eq!(l3.stream_count().unwrap(), 2);
        let mut ids = l3.stream_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec![id0, id2]);

        // Create another — should reuse the freed slot
        let id3 = l3.create_stream(false).unwrap();
        assert_eq!(id3, id1); // reuses freed descriptor slot
        assert_eq!(l3.stream_count().unwrap(), 3);
        let mut ids = l3.stream_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec![id0, id1, id2]);
    }

    #[test]
    fn test_depth2_write() {
        let path = std::env::temp_dir().join("yak_test_depth2.yak");
        let _ = std::fs::remove_file(&path); // clean up from previous run
        let path_str = path.to_str().unwrap();

        // block_size=64, block_index_width=4, fan_out=16
        let yk = YakDefault::create(
            path_str,
            CreateOptions {
                block_size_shift: 6,
                compressed_block_size_shift: 9,
                ..Default::default()
            },
        )
        .unwrap();
        let sh = yk.create_stream("big.bin", false).unwrap();

        // 1025 bytes requires 17 data blocks -> depth 2
        let len = 100025usize;
        let data: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        yk.write(&sh, &data).unwrap();

        assert_eq!(yk.stream_length(&sh).unwrap(), len as u64);
        yk.seek(&sh, 0).unwrap();
        let mut buf = vec![0u8; len];
        let n = yk.read(&sh, &mut buf).unwrap();
        assert_eq!(n, len);
        assert_eq!(buf, data);

        yk.close_stream(sh).unwrap();
        yk.close().unwrap();
    }

    #[test]
    fn test_verify_clean() {
        let path = std::env::temp_dir().join("yak_test_verify.yak");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Create with dirs, streams, and data
        {
            let yk = YakDefault::create(path_str, CreateOptions::default()).unwrap();
            yk.mkdir("docs").unwrap();

            let sh = yk.create_stream("hello.txt", false).unwrap();
            yk.write(&sh, b"Hello, World!").unwrap();
            yk.close_stream(sh).unwrap();

            let sh = yk.create_stream("docs/readme.txt", false).unwrap();
            yk.write(&sh, b"A readme").unwrap();
            yk.close_stream(sh).unwrap();

            // Verify while open for writing
            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues found: {:?}", issues);

            yk.close().unwrap();
        }

        // Reopen read-only and verify
        {
            let yk = YakDefault::open(path_str, OpenMode::Read).unwrap();
            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues found: {:?}", issues);
            yk.close().unwrap();
        }
    }

    #[test]
    fn test_no_cache() {
        let path = std::env::temp_dir().join("yak_test_no_cache.yak");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        type YakNoCache = Yak<StreamsFromBlocks<BlocksInFile<FileOnDisk, 0>>>;

        // Create, write, read back, verify — all without block cache
        {
            let yk = YakNoCache::create(path_str, CreateOptions::default()).unwrap();
            yk.mkdir("docs").unwrap();

            let sh = yk.create_stream("hello.txt", false).unwrap();
            yk.write(&sh, b"Hello, no cache!").unwrap();
            yk.close_stream(sh).unwrap();

            // Multi-block write to exercise allocate_blocks path
            let sh = yk.create_stream("docs/big.bin", false).unwrap();
            let data: Vec<u8> = (0..10000).map(|i| (i & 0xFF) as u8).collect();
            yk.write(&sh, &data).unwrap();
            yk.seek(&sh, 0).unwrap();
            let mut buf = vec![0u8; 10000];
            let n = yk.read(&sh, &mut buf).unwrap();
            assert_eq!(n, 10000);
            assert_eq!(buf, data);
            yk.close_stream(sh).unwrap();

            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);
            yk.close().unwrap();
        }

        // Reopen and verify persistence
        {
            let yk = YakNoCache::open(path_str, OpenMode::Read).unwrap();
            let sh = yk.open_stream("hello.txt", OpenMode::Read).unwrap();
            let mut buf = vec![0u8; 16];
            let n = yk.read(&sh, &mut buf).unwrap();
            assert_eq!(n, 16);
            assert_eq!(&buf, b"Hello, no cache!");
            yk.close_stream(sh).unwrap();

            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);
            yk.close().unwrap();
        }
    }

    #[test]
    fn test_reserve() {
        let path = std::env::temp_dir().join("yak_test_reserve.yak");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        {
            let yk = YakDefault::create(path_str, CreateOptions::default()).unwrap();
            let sh = yk.create_stream("data.bin", false).unwrap();

            // Empty stream: reserved = 0
            assert_eq!(yk.stream_reserved(&sh).unwrap(), 0);

            // Reserve 8192 bytes (2 blocks of 4096)
            yk.reserve(&sh, 8192).unwrap();
            assert_eq!(yk.stream_reserved(&sh).unwrap(), 8192);
            assert_eq!(yk.stream_length(&sh).unwrap(), 0);

            // Write within reserved capacity
            let data = vec![0xABu8; 5000];
            yk.write(&sh, &data).unwrap();
            assert_eq!(yk.stream_length(&sh).unwrap(), 5000);
            assert_eq!(yk.stream_reserved(&sh).unwrap(), 8192);

            // Read back
            yk.seek(&sh, 0).unwrap();
            let mut buf = vec![0u8; 5000];
            yk.read(&sh, &mut buf).unwrap();
            assert_eq!(buf, data);

            yk.close_stream(sh).unwrap();

            // Verify clean (must close stream first so descriptor is flushed)
            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);

            yk.close().unwrap();
        }

        // Reopen and verify reserved persists
        {
            let yk = YakDefault::open(path_str, OpenMode::Write).unwrap();
            let sh = yk.open_stream("data.bin", OpenMode::Write).unwrap();
            assert_eq!(yk.stream_reserved(&sh).unwrap(), 8192);
            assert_eq!(yk.stream_length(&sh).unwrap(), 5000);

            // Truncate clears reservation
            yk.truncate(&sh, 0).unwrap();
            assert_eq!(yk.stream_reserved(&sh).unwrap(), 0);
            assert_eq!(yk.stream_length(&sh).unwrap(), 0);

            yk.close_stream(sh).unwrap();

            // Verify after close (descriptor must be flushed first)
            let issues = yk.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);

            yk.close().unwrap();
        }
    }
}
