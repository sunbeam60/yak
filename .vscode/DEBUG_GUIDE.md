# VSCode Debug Configuration Guide

This workspace is now configured for debugging both Rust and Python code in the Yak project.

## 🚀 Quick Start

1. **Open the Debug panel**: Click the debug icon in the left sidebar (or press `Ctrl+Shift+D`)
2. **Select a configuration** from the dropdown at the top
3. **Press F5** to start debugging

## 📋 Available Debug Configurations

### Rust CLI Debugging (Debug Builds)

| Configuration | Description | Working Dir |
|---------------|-------------|-------------|
| **Debug CLI: create test.yak** | Create a new Yak file | `./temp/` |
| **Debug: Rust Tests (yak)** | Debug Rust unit tests | Project root |

### Python Test Debugging

| Configuration | Description |
|---------------|-------------|
| **Python: All Tests** | Debug all pytest tests |

## 🛠️ Build Tasks

Press `Ctrl+Shift+B` to see build tasks, or run them via `Terminal > Run Task`:

### Debug Builds (`Ctrl+Shift+B`, fast compile, unoptimized, in `target/debug/`)
- **cargo-build-cli** (default build task) - Build the CLI tool
- **cargo-build-all** - Build entire workspace
- **cargo-test-build** - Build tests without running

### Release Builds (`Ctrl+Shift+Alt+B`, slow compile, optimized, in `target/release/`)
- **cargo-build-cli-release** - Build CLI tool (optimized)
- **cargo-build-all-release** - Build entire workspace (optimized, bound to `Ctrl+Shift+Alt+B`)
- **copy-release-binaries** - Build release + copy binaries to `~/bin/`
- **test-release-build** - Build release + run Python tests

### Testing & Utility
- **run-pytest-all** - Run all Python tests
- **rebuild-and-test** - Debug build + Python tests (sequential)
- **cargo-clean** - Clean all build artifacts

## 🎯 Debugging Tips

### Rust Debugging

1. **Set breakpoints**: Click in the gutter (left of line numbers) to set breakpoints
2. **Step through code**: Use F10 (step over), F11 (step into), Shift+F11 (step out)
3. **Inspect variables**: Hover over variables or check the Variables panel
4. **Watch expressions**: Add expressions to the Watch panel

### Python Debugging

1. **Test-specific debugging**: Open a test file and use "Python: Current Test File"
2. **Break on exceptions**: Enable in the breakpoints panel
3. **justMyCode: false**: Configured to step into library code if needed

### Custom CLI Arguments

To debug with custom arguments, copy the "Debug CLI: create test.yak" configuration in `.vscode/launch.json` and modify the `args` array, e.g.:
   ```json
   "args": ["put", "test.yak", "myfile.txt", "stream.dat"]
   ```

## 📁 Directories

- **temp/** - Sandbox directory for CLI debugging (create/test Yak files here)
- **.vscode/** - VSCode configuration files

## 🔧 Requirements

### Rust Debugging (Windows)
- C++ Build Tools (MSVC) - for `cppvsdbg` debugger
- Rust toolchain with debug symbols

### Python Debugging
- Python extension (`ms-python.python`)
- Debugpy extension
- pytest installed in Python 3.11.9

## 📝 Adding New Configurations

To add a new debug configuration:

1. Open `.vscode/launch.json`
2. Copy an existing configuration
3. Modify the `name`, `program`, and `args` fields
4. Save and it will appear in the debug dropdown

## 🐛 Troubleshooting

**"Program not found"**: Run the build task first (`Ctrl+Shift+B`)

**"Cannot find Python interpreter"**: Check that `C:/Python311/python.exe` exists or update `settings.json`

**Rust debug symbols missing**: Ensure you're building in debug mode (not `--release`)

**Path not found**: Check that `C:/Users/bjorn/.cargo/bin` is in your PATH or set in tasks.json

---

Happy debugging! 🎉
