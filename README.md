# Stream File System (SFS)

A layered file system implementation in Rust that provides a "file system in a file" with hierarchical stream storage.

## Project Status

**L3 Mock: ✅ COMPLETE** (49/49 tests passing)

All core components implemented and tested:
- ✅ Rust library (`stream_fs`) — L4 generic over L3 trait
- ✅ L3 implementation (`StreamsFromFiles`) — numbered `.stream` files on disk
- ✅ C FFI wrapper (`stream_fs_c`)
- ✅ Python bindings (`sfs_pytest/sfs`)
- ✅ Command-line tool (`sfs_cl`)
- ✅ Comprehensive test suite including thread safety burn-in tests

## Quick Start

### Building

```bash
# Build all crates (library, C FFI, CLI)
cargo build

# Build just the CLI tool
cargo build --package sfs_cl
```

### Testing

```bash
cd sfs_pytest

# Run all tests (thread safety tests burn for 10s by default)
python -m pytest tests/ -v

# Quick run (1s burn for thread safety tests)
python -m pytest tests/ -v --burn-seconds=1
```

### Using the CLI

```bash
# Create an SFS file
sfs create mydata.sfs

# Create a directory
sfs mkdir mydata.sfs documents

# Import a file as a stream
sfs put mydata.sfs localfile.txt documents/readme.txt

# List contents (use -r for recursive)
sfs ls mydata.sfs documents

# Read a stream
sfs cat mydata.sfs documents/readme.txt

# Get help
sfs --help
```

## Project Structure

```
stream_fs/       - Core Rust library (L4 API + L3 trait + StreamsFromFiles)
stream_fs_c/     - C FFI wrapper for language interop
sfs_cl/          - Command-line tool
sfs_pytest/      - Python bindings and test suite
docs/            - Architecture and design documentation
```

## Documentation

- [Architecture Overview](docs/architecture.md) - Complete 4-layer architecture
- [L3 Mock Design](docs/L3_mock.md) - Current implementation phase details
- [L4 Mock Design](docs/L4_mock.md) - Previous phase design decisions
- [Project Instructions](.claude/CLAUDE.md) - Development guidelines

## Architecture Layers

SFS is built in 4 layers, implemented bottom-up using a "mock first" approach:

1. **L1 - File System Abstraction** - Wraps OS file operations
2. **L2 - Block Storage** - Manages fixed-size blocks
3. **L3 - Stream Abstraction** - Links blocks into streams (✅ current — mocked with files)
4. **L4 - Filing Abstraction** - Provides directories and named streams (✅ complete)

## Current Implementation (L3 Mock)

L4 is a real filing system built on numbered streams from L3:
- SFS "files" are directories on disk containing numbered `.stream` files
- L4 manages directory streams (serialized stream entries) and data streams
- Path resolution walks the directory stream hierarchy
- `block_index_width` and `block_size_shift` are runtime values stored in metadata
- All stream IDs are u64 internally; on-disk serialization uses `block_index_width` bytes
- Thread-safe: all methods take `&self` with interior mutability via `Mutex`

## Features

### Current (L3 Mock)
- Create/open/close SFS files with configurable block parameters
- Directory operations (mkdir, rmdir, rename, list)
- Stream operations (create, delete, rename)
- Stream I/O (read, write, seek, tell, truncate)
- Proper locking (one writer OR many readers per stream)
- Thread safety with interior mutability
- Full C ABI for language interop
- Python bindings with Pythonic API
- Command-line tool for manual operations
- Thread safety burn-in tests (configurable duration)

### Planned (L2/L1)
- Real block-based storage
- Space-efficient stream linking
- Cross-platform file abstraction
- Crash recovery
- Process-level locking

## License

[To be determined]

## Contributing

See [CLAUDE.md](.claude/CLAUDE.md) for development workflow and architectural constraints.
