# Yak

## Conventions
- L1                = Layer 1
- L2                = Layer 2
- L3                = Layer 3
- L4                = Layer 4

## Human & AI, working together
Before committing, at all times present & allow edits to the commit message to the user. Do not include "co-authored by Claude" message - human committers own the accountability.

Humans will use the extension Todo Tree extension. It's good practice to use //TODO: and //FIXME: comments where appropriate, to give humans an overview over possible improvements or concerns in the code. Literal constants in code should be avoided. It's brittle and humans don't deal well with them. Minimize the use of defined constants to as few places as possible.

Readability of the code for humans is important. If constants are being added, explain in a comment what the constant signifies.

When a human is making suggestions and something seems more trouble than it's worth, it's ok to question the human's intent, offering up pros and cons of the suggestions before asking for directions.

When a new, larger piece of work is to be undertaken, please create a worktree & a branch for the work so it can be separated and benchmarked in isolation, against what is currently in master.

## Architecture
Please read the [architecture](./../docs/architecture.md) for a comprehensive overview of Yak. This architecture must be respected - if it cannot, please stop, inform and ask for directions.

Whenever you are updating .md files, do not ever change ./../docs/architecture.md withot asking the user and explaining why you wish to change this file, what is wrong with the architecture and what you propose to change. Keep track of divergences from the defined architecture in [differences](./../docs/differences.md).

Duplication of code should be concerning when we write code. If duplicated code exists, and there's no performance penalty to extracting it into a function, we should consider doing so. In release mode, small functions are likely to be inlined by the compiler.

If a function gets above 100 lines, question whether some functionality can be extracted from this function and, if a similar function already exits that isn't quite right can be made generic to work for both places. This isn't a strict rule, but very long functions are mostly a sign that we're throwing too much code down - this may work for an AI but not for a human that wants to read and edit together with the AI.

Use generics rather than dynamic dispatch when possible.

If a function starts to acquire a lot of parameters and a lot of variables internally, usually the function is doing too many things at once. Can we extract and re-use functionality instead?

It's not healthy for us to store multiple versions of the same truth. If one truth can be derived from the other with some computation and no IO, we should derive the other truth when we need it.

## Performance and memory
This is a low level library. Memory allocations should be minimized when possible. Performance matters.
Samply is used for profiling release builds. Please see ./.vscode/tasks.json for how they are defined.

If we have to perform disk read/writing (i.e. if we have to ask L2 for a block or L1 to read/write), we must try to avoid doing that inside of a loop, unless absolutely required. Consider whether we can read the data once and then iterate through the data in the local loop, instead of continually asking for reading/writing at disk level inside of the loop. Even if we have a cache, touching the cache is not free. 

## Release
Whenever a new version is tagged and pushed to Github, workflows publish to crates.io and pypi.org. Before pushing a new version tag, please ensure that the README.md in the workspace root is aligned with the README.md in yak_python so that the description of Yak across github, crates.io and pypi.org are aligned and appropriate for the audience on those three platforms.

## Workspace harness
Whenever you make changes, they must be in sync across the five key projects:
./../yak/               - the main project for the library
./../yak_cl/            - the command line utility that enables command line manipulation of .yak files
./../yak_c/             - the C FFI wrapper around the main yak module
./../yak_python/        - the PyO3 Python bindings around the main yak module (installed via `maturin develop`)
./../yak_pytest/        - the python testing harness that enables rapid testing of .yak files the yak library

## Test Driven Development
As far as possible, when planning features, help the author first plan out how this feature should be tested in the pytest library. Only after tests have been planned and written, and failed, should we move to implementation and satisfy the tests. This won't always be possible, but whenever it is, this is the preferred method for development. Whenever large changes have been achieved, the full test suite in yak_pytest must pass too.

## Layers in Yak must be respected
It is critical that the four layers in Yak are kept as decoupled as possible: L4 should only know about L3, L3 should only know about L2, L2 should only know about L1. If you find yourself writing code that ignores this architectural decoupling, stop, inform and ask for directions.

## Warnings are errors - keeping it clean
If there's a cleaner/neater way to do something, please suggest it.
Don't use #[allow()] to get around warnings. Warnings from clippy shouldn't be worked around, they should be fixed.
There's a git hook to run fmt and clippy before we push to remote master. Please finish off big tasks by running both and fixing any warnings.

## Some files are not to be modified
Do not modify the following files without explicit permission:
* ./docs/architecture.md
* ./README.md

## Still in development
Version changes to the underlying storage formats is not a large problem. We are still developing this and no existing files are in the wild, needing to be considered.

## AES hardware acceleration
Yak uses the `aes` crate (0.8+) for AES-XTS block encryption at L2. Hardware acceleration depends on platform-specific configuration in `.cargo/config.toml`:
- **x86_64**: The `aes` crate auto-detects AES-NI at runtime. No build flags needed.
- **aarch64 (ARM64)**: Requires `--cfg aes_armv8` in rustflags to enable runtime detection
  of ARMv8 crypto extensions. Without this flag, ARM builds silently fall back to a pure
  software AES implementation. All aarch64 targets must have this flag in `.cargo/config.toml`.
- See https://docs.rs/aes/0.8.4/aes/ for the list of configuration flags.

## Building Pything bindings with Maturin
Maturin is set up via the .venv. Build the PyO3 with that, rather than attempting to locate it manually.3