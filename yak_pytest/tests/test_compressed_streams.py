"""Tests for compressed streams — leaf-level lz4 compression in L3."""

import os
import pytest
import libyak as yak


# Default parameters: 4KB blocks, 32KB compressed blocks
BLOCK_INDEX_WIDTH = 4
BLOCK_SIZE_SHIFT = 12  # 4096 bytes
COMPRESSED_BLOCK_SIZE_SHIFT = 15  # 32768 bytes
COMPRESSED_BLOCK_SIZE = 1 << COMPRESSED_BLOCK_SIZE_SHIFT
BLOCK_SIZE = 1 << BLOCK_SIZE_SHIFT


class TestCompressedLifecycle:
    """Create, open, close compressed Yak files."""

    def test_create_with_compression(self, tmp_path):
        """Create a Yak file with compression configured."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        fs.close()
        assert os.path.exists(yak_path)

    def test_reopen_compressed_yak(self, tmp_path):
        """Create compressed Yak file, close, reopen."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        fs.close()
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        fs.close()


class TestCompressedStreamIO:
    """Create, write, read compressed streams."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        yield f
        f.close()

    def test_create_compressed_stream(self, fs):
        """Create a compressed stream, verify it exists."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.close_stream(handle)
        entries = fs.list()
        assert any(e.name == "data.bin" for e in entries)

    def test_write_and_read(self, fs):
        """Write to compressed stream, seek back, read it."""
        handle = fs.create_stream("data.bin", compressed=True)
        data = b"hello compressed world"
        fs.write(handle, data)
        fs.seek(handle, 0)
        result = fs.read(handle, len(data))
        assert result == data
        fs.close_stream(handle)

    def test_write_extends_stream(self, fs):
        """Writing to compressed stream extends its length."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"hello")
        assert fs.stream_length(handle) == 5
        fs.write(handle, b" world")
        assert fs.stream_length(handle) == 11
        fs.close_stream(handle)

    def test_partial_compressed_block(self, fs):
        """Write less than one compressed block, read back correctly."""
        data = b"small data"
        handle = fs.create_stream("small.bin", compressed=True)
        fs.write(handle, data)
        fs.seek(handle, 0)
        result = fs.read(handle, len(data))
        assert result == data
        fs.close_stream(handle)

    def test_exact_compressed_block(self, fs):
        """Write exactly one compressed block of data."""
        data = bytes(range(256)) * (COMPRESSED_BLOCK_SIZE // 256)
        assert len(data) == COMPRESSED_BLOCK_SIZE
        handle = fs.create_stream("exact.bin", compressed=True)
        fs.write(handle, data)
        assert fs.stream_length(handle) == COMPRESSED_BLOCK_SIZE
        fs.seek(handle, 0)
        result = fs.read(handle, COMPRESSED_BLOCK_SIZE)
        assert result == data
        fs.close_stream(handle)

    def test_multi_compressed_block(self, fs):
        """Write data spanning multiple compressed blocks."""
        size = COMPRESSED_BLOCK_SIZE * 3 + 1000
        data = bytes(i & 0xFF for i in range(size))
        handle = fs.create_stream("multi.bin", compressed=True)
        fs.write(handle, data)
        assert fs.stream_length(handle) == size
        fs.seek(handle, 0)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)

    def test_overwrite_within_compressed_block(self, fs):
        """Write, then overwrite a portion within the same compressed block."""
        original = b"A" * 1000
        handle = fs.create_stream("over.bin", compressed=True)
        fs.write(handle, original)

        # Overwrite bytes 200-299 with 'B'
        fs.seek(handle, 200)
        fs.write(handle, b"B" * 100)

        # Read back full content
        fs.seek(handle, 0)
        result = fs.read(handle, 1000)
        expected = b"A" * 200 + b"B" * 100 + b"A" * 700
        assert result == expected
        fs.close_stream(handle)

    def test_seek_and_write_in_middle(self, fs):
        """Seek to middle of compressed stream, write, verify surrounding data."""
        data = bytes(range(256)) * 40  # 10240 bytes
        handle = fs.create_stream("middle.bin", compressed=True)
        fs.write(handle, data)

        # Overwrite at offset 5000
        patch = b"PATCHED!"
        fs.seek(handle, 5000)
        fs.write(handle, patch)

        # Read back and verify
        fs.seek(handle, 0)
        result = fs.read(handle, len(data))
        expected = bytearray(data)
        expected[5000:5000 + len(patch)] = patch
        assert result == bytes(expected)
        fs.close_stream(handle)

    def test_read_at_end(self, fs):
        """Reading at end of compressed stream returns empty."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"hello")
        result = fs.read(handle, 100)
        assert result == b""
        fs.close_stream(handle)


