# Stream File System (SFS)

## Conventions
- L1                = Layer 1
- L2                = Layer 2
- L3                = Layer 3
- L4                = Layer 4

## Human & AI, working together
Before committing, at all times present & allow edits to the commit message to the user. Do not include "co-authored by Claude" message.
Humans will use the extension Todo Tree extension. It's good practice to use //TODO: and //FIXME: comments where appropriate, to give humans an overview over possible improvements or concerns in the code. Literal constants in code should be avoided. It's brittle and humans don't deal well with them. Minimize the use of defined constants to as few places as possible.

Readability of the code for humans is important. If constants are being added, explain a comment what the constant signifies.

## Architecture
Please read the [architecture](./../docs/architecture.md) for a comprehensive overview of SFS. This architecture must be respected - if it cannot, please stop, inform and ask for directions.

Whenever you are updating .md files, do not ever change ./../docs/architecture.md withot asking the user and explaining why you wish to change this file, what is wrong with the architecture and what you propose to change. Keep track of divergences from the defined architecture in [differences](./../docs/differences.md).

## Performance and memory
This is a low level library. Memory allocations should be minimized when possible. Performance matters.

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

## Warnings are errors - keeping it clean
If there's a cleaner/neater way to do something, please suggest it.
Don't use #[allow()] to get around warnings. Warnings from clippy shouldn't be worked around, they should be fixed.