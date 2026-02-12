mod block_layer;
mod blocks_from_files;
mod blocks_in_file;
mod file_layer;
mod file_on_disk;
mod sfs;
mod stream_layer;
mod streams_from_blocks;
mod streams_from_files;

pub use block_layer::BlockLayer;
pub use blocks_from_files::BlocksFromFiles;
pub use blocks_in_file::BlocksInFile;
pub use file_layer::FileLayer;
pub use file_on_disk::FileOnDisk;
pub use sfs::{DirEntry, EntryType, OpenMode, Sfs, SfsError, StreamHandle};
pub use stream_layer::StreamLayer;
pub use streams_from_blocks::StreamsFromBlocks;
pub use streams_from_files::StreamsFromFiles;

/// Opaque token identifying a header section slot.
///
/// Each layer stores its own slot ID and uses it to read/write its header
/// section independently. Slot IDs are issued by L1 and passed through
/// upper layers via `header_slot_for_upper(index)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderSlotId(pub(crate) u8);

/// Default SFS configuration: single-file SFS with default L1, L2, L3, L4 layers.
pub type SfsDefault = Sfs<StreamsFromBlocks<BlocksInFile<FileOnDisk>>>;

/// Block-file-backed streams (L2 mock, debugging/testing tool).
pub type SfsBlockFileBacked = Sfs<StreamsFromBlocks<BlocksFromFiles>>;

/// File-backed streams (L3 mock, debugging/testing tool).
pub type SfsFileBacked = Sfs<StreamsFromFiles>;