class TestCompressedPersistence:
    """Compressed stream data survives close/reopen."""

    def test_persistence_small(self, tmp_path):
        """Write small data, close, reopen, read back."""
        yak_path = str(tmp_path / "test.yak")
        data = b"persistent compressed data"

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, data)
        fs.close_stream(handle)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)
        handle = fs.open_stream("data.bin", yak.OpenMode.READ)
        result = fs.read(handle, len(data))
        assert result == data
        fs.close_stream(handle)
        fs.close()

    def test_persistence_large(self, tmp_path):
        """Write multi-compressed-block data, close, reopen, read back."""
        yak_path = str(tmp_path / "test.yak")
        size = COMPRESSED_BLOCK_SIZE * 2 + 5000
        data = bytes(i & 0xFF for i in range(size))

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("big.bin", compressed=True)
        fs.write(handle, data)
        fs.close_stream(handle)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)
        handle = fs.open_stream("big.bin", yak.OpenMode.READ)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)
        fs.close()


class TestMixedStreams:
    """Compressed and uncompressed streams in the same Yak file."""

    def test_mixed_create_and_read(self, tmp_path):
        """Create both compressed and uncompressed streams, verify both work."""
        yak_path = str(tmp_path / "test.yak")
        plain_data = b"uncompressed content"
        comp_data = b"compressed content"

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)

        # Create uncompressed stream (normal create_stream)
        h_plain = fs.create_stream("plain.bin")
        fs.write(h_plain, plain_data)
        fs.close_stream(h_plain)

        # Create compressed stream
        h_comp = fs.create_stream("comp.bin", compressed=True)
        fs.write(h_comp, comp_data)
        fs.close_stream(h_comp)

        fs.close()

        # Reopen and verify both
        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)

        h_plain = fs.open_stream("plain.bin", yak.OpenMode.READ)
        assert fs.read(h_plain, len(plain_data)) == plain_data
        fs.close_stream(h_plain)

        h_comp = fs.open_stream("comp.bin", yak.OpenMode.READ)
        assert fs.read(h_comp, len(comp_data)) == comp_data
        fs.close_stream(h_comp)

        fs.close()


class TestCompressibility:
    """Verify that compression actually reduces file size."""

    def test_compressible_data_smaller(self, tmp_path):
        """Highly compressible data should produce a smaller file than uncompressed."""
        comp_path = str(tmp_path / "compressed.yak")
        plain_path = str(tmp_path / "plain.yak")
        size = COMPRESSED_BLOCK_SIZE * 4  # 128KB of zeros — highly compressible

        # Write compressed
        fs = yak.Yak.create(comp_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("zeros.bin", compressed=True)
        fs.write(handle, b"\x00" * size)
        fs.close_stream(handle)
        fs.close()

        # Write uncompressed (same data)
        fs = yak.Yak.create(plain_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("zeros.bin")
        fs.write(handle, b"\x00" * size)
        fs.close_stream(handle)
        fs.close()

        comp_file_size = os.path.getsize(comp_path)
        plain_file_size = os.path.getsize(plain_path)
        assert comp_file_size < plain_file_size, (
            f"Compressed {comp_file_size} should be < plain {plain_file_size}"
        )

    def test_incompressible_data_still_correct(self, tmp_path):
        """Random data may not compress but should still be correct."""
        import random

        yak_path = str(tmp_path / "test.yak")
        random.seed(42)
        data = bytes(random.getrandbits(8) for _ in range(COMPRESSED_BLOCK_SIZE + 1000))

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("random.bin", compressed=True)
        fs.write(handle, data)
        fs.close_stream(handle)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)
        handle = fs.open_stream("random.bin", yak.OpenMode.READ)
        result = fs.read(handle, len(data))
        assert result == data
        fs.close_stream(handle)
        fs.close()


class TestCompressedTruncate:
    """Truncation of compressed streams."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        yield f
        f.close()

    def test_truncate_to_zero(self, fs):
        """Truncate compressed stream to zero."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"x" * 10000)
        assert fs.stream_length(handle) == 10000
        fs.truncate(handle, 0)
        assert fs.stream_length(handle) == 0
        fs.close_stream(handle)

    def test_truncate_mid_stream(self, fs):
        """Truncate compressed stream to a shorter length."""
        size = COMPRESSED_BLOCK_SIZE * 3
        data = bytes(i & 0xFF for i in range(size))
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, data)

        new_len = COMPRESSED_BLOCK_SIZE + 5000
        fs.truncate(handle, new_len)
        assert fs.stream_length(handle) == new_len

        fs.seek(handle, 0)
        result = fs.read(handle, new_len)
        assert result == data[:new_len]
        fs.close_stream(handle)


