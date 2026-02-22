"""Tests for the sorted name table directory format.

These tests exercise the O(log n) lookup via binary search, in-place
compaction on delete, and mixed directory/stream coexistence.  They
complement the basic CRUD tests in test_directories.py and test_streams.py.
"""

import pytest
import libyak as yak


class TestNameTableLookup:
    """Verify O(log n) lookups work correctly for various directory sizes."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_single_entry_lookup(self, fs):
        """A directory with one entry can be looked up by path."""
        fs.mkdir("alpha")
        h = fs.create_stream("alpha/data.bin")
        fs.close_stream(h)
        h = fs.open_stream("alpha/data.bin", yak.OpenMode.READ)
        fs.close_stream(h)

    def test_many_entries_all_findable(self, fs):
        """Create 500 entries and verify all are independently findable."""
        count = 500
        for i in range(count):
            h = fs.create_stream(f"file_{i:04d}.bin")
            fs.close_stream(h)

        # Verify every entry can be opened
        for i in range(count):
            h = fs.open_stream(f"file_{i:04d}.bin", yak.OpenMode.READ)
            fs.close_stream(h)

        # Verify list returns all entries
        entries = fs.list("")
        assert len(entries) == count

    def test_many_directories_all_findable(self, fs):
        """Create 200 directories and verify all are listable."""
        count = 200
        for i in range(count):
            fs.mkdir(f"dir_{i:04d}")

        entries = fs.list("")
        assert len(entries) == count
        names = {e.name for e in entries}
        for i in range(count):
            assert f"dir_{i:04d}" in names

    def test_lookup_nonexistent_in_large_dir(self, fs):
        """Looking up a missing entry in a large directory returns not found."""
        for i in range(100):
            h = fs.create_stream(f"item_{i}.dat")
            fs.close_stream(h)

        with pytest.raises(yak.YakError):
            fs.open_stream("does_not_exist.dat", yak.OpenMode.READ)


class TestDeleteCompaction:
    """Verify in-place compaction preserves remaining entries after delete."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_delete_middle_entry(self, fs):
        """Create A, B, C; delete B; verify A and C still found."""
        ha = fs.create_stream("aaa.txt")
        fs.write(ha, b"AAA")
        fs.close_stream(ha)

        hb = fs.create_stream("bbb.txt")
        fs.write(hb, b"BBB")
        fs.close_stream(hb)

        hc = fs.create_stream("ccc.txt")
        fs.write(hc, b"CCC")
        fs.close_stream(hc)

        fs.delete_stream("bbb.txt")

        # A and C still accessible with correct data
        ha = fs.open_stream("aaa.txt", yak.OpenMode.READ)
        assert fs.read(ha, 10) == b"AAA"
        fs.close_stream(ha)

        hc = fs.open_stream("ccc.txt", yak.OpenMode.READ)
        assert fs.read(hc, 10) == b"CCC"
        fs.close_stream(hc)

        # B is gone
        with pytest.raises(yak.YakError):
            fs.open_stream("bbb.txt", yak.OpenMode.READ)

        entries = fs.list("")
        assert len(entries) == 2

    def test_delete_first_entry(self, fs):
        """Delete the first entry (offset 0) in a directory."""
        ha = fs.create_stream("first.txt")
        fs.close_stream(ha)
        hb = fs.create_stream("second.txt")
        fs.close_stream(hb)
        hc = fs.create_stream("third.txt")
        fs.close_stream(hc)

        fs.delete_stream("first.txt")

        h = fs.open_stream("second.txt", yak.OpenMode.READ)
        fs.close_stream(h)
        h = fs.open_stream("third.txt", yak.OpenMode.READ)
        fs.close_stream(h)
        entries = fs.list("")
        assert len(entries) == 2

    def test_delete_last_entry(self, fs):
        """Delete the last entry (just before name table) in a directory."""
        ha = fs.create_stream("first.txt")
        fs.close_stream(ha)
        hb = fs.create_stream("second.txt")
        fs.close_stream(hb)
        hc = fs.create_stream("third.txt")
        fs.close_stream(hc)

        fs.delete_stream("third.txt")

        h = fs.open_stream("first.txt", yak.OpenMode.READ)
        fs.close_stream(h)
        h = fs.open_stream("second.txt", yak.OpenMode.READ)
        fs.close_stream(h)
        entries = fs.list("")
        assert len(entries) == 2

    def test_delete_all_entries_one_by_one(self, fs):
        """Delete all entries sequentially, directory ends up empty."""
        names = [f"entry_{i}.bin" for i in range(10)]
        for name in names:
            h = fs.create_stream(name)
            fs.close_stream(h)

        for name in names:
            fs.delete_stream(name)

        entries = fs.list("")
        assert len(entries) == 0

    def test_add_after_delete(self, fs):
        """After deleting entries, new entries can be added and found."""
        ha = fs.create_stream("alpha.txt")
        fs.close_stream(ha)
        hb = fs.create_stream("beta.txt")
        fs.close_stream(hb)

        fs.delete_stream("alpha.txt")

        # Add a new entry
        hc = fs.create_stream("gamma.txt")
        fs.close_stream(hc)

        entries = fs.list("")
        assert len(entries) == 2
        names = {e.name for e in entries}
        assert names == {"beta.txt", "gamma.txt"}

        # Both are findable
        h = fs.open_stream("beta.txt", yak.OpenMode.READ)
        fs.close_stream(h)
        h = fs.open_stream("gamma.txt", yak.OpenMode.READ)
        fs.close_stream(h)

    def test_rmdir_compaction(self, fs):
        """Removing a directory from the middle compacts correctly."""
        fs.mkdir("aaa")
        fs.mkdir("bbb")
        fs.mkdir("ccc")

        fs.rmdir("bbb")

        entries = fs.list("")
        assert len(entries) == 2
        names = {e.name for e in entries}
        assert names == {"aaa", "ccc"}


