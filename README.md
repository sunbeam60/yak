# Yak

Yet Another Kontainer -- a layered file system implementation in Rust that provides a "file system in a file" with hierarchical stream storage.

## Project Status

**All layers complete** (132 Python tests + 7 cargo tests)

Yak now operates as a true single-file filesystem. All four layers are implemented:
- L1 (`FileOnDisk`) -- single-file backend with process-level locking via `fs2`
- L2 (`BlocksInFile`) -- real block storage in a single file via L1
- L3 (`StreamsFromBlocks`) -- pyramid block linking for numbered streams
- L4 (`Yak`) -- filing abstraction with directories and named streams
- Header chain (L4 -> L3 -> L2 -> L1 -> disk) with two-pass create
- C FFI wrapper (`yak_c`)
- Python bindings (`yak_pytest/yak`)
- Command-line tool (`yak_cl`)
- Integrity verification chain (`verify()` across L4->L3->L2->L1)
- Comprehensive test suite including multi-block, single-file persistence, thread safety burn-in, and corruption detection tests

## Quick Start

### Building

```bash
# Build all crates (library, C FFI, CLI)
cargo build

# Build just the CLI tool
cargo build --package yak_cl
```

### Testing

```bash
cd yak_pytest

# Run all tests (thread safety tests burn for 10s by default)
python -m pytest tests/ -v

# Quick run (1s burn for thread safety tests)
python -m pytest tests/ -v --burn-seconds=1
```

### Using the CLI

```bash
# Create a Yak file
yak create mydata.yak

# Directory operations
yak mkdir mydata.yak documents
yak rmdir mydata.yak documents
yak mv-dir mydata.yak old_name new_name

# List contents (use -r for recursive)
yak ls mydata.yak documents

# Stream operations
yak put mydata.yak localfile.txt documents/readme.txt
yak get mydata.yak documents/readme.txt localfile.txt
yak cat mydata.yak documents/readme.txt
yak rm mydata.yak documents/readme.txt
yak mv mydata.yak documents/old.txt documents/new.txt
yak info mydata.yak documents/readme.txt

# Verify integrity
yak verify mydata.yak

# Run benchmarks
yak bench <scenario>
```

## Project Structure

```
yak/             - Core Rust library (L4 + L3 + L2 traits and implementations)
yak_c/           - C FFI wrapper for language interop
yak_cl/          - Command-line tool
yak_pytest/      - Python bindings and test suite
docs/            - Architecture and design documentation
```

## Documentation

- [Architecture Overview](docs/architecture.md) - Complete 4-layer architecture and implementation notes
- [Project Instructions](.claude/CLAUDE.md) - Development guidelines

## Architecture Layers

Yak is built in 4 layers, implemented bottom-up using a "mock first" approach:

1. **L1 - File System Abstraction** - Wraps OS file operations with process-level locking
2. **L2 - Block Storage** - Manages fixed-size blocks in a single file
3. **L3 - Stream Abstraction** - Links blocks into streams via pyramid structure
4. **L4 - Filing Abstraction** - Provides directories and named streams

## Current Implementation

Yak operates as a true single-file filesystem (`YakDefault = Yak<StreamsFromBlocks<BlocksInFile<FileOnDisk, DEFAULT_CACHE_BUDGET_BYTES>>>`):
- A `.yak` file is a single binary file on disk
- L1 (`FileOnDisk`) wraps OS file I/O with exclusive process-level locking via `fs2`
- L2 (`BlocksInFile`) stores fixed-size blocks within the single file, with a free list for block reuse and a write-through cache for redirector blocks
- L3 (`StreamsFromBlocks`) links blocks into streams using pyramid block linking
- L4 (`Yak`) provides the filing abstraction with directories and named streams
- Header chain: L4 -> L3 -> L2 -> L1 -> disk, each layer with its own section
- `block_index_width` and `block_size_shift` are runtime values stored in the header
- Two-pass create: layers pass placeholder headers down, then rewrite with real values
- Thread-safe: RWLock on Streams stream + Mutex for bookkeeping + per-stream locking

## Features

- Create/open/close Yak files with configurable block parameters
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
- Integrity verification chain (L4->L3->L2->L1) with corruption detection
- Thread safety burn-in tests (configurable duration)
- Header chain with per-layer metadata persistence
- Data persistence across close/reopen cycles

## Mock Backends (for debugging)

The mock backends from earlier development phases are retained for debugging:
- `YakBlockFileBacked = Yak<StreamsFromBlocks<BlocksFromFiles>>` -- L2 mock (numbered `.block` files)
- `YakFileBacked = Yak<StreamsFromFiles>` -- L3 mock (numbered `.stream` files)

## License

[To be determined]

## Contributing

See [CLAUDE.md](.claude/CLAUDE.md) for development workflow and architectural constraints.