class TestCompressedDelete:
    """Deleting compressed streams."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        yield f
        f.close()

    def test_delete_compressed_stream(self, fs):
        """Delete a compressed stream, verify it no longer exists."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"x" * 10000)
        fs.close_stream(handle)

        fs.delete_stream("data.bin")
        entries = fs.list()
        assert not any(e.name == "data.bin" for e in entries)

    def test_delete_then_recreate(self, fs):
        """Delete compressed stream, create a new one with same name."""
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"first")
        fs.close_stream(handle)

        fs.delete_stream("data.bin")

        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"second")
        fs.seek(handle, 0)
        result = fs.read(handle, 6)
        assert result == b"second"
        fs.close_stream(handle)


class TestCompressedVerify:
    """Integrity verification of compressed streams."""

    def test_verify_after_create(self, tmp_path):
        """Verify passes after creating compressed stream."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"test data")
        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == [], f"Issues: {issues}"
        fs.close()

    def test_verify_after_mixed_operations(self, tmp_path):
        """Verify passes after mixed compressed/uncompressed operations."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)

        # Compressed stream
        h = fs.create_stream("comp.bin", compressed=True)
        fs.write(h, b"x" * (COMPRESSED_BLOCK_SIZE * 2))
        fs.close_stream(h)

        # Uncompressed stream
        h = fs.create_stream("plain.bin")
        fs.write(h, b"y" * 5000)
        fs.close_stream(h)

        # Delete one, add another
        fs.delete_stream("plain.bin")
        h = fs.create_stream("comp2.bin", compressed=True)
        fs.write(h, b"z" * 1000)
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Issues: {issues}"
        fs.close()

    def test_verify_after_reopen(self, tmp_path):
        """Verify passes after close/reopen."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        h = fs.create_stream("data.bin", compressed=True)
        fs.write(h, b"persistent" * 1000)
        fs.close_stream(h)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = fs.verify()
        assert issues == [], f"Issues: {issues}"
        fs.close()


class TestCompressedReadOnly:
    """Read-only mode with compressed streams."""

    def test_write_fails_readonly(self, tmp_path):
        """Writing to compressed stream opened read-only fails."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        h = fs.create_stream("data.bin", compressed=True)
        fs.write(h, b"data")
        fs.close_stream(h)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = fs.open_stream("data.bin", yak.OpenMode.READ)
        with pytest.raises(yak.YakError):
            fs.write(h, b"nope")
        fs.close_stream(h)
        fs.close()


class TestCompressedLargeData:
    """Large data tests for compressed streams."""

    def test_one_megabyte_compressible(self, tmp_path):
        """Write 1MB of compressible data, verify correctness."""
        yak_path = str(tmp_path / "test.yak")
        size = 1024 * 1024
        # Repeating pattern — highly compressible
        data = bytes(i & 0xFF for i in range(size))

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, BLOCK_SIZE_SHIFT)
        handle = fs.create_stream("big.bin", compressed=True)
        fs.write(handle, data)
        fs.seek(handle, 0)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)
        fs.close()


class TestCompressedErrors:
    """Error conditions for compressed streams."""

    def test_compressed_works_on_default_create(self, tmp_path):
        """Compressed streams work on a default-created Yak file (cbss always enabled)."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, b"hello")
        fs.seek(handle, 0)
        assert fs.read(handle, 5) == b"hello"
        fs.close_stream(handle)
        fs.close()


class TestCompressedParametrized:
    """Test with different block size / compressed block size combinations."""

    @pytest.mark.parametrize(
        "bss,cbss",
        [
            (10, 13),  # 1KB blocks, 8KB compressed
            (12, 15),  # 4KB blocks, 32KB compressed
            (12, 16),  # 4KB blocks, 64KB compressed
        ],
    )
    def test_write_read_various_sizes(self, tmp_path, bss, cbss):
        """Write and read back with various block/compressed block sizes."""
        yak_path = str(tmp_path / f"test_{bss}_{cbss}.yak")
        cbs = 1 << cbss
        # Write 3 compressed blocks + partial
        size = cbs * 3 + 500
        data = bytes(i & 0xFF for i in range(size))

        fs = yak.Yak.create(yak_path, BLOCK_INDEX_WIDTH, bss, cbss)
        handle = fs.create_stream("data.bin", compressed=True)
        fs.write(handle, data)
        fs.seek(handle, 0)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)
        fs.close()
