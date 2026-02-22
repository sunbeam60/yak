<p align="center">
<picture>
  <source srcset="https://github.com/user-attachments/assets/765a4352-acbd-45e7-b071-dae1d66342e7" media="(prefers-color-scheme: dark)">
  <img width="256" height="256" alt="An icon of a yak coming out of a square" src="https://github.com/user-attachments/assets/775aac25-e418-43ab-977f-b0fb4e3aa540" />
</picture>
</p>


# Yak - Yet Another Kontainer
Yak is a library that helps you to easily build your own binary file formats. 

Yak is a bit like the [Compound File Binary File](https://en.wikipedia.org/wiki/Compound_File_Binary_Format), but without the legacy. 

Yak is a file system, inside a file:

* A Yak file can have many streams.
* Organise streams into Yak directories - stream names cannot clash inside the same directory.
* Treat streams like you would files - write to them, read from them, truncate them, delete them, move them (inside the Yak file).
* Streams can grow or shrink inside the Yak file - Yak will figure out how to organise it efficiently.
* Seek in streams with O(log n) efficiency due to Yak's pyramid storage structure.
* Streams can be optionally compressed - Yak handles this transparently, compressing and decompressing as you read and write.
* Yak files can be optionally encrypted - Yak handles this transparently too, encrypting and decrypting as you read and write.

```python
from libyak import Yak, OpenMode

path = "example.yak"

# Create a new Yak file with default settings
yk = Yak.create(path)

# Write a demonstration text buffer
s1 = yk.create_stream("hello.txt")
yk.write(s1, b"The yak does not speak, but its presence speaks volumes.")
yk.close_stream(s1)

# Close the Yak file
yk.close()

# Read back the text stream
yk = Yak.open(path, OpenMode.Read)

# We stored it as "hello.txt" but you can use whatever
h = yk.open_stream("hello.txt", OpenMode.Read)

# Read into buffer
buf = yk.read(h, 56)

# We've shown text here, but you can write all types of data
assert buf == b"The yak does not speak, but its presence speaks volumes."

yk.close_stream(h)
yk.close()
```

Yak is implemented in Rust and [published on crates.io](https://crates.io/crates/yak) but can be used in many languages:
* C/C++ via the included yak_c library, which builds to a static lib and a .h file using CBindGen.
* Python via yak_python library, which is [published as a PyPI wheel](https://pypi.org/project/libyak/), using PyO3 to create native Python bindings.
* Any language that can use C libraries (dynamic or static), such as Objective-C, Zig, D, Lua & LuaJIT, C#, Haskell, Node.js etc.

## Ready to go
Yak is reasonably well optimized and pulls a number of tricks to balance these desired features:
* A simple, name-based filing system.
* Automatic space management - even after you've written a stream and closed it, you can reopen the same stream much later and write more stuff to it.
* Fast seeking within streams, without holding lots of extra data - the underlying pyramid data structure wastes very little space and is still very efficient.
* Minimal disk IO - Yak will optimize for long, contiguous segments of data to get near-raw-buffer reading & writing speed and use a configurable write-through cache to avoid disk IO altogether for hotspots in the file.
* Thread safety - multiple threads can write to multiple streams at the same time.
* Endian aware - if you need a Yak file to move between little endian and big endian systems, it can.
* Transparent, fast compression - using LZ4, any data written to a stream gets automatically compressed and any data read gets automatically decompressed.
* Transparent, fast encryption - using AES-XTS with hardware based encryption/decryption (where hardware is available).

... but due to the architecture of Yak, you can optionally write a new layer that rethinks how streams are linked together from blocks, how blocks are handled and how streams are addressed. By default, Yak is ready to go, but it can also be a toolbox for you to do something custom. Yak includes a broad test suite (~200 tests) that'll help you stress-test anything custom you write.

Yak includes a command line utility to create, modify and optimize Yak files.

```nushell
~/yak-test> yak create testfile.yak
Created Yak file: testfile.yak
~/yak-test> yak put testfile.yak test3.mp4 test3.mp4
Imported 64435159 bytes from test3.mp4 to test3.mp4 (4875.2 MB/s)
~/yak-test> yak putc testfile.yak test6.md test6.md
Imported 4283120 bytes from test6.md to test6.md (compressed, 182.3 MB/s)
~/yak-test> yak ls testfile.yak
STREAM test3.mp4
STREAM test6.md
~/yak-test> yak mkdir testfile.yak "folder1"
Created directory: folder1
~/yak-test> yak put testfile.yak test5.pdf "folder1/test5.pdf"
Imported 51739870 bytes from test5.pdf to folder1/test5.pdf (6122.1 MB/s)
~/yak-test> yak ls testfile.yak
DIR    folder1
STREAM test3.mp4
STREAM test6.md
~/yak-test> yak ls testfile.yak "folder1"
STREAM test5.pdf
~/yak-test>
```

In addition it includes a broad benchmark suite you can use to understand Yak's default performance and the performance of any customised layers you build:
```nushell
~/yak-test> yak bench
Usage: yak bench <scenario> [--block-shift N] [--index-width N] [--cbss N] [--threads N] [--case CASES]

Each scenario runs once per selected case (default: all four).

Scenarios:
  large-write      Write 5 streams of 30MB each
  small-write      Write 180 streams of 10KB each
  large-read       Read back 17x10MB + 17x20MB (510MB total)
  small-read       Read back 2750 streams of 10KB each
  threaded-write   T threads writing streams concurrently
  threaded-read    T threads reading streams concurrently
  threaded-mixed   T/2 readers + T/2 writers concurrently
  churn            Write then delete many streams (5 rounds)
  reuse            Fresh vs recycled-block read throughput
  single-stream    Write + read one 64MB stream (contiguous I/O test)
  warm-read        Write then read 2750x10KB streams (same instance, warm cache)
  overwrite        Write 10MB then 5000 overwrites (compress/decompress stress)
  all              Run all scenarios in sequence

Options:
  --block-shift N  Block size as power of 2 (default: 12 = 4KB blocks)
                   Examples: 10 = 1KB, 12 = 4KB, 16 = 64KB
  --index-width N  Block index size (default: 4 = 4B index / 32 bits)
                   Examples: 2 = 2B index, 4 = 4B index, 8 = 8B index
  --cbss N         Compressed block size shift for compression (default: 15 = 32KB)
                   Examples: 13 = 8KB, 15 = 32KB, 17 = 128KB
  --threads N      Thread count for threaded scenarios (default: 4)
  --case CASES     Which cases to run (default: necb = all four)
                   n = normal, e = encrypted, c = compressed, b = both
                   Examples: --case nc (normal + compressed)
                             --case c  (compressed only)
```

## Yak is not ...
* A database - Yak thinks in binary only and doesn't care or know what you write to your streams.
* ACID compliant - but then neither is your homegrown file format (most likely). If you need Atomic, Consistent, Isolated and Durable writes to your file, you should choose a [different format](https://sqlite.org). Yak does include functionality to verify the integrity of a Yak file.
* An archive format - To optimize for speed, random seeks within streams and mutability of streams, Yak will not achieve the same compression ratio as a modern archive.

## ... but Yak could be
* How you save your app's data
* How you load your game's assets
* How you compactly receive a bunch of streaming data
* How you rewire that other library's file-spew into a single location
* How you get started writing data to a file, until you've got that perfect, custom solution
* How you handle a number of threads writing to the same file, at the same time
* An alternative to [rust-cfb](https://github.com/mdsteele/rust-cfb)

## Want to know more about how Yak works?
Read the [Architecture Overview](https://github.com/sunbeam60/yak/blob/master/docs/architecture.md)

## Quick Start

### Building

```bash
# Build all crates (library, C FFI, CLI)
cargo build

# Build just the CLI tool
cargo build --package yak_cl
```

### Testing

```bash
cd yak_pytest

# Run all tests (thread safety tests burn for 10s by default, but you can override)
python -m pytest tests/ -v --burn-seconds=1
```

## Project Structure

```
yak/             - Core Rust library (L4 + L3 + L2 traits and implementations)
yak_c/           - C FFI wrapper for language interop
yak_pytest/      - Python bindings and test suite
yak_cl/          - Command-line tool
docs/            - Architecture and design documentation
graphics/        - Yak logo
```

## Mock Backends (for debugging)
Yak includes a couple of supporting debugging layers to simply development of your own file format, should you choose to fork.
- `YakBlockFileBacked = Yak<StreamsFromBlocks<BlocksFromFiles>>` -- L2 mock (numbered `.block` files)
- `YakFileBacked = Yak<StreamsFromFiles>` -- L3 mock (numbered `.stream` files)