class TestMixedTypes:
    """Directories and streams coexisting in the same parent."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_dir_and_stream_same_parent(self, fs):
        """A directory and a stream can coexist in the same parent."""
        fs.mkdir("subdir")
        h = fs.create_stream("file.txt")
        fs.close_stream(h)

        entries = fs.list("")
        assert len(entries) == 2
        types = {e.name: e.entry_type for e in entries}
        assert types["subdir"] == yak.EntryType.DIRECTORY
        assert types["file.txt"] == yak.EntryType.STREAM

    def test_no_name_collision_dir_and_stream(self, fs):
        """Cannot create a stream with same name as existing directory."""
        fs.mkdir("data")
        with pytest.raises(yak.YakError):
            fs.create_stream("data")

    def test_no_name_collision_stream_and_dir(self, fs):
        """Cannot create a directory with same name as existing stream."""
        h = fs.create_stream("data")
        fs.close_stream(h)
        with pytest.raises(yak.YakError):
            fs.mkdir("data")

    def test_delete_stream_keeps_dir(self, fs):
        """Deleting a stream doesn't affect a sibling directory."""
        fs.mkdir("subdir")
        h = fs.create_stream("file.txt")
        fs.close_stream(h)

        fs.delete_stream("file.txt")

        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "subdir"
        assert entries[0].entry_type == yak.EntryType.DIRECTORY


