"""Phase 4: Stream I/O tests."""

import pytest
import libyak as yak


class TestStreamIO:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_write_and_read(self, fs):
        """Write bytes, seek to 0, read them back."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        fs.seek(handle, 0)
        data = fs.read(handle, 11)
        assert data == b"hello world"
        fs.close_stream(handle)

    def test_write_extends_stream(self, fs):
        """Writing past end extends the stream."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        assert fs.stream_length(handle) == 5
        fs.write(handle, b" world")
        assert fs.stream_length(handle) == 11
        fs.close_stream(handle)

    def test_read_at_end(self, fs):
        """Reading at end of stream returns 0 bytes."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        data = fs.read(handle, 10)
        assert data == b""
        fs.close_stream(handle)

    def test_read_partial(self, fs):
        """Reading more than available returns what's there."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.seek(handle, 2)
        data = fs.read(handle, 100)
        assert data == b"llo"
        fs.close_stream(handle)

    def test_seek_and_tell(self, fs):
        """Seek to position, tell returns that position."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        assert fs.tell(handle) == 11
        fs.seek(handle, 5)
        assert fs.tell(handle) == 5
        fs.close_stream(handle)

    def test_seek_out_of_bounds(self, fs):
        """Seeking past stream length fails."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        with pytest.raises(yak.YakError):
            fs.seek(handle, 10)
        fs.close_stream(handle)

    def test_stream_length(self, fs):
        """Length reflects what's been written."""
        handle = fs.create_stream("data.bin")
        assert fs.stream_length(handle) == 0
        fs.write(handle, b"hello")
        assert fs.stream_length(handle) == 5
        fs.close_stream(handle)

    def test_sequential_writes(self, fs):
        """Multiple writes advance head correctly."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello")
        fs.write(handle, b" ")
        fs.write(handle, b"world")
        fs.seek(handle, 0)
        data = fs.read(handle, 11)
        assert data == b"hello world"
        fs.close_stream(handle)

    def test_sequential_reads(self, fs):
        """Multiple reads advance head correctly."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        fs.seek(handle, 0)
        d1 = fs.read(handle, 5)
        d2 = fs.read(handle, 1)
        d3 = fs.read(handle, 5)
        assert d1 == b"hello"
        assert d2 == b" "
        assert d3 == b"world"
        fs.close_stream(handle)

    def test_truncate(self, fs):
        """Truncate stream to shorter length."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        fs.truncate(handle, 5)
        assert fs.stream_length(handle) == 5
        fs.seek(handle, 0)
        data = fs.read(handle, 10)
        assert data == b"hello"
        fs.close_stream(handle)

    def test_truncate_moves_position(self, fs):
        """If head is past new length, head moves to new length."""
        handle = fs.create_stream("data.bin")
        fs.write(handle, b"hello world")
        assert fs.tell(handle) == 11
        fs.truncate(handle, 5)
        assert fs.tell(handle) == 5
        fs.close_stream(handle)
