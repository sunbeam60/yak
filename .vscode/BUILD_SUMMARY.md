# SFS Build Configuration Summary

## 🎯 Build Types

### Debug Builds (Default)
- **Location**: `target/debug/`
- **Size**: ~368 KB
- **Compile time**: Fast (~1-2 seconds)
- **Performance**: Unoptimized, includes debug symbols
- **Use case**: Development, debugging, testing

### Release Builds
- **Location**: `target/release/`
- **Size**: ~236 KB (35% smaller)
- **Compile time**: Slower (~1-2 seconds with optimizations)
- **Performance**: Fully optimized, stripped symbols
- **Use case**: Production, performance testing, distribution

## 📦 Build Outputs

### CLI Tool
- **Binary name**: `sfs.exe`
- **Package**: `sfs_cl`
- **Debug**: `target/debug/sfs.exe`
- **Release**: `target/release/sfs.exe`

### Core Library
- **Package**: `stream_fs`
- **Output**: `libstream_fs.rlib` (Rust library)

### C FFI Library
- **Package**: `stream_fs_c`
- **Output**: `stream_fs_c.dll` (Windows DLL)
- **Location**: `target/{debug,release}/stream_fs_c.dll`

## 🛠️ Quick Commands

### Build Release CLI
```bash
cargo build --release --package sfs_cl
```

### Build All Release
```bash
cargo build --release --workspace
```

### Copy Binaries to ~/bin/
Via VSCode task: `Ctrl+Shift+P` → "Tasks: Run Task" → "copy-release-binaries"

Or manually:
```bash
mkdir -p ~/bin
cp target/release/sfs.exe ~/bin/
cp target/release/stream_fs_c.dll ~/bin/
```

## 🚀 Running the CLI

### Debug Build
```bash
./target/debug/sfs.exe --help
```

### Release Build
```bash
./target/release/sfs.exe --help
```

### Installed Version
```bash
sfs --help  # From ~/.cargo/bin/
```

## 📁 Directory Structure

```
SFS/
├── target/
│   ├── debug/
│   │   ├── sfs.exe                 (CLI - debug)
│   │   ├── stream_fs_c.dll         (C FFI - debug)
│   │   └── libstream_fs.rlib       (Core library - debug)
│   └── release/
│       ├── sfs.exe                 (CLI - optimized)
│       ├── stream_fs_c.dll         (C FFI - optimized)
│       └── libstream_fs.rlib       (Core library - optimized)
└── temp/                           (Test/debug sandbox)

~/bin/                              (Copied release binaries)
├── sfs.exe
└── stream_fs_c.dll
```

## ⚙️ VSCode Integration

### Launch Configurations
- **Debug CLI: create test.sfs** — create a new SFS file in `./temp/`
- **Debug: Rust Tests (stream_fs)** — debug cargo unit tests
- **Python: All Tests** — debug all pytest tests

### Tasks
- `Ctrl+Shift+B` → `cargo-build-cli` (default debug build)
- `Ctrl+Shift+Alt+B` → `cargo-build-all-release` (workspace release build)
- `copy-release-binaries` — builds release + copies to `~/bin/`
- `rebuild-and-test` — debug build + Python tests (sequential)

## 📊 Performance Comparison

| Metric | Debug | Release | Improvement |
|--------|-------|---------|-------------|
| Binary size | 368 KB | 236 KB | 35% smaller |
| Optimization | None | Full | - |
| Debug symbols | Yes | No | - |
| Compile time | Fast | Slower | - |

## 🔍 Testing

All tests use the debug build by default for faster iteration:

```bash
# Run Python tests (uses debug build via FFI)
cd sfs_pytest && python -m pytest tests/ -v

# Build and test release (verify optimizations don't break functionality)
cargo build --release --workspace
cd sfs_pytest && python -m pytest tests/ -v
```

## 📝 Notes

- Release builds are recommended for benchmarking and distribution
- Debug builds are faster to compile and easier to debug
- The `copy-release-binaries` task copies to `~/bin/` (add to PATH for easy access)
- VSCode tasks handle build dependencies automatically
