"""Phase 5: Locking tests."""

import pytest
import libyak as yak


class TestLocking:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_multiple_readers(self, fs):
        """Open same stream for reading twice, both work."""
        # Create and close a stream with some data
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        fs.close_stream(handle)

        # Open for reading twice
        h1 = fs.open_stream("data.bin", yak.OpenMode.READ)
        h2 = fs.open_stream("data.bin", yak.OpenMode.READ)

        # Both should be able to read independently
        data1 = fs.read(h1, 5)
        data2 = fs.read(h2, 5)
        assert data1 == b"hello"
        assert data2 == b"hello"

        fs.close_stream(h1)
        fs.close_stream(h2)

    def test_reader_blocks_writer(self, fs):
        """Open for read, then open for write fails."""
        # Create and close a stream with some data
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.close_stream(handle)

        # Open for reading
        h_read = fs.open_stream("data.bin", yak.OpenMode.READ)

        # Try to open for writing - should fail
        with pytest.raises(yak.YakError):
            fs.open_stream("data.bin", yak.OpenMode.WRITE)

        fs.close_stream(h_read)

    def test_writer_blocks_reader(self, fs):
        """Open for write, then open for read fails."""
        # Create and close a stream
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.close_stream(handle)

        # Open for writing
        h_write = fs.open_stream("data.bin", yak.OpenMode.WRITE)

        # Try to open for reading - should fail
        with pytest.raises(yak.YakError):
            fs.open_stream("data.bin", yak.OpenMode.READ)

        fs.close_stream(h_write)

    def test_writer_blocks_writer(self, fs):
        """Open for write, then open for write again fails."""
        # Create and close a stream
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.close_stream(handle)

        # Open for writing
        h1 = fs.open_stream("data.bin", yak.OpenMode.WRITE)

        # Try to open for writing again - should fail
        with pytest.raises(yak.YakError):
            fs.open_stream("data.bin", yak.OpenMode.WRITE)

        fs.close_stream(h1)

    def test_close_writer_then_read(self, fs):
        """Close writer, then opening for read succeeds."""
        # Create and close a stream
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.close_stream(handle)

        # Open for writing
        h_write = fs.open_stream("data.bin", yak.OpenMode.WRITE)

        # Close the writer
        fs.close_stream(h_write)

        # Now opening for read should succeed
        h_read = fs.open_stream("data.bin", yak.OpenMode.READ)
        data = fs.read(h_read, 5)
        assert data == b"hello"
        fs.close_stream(h_read)

    def test_close_readers_then_write(self, fs):
        """Close all readers, then opening for write succeeds."""
        # Create and close a stream
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.close_stream(handle)

        # Open for reading twice
        h1 = fs.open_stream("data.bin", yak.OpenMode.READ)
        h2 = fs.open_stream("data.bin", yak.OpenMode.READ)

        # Close both readers
        fs.close_stream(h1)
        fs.close_stream(h2)

        # Now opening for write should succeed
        h_write = fs.open_stream("data.bin", yak.OpenMode.WRITE)
        fs.write(h_write, b"world")
        fs.close_stream(h_write)

    def test_create_stream_is_writer(self, fs):
        """create_stream counts as a writer (blocks other opens)."""
        # Create a stream (returns write handle)
        h_create = fs.create_stream("data.bin")

        # Try to open for reading - should fail
        with pytest.raises(yak.YakError):
            fs.open_stream("data.bin", yak.OpenMode.READ)

        # Try to open for writing - should fail
        with pytest.raises(yak.YakError):
            fs.open_stream("data.bin", yak.OpenMode.WRITE)

        # After closing, should be able to open
        fs.close_stream(h_create)
        h_read = fs.open_stream("data.bin", yak.OpenMode.READ)
        fs.close_stream(h_read)
