"""Phase 3: Stream creation and basic operations tests."""

import pytest
import libyak as yak


class TestStreams:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_create_stream(self, fs):
        """Create a stream, verify it appears in list."""
        handle = fs.create_stream("image.png")
        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "image.png"
        assert entries[0].entry_type == yak.EntryType.STREAM
        fs.close_stream(handle)

    def test_create_stream_in_subdir(self, fs):
        """Create stream in subdirectory, list shows it."""
        fs.mkdir("textures")
        handle = fs.create_stream("textures/img.png")
        entries = fs.list("textures")
        assert len(entries) == 1
        assert entries[0].name == "img.png"
        assert entries[0].entry_type == yak.EntryType.STREAM
        fs.close_stream(handle)

    def test_create_stream_dup(self, fs):
        """Creating a stream with existing name fails."""
        handle = fs.create_stream("image.png")
        with pytest.raises(yak.YakError):
            fs.create_stream("image.png")
        fs.close_stream(handle)

    def test_delete_stream(self, fs):
        """Delete a stream, no longer in list."""
        handle = fs.create_stream("image.png")
        fs.close_stream(handle)
        fs.delete_stream("image.png")
        entries = fs.list("")
        assert len(entries) == 0

    def test_delete_stream_while_open(self, fs):
        """Deleting an open stream fails."""
        handle = fs.create_stream("image.png")
        with pytest.raises(yak.YakError):
            fs.delete_stream("image.png")
        fs.close_stream(handle)

    def test_rename_stream(self, fs):
        """Rename a stream."""
        handle = fs.create_stream("old.png")
        fs.close_stream(handle)
        fs.rename_stream("old.png", "new.png")
        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "new.png"
        assert entries[0].entry_type == yak.EntryType.STREAM
