"""Tests for the trunk-and-leaf free list.

Exercises block allocation and deallocation through stream operations,
specifically targeting the extent-based free list behaviour:
- Contiguous deallocations form single extents
- Trunk overflow when extent slots are exhausted
- Trunk reclamation when all extents are consumed
- Mixed alloc/dealloc cycles
- Verify passes after all operations

Uses small blocks (bss=9 -> 512 bytes) to keep data sizes
manageable and to hit trunk capacity limits with fewer operations.
"""

import pytest
import libyak as yak


BLOCK_SHIFT = 9  # 512-byte blocks
BLOCK_SIZE = 1 << BLOCK_SHIFT
# Extents per trunk: (block_size - 4) / (2 * 4) = (512 - 4) / 8 = 63
EXTENTS_PER_TRUNK = (BLOCK_SIZE - 4) // 8


class TestFreeListBasic:
    """Basic free list operations: deallocate then reallocate."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_delete_then_create_reuses_blocks(self, fs):
        """Deleting a stream and creating another reuses free blocks."""
        # Write enough to use several blocks
        h = fs.create_stream("a.bin")
        fs.write(h, b"A" * (BLOCK_SIZE * 4))
        fs.close_stream(h)
        fs.delete_stream("a.bin")

        # Create new stream - should reuse freed blocks (no file growth)
        h = fs.create_stream("b.bin")
        fs.write(h, b"B" * (BLOCK_SIZE * 4))
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_single_block_dealloc_realloc(self, fs):
        """Free a single-block stream, reallocate, verify data."""
        h = fs.create_stream("one.bin")
        fs.write(h, b"X" * 10)
        fs.close_stream(h)
        fs.delete_stream("one.bin")

        h = fs.create_stream("two.bin")
        fs.write(h, b"Y" * 10)
        fs.seek(h, 0)
        assert fs.read(h, 10) == b"Y" * 10
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == []

    def test_contiguous_deallocation(self, fs):
        """Freeing a multi-block stream creates contiguous extent(s)."""
        # Write a large contiguous stream
        size = BLOCK_SIZE * 20
        h = fs.create_stream("big.bin")
        fs.write(h, b"D" * size)
        fs.close_stream(h)
        fs.delete_stream("big.bin")

        # Reallocate - should come from freed extents
        h = fs.create_stream("big2.bin")
        data = bytes([i & 0xFF for i in range(size)])
        fs.write(h, data)
        fs.seek(h, 0)
        assert fs.read(h, size) == data
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == []


class TestFreeListTrunkOverflow:
    """Tests that exercise trunk overflow (more extents than fit in one trunk)."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_many_individual_deallocations(self, fs):
        """Create and delete many small streams to fill trunk extent slots.

        Each single-block stream deletion creates one extent (index, 1).
        After EXTENTS_PER_TRUNK deletions, the next one must create a new trunk.
        """
        # We need to create more streams than EXTENTS_PER_TRUNK to overflow.
        # Each stream uses at least 1 data block. Some also use redirector blocks.
        # Creating 80 streams should comfortably exceed 63 extent slots.
        stream_count = EXTENTS_PER_TRUNK + 20  # 83 streams

        for i in range(stream_count):
            name = f"s{i}.bin"
            h = fs.create_stream(name)
            fs.write(h, b"X" * 10)  # fits in 1 data block
            fs.close_stream(h)

        # Delete all - each deletion adds an extent to the free list
        for i in range(stream_count):
            fs.delete_stream(f"s{i}.bin")

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_overflow_then_allocate(self, fs):
        """Overflow trunk, then allocate all blocks back."""
        stream_count = EXTENTS_PER_TRUNK + 20

        for i in range(stream_count):
            h = fs.create_stream(f"s{i}.bin")
            fs.write(h, b"D" * 10)
            fs.close_stream(h)

        for i in range(stream_count):
            fs.delete_stream(f"s{i}.bin")

        # Now reallocate everything back
        for i in range(stream_count):
            h = fs.create_stream(f"r{i}.bin")
            fs.write(h, b"R" * 10)
            fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"


