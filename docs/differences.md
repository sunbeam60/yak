# Differences from architecture.md

This file tracks divergences between the implementation and the architecture document.

## Python bindings use PyO3, not C FFI

**Architecture says:** "The library is tested using Python/pytest. This allows us to rapidly develop tests and to test the C FFI wrapper around the Rust library."

**Reality:** The Python test harness now uses PyO3 bindings (`stream_fs_python/`) that wrap `stream_fs` directly, bypassing the C FFI layer entirely. The C FFI wrapper (`stream_fs_c/`) is retained for future C consumers but is no longer exercised by the Python tests.

## Project table is out of date

**Architecture says:** The project table lists four modules (stream_fs, stream_fs_c, sfs_cl, sfs_pytest).

**Reality:** There are now five modules — `stream_fs_python/` (PyO3 Python bindings) was added as a workspace member.
