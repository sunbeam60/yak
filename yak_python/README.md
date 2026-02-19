# libyak

Python bindings for [Yak](https://github.com/sunbeam60/yak) — yet another kontainer. A layered, embeddable file-in-file storage system written in Rust.

## Installation

```bash
pip install libyak
```

## Quick Start

```python
import yak

# Create a new Yak file
yk = yak.Yak.create("mydata.yak")

# Write a stream
sh = yk.create_stream("hello.txt", compressed=False)
yk.write(sh, b"Hello, World!")
yk.close_stream(sh)

# Read it back
sh = yk.open_stream("hello.txt", yak.OpenMode.Read)
data = yk.read(sh, 1024)
print(data)  # b"Hello, World!"
yk.close_stream(sh)

# Directory operations
yk.mkdir("docs")
sh = yk.create_stream("docs/readme.txt", compressed=True)
yk.write(sh, b"Compressed content")
yk.close_stream(sh)

# List directory contents
for entry in yk.list(""):
    print(f"{entry.name} ({entry.entry_type})")

yk.close()
```

## Features

- Single-file storage with hierarchical directories and named streams
- Optional LZ4 compression per stream
- Optional AES-256-XTS encryption
- Thread-safe with interior mutability
- File optimization (compaction and defragmentation)

## Encryption

```python
# Create an encrypted Yak file
yk = yak.Yak.create("secret.yak", password=b"my-password")
# ... use normally ...
yk.close()

# Re-open with password
yk = yak.Yak.open("secret.yak", yak.OpenMode.Write, password=b"my-password")
```

## Optimization

```python
# Compact a Yak file (remove free blocks, maximize contiguity)
bytes_saved = yak.Yak.optimize("mydata.yak")
```

## License

MIT OR Apache-2.0
