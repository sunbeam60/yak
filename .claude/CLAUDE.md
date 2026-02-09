# Stream File System (SFS)

## Conventions
- L1                = Layer 1
- L2                = Layer 2
- L3                = Layer 3
- L4                = Layer 4

## Architecture
Please read the [architecture](./../docs/architecture.md) for a comprehensive overview of SFS. This architecture must be respected - if it cannot, please stop, inform and ask for directions.

Whenever you are updating .md files, do not ever change ./../docs/architecture.md withot asking the user and explaining why you wish to change this file, what is wrong with the architecture and what you propose to change.

## Workspace harness
Whenever you make changes, they must be in sync across the four key projects:
./../stream_fs/     - the main project for the library
./../sfs_cl/        - the command line utility that enables command line manipulation of .stream_fs files
./../stream_fs_c    - the C FFI wrapper around the main stream_fs module
./../sfs_pytest     - the python testing harness that enables rapid testing of SFS files the stream_fs library

## Test Driven Development
As far as possible, when planning features, help the author first plan out how this feature should be tested in the pytest library. Only after tests have been planned and written, and failed, should we move to implementation and satisfy the tests. This won't always be possible, but whenever it is, this is the preferred method for development.

## Layers in SFS must be respected
It is critical that the four layers in SFS are kept as decoupled as possible: L4 should only know about L3, L3 should only know about L2, L2 should only know about L1. If you find yourself writing code that ignores this architectural decoupling, stop, inform and ask for directions.

## Implementation approach
1. L4 Mock: Implement L4, but instead of calling into L3 (which doesn't yet exist), when creating an SFS file, instead create a directory on disk and mock out streams with real files. The aim is the get the API of L4 so that it "feels" to a caller that SFS is fully working ... when in fact it's creating a directory with real files. Simulate the hierarchy of files inside an SFS file with sub-directories.
2. L3 Mock: Implement L3 and plug it into L4, but instead of L3 calling into L2, L3 mocks out each stream inside of the SFS using a real file on disk.
3. L2 Mock: Implement L2 and plug it into L3, but instead of L2 writing blocks into a real file, simulate each block with a numbered file on disk.
4. L1 Mock: Implement L1 and plug it into L2.

### Current project phase
**L4 Mock: ✅ COMPLETE** - See [L4 mock](./../docs/L4_mock.md) for full documentation.

All 35 tests passing. Ready to proceed to L3 Mock phase when you're ready.

### Implementation Status Summary

| Phase | Component | Status | Tests |
|-------|-----------|--------|-------|
| L4 Mock | Rust core (`stream_fs`) | ✅ Complete | - |
| L4 Mock | C FFI (`stream_fs_c`) | ✅ Complete | - |
| L4 Mock | Python bindings (`sfs_pytest/sfs`) | ✅ Complete | - |
| L4 Mock | CLI tool (`sfs_cl`) | ✅ Complete | - |
| L4 Mock | Test suite | ✅ Complete | 35/35 |
| **Total** | **L4 Mock** | **✅ COMPLETE** | **35/35** |

### Next Steps (L3 Mock Phase)
When ready to proceed:
1. Define L3 trait for stream abstraction
2. Make L4 generic over L3 trait
3. Implement L3 mock that uses real files for each stream (one file per stream)
4. Update/extend tests to validate L3 layer separation
5. Update CLI to work with L3-backed L4