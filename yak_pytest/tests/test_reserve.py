"""Tests for stream reserve (pre-allocation) functionality."""

import pytest
import libyak as yak


class TestReserve:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_reserve_basic(self, fs):
        """Reserve capacity on a new stream, close, verify clean."""
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 8192)
        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == []

    def test_reserve_returns_block_aligned(self, fs):
        """reserve(5000) on 4096-block system allocates 2 blocks = 8192."""
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 5000)
        reserved = fs.stream_reserved(handle)
        assert reserved == 8192  # 2 * 4096
        fs.close_stream(handle)

    def test_reserve_less_than_size_fails(self, fs):
        """Cannot reserve less than current stream size."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"x" * 5000)
        with pytest.raises(yak.YakError):
            fs.reserve(handle, 1000)
        fs.close_stream(handle)

    def test_reserve_preserves_data(self, fs):
        """Reserving more capacity does not corrupt existing data."""
        handle = fs.create_stream("data.bin")
        data = b"hello world"
        fs.write(handle, data)
        fs.reserve(handle, 16384)
        fs.seek(handle, 0)
        read_back = fs.read(handle, len(data))
        assert read_back == data
        fs.close_stream(handle)

    def test_reserve_then_write(self, fs):
        """Reserve capacity, then write up to it."""
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 8192)
        data = b"A" * 8192
        fs.write(handle, data)
        assert fs.stream_length(handle) == 8192
        fs.seek(handle, 0)
        read_back = fs.read(handle, 8192)
        assert read_back == data
        fs.close_stream(handle)

    def test_reserve_on_empty_stream(self, fs):
        """Reserve on a brand-new empty stream."""
        handle = fs.create_stream("data.bin")
        assert fs.stream_length(handle) == 0
        fs.reserve(handle, 4096)
        reserved = fs.stream_reserved(handle)
        assert reserved == 4096
        assert fs.stream_length(handle) == 0  # size unchanged
        fs.close_stream(handle)

    def test_reserve_on_reader_fails(self, fs):
        """Cannot reserve on a read-only handle."""
        handle = fs.create_stream("data.bin")
        fs.close_stream(handle)
        reader = fs.open_stream("data.bin", yak.OpenMode.READ)
        with pytest.raises(yak.YakError):
            fs.reserve(reader, 4096)
        fs.close_stream(reader)

    def test_reserve_zero_noop(self, fs):
        """reserve(0) on empty stream is a no-op."""
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 0)
        assert fs.stream_reserved(handle) == 0
        fs.close_stream(handle)

    def test_truncate_clears_reservation(self, fs):
        """After truncate, reserved matches the new (smaller) block capacity."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"x" * 8192)
        fs.reserve(handle, 16384)
        assert fs.stream_reserved(handle) == 16384
        fs.truncate(handle, 100)
        # Truncate to 100 bytes -> 1 block -> reserved = 4096
        assert fs.stream_reserved(handle) == 4096
        assert fs.stream_length(handle) == 100
        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == []

    def test_truncate_to_zero_clears_reservation(self, fs):
        """Truncating to 0 frees all blocks and resets reserved to 0."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"x" * 4096)
        fs.reserve(handle, 8192)
        fs.truncate(handle, 0)
        assert fs.stream_reserved(handle) == 0
        assert fs.stream_length(handle) == 0
        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == []

    def test_reserve_across_reopen(self, tmp_path):
        """Reserved capacity persists across close and reopen."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 16384)
        assert fs.stream_reserved(handle) == 16384
        fs.close_stream(handle)
        fs.close()

        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        handle = fs.open_stream("data.bin", yak.OpenMode.WRITE)
        assert fs.stream_reserved(handle) == 16384
        fs.close_stream(handle)
        fs.close()

    def test_reserve_multiple_calls(self, fs):
        """Multiple reserves: grows, but doesn't shrink."""
        handle = fs.create_stream("data.bin")
        fs.reserve(handle, 4096)
        assert fs.stream_reserved(handle) == 4096

        fs.reserve(handle, 8192)
        assert fs.stream_reserved(handle) == 8192

        # Requesting less than current reservation is a no-op (already have 8192)
        fs.reserve(handle, 4096)
        assert fs.stream_reserved(handle) == 8192
        fs.close_stream(handle)

    def test_reserve_equal_to_size(self, fs):
        """reserve(size) when size exactly matches is a no-op."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"x" * 4096)
        # Size is 4096, reserved is already 4096 (1 block)
        fs.reserve(handle, 4096)
        assert fs.stream_reserved(handle) == 4096
        fs.close_stream(handle)

    def test_verify_after_reserve(self, fs):
        """verify() is clean after various reserve operations."""
        h1 = fs.create_stream("a.bin")
        fs.reserve(h1, 4096)
        fs.close_stream(h1)

        h2 = fs.create_stream("b.bin")
        fs.write(h2, b"data")
        fs.reserve(h2, 8192)
        fs.close_stream(h2)

        h3 = fs.create_stream("c.bin")
        fs.reserve(h3, 16384)
        fs.write(h3, b"x" * 10000)
        fs.close_stream(h3)

        issues = fs.verify()
        assert issues == []

    @pytest.mark.parametrize("biw,bss", [
        (2, 9),   # block_index_width=2, block_size=512
        (3, 10),  # block_index_width=3, block_size=1024
        (4, 12),  # block_index_width=4, block_size=4096
    ])
    def test_reserve_parametrized_block_params(self, tmp_path, biw, bss):
        """Reserve works across different block parameter combos."""
        block_size = 1 << bss
        yak_path = str(tmp_path / f"test_{biw}_{bss}.yak")
        fs = yak.Yak.create(yak_path, block_index_width=biw, block_size_shift=bss)

        handle = fs.create_stream("data.bin")
        reserve_bytes = block_size * 3  # 3 blocks
        fs.reserve(handle, reserve_bytes)
        assert fs.stream_reserved(handle) == reserve_bytes

        # Write data up to reserved capacity
        data = b"D" * reserve_bytes
        fs.write(handle, data)
        assert fs.stream_length(handle) == reserve_bytes
        fs.seek(handle, 0)
        assert fs.read(handle, reserve_bytes) == data

        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == []
        fs.close()

    def test_reserve_large_multiblock(self, fs):
        """Reserve a large amount spanning many blocks, then fill it."""
        handle = fs.create_stream("big.bin")
        # Reserve 64 blocks = 256KB (with default 4096 block size)
        reserve_size = 64 * 4096
        fs.reserve(handle, reserve_size)
        assert fs.stream_reserved(handle) == reserve_size

        # Write in chunks
        chunk = b"X" * 4096
        for _ in range(64):
            fs.write(handle, chunk)

        assert fs.stream_length(handle) == reserve_size
        fs.close_stream(handle)
        issues = fs.verify()
        assert issues == []
