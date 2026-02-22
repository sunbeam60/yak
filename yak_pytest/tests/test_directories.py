"""Phase 2: Directory operation tests."""

import pytest
import libyak as yak


class TestDirectories:
    @pytest.fixture
    def fs(self, tmp_path):
        yak_path = str(tmp_path / "test.yak")
        f = yak.Yak.create(yak_path)
        yield f
        f.close()

    def test_mkdir_root(self, fs):
        """Create a directory at root, list shows it."""
        fs.mkdir("textures")
        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "textures"
        assert entries[0].entry_type == yak.EntryType.DIRECTORY

    def test_mkdir_nested(self, fs):
        """Create nested directories (parent first)."""
        fs.mkdir("textures")
        fs.mkdir("textures/skins")
        entries = fs.list("textures")
        assert len(entries) == 1
        assert entries[0].name == "skins"
        assert entries[0].entry_type == yak.EntryType.DIRECTORY

    def test_mkdir_parent_missing(self, fs):
        """Creating 'a/b' when 'a' doesn't exist fails."""
        with pytest.raises(yak.YakError):
            fs.mkdir("a/b")

    def test_mkdir_already_exists(self, fs):
        """Creating a directory that exists fails."""
        fs.mkdir("textures")
        with pytest.raises(yak.YakError):
            fs.mkdir("textures")

    def test_rmdir_empty(self, fs):
        """Delete an empty directory."""
        fs.mkdir("textures")
        fs.rmdir("textures")
        entries = fs.list("")
        assert len(entries) == 0

    def test_rmdir_not_empty(self, fs):
        """Deleting a non-empty directory fails."""
        fs.mkdir("textures")
        fs.mkdir("textures/skins")
        with pytest.raises(yak.YakError):
            fs.rmdir("textures")

    def test_rmdir_nonexistent(self, fs):
        """Deleting non-existent directory fails."""
        with pytest.raises(yak.YakError):
            fs.rmdir("nonexistent")

    def test_list_root_empty(self, fs):
        """Listing root of new Yak returns empty list."""
        entries = fs.list("")
        assert len(entries) == 0

    def test_list_root_with_entries(self, fs):
        """Listing root shows directories."""
        fs.mkdir("textures")
        fs.mkdir("sounds")
        entries = fs.list("")
        assert len(entries) == 2
        names = [e.name for e in entries]
        assert "textures" in names
        assert "sounds" in names
        for e in entries:
            assert e.entry_type == yak.EntryType.DIRECTORY

    def test_list_subdirectory(self, fs):
        """Listing a subdirectory shows its contents."""
        fs.mkdir("textures")
        fs.mkdir("textures/skins")
        fs.mkdir("textures/envmaps")
        entries = fs.list("textures")
        assert len(entries) == 2
        names = [e.name for e in entries]
        assert "skins" in names
        assert "envmaps" in names

    def test_list_nonexistent(self, fs):
        """Listing a non-existent path fails."""
        with pytest.raises(yak.YakError):
            fs.list("nonexistent")

    def test_rename_dir(self, fs):
        """Rename a directory, old gone, new exists."""
        fs.mkdir("old_name")
        fs.rename_dir("old_name", "new_name")
        entries = fs.list("")
        assert len(entries) == 1
        assert entries[0].name == "new_name"
        assert entries[0].entry_type == yak.EntryType.DIRECTORY
