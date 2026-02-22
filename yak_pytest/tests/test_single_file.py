"""L1 tests: Yak stored as a single file on disk."""

import os
import pytest
from pathlib import Path
import libyak as yak


class TestSingleFile:
    """Verify that YakDefault produces a single file, not a directory."""

    def test_create_produces_file(self, tmp_path):
        """Create a Yak file -- result is a regular file, not a directory."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        assert Path(yak_path).is_file(), "Yak should be a regular file"
        assert not Path(yak_path).is_dir(), "Yak should not be a directory"
        f.close()

    def test_file_has_nonzero_size(self, tmp_path):
        """A freshly created Yak file has a header (non-zero size)."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()
        size = os.path.getsize(yak_path)
        assert size > 0, "Yak file should have a header"

    def test_file_grows_with_data(self, tmp_path):
        """Writing data to streams makes the file grow."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()
        initial_size = os.path.getsize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = f.create_stream("bigfile")
        f.write(h, b"X" * 100_000)
        f.close_stream(h)
        f.close()

        final_size = os.path.getsize(yak_path)
        assert final_size > initial_size + 100_000, (
            f"File should have grown by at least 100KB: {initial_size} -> {final_size}"
        )


class TestSingleFilePersistence:
    """Verify data survives close/reopen cycles on the single-file backend."""

    def test_create_close_reopen_list(self, tmp_path):
        """Directories and streams survive close/reopen."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        f.mkdir("docs")
        f.mkdir("docs/images")
        h = f.create_stream("readme")
        f.write(h, b"hello")
        f.close_stream(h)
        h = f.create_stream("docs/images/logo")
        f.write(h, b"PNG")
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        root = {e.name for e in f.list()}
        assert "docs" in root
        assert "readme" in root

        images = {e.name for e in f.list("docs/images")}
        assert "logo" in images

        h = f.open_stream("readme", yak.OpenMode.READ)
        assert f.read(h, 100) == b"hello"
        f.close_stream(h)
        f.close()

    def test_multiple_reopen_cycles(self, tmp_path):
        """Data survives multiple close/reopen cycles."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        h = f.create_stream("counter")
        f.write(h, b"\x00")
        f.close_stream(h)
        f.close()

        for i in range(1, 6):
            f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
            h = f.open_stream("counter", yak.OpenMode.READ)
            data = f.read(h, 1)
            assert data == bytes([i - 1]), f"cycle {i}: expected {i-1}, got {data}"
            f.close_stream(h)

            h = f.open_stream("counter", yak.OpenMode.WRITE)
            f.seek(h, 0)
            f.write(h, bytes([i]))
            f.close_stream(h)
            f.close()

        # Final check
        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = f.open_stream("counter", yak.OpenMode.READ)
        assert f.read(h, 1) == b"\x05"
        f.close_stream(h)
        f.close()

    def test_large_data_persists(self, tmp_path):
        """Large stream data (multiple blocks) persists across reopen."""
        yak_path = str(tmp_path / "test.yak")

        # Use default 4KB blocks -- write 50KB to span many blocks
        data = bytes(range(256)) * 200  # 51200 bytes
        f = yak.Yak.create(yak_path)
        h = f.create_stream("big")
        f.write(h, data)
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = f.open_stream("big", yak.OpenMode.READ)
        length = f.stream_length(h)
        assert length == len(data)
        readback = f.read(h, length)
        assert readback == data, "large data mismatch after reopen"
        f.close_stream(h)
        f.close()


class TestCloseFlushesHandles:
    """Verify that Yak.close() flushes any open stream handles."""

    def test_close_flushes_open_write_handle(self, tmp_path):
        """Data persists when stream handle is not closed before Yak close."""
        yak_path = str(tmp_path / "test.yak")
        data = b"flush test data " * 100  # 1600 bytes

        f = yak.Yak.create(yak_path)
        handle = f.create_stream("unclosed")
        f.write(handle, data)
        # Deliberately NOT calling f.close_stream(handle)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        handle = f.open_stream("unclosed", yak.OpenMode.READ)
        readback = f.read(handle, len(data))
        assert readback == data, "Data must persist when Yak is closed with open write handles"
        f.close_stream(handle)
        f.close()

    def test_close_flushes_multiple_open_handles(self, tmp_path):
        """Multiple open write handles are all flushed when Yak is closed."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        f.mkdir("data")
        h1 = f.create_stream("data/stream_a")
        h2 = f.create_stream("data/stream_b")
        f.write(h1, b"alpha")
        f.write(h2, b"bravo")
        # Neither stream handle closed explicitly
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("data/stream_a", yak.OpenMode.READ)
        assert f.read(h, 10) == b"alpha"
        f.close_stream(h)
        h = f.open_stream("data/stream_b", yak.OpenMode.READ)
        assert f.read(h, 10) == b"bravo"
        f.close_stream(h)
        f.close()

    def test_close_with_open_read_handles(self, tmp_path):
        """Closing Yak with open reader handles succeeds without error."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        h = f.create_stream("readme")
        f.write(h, b"hello")
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("readme", yak.OpenMode.READ)
        f.read(h, 5)
        # Leave reader handle open
        f.close()  # Should not raise

    def test_verify_clean_after_unflushed_close(self, tmp_path):
        """Verify reports no issues after Yak is closed with open write handles."""
        yak_path = str(tmp_path / "test.yak")
        data = bytes(range(256)) * 50  # 12800 bytes, spans multiple blocks

        f = yak.Yak.create(yak_path)
        h = f.create_stream("verified")
        f.write(h, data)
        # Stream handle left open
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()


class TestBlockParameters:
    """Verify that different block_index_width and block_size_shift work."""

    @pytest.mark.parametrize("biw", [2, 3, 4, 8])
    def test_varying_block_index_width(self, tmp_path, biw):
        """Yak works with different block_index_width values."""
        yak_path = str(tmp_path / f"test_biw{biw}.yak")
        f = yak.Yak.create(yak_path, block_index_width=biw, block_size_shift=9)
        f.mkdir("data")
        h = f.create_stream("data/test")
        content = b"biw-test-" + bytes(range(256)) * 4
        f.write(h, content)
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = f.open_stream("data/test", yak.OpenMode.READ)
        readback = f.read(h, f.stream_length(h))
        assert readback == content
        f.close_stream(h)
        f.close()

    @pytest.mark.parametrize("bss", [6, 9, 12, 16])
    def test_varying_block_size_shift(self, tmp_path, bss):
        """Yak works with different block_size_shift values (64B to 64KB blocks)."""
        yak_path = str(tmp_path / f"test_bss{bss}.yak")
        # cbss must be >= bss; use bss + 3 to ensure validity for large block sizes
        cbss = max(bss + 3, 15)
        f = yak.Yak.create(yak_path, block_index_width=4, block_size_shift=bss,
                           compressed_block_size_shift=cbss)
        h = f.create_stream("test")
        # Write enough data to span a few blocks at the given block size
        block_size = 1 << bss
        content = bytes(range(256)) * ((block_size * 3) // 256 + 1)
        content = content[:block_size * 3]  # exactly 3 blocks worth
        f.write(h, content)
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = f.open_stream("test", yak.OpenMode.READ)
        readback = f.read(h, f.stream_length(h))
        assert readback == content
        f.close_stream(h)
        f.close()


class TestOptimize:
    """Verify that Yak.optimize() compacts and defragments a Yak file."""

    def test_optimize_reclaims_space(self, tmp_path):
        """Deleting streams and optimizing reclaims disk space."""
        yak_path = str(tmp_path / "test.yak")
        data_a = b"A" * 50_000
        data_b = b"B" * 50_000
        data_keep = b"K" * 10_000

        f = yak.Yak.create(yak_path)
        h = f.create_stream("stream_a")
        f.write(h, data_a)
        f.close_stream(h)
        h = f.create_stream("stream_b")
        f.write(h, data_b)
        f.close_stream(h)
        h = f.create_stream("keeper")
        f.write(h, data_keep)
        f.close_stream(h)
        f.close()

        # Delete the two big streams
        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f.delete_stream("stream_a")
        f.delete_stream("stream_b")
        f.close()

        size_before = os.path.getsize(yak_path)
        saved = yak.Yak.optimize(yak_path)
        size_after = os.path.getsize(yak_path)

        assert saved > 0, "optimize should report bytes saved"
        assert size_after < size_before, (
            f"File should be smaller after optimize: {size_before} -> {size_after}"
        )

        # Verify remaining data is intact
        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("keeper", yak.OpenMode.READ)
        assert f.read(h, f.stream_length(h)) == data_keep
        f.close_stream(h)
        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()

    def test_optimize_with_nested_directories(self, tmp_path):
        """Directory structure and streams survive optimize."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        f.mkdir("a")
        f.mkdir("a/b")
        f.mkdir("c")
        h = f.create_stream("a/file1")
        f.write(h, b"hello from a")
        f.close_stream(h)
        h = f.create_stream("a/b/file2")
        f.write(h, b"hello from a/b")
        f.close_stream(h)
        h = f.create_stream("c/file3")
        f.write(h, b"hello from c")
        f.close_stream(h)
        # Create and delete a stream to produce free blocks
        h = f.create_stream("a/trash")
        f.write(h, b"X" * 20_000)
        f.close_stream(h)
        f.delete_stream("a/trash")
        f.close()

        yak.Yak.optimize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        # Verify directories
        root = {e.name for e in f.list()}
        assert "a" in root
        assert "c" in root
        a_entries = {e.name for e in f.list("a")}
        assert "b" in a_entries
        assert "file1" in a_entries
        ab_entries = {e.name for e in f.list("a/b")}
        assert "file2" in ab_entries

        # Verify stream data
        h = f.open_stream("a/file1", yak.OpenMode.READ)
        assert f.read(h, 100) == b"hello from a"
        f.close_stream(h)
        h = f.open_stream("a/b/file2", yak.OpenMode.READ)
        assert f.read(h, 100) == b"hello from a/b"
        f.close_stream(h)
        h = f.open_stream("c/file3", yak.OpenMode.READ)
        assert f.read(h, 100) == b"hello from c"
        f.close_stream(h)

        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()

    def test_optimize_with_compressed_streams(self, tmp_path):
        """Compressed and uncompressed streams both survive optimize."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        # Compressed stream with repetitive data (compresses well)
        h = f.create_stream("compressed", compressed=True)
        comp_data = b"ABCDEFGH" * 10_000
        f.write(h, comp_data)
        f.close_stream(h)
        # Uncompressed stream
        h = f.create_stream("plain")
        plain_data = bytes(range(256)) * 100
        f.write(h, plain_data)
        f.close_stream(h)
        # Create and delete something to produce free blocks
        h = f.create_stream("trash")
        f.write(h, b"Z" * 40_000)
        f.close_stream(h)
        f.delete_stream("trash")
        f.close()

        yak.Yak.optimize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("compressed", yak.OpenMode.READ)
        assert f.is_stream_compressed(h) is True
        assert f.read(h, f.stream_length(h)) == comp_data
        f.close_stream(h)

        h = f.open_stream("plain", yak.OpenMode.READ)
        assert f.is_stream_compressed(h) is False
        assert f.read(h, f.stream_length(h)) == plain_data
        f.close_stream(h)

        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()

    def test_optimize_no_free_blocks(self, tmp_path):
        """Optimize on a file with no free blocks is a valid no-op."""
        yak_path = str(tmp_path / "test.yak")
        data = b"no wasted space" * 1000

        f = yak.Yak.create(yak_path)
        h = f.create_stream("data")
        f.write(h, data)
        f.close_stream(h)
        f.close()

        size_before = os.path.getsize(yak_path)
        yak.Yak.optimize(yak_path)
        size_after = os.path.getsize(yak_path)

        # Size should be roughly the same (new file may differ slightly due
        # to directory layout, but should not be significantly larger)
        assert size_after <= size_before + 4096, (
            f"Optimize should not bloat: {size_before} -> {size_after}"
        )

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("data", yak.OpenMode.READ)
        assert f.read(h, f.stream_length(h)) == data
        f.close_stream(h)
        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()

    def test_optimize_empty_file(self, tmp_path):
        """Optimize on a file with no streams is valid."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        f.close()

        yak.Yak.optimize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        entries = f.list()
        assert len(entries) == 0
        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()

    def test_optimize_strips_reserved_capacity(self, tmp_path):
        """Optimize strips excess reserved capacity from streams."""
        yak_path = str(tmp_path / "test.yak")

        f = yak.Yak.create(yak_path)
        h = f.create_stream("data")
        f.write(h, b"small")
        f.reserve(h, 1024 * 1024)  # Reserve 1MB for 5 bytes of data
        assert f.stream_reserved(h) == 1024 * 1024
        f.close_stream(h)
        f.close()

        yak.Yak.optimize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        h = f.open_stream("data", yak.OpenMode.READ)
        assert f.read(h, 100) == b"small"
        # Reserved should be stripped down to actual data needs
        reserved = f.stream_reserved(h)
        assert reserved < 1024 * 1024, (
            f"Reserved should be stripped: got {reserved}"
        )
        f.close_stream(h)
        issues = f.verify()
        assert issues == [], f"Verify found issues: {issues}"
        f.close()


class TestProcessLocking:
    """Verify that exclusive process-level locking is enforced."""

    def test_double_open_fails(self, tmp_path):
        """Cannot open the same Yak file twice simultaneously."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        f1 = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        with pytest.raises(yak.YakError, match="locked"):
            yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f1.close()

    def test_reopen_after_close_succeeds(self, tmp_path):
        """After closing, another open succeeds."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f.close()

    def test_create_on_existing_file_fails(self, tmp_path):
        """Cannot create where a file already exists."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        with pytest.raises(yak.YakError):
            yak.Yak.create(yak_path)