class TestRenameOperations:
    """Rename operations (implemented as delete + add)."""

    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_rename_stream_same_parent(self, fs):
        """Rename stream within same directory, data preserved."""
        h = fs.create_stream("old.txt")
        fs.write(h, b"precious data")
        fs.close_stream(h)

        fs.rename_stream("old.txt", "new.txt")

        with pytest.raises(yak.YakError):
            fs.open_stream("old.txt", yak.OpenMode.READ)

        h = fs.open_stream("new.txt", yak.OpenMode.READ)
        assert fs.read(h, 100) == b"precious data"
        fs.close_stream(h)

    def test_rename_stream_cross_parent(self, fs):
        """Move stream between directories, data preserved."""
        fs.mkdir("src")
        fs.mkdir("dst")

        h = fs.create_stream("src/file.bin")
        fs.write(h, b"payload")
        fs.close_stream(h)

        fs.rename_stream("src/file.bin", "dst/file.bin")

        with pytest.raises(yak.YakError):
            fs.open_stream("src/file.bin", yak.OpenMode.READ)

        h = fs.open_stream("dst/file.bin", yak.OpenMode.READ)
        assert fs.read(h, 100) == b"payload"
        fs.close_stream(h)

        # Source directory is now empty
        assert len(fs.list("src")) == 0

    def test_rename_dir_same_parent(self, fs):
        """Rename directory, children survive."""
        fs.mkdir("old_dir")
        h = fs.create_stream("old_dir/child.txt")
        fs.write(h, b"child data")
        fs.close_stream(h)

        fs.rename_dir("old_dir", "new_dir")

        with pytest.raises(yak.YakError):
            fs.list("old_dir")

        entries = fs.list("new_dir")
        assert len(entries) == 1
        assert entries[0].name == "child.txt"

        h = fs.open_stream("new_dir/child.txt", yak.OpenMode.READ)
        assert fs.read(h, 100) == b"child data"
        fs.close_stream(h)

    def test_rename_dir_cross_parent(self, fs):
        """Move directory between parents."""
        fs.mkdir("src")
        fs.mkdir("dst")
        fs.mkdir("src/moveme")

        fs.rename_dir("src/moveme", "dst/moveme")

        assert len(fs.list("src")) == 0
        entries = fs.list("dst")
        assert len(entries) == 1
        assert entries[0].name == "moveme"


class TestOptimizeRoundTrip:
    """Entries survive optimize (full rewrite)."""

    def test_optimize_preserves_all_entries(self, tmp_path):
        """Create dirs and streams, optimize, reopen, verify all present."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.mkdir("alpha")
        f.mkdir("beta")
        h = f.create_stream("alpha/file1.txt")
        f.write(h, b"file1 data")
        f.close_stream(h)
        h = f.create_stream("beta/file2.txt")
        f.write(h, b"file2 data")
        f.close_stream(h)
        h = f.create_stream("root_file.bin")
        f.write(h, b"root data")
        f.close_stream(h)
        f.close()

        yak.Yak.optimize(yak_path)

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        root = f.list("")
        assert len(root) == 3
        names = {e.name for e in root}
        assert names == {"alpha", "beta", "root_file.bin"}

        h = f.open_stream("alpha/file1.txt", yak.OpenMode.READ)
        assert f.read(h, 100) == b"file1 data"
        f.close_stream(h)

        h = f.open_stream("beta/file2.txt", yak.OpenMode.READ)
        assert f.read(h, 100) == b"file2 data"
        f.close_stream(h)

        h = f.open_stream("root_file.bin", yak.OpenMode.READ)
        assert f.read(h, 100) == b"root data"
        f.close_stream(h)
        f.close()


class TestVerifyAfterMutations:
    """Verify passes after various directory mutations."""

    def test_verify_after_add_delete_rename(self, tmp_path):
        """Create, delete, rename, then verify reports no issues."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)

        # Build up
        f.mkdir("dir_a")
        f.mkdir("dir_b")
        h = f.create_stream("dir_a/s1.txt")
        f.write(h, b"data1")
        f.close_stream(h)
        h = f.create_stream("dir_a/s2.txt")
        f.write(h, b"data2")
        f.close_stream(h)

        # Delete
        f.delete_stream("dir_a/s1.txt")

        # Rename
        f.rename_stream("dir_a/s2.txt", "dir_b/moved.txt")

        # Verify
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"
