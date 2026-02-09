# Stream File System (SFS)

A layered file system implementation in Rust that provides a "file system in a file" with hierarchical stream storage.

## Project Status

**L2 Mock: ✅ COMPLETE** (68/68 tests passing)

All core components implemented and tested:
- ✅ Rust library (`stream_fs`) — L4 generic over L3 trait, L3 generic over L2 trait
- ✅ Real L3 implementation (`StreamsFromBlocks`) — pyramid block linking
- ✅ L2 mock (`BlocksFromFiles`) — numbered `.block` files on disk
- ✅ L3 mock (`StreamsFromFiles`) — kept as debugging tool
- ✅ Header chain (L4 → L3 → L2 → disk)
- ✅ C FFI wrapper (`stream_fs_c`)
- ✅ Python bindings (`sfs_pytest/sfs`)
- ✅ Command-line tool (`sfs_cl`)
- ✅ Comprehensive test suite including multi-block and thread safety burn-in tests

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
stream_fs/       - Core Rust library (L4 + L3 + L2 traits and implementations)
stream_fs_c/     - C FFI wrapper for language interop
sfs_cl/          - Command-line tool
sfs_pytest/      - Python bindings and test suite
docs/            - Architecture and design documentation
```

## Documentation

- [Architecture Overview](docs/architecture.md) - Complete 4-layer architecture
- [L2 Mock Design](docs/L2_mock.md) - Current implementation phase details
- [L3 Mock Design](docs/L3_mock.md) - Previous phase design decisions
- [L4 Mock Design](docs/L4_mock.md) - Initial phase design decisions
- [Project Instructions](.claude/CLAUDE.md) - Development guidelines

## Architecture Layers

SFS is built in 4 layers, implemented bottom-up using a "mock first" approach:

1. **L1 - File System Abstraction** - Wraps OS file operations (planned)
2. **L2 - Block Storage** - Manages fixed-size blocks (✅ complete — mocked with files)
3. **L3 - Stream Abstraction** - Links blocks into streams via pyramid structure (✅ complete)
4. **L4 - Filing Abstraction** - Provides directories and named streams (✅ complete)

## Current Implementation (L2 Mock)

L3 is a real stream layer built on numbered blocks from L2:
- SFS "files" are directories on disk containing numbered `.block` files
- L3 (`StreamsFromBlocks`) links blocks into streams using pyramid block linking
- Stream descriptors stored in a "Streams stream" with out-of-band descriptor in the header chain
- Header chain: L4 → L3 → L2 → disk, each layer with its own header section
- `block_index_width` and `block_size_shift` are runtime values stored in the header
- Thread-safe: RWLock on Streams stream + Mutex for bookkeeping + per-stream locking

## Features

### Current (L2 Mock)
- Create/open/close SFS files with configurable block parameters
- Directory operations (mkdir, rmdir, rename, list)
- Stream operations (create, delete, rename)
- Stream I/O (read, write, seek, tell, truncate)
- Multi-block streams with pyramid block linking (depth 0, 1, 2+)
- Proper locking (one writer OR many readers per stream)
- Thread safety with interior mutability
- Full C ABI for language interop
- Python bindings with Pythonic API
- Command-line tool for manual operations
- Thread safety burn-in tests (configurable duration)
- Header chain with per-layer metadata persistence

### Planned (L1)
- Cross-platform file abstraction (single-file SFS)
- Crash recovery
- Process-level locking

## License

[To be determined]

## Contributing

See [CLAUDE.md](.claude/CLAUDE.md) for development workflow and architectural constraints.
