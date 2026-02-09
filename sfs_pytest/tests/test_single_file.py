"""L1 tests: SFS stored as a single file on disk."""

import os
import pytest
from pathlib import Path
import sfs


class TestSingleFile:
    """Verify that SfsDefault produces a single file, not a directory."""

    def test_create_produces_file(self, tmp_path):
        """Create an SFS — result is a regular file, not a directory."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        assert Path(sfs_path).is_file(), "SFS should be a regular file"
        assert not Path(sfs_path).is_dir(), "SFS should not be a directory"
        f.close()

    def test_file_has_nonzero_size(self, tmp_path):
        """A freshly created SFS file has a header (non-zero size)."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()
        size = os.path.getsize(sfs_path)
        assert size > 0, "SFS file should have a header"

    def test_file_grows_with_data(self, tmp_path):
        """Writing data to streams makes the file grow."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()
        initial_size = os.path.getsize(sfs_path)

        f = sfs.Sfs.open(sfs_path)
        h = f.create_stream("bigfile")
        f.write(h, b"X" * 100_000)
        f.close_stream(h)
        f.close()

        final_size = os.path.getsize(sfs_path)
        assert final_size > initial_size + 100_000, (
            f"File should have grown by at least 100KB: {initial_size} -> {final_size}"
        )


class TestSingleFilePersistence:
    """Verify data survives close/reopen cycles on the single-file backend."""

    def test_create_close_reopen_list(self, tmp_path):
        """Directories and streams survive close/reopen."""
        sfs_path = str(tmp_path / "test.sfs")

        f = sfs.Sfs.create(sfs_path)
        f.mkdir("docs")
        f.mkdir("docs/images")
        h = f.create_stream("readme")
        f.write(h, b"hello")
        f.close_stream(h)
        h = f.create_stream("docs/images/logo")
        f.write(h, b"PNG")
        f.close_stream(h)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        root = {e.name for e in f.list()}
        assert "docs" in root
        assert "readme" in root

        images = {e.name for e in f.list("docs/images")}
        assert "logo" in images

        h = f.open_stream("readme", sfs.OpenMode.READ)
        assert f.read(h, 100) == b"hello"
        f.close_stream(h)
        f.close()

    def test_multiple_reopen_cycles(self, tmp_path):
        """Data survives multiple close/reopen cycles."""
        sfs_path = str(tmp_path / "test.sfs")

        f = sfs.Sfs.create(sfs_path)
        h = f.create_stream("counter")
        f.write(h, b"\x00")
        f.close_stream(h)
        f.close()

        for i in range(1, 6):
            f = sfs.Sfs.open(sfs_path)
            h = f.open_stream("counter", sfs.OpenMode.READ)
            data = f.read(h, 1)
            assert data == bytes([i - 1]), f"cycle {i}: expected {i-1}, got {data}"
            f.close_stream(h)

            h = f.open_stream("counter", sfs.OpenMode.WRITE)
            f.seek(h, 0)
            f.write(h, bytes([i]))
            f.close_stream(h)
            f.close()

        # Final check
        f = sfs.Sfs.open(sfs_path)
        h = f.open_stream("counter", sfs.OpenMode.READ)
        assert f.read(h, 1) == b"\x05"
        f.close_stream(h)
        f.close()

    def test_large_data_persists(self, tmp_path):
        """Large stream data (multiple blocks) persists across reopen."""
        sfs_path = str(tmp_path / "test.sfs")

        # Use default 4KB blocks — write 50KB to span many blocks
        data = bytes(range(256)) * 200  # 51200 bytes
        f = sfs.Sfs.create(sfs_path)
        h = f.create_stream("big")
        f.write(h, data)
        f.close_stream(h)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        h = f.open_stream("big", sfs.OpenMode.READ)
        length = f.stream_length(h)
        assert length == len(data)
        readback = f.read(h, length)
        assert readback == data, "large data mismatch after reopen"
        f.close_stream(h)
        f.close()


class TestBlockParameters:
    """Verify that different block_index_width and block_size_shift work."""

    @pytest.mark.parametrize("biw", [2, 3, 4, 8])
    def test_varying_block_index_width(self, tmp_path, biw):
        """SFS works with different block_index_width values."""
        sfs_path = str(tmp_path / f"test_biw{biw}.sfs")
        f = sfs.Sfs.create(sfs_path, block_index_width=biw, block_size_shift=9)
        f.mkdir("data")
        h = f.create_stream("data/test")
        content = b"biw-test-" + bytes(range(256)) * 4
        f.write(h, content)
        f.close_stream(h)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        h = f.open_stream("data/test", sfs.OpenMode.READ)
        readback = f.read(h, f.stream_length(h))
        assert readback == content
        f.close_stream(h)
        f.close()

    @pytest.mark.parametrize("bss", [6, 9, 12, 16])
    def test_varying_block_size_shift(self, tmp_path, bss):
        """SFS works with different block_size_shift values (64B to 64KB blocks)."""
        sfs_path = str(tmp_path / f"test_bss{bss}.sfs")
        f = sfs.Sfs.create(sfs_path, block_index_width=4, block_size_shift=bss)
        h = f.create_stream("test")
        # Write enough data to span a few blocks at the given block size
        block_size = 1 << bss
        content = bytes(range(256)) * ((block_size * 3) // 256 + 1)
        content = content[:block_size * 3]  # exactly 3 blocks worth
        f.write(h, content)
        f.close_stream(h)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        h = f.open_stream("test", sfs.OpenMode.READ)
        readback = f.read(h, f.stream_length(h))
        assert readback == content
        f.close_stream(h)
        f.close()


class TestProcessLocking:
    """Verify that exclusive process-level locking is enforced."""

    def test_double_open_fails(self, tmp_path):
        """Cannot open the same SFS file twice simultaneously."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        f1 = sfs.Sfs.open(sfs_path)
        with pytest.raises(sfs.SfsError, match="locked"):
            sfs.Sfs.open(sfs_path)
        f1.close()

    def test_reopen_after_close_succeeds(self, tmp_path):
        """After closing, another open succeeds."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        f.close()

        f = sfs.Sfs.open(sfs_path)
        f.close()

    def test_create_on_existing_file_fails(self, tmp_path):
        """Cannot create where a file already exists."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        with pytest.raises(sfs.SfsError):
            sfs.Sfs.create(sfs_path)
