"""Phase 6: Edge cases tests."""

import pytest
import libyak as yak


class TestEdgeCases:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_empty_stream(self, fs):
        """Create stream, don't write, length is 0."""
        handle = fs.create_stream("empty.bin")
        assert fs.stream_length(handle) == 0
        assert fs.tell(handle) == 0

        # Reading from empty stream should return empty bytes
        data = fs.read(handle, 10)
        assert data == b""

        fs.close_stream(handle)

    def test_write_empty_buffer(self, fs):
        """Writing 0 bytes succeeds, stream unchanged."""
        handle = fs.create_stream("data.bin")

        # Write some data
        fs.write(handle, b"hello")
        assert fs.stream_length(handle) == 5
        assert fs.tell(handle) == 5

        # Write empty buffer
        fs.write(handle, b"")

        # Stream should be unchanged
        assert fs.stream_length(handle) == 5
        assert fs.tell(handle) == 5

        fs.close_stream(handle)

    def test_deeply_nested(self, fs):
        """Create deeply nested path, read/write works."""
        # Create nested directories
        fs.mkdir("a")
        fs.mkdir("a/b")
        fs.mkdir("a/b/c")
        fs.mkdir("a/b/c/d")
        fs.mkdir("a/b/c/d/e")

        # Create stream in deep path
        handle = fs.create_stream("a/b/c/d/e/f.dat")
        fs.write(handle, b"deep data")
        fs.seek(handle, 0)
        data = fs.read(handle, 9)
        assert data == b"deep data"
        fs.close_stream(handle)

        # Verify it appears in listing
        entries = fs.list("a/b/c/d/e")
        assert len(entries) == 1
        assert entries[0].name == "f.dat"
        assert entries[0].entry_type == yak.EntryType.STREAM

    def test_stream_names_with_spaces(self, fs):
        """Stream names with spaces work correctly."""
        handle = fs.create_stream("my file.txt")
        fs.write(handle, b"hello world")
        fs.close_stream(handle)

        # Verify it appears in listing
        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "my file.txt"

        # Reopen and read
        h_read = fs.open_stream("my file.txt", yak.OpenMode.READ)
        data = fs.read(h_read, 11)
        assert data == b"hello world"
        fs.close_stream(h_read)

    def test_reopen_after_close(self, fs):
        """Close and reopen a stream, data persists."""
        # Create and write data
        handle = fs.create_stream("persistent.dat")
        fs.write(handle, b"persistent data")
        fs.close_stream(handle)

        # Reopen for reading
        h_read = fs.open_stream("persistent.dat", yak.OpenMode.READ)
        data = fs.read(h_read, 15)
        assert data == b"persistent data"
        assert fs.stream_length(h_read) == 15
        fs.close_stream(h_read)

        # Reopen for writing (appending)
        h_write = fs.open_stream("persistent.dat", yak.OpenMode.WRITE)
        fs.seek(h_write, 15)  # Seek to end
        fs.write(h_write, b" more")
        fs.close_stream(h_write)

        # Reopen again and verify
        h_read2 = fs.open_stream("persistent.dat", yak.OpenMode.READ)
        data = fs.read(h_read2, 20)
        assert data == b"persistent data more"
        fs.close_stream(h_read2)
