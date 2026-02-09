"""Tests for file-level open mode (read vs write).

When an SFS file is opened for reading:
- Read operations are allowed (list, open_stream for read, read, stream_length)
- Write operations are rejected with SfsError (mkdir, create_stream, write, delete, rename, truncate)

When opened for writing:
- All operations work (existing behavior)

Cross-process tests verify OS-level file locking via fs2:
- Multiple reader processes can coexist
- A writer process excludes other writers
- A writer process excludes readers
- A reader process excludes writers
"""

import subprocess
import sys

import pytest
import sfs


class TestOpenModeReadOnly:
    """Tests that a read-only SFS file rejects all write operations."""

    @pytest.fixture()
    def populated_sfs(self, tmp_path):
        """Create an SFS file with some content, then close it."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.mkdir("docs")
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello, World!")
        f.close_stream(h)
        h = f.create_stream("docs/readme.txt")
        f.write(h, b"README content")
        f.close_stream(h)
        f.close()
        return sfs_path

    def test_open_for_read_allows_list(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        entries = f.list()
        names = {e.name for e in entries}
        assert "hello.txt" in names
        assert "docs" in names
        f.close()

    def test_open_for_read_allows_list_subdir(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        entries = f.list("docs")
        names = {e.name for e in entries}
        assert "readme.txt" in names
        f.close()

    def test_open_for_read_allows_read_stream(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        h = f.open_stream("hello.txt", sfs.OpenMode.READ)
        data = f.read(h, 100)
        assert data == b"Hello, World!"
        f.close_stream(h)
        f.close()

    def test_open_for_read_allows_stream_length(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        h = f.open_stream("hello.txt", sfs.OpenMode.READ)
        length = f.stream_length(h)
        assert length == 13
        f.close_stream(h)
        f.close()

    def test_open_for_read_allows_seek_tell(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        h = f.open_stream("hello.txt", sfs.OpenMode.READ)
        f.seek(h, 7)
        assert f.tell(h) == 7
        data = f.read(h, 100)
        assert data == b"World!"
        f.close_stream(h)
        f.close()

    def test_open_for_read_rejects_mkdir(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.mkdir("newdir")
        f.close()

    def test_open_for_read_rejects_rmdir(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.rmdir("docs")
        f.close()

    def test_open_for_read_rejects_create_stream(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.create_stream("new.txt")
        f.close()

    def test_open_for_read_rejects_open_stream_write(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.open_stream("hello.txt", sfs.OpenMode.WRITE)
        f.close()

    def test_open_for_read_rejects_delete_stream(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.delete_stream("hello.txt")
        f.close()

    def test_open_for_read_rejects_rename_stream(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.rename_stream("hello.txt", "goodbye.txt")
        f.close()

    def test_open_for_read_rejects_rename_dir(self, populated_sfs):
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError, match="read.only"):
            f.rename_dir("docs", "documents")
        f.close()

    def test_open_for_read_rejects_truncate(self, populated_sfs):
        """Even if a stream is opened for read, truncate should fail."""
        f = sfs.Sfs.open(populated_sfs, sfs.OpenMode.READ)
        h = f.open_stream("hello.txt", sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError):
            f.truncate(h, 5)
        f.close_stream(h)
        f.close()


class TestOpenModeWrite:
    """Tests that a write-mode SFS file allows all operations."""

    def test_open_for_write_allows_all(self, tmp_path):
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello")
        f.close_stream(h)
        f.close()

        f = sfs.Sfs.open(sfs_path, sfs.OpenMode.WRITE)
        # Read operations
        entries = f.list()
        assert len(entries) == 1
        h = f.open_stream("hello.txt", sfs.OpenMode.READ)
        data = f.read(h, 100)
        assert data == b"Hello"
        f.close_stream(h)

        # Write operations
        f.mkdir("newdir")
        h = f.create_stream("newdir/new.txt")
        f.write(h, b"New")
        f.close_stream(h)
        f.close()


class TestOpenModeLocking:
    """Tests for process-level file locking semantics."""

    def test_multiple_read_opens(self, tmp_path):
        """Multiple read-mode opens on the same file should succeed."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello")
        f.close_stream(h)
        f.close()

        f1 = sfs.Sfs.open(sfs_path, sfs.OpenMode.READ)
        f2 = sfs.Sfs.open(sfs_path, sfs.OpenMode.READ)

        # Both can read
        h1 = f1.open_stream("hello.txt", sfs.OpenMode.READ)
        h2 = f2.open_stream("hello.txt", sfs.OpenMode.READ)
        assert f1.read(h1, 100) == b"Hello"
        assert f2.read(h2, 100) == b"Hello"
        f1.close_stream(h1)
        f2.close_stream(h2)

        f2.close()
        f1.close()

    def test_write_excludes_second_write(self, tmp_path):
        """Only one write-mode open should succeed at a time."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        f1 = sfs.Sfs.open(sfs_path, sfs.OpenMode.WRITE)
        with pytest.raises(sfs.SfsError):
            sfs.Sfs.open(sfs_path, sfs.OpenMode.WRITE)
        f1.close()

    def test_write_excludes_read(self, tmp_path):
        """A write-mode open should exclude read-mode opens."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        f1 = sfs.Sfs.open(sfs_path, sfs.OpenMode.WRITE)
        with pytest.raises(sfs.SfsError):
            sfs.Sfs.open(sfs_path, sfs.OpenMode.READ)
        f1.close()

    def test_read_excludes_write(self, tmp_path):
        """A read-mode open should exclude write-mode opens."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        f.close()

        f1 = sfs.Sfs.open(sfs_path, sfs.OpenMode.READ)
        with pytest.raises(sfs.SfsError):
            sfs.Sfs.open(sfs_path, sfs.OpenMode.WRITE)
        f1.close()


# ---------------------------------------------------------------------------
# Helper script for cross-process tests
# ---------------------------------------------------------------------------

# Child script: tries to open an SFS file in the given mode.
# Exit code 0 = open succeeded, exit code 1 = open was rejected.
_CHILD_SCRIPT = """\
import os, sys
sys.path.insert(0, sys.argv[1])
import sfs

