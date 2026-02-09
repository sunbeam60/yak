mod sfs;
mod stream_layer;
mod streams_from_files;

pub use sfs::{DirEntry, EntryType, OpenMode, Sfs, SfsError, StreamHandle};
pub use stream_layer::StreamLayer;
pub use streams_from_files::StreamsFromFiles;

/// Default SFS configuration: file-based streams.
pub type SfsDefault = Sfs<StreamsFromFiles>;

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
}
