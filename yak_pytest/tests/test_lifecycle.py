"""Phase 1: Yak file lifecycle tests."""

import pytest
from pathlib import Path
import libyak as yak


class TestYakLifecycle:
    def test_create_new_yak(self, tmp_path):
        """Create a Yak file, verify it exists on disk."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        assert Path(yak_path).exists()
        fs.close()

    def test_create_already_exists(self, tmp_path):
        """Creating where directory already exists fails."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        fs.close()
        with pytest.raises(yak.YakError):
            yak.Yak.create(yak_path)

    def test_open_existing(self, tmp_path):
        """Open a previously created Yak file."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        fs.close()
        fs = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
        fs.close()

    def test_open_nonexistent(self, tmp_path):
        """Opening a non-existent path fails."""
        yak_path = str(tmp_path / "nonexistent.yak")
        with pytest.raises(yak.YakError):
            yak.Yak.open(yak_path, yak.OpenMode.WRITE)

    def test_close(self, tmp_path):
        """Close a Yak file without error."""
        yak_path = str(tmp_path / "test.yak")
        fs = yak.Yak.create(yak_path)
        fs.close()  # should not raise
