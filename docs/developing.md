# Developing SFS

## Prerequisites

- **Rust** (stable toolchain) — install via [rustup](https://rustup.rs/)
- **Python 3.10+** — required for the test harness
- **maturin** — builds the PyO3 Python bindings (`pip install maturin`)

## Project structure

| Module                   | Language      | Path                 |
| ------------------------ | ------------- | -------------------- |
| SFS library              | Rust          | `stream_fs/`         |
| C ABI wrapper            | Rust          | `stream_fs_c/`       |
| Python bindings (PyO3)   | Rust          | `stream_fs_python/`  |
| Command line tool        | Rust          | `sfs_cl/`            |
| Test harness             | Python/pytest | `sfs_pytest/`        |

## First-time setup

```bash
# Clone and build the Rust workspace
cargo build

# Set up the Python test environment
cd sfs_pytest
python -m venv .venv
source .venv/bin/activate
pip install pytest maturin

# Build and install the native Python module into the venv
cd ../stream_fs_python
maturin develop --release
```

## Day-to-day workflow

After making changes to `stream_fs` or `stream_fs_python`, rebuild the Python module before running tests:

```bash
cd stream_fs_python && maturin develop --release
```

Then run the test suite:

```bash
cd ../sfs_pytest && .venv/bin/python -m pytest tests/ -v
```

## Quality checks

A git pre-push hook runs `fmt` and `clippy`. You can run them manually:

```bash
cargo fmt --all --check
cargo clippy --all
```

## Profiling

Release builds can be profiled with [samply](https://github.com/mstange/samply). See `.vscode/tasks.json` for predefined profiling tasks.

## Publishing

- **crates.io**: `cargo publish -p stream_fs` (publishes the Rust library)
- **PyPI**: `cd stream_fs_python && maturin publish` (publishes pre-built Python wheels)

Once published to PyPI, any Python project can install the bindings with `pip install sfs`.
