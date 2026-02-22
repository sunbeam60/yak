"""Phase 8: Multi-block stream tests.

Uses small block sizes (block_size_shift=6 -> 64-byte blocks,
block_index_width=4 -> fan_out=16) to exercise the pyramid block
linking with small test data.
"""

import pytest
import libyak as yak


BLOCK_SHIFT = 6       # 64-byte blocks
BLOCK_SIZE = 1 << BLOCK_SHIFT
BLOCK_INDEX_WIDTH = 4
FAN_OUT = BLOCK_SIZE // BLOCK_INDEX_WIDTH  # 16


class TestMultiBlock:
    """Tests that exercise data spanning multiple blocks."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                           block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_write_exactly_one_block(self, fs):
        """Write exactly one block of data (depth 0)."""
        data = bytes(range(256))[:BLOCK_SIZE]  # 64 bytes
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)
        assert fs.stream_length(handle) == BLOCK_SIZE
        fs.seek(handle, 0)
        result = fs.read(handle, BLOCK_SIZE)
        assert result == data
        fs.close_stream(handle)

    def test_write_spanning_two_blocks(self, fs):
        """Write data spanning two blocks (depth 1)."""
        data = b"A" * (BLOCK_SIZE + 1)  # 65 bytes -> 2 data blocks
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)
        assert fs.stream_length(handle) == BLOCK_SIZE + 1
        fs.seek(handle, 0)
        result = fs.read(handle, BLOCK_SIZE + 1)
        assert result == data
        fs.close_stream(handle)

    def test_write_spanning_multiple_blocks(self, fs):
        """Write data spanning 4 blocks (depth 1, 200 bytes with 64-byte blocks)."""
        data = bytes(range(256))[:200]  # 200 bytes -> ceil(200/64) = 4 data blocks
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)
        assert fs.stream_length(handle) == 200
        fs.seek(handle, 0)
        result = fs.read(handle, 200)
        assert result == data
        fs.close_stream(handle)

    def test_write_requiring_depth_2(self, fs):
        """Write data requiring depth 2 pyramid (>fan_out data blocks).

        fan_out=16, block_size=64 -> depth 1 covers 16*64=1024 bytes.
        Writing 1025 bytes requires 17 data blocks -> depth 2.
        """
        size = FAN_OUT * BLOCK_SIZE + 1  # 1025 bytes
        data = bytes([i & 0xFF for i in range(size)])
        handle = fs.create_stream("big.bin")
        fs.write(handle, data)
        assert fs.stream_length(handle) == size
        fs.seek(handle, 0)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)

    def test_read_across_block_boundary(self, fs):
        """Read a chunk that crosses a block boundary."""
        data = b"X" * (BLOCK_SIZE * 2)  # 128 bytes
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        # Read 32 bytes starting at offset 48 (crosses the 64-byte boundary)
        fs.seek(handle, 48)
        result = fs.read(handle, 32)
        assert result == b"X" * 32
        assert fs.tell(handle) == 80
        fs.close_stream(handle)

    def test_random_access_across_blocks(self, fs):
        """Seek to various positions across blocks and read."""
        # Write 256 bytes of distinguishable data (4 blocks)
        data = bytes([i & 0xFF for i in range(256)])
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        # Read from middle of second block (offset 96)
        fs.seek(handle, 96)
        result = fs.read(handle, 8)
        assert result == bytes([96, 97, 98, 99, 100, 101, 102, 103])

        # Read last byte
        fs.seek(handle, 255)
        result = fs.read(handle, 1)
        assert result == bytes([255])

        # Read from start of third block (offset 128)
        fs.seek(handle, 128)
        result = fs.read(handle, 4)
        assert result == bytes([128, 129, 130, 131])

        fs.close_stream(handle)

    def test_sequential_writes_across_blocks(self, fs):
        """Multiple small writes that collectively span blocks."""
        handle = fs.create_stream("data.bin")

        # Write 10 bytes at a time, 20 times = 200 bytes (4 blocks)
        for i in range(20):
            chunk = bytes([i] * 10)
            fs.write(handle, chunk)

        assert fs.stream_length(handle) == 200

        # Read back and verify
        fs.seek(handle, 0)
        for i in range(20):
            chunk = fs.read(handle, 10)
            assert chunk == bytes([i] * 10), f"Mismatch at chunk {i}"

        fs.close_stream(handle)

    def test_sequential_reads_across_blocks(self, fs):
        """Write a large chunk, then read in small sequential pieces."""
        data = bytes([i & 0xFF for i in range(200)])
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        fs.seek(handle, 0)
        collected = b""
        while len(collected) < 200:
            chunk = fs.read(handle, 7)  # odd size to cross boundaries
            if not chunk:
                break
            collected += chunk

        assert collected == data
        fs.close_stream(handle)


class TestMultiBlockTruncate:
    """Tests for truncation of multi-block streams."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                           block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_truncate_multi_block_to_smaller(self, fs):
        """Truncate a 4-block stream to fit in 2 blocks."""
        data = b"D" * 200  # 4 blocks
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)
        assert fs.stream_length(handle) == 200

        fs.truncate(handle, 100)  # 2 blocks
        assert fs.stream_length(handle) == 100
        fs.seek(handle, 0)
        result = fs.read(handle, 100)
        assert result == b"D" * 100
        fs.close_stream(handle)

    def test_truncate_multi_block_to_one_block(self, fs):
        """Truncate from depth 1 back to depth 0 (single block)."""
        data = b"E" * 200
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        fs.truncate(handle, 32)  # fits in 1 block (depth 0)
        assert fs.stream_length(handle) == 32
        fs.seek(handle, 0)
        result = fs.read(handle, 32)
        assert result == b"E" * 32
        fs.close_stream(handle)

    def test_truncate_multi_block_to_zero(self, fs):
        """Truncate a multi-block stream to 0 (all blocks freed)."""
        data = b"F" * 200
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        fs.truncate(handle, 0)
        assert fs.stream_length(handle) == 0
        fs.close_stream(handle)

    def test_truncate_depth2_to_depth1(self, fs):
        """Truncate a depth-2 stream down to depth 1."""
        size = FAN_OUT * BLOCK_SIZE + 1  # 1025 bytes, depth 2
        data = bytes([i & 0xFF for i in range(size)])
        handle = fs.create_stream("big.bin")
        fs.write(handle, data)

        # Truncate to 500 bytes (fits in depth 1: 8 data blocks)
        fs.truncate(handle, 500)
        assert fs.stream_length(handle) == 500
        fs.seek(handle, 0)
        result = fs.read(handle, 500)
        assert result == data[:500]
        fs.close_stream(handle)

    def test_truncate_then_extend(self, fs):
        """Truncate a stream, then write more data to extend again."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"A" * 200)

        fs.truncate(handle, 50)
        assert fs.stream_length(handle) == 50

        # Extend again
        fs.seek(handle, 50)
        fs.write(handle, b"B" * 150)
        assert fs.stream_length(handle) == 200

        fs.seek(handle, 0)
        result = fs.read(handle, 200)
        assert result == b"A" * 50 + b"B" * 150
        fs.close_stream(handle)


class TestMultiBlockPersistence:
    """Tests for close/reopen of multi-block streams."""

    def test_write_close_reopen_read(self, tmp_path):
        """Write multi-block data, close Yak, reopen, read back."""
        yak_path = str(tmp_path / "test.yak")
        data = bytes([i & 0xFF for i in range(200)])

        # Write
        fs = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                            block_size_shift=BLOCK_SHIFT)
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)
        fs.close_stream(handle)
        fs.close()

        # Reopen and read
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        handle = fs.open_stream("data.bin", yak.OpenMode.READ)
        result = fs.read(handle, 200)
        assert result == data
        assert fs.stream_length(handle) == 200
        fs.close_stream(handle)
        fs.close()

    def test_depth2_close_reopen_read(self, tmp_path):
        """Write depth-2 data, close Yak, reopen, read back."""
        yak_path = str(tmp_path / "test.yak")
        size = FAN_OUT * BLOCK_SIZE + 1  # 1025 bytes, depth 2
        data = bytes([i & 0xFF for i in range(size)])

        # Write
        fs = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                            block_size_shift=BLOCK_SHIFT)
        handle = fs.create_stream("big.bin")
        fs.write(handle, data)
        fs.close_stream(handle)
        fs.close()

        # Reopen and read
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        handle = fs.open_stream("big.bin", yak.OpenMode.READ)
        result = fs.read(handle, size)
        assert result == data
        fs.close_stream(handle)
        fs.close()

    def test_multiple_streams_persist(self, tmp_path):
        """Multiple multi-block streams survive close/reopen."""
        yak_path = str(tmp_path / "test.yak")
        data_a = b"A" * 150
        data_b = b"B" * 200

        # Write two streams
        fs = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                            block_size_shift=BLOCK_SHIFT)
        h1 = fs.create_stream("a.bin")
        fs.write(h1, data_a)
        fs.close_stream(h1)
        h2 = fs.create_stream("b.bin")
        fs.write(h2, data_b)
        fs.close_stream(h2)
        fs.close()

        # Reopen and verify both
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h1 = fs.open_stream("a.bin", yak.OpenMode.READ)
        assert fs.read(h1, 150) == data_a
        fs.close_stream(h1)
        h2 = fs.open_stream("b.bin", yak.OpenMode.READ)
        assert fs.read(h2, 200) == data_b
        fs.close_stream(h2)
        fs.close()


class TestMultiBlockOverwrite:
    """Tests for overwriting data in multi-block streams."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_index_width=BLOCK_INDEX_WIDTH,
                           block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_overwrite_within_block(self, fs):
        """Overwrite data within a single block of a multi-block stream."""
        data = bytes([i & 0xFF for i in range(200)])
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        # Overwrite bytes 10-19 (within first block)
        fs.seek(handle, 10)
        fs.write(handle, b"Z" * 10)

        # Read full stream and verify
        fs.seek(handle, 0)
        result = fs.read(handle, 200)
        expected = bytearray(data)
        expected[10:20] = b"Z" * 10
        assert result == bytes(expected)
        fs.close_stream(handle)

    def test_overwrite_across_block_boundary(self, fs):
        """Overwrite data that spans a block boundary."""
        data = b"X" * 200
        handle = fs.create_stream("data.bin")
        fs.write(handle, data)

        # Overwrite bytes 60-67 (crosses the 64-byte block boundary)
        fs.seek(handle, 60)
        fs.write(handle, b"YYYYYYYY")

        fs.seek(handle, 0)
        result = fs.read(handle, 200)
        expected = bytearray(b"X" * 200)
        expected[60:68] = b"YYYYYYYY"
        assert result == bytes(expected)
        assert fs.stream_length(handle) == 200  # length unchanged
        fs.close_stream(handle)

    def test_write_past_end_extends(self, fs):
        """Seek to middle, write past end to extend the stream."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"A" * BLOCK_SIZE)  # 64 bytes (1 block)
        assert fs.stream_length(handle) == BLOCK_SIZE

        # Seek to byte 32, write 64 bytes (extends to 96 = 2 blocks)
        fs.seek(handle, 32)
        fs.write(handle, b"B" * BLOCK_SIZE)
        assert fs.stream_length(handle) == 32 + BLOCK_SIZE  # = 96

        fs.seek(handle, 0)
        result = fs.read(handle, 96)
        assert result == b"A" * 32 + b"B" * BLOCK_SIZE
        fs.close_stream(handle)
