# Stream File System (SFS)

## Conventions
- L1                = Layer 1
- L2                = Layer 2
- L3                = Layer 3
- L4                = Layer 4

## Human & AI, working together
Before committing, at all times present & allow edits to the commit message to the user. Do not include "co-authored by Claude" message.
Humans will use the extension Todo Tree extension. It's good practice to use //TODO: and //FIXME: comments where appropriate, to give humans an overview over possible improvements or concerns in the code.

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
As far as possible, when planning features, help the author first plan out how this feature should be tested in the pytest library. Only after tests have been planned and written, and failed, should we move to implementation and satisfy the tests. This won't always be possible, but whenever it is, this is the preferred method for development. Whenever large changes have been achieved, the full test suite in sfs_pytest must pass too.

## Layers in SFS must be respected
It is critical that the four layers in SFS are kept as decoupled as possible: L4 should only know about L3, L3 should only know about L2, L2 should only know about L1. If you find yourself writing code that ignores this architectural decoupling, stop, inform and ask for directions.

## Implementation approach
1. L4 Mock: Implement L4, but instead of calling into L3 (which doesn't yet exist), when creating an SFS file, instead create a directory on disk and mock out streams with real files. The aim is the get the API of L4 so that it "feels" to a caller that SFS is fully working ... when in fact it's creating a directory with real files. Simulate the hierarchy of files inside an SFS file with sub-directories.
2. L3 Mock: Implement L3 and plug it into L4, but instead of L3 calling into L2, L3 mocks out each stream inside of the SFS using a real file on disk.
3. L2 Mock: Implement L2 and plug it into L3, but instead of L2 writing blocks into a real file, simulate each block with a numbered file on disk.
4. L1 Mock: Implement L1 and plug it into L2.

### Current project phase
**L1: ✅ COMPLETE** - See [L1](./../docs/L1.md) for full documentation.

All 85 tests passing (68 original + 17 L1-specific), plus 3 cargo tests. SFS now operates as a true single-file filesystem.

### Implementation Status Summary

| Phase     | Component                           | Status         | Tests     |
| --------- | ----------------------------------- | -------------- | --------- |
| L4 Mock   | All components                      | ✅ Complete     | -         |
| L3 Mock   | `StreamLayer` trait (no generics)   | ✅ Complete     | -         |
| L3 Mock   | `StreamsFromFiles` (L3 impl)        | ✅ Complete     | -         |
| L3 Mock   | L4 rewrite (generic over L3)        | ✅ Complete     | -         |
| L3 Mock   | Runtime block params refactor       | ✅ Complete     | -         |
| L3 Mock   | Thread safety (`&self` + Mutex)     | ✅ Complete     | -         |
| L3 Mock   | C FFI (`stream_fs_c`)               | ✅ Complete     | -         |
| L3 Mock   | CLI tool (`sfs_cl`)                 | ✅ Complete     | -         |
| L3 Mock   | Thread safety burn-in tests         | ✅ Complete     | -         |
| L3 Mock   | Full test suite                     | ✅ Complete     | 49/49     |
| L2 Mock   | `BlockLayer` trait                  | ✅ Complete     | -         |
| L2 Mock   | `BlocksFromFiles` (L2 impl)         | ✅ Complete     | -         |
| L2 Mock   | `StreamsFromBlocks` (real L3)       | ✅ Complete     | -         |
| L2 Mock   | Header chain (L4→L3→L2→disk)        | ✅ Complete     | -         |
| L2 Mock   | `SfsDefault` updated                | ✅ Complete     | -         |
| L2 Mock   | Phase 8 multi-block tests           | ✅ Complete     | 19/19     |
| L2 Mock   | Thread safety burn-in               | ✅ Complete     | -         |
| L2 Mock   | Full test suite                     | ✅ Complete     | 68/68     |
| **Total** | **L2 Mock**                         | **✅ COMPLETE** | **68/68** |
| L1        | `FileLayer` trait (L1 contract)     | ✅ Complete     | -         |
| L1        | `FileOnDisk` (L1 impl)              | ✅ Complete     | -         |
| L1        | `BlocksInFile` (real L2)            | ✅ Complete     | -         |
| L1        | Header chain (L4→L3→L2→L1→disk)     | ✅ Complete     | -         |
| L1        | Two-pass create with `upper_layers` | ✅ Complete     | -         |
| L1        | `SfsDefault` = single-file backend  | ✅ Complete     | -         |
| L1        | Process-level locking (`fs2`)       | ✅ Complete     | -         |
| L1        | L1-specific test suite              | ✅ Complete     | 17/17     |
| L1        | Full test suite                     | ✅ Complete     | 85/85     |
| **Total** | **L1**                              | **✅ COMPLETE** | **85/85** |