class TestFreeListTrunkReclamation:
    """Tests that trunk blocks are reclaimed when all their extents are consumed."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_full_reclamation(self, fs):
        """Free blocks into trunk, allocate all back - trunk itself must be reclaimed."""
        # Create and delete a big stream to put many blocks on free list
        size = BLOCK_SIZE * 50
        h = fs.create_stream("big.bin")
        fs.write(h, b"A" * size)
        fs.close_stream(h)
        fs.delete_stream("big.bin")

        # Now allocate a stream that uses all those blocks plus more
        h = fs.create_stream("bigger.bin")
        fs.write(h, b"B" * (BLOCK_SIZE * 60))
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_partial_then_full_drain(self, fs):
        """Partially drain a trunk, add more, then fully drain."""
        # Phase 1: Create extent entries
        for i in range(10):
            h = fs.create_stream(f"p{i}.bin")
            fs.write(h, b"X" * BLOCK_SIZE)
            fs.close_stream(h)

        for i in range(10):
            fs.delete_stream(f"p{i}.bin")

        # Phase 2: Reallocate some (partial drain)
        for i in range(5):
            h = fs.create_stream(f"q{i}.bin")
            fs.write(h, b"Y" * BLOCK_SIZE)
            fs.close_stream(h)

        # Phase 3: Free more blocks
        for i in range(5):
            fs.delete_stream(f"q{i}.bin")

        # Phase 4: Allocate everything
        h = fs.create_stream("drain.bin")
        fs.write(h, b"Z" * (BLOCK_SIZE * 20))
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"


class TestFreeListMixedCycles:
    """Interleaved allocation and deallocation cycles."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)
        yield f
        f.close()

    def test_interleaved_create_delete(self, fs):
        """Interleave stream creation and deletion."""
        for cycle in range(5):
            # Create 10 streams
            for i in range(10):
                name = f"c{cycle}_s{i}.bin"
                h = fs.create_stream(name)
                fs.write(h, b"D" * (BLOCK_SIZE * 2))
                fs.close_stream(h)

            # Delete half of them
            for i in range(0, 10, 2):
                fs.delete_stream(f"c{cycle}_s{i}.bin")

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_growing_and_shrinking(self, fs):
        """Repeatedly grow and shrink a stream via truncation."""
        h = fs.create_stream("elastic.bin")

        for cycle in range(10):
            # Grow
            size = BLOCK_SIZE * (cycle + 1) * 3
            fs.seek(h, 0)
            fs.write(h, b"G" * size)
            assert fs.stream_length(h) == size

            # Shrink
            small = BLOCK_SIZE
            fs.truncate(h, small)
            assert fs.stream_length(h) == small

        fs.close_stream(h)
        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_delete_recreate_same_name(self, fs):
        """Delete and recreate the same stream repeatedly."""
        for cycle in range(20):
            h = fs.create_stream("reuse.bin")
            data = bytes([cycle & 0xFF]) * (BLOCK_SIZE * 3)
            fs.write(h, data)
            fs.close_stream(h)

            # Read back
            h = fs.open_stream("reuse.bin", yak.OpenMode.READ)
            assert fs.read(h, len(data)) == data
            fs.close_stream(h)

            fs.delete_stream("reuse.bin")

        issues = fs.verify()
        assert issues == []


class TestFreeListPersistence:
    """Free list survives close and reopen."""

    def test_free_list_persists(self, tmp_path):
        """Free blocks, close, reopen - blocks are still free and reusable."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)

        # Create and delete to put blocks on free list
        h = fs.create_stream("temp.bin")
        fs.write(h, b"T" * (BLOCK_SIZE * 10))
        fs.close_stream(h)
        fs.delete_stream("temp.bin")
        fs.close()

        # Reopen - free list should be intact
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        h = fs.create_stream("new.bin")
        data = b"N" * (BLOCK_SIZE * 10)
        fs.write(h, data)
        fs.seek(h, 0)
        assert fs.read(h, len(data)) == data
        fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"
        fs.close()

    def test_multiple_reopen_cycles(self, tmp_path):
        """Multiple close/reopen cycles with free list operations each time."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path, block_size_shift=BLOCK_SHIFT)

        for cycle in range(5):
            h = fs.create_stream(f"cycle{cycle}.bin")
            fs.write(h, b"C" * (BLOCK_SIZE * 5))
            fs.close_stream(h)

            if cycle > 0:
                fs.delete_stream(f"cycle{cycle - 1}.bin")

            fs.close()
            fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"
        fs.close()


class TestFreeListParametrized:
    """Free list with different block parameters."""

    @pytest.mark.parametrize("bss", [
        9,   # 512-byte blocks
        12,  # 4096-byte blocks
    ])
    def test_alloc_dealloc_cycle(self, tmp_path, bss):
        """Allocate, deallocate, reallocate with various block params."""
        block_size = 1 << bss
        yak_path = str(tmp_path / f"test_{bss}.yak")
        fs = yak.Yak.create(yak_path, block_size_shift=bss)

        # Create streams using several blocks
        for i in range(5):
            h = fs.create_stream(f"s{i}.bin")
            fs.write(h, b"X" * (block_size * 3))
            fs.close_stream(h)

        # Delete all
        for i in range(5):
            fs.delete_stream(f"s{i}.bin")

        # Recreate
        for i in range(5):
            h = fs.create_stream(f"r{i}.bin")
            data = bytes([i & 0xFF]) * (block_size * 3)
            fs.write(h, data)
            fs.seek(h, 0)
            assert fs.read(h, len(data)) == data
            fs.close_stream(h)

        issues = fs.verify()
        assert issues == [], f"Expected no issues, got: {issues}"
        fs.close()
