"""Tests for file-level open mode (read vs write).

When a Yak file is opened for reading:
- Read operations are allowed (list, open_stream for read, read, stream_length)
- Write operations are rejected with YakError (mkdir, create_stream, write, delete, rename, truncate)

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
import libyak as yak


class TestOpenModeReadOnly:
    """Tests that a read-only Yak file rejects all write operations."""

    @pytest.fixture()
    def populated_yak(self, tmp_path):
        """Create a Yak file with some content, then close it."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.mkdir("docs")
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello, World!")
        f.close_stream(h)
        h = f.create_stream("docs/readme.txt")
        f.write(h, b"README content")
        f.close_stream(h)
        f.close()
        return yak_path

    def test_open_for_read_allows_list(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        entries = f.list()
        names = {e.name for e in entries}
        assert "hello.txt" in names
        assert "docs" in names
        f.close()

    def test_open_for_read_allows_list_subdir(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        entries = f.list("docs")
        names = {e.name for e in entries}
        assert "readme.txt" in names
        f.close()

    def test_open_for_read_allows_read_stream(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        h = f.open_stream("hello.txt", yak.OpenMode.READ)
        data = f.read(h, 100)
        assert data == b"Hello, World!"
        f.close_stream(h)
        f.close()

    def test_open_for_read_allows_stream_length(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        h = f.open_stream("hello.txt", yak.OpenMode.READ)
        length = f.stream_length(h)
        assert length == 13
        f.close_stream(h)
        f.close()

    def test_open_for_read_allows_seek_tell(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        h = f.open_stream("hello.txt", yak.OpenMode.READ)
        f.seek(h, 7)
        assert f.tell(h) == 7
        data = f.read(h, 100)
        assert data == b"World!"
        f.close_stream(h)
        f.close()

    def test_open_for_read_rejects_mkdir(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.mkdir("newdir")
        f.close()

    def test_open_for_read_rejects_rmdir(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.rmdir("docs")
        f.close()

    def test_open_for_read_rejects_create_stream(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.create_stream("new.txt")
        f.close()

    def test_open_for_read_rejects_open_stream_write(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.open_stream("hello.txt", yak.OpenMode.WRITE)
        f.close()

    def test_open_for_read_rejects_delete_stream(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.delete_stream("hello.txt")
        f.close()

    def test_open_for_read_rejects_rename_stream(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.rename_stream("hello.txt", "goodbye.txt")
        f.close()

    def test_open_for_read_rejects_rename_dir(self, populated_yak):
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        with pytest.raises(yak.YakError, match="read.only"):
            f.rename_dir("docs", "documents")
        f.close()

    def test_open_for_read_rejects_truncate(self, populated_yak):
        """Even if a stream is opened for read, truncate should fail."""
        f = yak.Yak.open(populated_yak, yak.OpenMode.READ)
        h = f.open_stream("hello.txt", yak.OpenMode.READ)
        with pytest.raises(yak.YakError):
            f.truncate(h, 5)
        f.close_stream(h)
        f.close()


class TestOpenModeWrite:
    """Tests that a write-mode Yak file allows all operations."""

    def test_open_for_write_allows_all(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello")
        f.close_stream(h)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        # Read operations
        entries = f.list()
        assert len(entries) == 1
        h = f.open_stream("hello.txt", yak.OpenMode.READ)
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
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello")
        f.close_stream(h)
        f.close()

        f1 = yak.Yak.open(yak_path, yak.OpenMode.READ)
        f2 = yak.Yak.open(yak_path, yak.OpenMode.READ)

        # Both can read
        h1 = f1.open_stream("hello.txt", yak.OpenMode.READ)
        h2 = f2.open_stream("hello.txt", yak.OpenMode.READ)
        assert f1.read(h1, 100) == b"Hello"
        assert f2.read(h2, 100) == b"Hello"
        f1.close_stream(h1)
        f2.close_stream(h2)

        f2.close()
        f1.close()

    def test_write_excludes_second_write(self, tmp_path):
        """Only one write-mode open should succeed at a time."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        f1 = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        with pytest.raises(yak.YakError):
            yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f1.close()

    def test_write_excludes_read(self, tmp_path):
        """A write-mode open should exclude read-mode opens."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        f1 = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        with pytest.raises(yak.YakError):
            yak.Yak.open(yak_path, yak.OpenMode.READ)
        f1.close()

    def test_read_excludes_write(self, tmp_path):
        """A read-mode open should exclude write-mode opens."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        f.close()

        f1 = yak.Yak.open(yak_path, yak.OpenMode.READ)
        with pytest.raises(yak.YakError):
            yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        f1.close()


# ---------------------------------------------------------------------------
# Helper script for cross-process tests
# ---------------------------------------------------------------------------

# Child script: tries to open a Yak file in the given mode.
# Exit code 0 = open succeeded, exit code 1 = open was rejected.
_CHILD_SCRIPT = """\
import os, sys
sys.path.insert(0, sys.argv[1])
import libyak as yak

mode = yak.OpenMode.READ if sys.argv[3] == "0" else yak.OpenMode.WRITE
try:
    f = yak.Yak.open(sys.argv[2], mode)
    f.close()
except yak.YakError:
    os._exit(1)
"""


def _try_open_in_child(yak_path, mode):
    """Spawn a child process that tries to open the Yak file.

    Returns True if the child opened successfully, False if rejected.
    """
    import os
    yak_pytest_dir = os.path.abspath(
        os.path.join(os.path.dirname(__file__), "..")
    )
    result = subprocess.run(
        [sys.executable, "-c", _CHILD_SCRIPT, yak_pytest_dir, yak_path, str(int(mode))],
        capture_output=True, timeout=10,
    )
    return result.returncode == 0


class TestCrossProcessLocking:
    """Tests that OS-level file locks work across separate processes.

    The parent holds the lock; child processes attempt to open the file.
    """

    @pytest.fixture()
    def yak_file(self, tmp_path):
        """Create a populated Yak file for locking tests."""
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        h = f.create_stream("hello.txt")
        f.write(h, b"Hello, World!")
        f.close_stream(h)
        f.close()
        return yak_path

    def test_concurrent_readers_cross_process(self, yak_file):
        """Parent holds a read lock; child can also read."""
        f = yak.Yak.open(yak_file, yak.OpenMode.READ)
        assert _try_open_in_child(yak_file, yak.OpenMode.READ)
        f.close()

    def test_writer_excludes_writer_cross_process(self, yak_file):
        """Parent holds a write lock; child cannot write."""
        f = yak.Yak.open(yak_file, yak.OpenMode.WRITE)
        assert not _try_open_in_child(yak_file, yak.OpenMode.WRITE)
        f.close()

    def test_writer_excludes_reader_cross_process(self, yak_file):
        """Parent holds a write lock; child cannot read."""
        f = yak.Yak.open(yak_file, yak.OpenMode.WRITE)
        assert not _try_open_in_child(yak_file, yak.OpenMode.READ)
        f.close()

    def test_reader_excludes_writer_cross_process(self, yak_file):
        """Parent holds a read lock; child cannot write."""
        f = yak.Yak.open(yak_file, yak.OpenMode.READ)
        assert not _try_open_in_child(yak_file, yak.OpenMode.WRITE)
        f.close()
