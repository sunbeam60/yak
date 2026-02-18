# Developing Yak

## Prerequisites

- **Rust** (stable toolchain) -- install via [rustup](https://rustup.rs/)
- **Python 3.10+** -- required for the test harness
- **maturin** -- builds the PyO3 Python bindings (`pip install maturin`)

## Project structure

| Module                   | Language      | Path                 |
| ------------------------ | ------------- | -------------------- |
| Yak library              | Rust          | `yak/`               |
| C ABI wrapper            | Rust          | `yak_c/`             |
| Python bindings (PyO3)   | Rust          | `yak_python/`        |
| Command line tool        | Rust          | `yak_cl/`            |
| Test harness             | Python/pytest | `yak_pytest/`        |

## First-time setup

```bash
# Clone and build the Rust workspace
cargo build

# Set up the Python test environment
cd yak_pytest
python -m venv .venv
source .venv/bin/activate
pip install pytest maturin

# Build and install the native Python module into the venv
cd ../yak_python
maturin develop --release
```

## Day-to-day workflow

After making changes to `yak` or `yak_python`, rebuild the Python module before running tests:

```bash
cd yak_python && maturin develop --release
```

Then run the test suite:

```bash
cd ../yak_pytest && .venv/bin/python -m pytest tests/ -v
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

- **crates.io**: `cargo publish -p yak` (publishes the Rust library)
- **PyPI**: `cd yak_python && maturin publish` (publishes pre-built Python wheels)

Once published to PyPI, any Python project can install the bindings with `pip install yak`.
