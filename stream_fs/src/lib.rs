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

/// Default SFS configuration: single-file SFS with real L1+L2.
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
            let sfs = SfsDefault::open(path_str).unwrap();
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
}
