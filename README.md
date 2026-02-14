# Stream File System (SFS)

A layered file system implementation in Rust that provides a "file system in a file" with hierarchical stream storage.

## Project Status

**All layers complete** (132 Python tests + 7 cargo tests)

SFS now operates as a true single-file filesystem. All four layers are implemented:
- ✅ L1 (`FileOnDisk`) — single-file backend with process-level locking via `fs2`
- ✅ L2 (`BlocksInFile`) — real block storage in a single file via L1
- ✅ L3 (`StreamsFromBlocks`) — pyramid block linking for numbered streams
- ✅ L4 (`Sfs`) — filing abstraction with directories and named streams
- ✅ Header chain (L4 → L3 → L2 → L1 → disk) with two-pass create
- ✅ C FFI wrapper (`stream_fs_c`)
- ✅ Python bindings (`sfs_pytest/sfs`)
- ✅ Command-line tool (`sfs_cl`)
- ✅ Integrity verification chain (`verify()` across L4→L3→L2→L1)
- ✅ Comprehensive test suite including multi-block, single-file persistence, thread safety burn-in, and corruption detection tests

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

# Directory operations
sfs mkdir mydata.sfs documents
sfs rmdir mydata.sfs documents
sfs mv-dir mydata.sfs old_name new_name

# List contents (use -r for recursive)
sfs ls mydata.sfs documents

# Stream operations
sfs put mydata.sfs localfile.txt documents/readme.txt
sfs get mydata.sfs documents/readme.txt localfile.txt
sfs cat mydata.sfs documents/readme.txt
sfs rm mydata.sfs documents/readme.txt
sfs mv mydata.sfs documents/old.txt documents/new.txt
sfs info mydata.sfs documents/readme.txt

# Verify integrity
sfs verify mydata.sfs

# Run benchmarks
sfs bench <scenario>
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

- [Architecture Overview](docs/architecture.md) - Complete 4-layer architecture and implementation notes
- [Project Instructions](.claude/CLAUDE.md) - Development guidelines

## Architecture Layers

SFS is built in 4 layers, implemented bottom-up using a "mock first" approach:

1. **L1 - File System Abstraction** - Wraps OS file operations with process-level locking (✅ complete)
2. **L2 - Block Storage** - Manages fixed-size blocks in a single file (✅ complete)
3. **L3 - Stream Abstraction** - Links blocks into streams via pyramid structure (✅ complete)
4. **L4 - Filing Abstraction** - Provides directories and named streams (✅ complete)

## Current Implementation

SFS operates as a true single-file filesystem (`SfsDefault = Sfs<StreamsFromBlocks<BlocksInFile<FileOnDisk, DEFAULT_CACHE_BUDGET_BYTES>>>`):
- A `.sfs` file is a single binary file on disk
- L1 (`FileOnDisk`) wraps OS file I/O with exclusive process-level locking via `fs2`
- L2 (`BlocksInFile`) stores fixed-size blocks within the single file, with a free list for block reuse and a write-through cache for redirector blocks
- L3 (`StreamsFromBlocks`) links blocks into streams using pyramid block linking
- L4 (`Sfs`) provides the filing abstraction with directories and named streams
- Header chain: L4 → L3 → L2 → L1 → disk, each layer with its own section
- `block_index_width` and `block_size_shift` are runtime values stored in the header
- Two-pass create: layers pass placeholder headers down, then rewrite with real values
- Thread-safe: RWLock on Streams stream + Mutex for bookkeeping + per-stream locking

## Features

- Create/open/close SFS files with configurable block parameters
- Single-file storage (no directory-based mocks needed for production use)
- Directory operations (mkdir, rmdir, rename, list)
- Stream operations (create, delete, rename)
- Stream I/O (read, write, seek, tell, truncate)
- Multi-block streams with pyramid block linking (depth 0, 1, 2+)
- Block reuse via free list (singly-linked in block content)
- Write-through cache for redirector blocks (configurable budget)
- Stream space reservation to reduce fragmentation
- Proper locking (one writer OR many readers per stream)
- Process-level file locking (exclusive access via `fs2`)
- Thread safety with interior mutability
- Full C ABI for language interop
- Python bindings with Pythonic API
- Command-line tool for manual operations
- Integrity verification chain (L4→L3→L2→L1) with corruption detection
- Thread safety burn-in tests (configurable duration)
- Header chain with per-layer metadata persistence
- Data persistence across close/reopen cycles

## Mock Backends (for debugging)

The mock backends from earlier development phases are retained for debugging:
- `SfsBlockFileBacked = Sfs<StreamsFromBlocks<BlocksFromFiles>>` — L2 mock (numbered `.block` files)
- `SfsFileBacked = Sfs<StreamsFromFiles>` — L3 mock (numbered `.stream` files)

## License

[To be determined]

## Contributing

See [CLAUDE.md](.claude/CLAUDE.md) for development workflow and architectural constraints.
