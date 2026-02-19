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

Publishing is automated via GitHub Actions on tag push. To create a release:

```bash
# Ensure version is bumped in Cargo.toml and yak_python/pyproject.toml
git tag v0.9.0
git push origin v0.9.0
```

This triggers the release workflow which:
- Builds CLI binaries and C libraries for 6 platform targets → GitHub Release
- Publishes `yak` and `yak_cl` to crates.io
- Builds Python wheels and publishes `libyak` to PyPI

**Manual publishing** (if needed):
- **crates.io**: `cargo publish -p yak` then `cargo publish -p yak_cl`
- **PyPI**: `cd yak_python && maturin publish`

Once published, install the Python bindings with `pip install libyak` (import as `import yak`).
