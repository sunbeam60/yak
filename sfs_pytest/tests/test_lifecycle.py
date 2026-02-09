"""Phase 1: SFS file lifecycle tests."""

import pytest
from pathlib import Path
import sfs


class TestSfsLifecycle:
    def test_create_new_sfs(self, tmp_path):
        """Create an SFS file, verify directory exists on disk."""
        sfs_path = str(tmp_path / "test.sfs")
        fs = sfs.Sfs.create(sfs_path)
        assert Path(sfs_path).is_dir()
        fs.close()

    def test_create_already_exists(self, tmp_path):
        """Creating where directory already exists fails."""
        sfs_path = str(tmp_path / "test.sfs")
        fs = sfs.Sfs.create(sfs_path)
        fs.close()
        with pytest.raises(sfs.SfsError):
            sfs.Sfs.create(sfs_path)

    def test_open_existing(self, tmp_path):
        """Open a previously created SFS file."""
        sfs_path = str(tmp_path / "test.sfs")
        fs = sfs.Sfs.create(sfs_path)
        fs.close()
        fs = sfs.Sfs.open(sfs_path)
        fs.close()

    def test_open_nonexistent(self, tmp_path):
        """Opening a non-existent path fails."""
        sfs_path = str(tmp_path / "nonexistent.sfs")
        with pytest.raises(sfs.SfsError):
            sfs.Sfs.open(sfs_path)

    def test_close(self, tmp_path):
        """Close an SFS file without error."""
        sfs_path = str(tmp_path / "test.sfs")
        fs = sfs.Sfs.create(sfs_path)
        fs.close()  # should not raise
