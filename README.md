# Stream File System (SFS)

A layered file system implementation in Rust that provides a "file system in a file" with hierarchical stream storage.

## Project Status

**L4 Mock: ✅ COMPLETE** (35/35 tests passing)

All core components implemented and tested:
- ✅ Rust library (`stream_fs`)
- ✅ C FFI wrapper (`stream_fs_c`)
- ✅ Python bindings (`sfs_pytest/sfs`)
- ✅ Command-line tool (`sfs_cl`)
- ✅ Comprehensive test suite (pytest)

## Quick Start

### Building

```bash
# Build the Rust library and C FFI
cd stream_fs_c
cargo build

# Build the CLI tool
cd ../sfs_cl
cargo build
```

### Testing

```bash
cd sfs_pytest
python -m pytest tests/ -v
```

### Using the CLI

```bash
# Create an SFS file
sfs create mydata.sfs

# Create a directory
sfs mkdir mydata.sfs documents

# Import a file as a stream
sfs put mydata.sfs localfile.txt documents/readme.txt

# List contents
sfs ls mydata.sfs documents

# Read a stream
sfs cat mydata.sfs documents/readme.txt

# Get help
sfs --help
```

## Project Structure

```
stream_fs/       - Core Rust library (L4 API)
stream_fs_c/     - C FFI wrapper for language interop
sfs_cl/          - Command-line tool
sfs_pytest/      - Python bindings and test suite
docs/            - Architecture and design documentation
```

## Documentation

- [Architecture Overview](docs/architecture.md) - Complete 4-layer architecture
- [L4 Mock Design](docs/L4_mock.md) - Current implementation phase details
- [Project Instructions](.claude/CLAUDE.md) - Development guidelines

## Architecture Layers

SFS is built in 4 layers, implemented bottom-up using a "mock first" approach:

1. **L1 - File System Abstraction** - Wraps OS file operations
2. **L2 - Block Storage** - Manages fixed-size blocks
3. **L3 - Stream Abstraction** - Links blocks into streams
4. **L4 - Filing Abstraction** - Provides directories and named streams (✅ current)

## Current Implementation (L4 Mock)

The L4 layer is currently implemented as a "mock" that uses the real filesystem:
- SFS "files" are directories on disk (e.g., `mydata.sfs/`)
- SFS directories map to real subdirectories
- SFS streams map to real files

This allows the full L4 API to be tested and validated before implementing the lower layers. When L3/L2/L1 are implemented, the mock will be replaced with the real layered implementation.

## Features

### Current (L4 Mock)
- Create/open/close SFS files
- Directory operations (mkdir, rmdir, rename, list)
- Stream operations (create, delete, rename)
- Stream I/O (read, write, seek, tell, truncate)
- Proper locking (one writer OR many readers per stream)
- Full C ABI for language interop
- Python bindings with Pythonic API
- Command-line tool for manual operations

### Planned (L3/L2/L1)
- Real block-based storage
- Space-efficient stream linking
- Cross-platform file abstraction
- Crash recovery
- Process-level locking

## License

[To be determined]

## Contributing

See [CLAUDE.md](.claude/CLAUDE.md) for development workflow and architectural constraints.