try:
    f = sfs.Sfs.open(sys.argv[2], sfs.OpenMode(int(sys.argv[3])))
    f.close()
except sfs.SfsError:
    os._exit(1)
"""


def _try_open_in_child(sfs_path, mode):
    """Spawn a child process that tries to open the SFS file.

    Returns True if the child opened successfully, False if rejected.
    """
    import os
    sfs_pytest_dir = os.path.abspath(
        os.path.join(os.path.dirname(__file__), "..")
    )
    result = subprocess.run(
        [sys.executable, "-c", _CHILD_SCRIPT, sfs_pytest_dir, sfs_path, str(mode)],
        capture_output=True, timeout=10,
    )
    return result.returncode == 0


class TestCrossProcessLocking:
    """Tests that OS-level file locks work across separate processes.

    The parent holds the lock; child processes attempt to open the file.
    """

    @pytest.fixture()
    def sfs_file(self, tmp_path):
        """Create a populated SFS file for locking tests."""
        sfs_path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(sfs_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello, World!")
        f.close_stream(h)
        f.close()
        return sfs_path

    def test_concurrent_readers_cross_process(self, sfs_file):
        """Parent holds a read lock; child can also read."""
        f = sfs.Sfs.open(sfs_file, sfs.OpenMode.READ)
        assert _try_open_in_child(sfs_file, sfs.OpenMode.READ)
        f.close()

    def test_writer_excludes_writer_cross_process(self, sfs_file):
        """Parent holds a write lock; child cannot write."""
        f = sfs.Sfs.open(sfs_file, sfs.OpenMode.WRITE)
        assert not _try_open_in_child(sfs_file, sfs.OpenMode.WRITE)
        f.close()

    def test_writer_excludes_reader_cross_process(self, sfs_file):
        """Parent holds a write lock; child cannot read."""
        f = sfs.Sfs.open(sfs_file, sfs.OpenMode.WRITE)
        assert not _try_open_in_child(sfs_file, sfs.OpenMode.READ)
        f.close()

    def test_reader_excludes_writer_cross_process(self, sfs_file):
        """Parent holds a read lock; child cannot write."""
        f = sfs.Sfs.open(sfs_file, sfs.OpenMode.READ)
        assert not _try_open_in_child(sfs_file, sfs.OpenMode.WRITE)
        f.close()
