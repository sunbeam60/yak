"""Tests for the verify() integrity-check chain."""

import os
import struct
import pytest
from pathlib import Path
import libyak as yak


class TestVerifyClean:
    """Verify returns empty list on valid Yak files."""

    def test_empty_yak(self, tmp_path):
        """A freshly created Yak (just root dir stream) is clean."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()
        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_with_streams(self, tmp_path):
        """Yak with dirs and data streams verifies clean."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.mkdir("docs")
        f.mkdir("docs/sub")
        sh = f.create_stream("hello.txt")
        f.write(sh, b"Hello, World!")
        f.close_stream(sh)
        sh = f.create_stream("docs/readme.txt")
        f.write(sh, b"A readme file")
        f.close_stream(sh)
        sh = f.create_stream("docs/sub/deep.txt")
        f.write(sh, b"Deep file")
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_after_delete(self, tmp_path):
        """After deleting streams (blocks returned to free list), verify still clean."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)

        # Create several streams
        sh = f.create_stream("a.txt")
        f.write(sh, b"AAAA" * 1000)
        f.close_stream(sh)
        sh = f.create_stream("b.txt")
        f.write(sh, b"BBBB" * 1000)
        f.close_stream(sh)
        sh = f.create_stream("c.txt")
        f.write(sh, b"CCCC" * 1000)
        f.close_stream(sh)

        # Delete one
        f.delete_stream("b.txt")
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    @pytest.mark.parametrize("bss", [9, 12])
    def test_varying_block_params(self, tmp_path, bss):
        """Verify works with different bss values."""
        yak_path = str(tmp_path / f"test_{bss}.yak")
        f = yak.Yak.create(yak_path, block_size_shift=bss)
        sh = f.create_stream("data.bin")
        f.write(sh, b"X" * 500)
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_verify_in_write_mode(self, tmp_path):
        """Verify works when Yak is opened in write mode too."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        sh = f.create_stream("test.txt")
        f.write(sh, b"data")
        f.close_stream(sh)
        # Verify while still open in write mode (no reopen needed)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_multiblock_streams(self, tmp_path):
        """Verify works with streams spanning many blocks (pyramid depth > 0)."""
        yak_path = str(tmp_path / "test.yak")
        # bss=9 -> 512-byte blocks, fan_out=128
        f = yak.Yak.create(yak_path, block_size_shift=9)
        sh = f.create_stream("big.bin")
        # 2000 bytes -> ~32 data blocks -> pyramid depth 2
        f.write(sh, b"X" * 2000)
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"


class TestVerifyCorruption:
    """Intentional corruption detected by verify."""

    def test_corrupt_file_truncated(self, tmp_path):
        """Truncated file detected by verify."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        sh = f.create_stream("data.txt")
        f.write(sh, b"Hello" * 100)
        f.close_stream(sh)
        f.close()

        # Truncate the file by cutting off the last 50 bytes
        original_size = os.path.getsize(yak_path)
        with open(yak_path, "r+b") as raw:
            raw.truncate(original_size - 50)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert len(issues) > 0, "Truncated file should produce verify issues"
        # Should mention file size or block count mismatch
        assert any("L2" in i or "L1" in i for i in issues), \
            f"Expected L1 or L2 issue, got: {issues}"

    def test_corrupt_free_list_cycle(self, tmp_path):
        """A free list that cycles is detected."""
        yak_path = str(tmp_path / "test.yak")
        # Use small blocks to make corruption easier to set up
        f = yak.Yak.create(yak_path, block_size_shift=9)
        # Create and delete a stream to put blocks on the free list
        sh = f.create_stream("temp.txt")
        f.write(sh, b"X" * 2000)  # multiple blocks
        f.close_stream(sh)
        f.delete_stream("temp.txt")
        f.close()

        # New format: bootstrap header is magic(12) + bss(1) + encrypted(1)
        # Block 0 (superblock) starts at data_offset.
        # Block 0 layout: "blk0"(4) + version(1) + free_list_head(4) + slot_count(1)
        # So free_list_head is at data_offset + 5, 4 bytes LE (u32).
        block_size = 1 << 9  # bss=9 -> 512 bytes

        with open(yak_path, "r+b") as raw:
            # Read data_offset from magic prefix (offset 10, 2 bytes LE)
            raw.seek(10)
            data_offset = struct.unpack("<H", raw.read(2))[0]

            # Read free_list_head from block 0 (u32, 4 bytes)
            free_list_offset = data_offset + 5
            raw.seek(free_list_offset)
            free_head_bytes = raw.read(4)
            free_head = struct.unpack("<I", free_head_bytes)[0]

            if free_head != 0xFFFFFFFF:  # sentinel for u32 block indices
                # Make the first free block point to itself (cycle)
                block_offset = data_offset + free_head * block_size
                # Write the block's own ID as its next pointer (cycle!)
                raw.seek(block_offset)
                raw.write(struct.pack("<I", free_head))  # 4-byte block index

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        issues = f.verify()
        f.close()
        assert len(issues) > 0, "Free list cycle should produce verify issues"
        assert any("cycle" in i.lower() for i in issues), \
            f"Expected cycle issue, got: {issues}"