/// Returns a hello world greeting from the Stream File System library.
pub fn hello() -> String {
    "Hello from Stream File System!".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hello() {
        assert_eq!(hello(), "Hello from Stream File System!");
    }

    #[test]
    fn test_reopen() {
        let path = std::env::temp_dir().join("sfs_test_reopen.sfs");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Create and write
        {
            let sfs = SfsDefault::create(path_str, 4, 12).unwrap();
            let sh = sfs.create_stream("hello.txt").unwrap();
            sfs.write(&sh, b"Hello, World!").unwrap();
            sfs.close_stream(sh).unwrap();
            sfs.close();
        }

        // Reopen and verify
        {
            let sfs = SfsDefault::open(path_str, OpenMode::Write).unwrap();
            let sh = sfs.open_stream("hello.txt", OpenMode::Read).unwrap();
            let mut buf = vec![0u8; 13];
            let n = sfs.read(&sh, &mut buf).unwrap();
            assert_eq!(n, 13);
            assert_eq!(&buf, b"Hello, World!");
            sfs.close_stream(sh).unwrap();
            sfs.close();
        }
    }

    #[test]
    fn test_stream_enumeration() {
        let path = std::env::temp_dir().join("sfs_test_stream_enum.sfs");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Test L3 directly via StreamsFromBlocks
        type L3 = StreamsFromBlocks<BlocksInFile<FileOnDisk>>;
        let l3 = L3::create(path_str, 4, 12, std::collections::VecDeque::new()).unwrap();

        // Initially no streams
        assert_eq!(l3.stream_count().unwrap(), 0);
        assert!(l3.stream_ids().unwrap().is_empty());

        // Create 3 streams
        let id0 = l3.create_stream().unwrap();
        let id1 = l3.create_stream().unwrap();
        let id2 = l3.create_stream().unwrap();

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
        let id3 = l3.create_stream().unwrap();
        assert_eq!(id3, id1); // reuses freed descriptor slot
        assert_eq!(l3.stream_count().unwrap(), 3);
        let mut ids = l3.stream_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec![id0, id1, id2]);
    }

    #[test]
    fn test_depth2_write() {
        let path = std::env::temp_dir().join("sfs_test_depth2.sfs");
        let _ = std::fs::remove_file(&path); // clean up from previous run
        let path_str = path.to_str().unwrap();

        // block_size=64, block_index_width=4, fan_out=16
        let sfs = SfsDefault::create(path_str, 4, 6).unwrap();
        let sh = sfs.create_stream("big.bin").unwrap();

        // 1025 bytes requires 17 data blocks -> depth 2
        let len = 100025usize;
        let data: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
        sfs.write(&sh, &data).unwrap();

        assert_eq!(sfs.stream_length(&sh).unwrap(), len as u64);
        sfs.seek(&sh, 0).unwrap();
        let mut buf = vec![0u8; len];
        let n = sfs.read(&sh, &mut buf).unwrap();
        assert_eq!(n, len);
        assert_eq!(buf, data);

        sfs.close_stream(sh).unwrap();
        sfs.close();
    }

    #[test]
    fn test_verify_clean() {
        let path = std::env::temp_dir().join("sfs_test_verify.sfs");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        // Create with dirs, streams, and data
        {
            let sfs = SfsDefault::create(path_str, 4, 12).unwrap();
            sfs.mkdir("docs").unwrap();

            let sh = sfs.create_stream("hello.txt").unwrap();
            sfs.write(&sh, b"Hello, World!").unwrap();
            sfs.close_stream(sh).unwrap();

            let sh = sfs.create_stream("docs/readme.txt").unwrap();
            sfs.write(&sh, b"A readme").unwrap();
            sfs.close_stream(sh).unwrap();

            // Verify while open for writing
            let issues = sfs.verify().unwrap();
            assert!(issues.is_empty(), "Issues found: {:?}", issues);

            sfs.close();
        }

        // Reopen read-only and verify
        {
            let sfs = SfsDefault::open(path_str, OpenMode::Read).unwrap();
            let issues = sfs.verify().unwrap();
            assert!(issues.is_empty(), "Issues found: {:?}", issues);
            sfs.close();
        }
    }

    #[test]
    fn test_reserve() {
        let path = std::env::temp_dir().join("sfs_test_reserve.sfs");
        let _ = std::fs::remove_file(&path);
        let path_str = path.to_str().unwrap();

        {
            let sfs = SfsDefault::create(path_str, 4, 12).unwrap();
            let sh = sfs.create_stream("data.bin").unwrap();

            // Empty stream: reserved = 0
            assert_eq!(sfs.stream_reserved(&sh).unwrap(), 0);

            // Reserve 8192 bytes (2 blocks of 4096)
            sfs.reserve(&sh, 8192).unwrap();
            assert_eq!(sfs.stream_reserved(&sh).unwrap(), 8192);
            assert_eq!(sfs.stream_length(&sh).unwrap(), 0);

            // Write within reserved capacity
            let data = vec![0xABu8; 5000];
            sfs.write(&sh, &data).unwrap();
            assert_eq!(sfs.stream_length(&sh).unwrap(), 5000);
            assert_eq!(sfs.stream_reserved(&sh).unwrap(), 8192);

            // Read back
            sfs.seek(&sh, 0).unwrap();
            let mut buf = vec![0u8; 5000];
            sfs.read(&sh, &mut buf).unwrap();
            assert_eq!(buf, data);

            sfs.close_stream(sh).unwrap();

            // Verify clean (must close stream first so descriptor is flushed)
            let issues = sfs.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);

            sfs.close();
        }

        // Reopen and verify reserved persists
        {
            let sfs = SfsDefault::open(path_str, OpenMode::Write).unwrap();
            let sh = sfs.open_stream("data.bin", OpenMode::Write).unwrap();
            assert_eq!(sfs.stream_reserved(&sh).unwrap(), 8192);
            assert_eq!(sfs.stream_length(&sh).unwrap(), 5000);

            // Truncate clears reservation
            sfs.truncate(&sh, 0).unwrap();
            assert_eq!(sfs.stream_reserved(&sh).unwrap(), 0);
            assert_eq!(sfs.stream_length(&sh).unwrap(), 0);

            sfs.close_stream(sh).unwrap();

            // Verify after close (descriptor must be flushed first)
            let issues = sfs.verify().unwrap();
            assert!(issues.is_empty(), "Issues: {:?}", issues);

            sfs.close();
        }
    }
}
