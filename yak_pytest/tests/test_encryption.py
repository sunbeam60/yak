"""Tests for optional AES-XTS encryption at L2."""

import pytest
import libyak as yak


class TestEncryptionBasic:
    """Basic encryption lifecycle: create, write, close, reopen, read."""

    def test_create_encrypted(self, tmp_path):
        """Create an encrypted Yak file, write data, close, reopen with
        correct password, and read back the data."""
        yak_path = str(tmp_path / "enc.yak")
        password = b"test-password-123"

        # Create and write
        f = yak.Yak.create(yak_path, password=password)
        sh = f.create_stream("hello.txt")
        f.write(sh, b"Hello, encrypted world!")
        f.close_stream(sh)
        f.close()

        # Reopen with correct password and verify
        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("hello.txt", yak.OpenMode.READ)
        data = f.read(sh, 100)
        assert data == b"Hello, encrypted world!"
        f.close_stream(sh)
        f.close()

    def test_wrong_password(self, tmp_path):
        """Reopening an encrypted file with the wrong password raises YakError."""
        yak_path = str(tmp_path / "enc.yak")
        f = yak.Yak.create(yak_path, password=b"correct")
        f.close()

        with pytest.raises(yak.YakError, match="wrong password"):
            yak.Yak.open(yak_path, yak.OpenMode.READ, password=b"incorrect")

    def test_no_password_on_encrypted(self, tmp_path):
        """Reopening an encrypted file without a password raises YakError."""
        yak_path = str(tmp_path / "enc.yak")
        f = yak.Yak.create(yak_path, password=b"secret")
        f.close()

        with pytest.raises(yak.YakError, match="encryption required"):
            yak.Yak.open(yak_path, yak.OpenMode.READ)

    def test_unencrypted_unchanged(self, tmp_path):
        """Creating and opening without a password still works as before."""
        yak_path = str(tmp_path / "plain.yak")
        f = yak.Yak.create(yak_path)
        sh = f.create_stream("test.txt")
        f.write(sh, b"plaintext data")
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ)
        sh = f.open_stream("test.txt", yak.OpenMode.READ)
        data = f.read(sh, 100)
        assert data == b"plaintext data"
        f.close_stream(sh)
        f.close()

    def test_is_encrypted(self, tmp_path):
        """is_encrypted() returns the correct value for encrypted and
        unencrypted files."""
        enc_path = str(tmp_path / "enc.yak")
        plain_path = str(tmp_path / "plain.yak")

        f = yak.Yak.create(enc_path, password=b"secret")
        assert f.is_encrypted() is True
        f.close()

        f = yak.Yak.create(plain_path)
        assert f.is_encrypted() is False
        f.close()

        # Verify is_encrypted persists across reopen
        f = yak.Yak.open(enc_path, yak.OpenMode.READ, password=b"secret")
        assert f.is_encrypted() is True
        f.close()

        f = yak.Yak.open(plain_path, yak.OpenMode.READ)
        assert f.is_encrypted() is False
        f.close()


class TestEncryptionIntegrity:
    """Encryption + verify and persistence."""

    def test_encrypted_verify(self, tmp_path):
        """verify() passes on an encrypted file with data."""
        yak_path = str(tmp_path / "enc.yak")
        f = yak.Yak.create(yak_path, password=b"verify-pw")
        f.mkdir("docs")
        sh = f.create_stream("hello.txt")
        f.write(sh, b"Hello!")
        f.close_stream(sh)
        sh = f.create_stream("docs/readme.txt")
        f.write(sh, b"A readme")
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=b"verify-pw")
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_encrypted_verify_after_delete(self, tmp_path):
        """verify() passes after deleting streams (free list with encryption)."""
        yak_path = str(tmp_path / "enc.yak")
        f = yak.Yak.create(yak_path, password=b"del-pw")
        sh = f.create_stream("a.txt")
        f.write(sh, b"AAAA" * 1000)
        f.close_stream(sh)
        sh = f.create_stream("b.txt")
        f.write(sh, b"BBBB" * 1000)
        f.close_stream(sh)
        f.delete_stream("b.txt")
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=b"del-pw")
        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"

    def test_encrypted_persistence(self, tmp_path):
        """Data survives close/reopen cycle with encryption."""
        yak_path = str(tmp_path / "enc.yak")
        password = b"persist-pw"
        data = bytes(range(256)) * 40  # 10240 bytes

        f = yak.Yak.create(yak_path, password=password)
        sh = f.create_stream("data.bin")
        f.write(sh, data)
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("data.bin", yak.OpenMode.READ)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)
        f.close()


class TestEncryptionDirectories:
    """Directory operations work with encryption."""

    def test_encrypted_directories(self, tmp_path):
        """mkdir, list, and stream ops work under encryption."""
        yak_path = str(tmp_path / "enc.yak")
        password = b"dir-pw"

        f = yak.Yak.create(yak_path, password=password)
        f.mkdir("alpha")
        f.mkdir("alpha/beta")
        sh = f.create_stream("alpha/file.txt")
        f.write(sh, b"file in alpha")
        f.close_stream(sh)
        sh = f.create_stream("alpha/beta/deep.txt")
        f.write(sh, b"deep file")
        f.close_stream(sh)
        f.close()

        # Reopen and verify directory structure
        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        root_entries = f.list()
        assert any(e.name == "alpha" for e in root_entries)

        alpha_entries = f.list("alpha")
        names = {e.name for e in alpha_entries}
        assert "beta" in names
        assert "file.txt" in names

        sh = f.open_stream("alpha/beta/deep.txt", yak.OpenMode.READ)
        data = f.read(sh, 100)
        assert data == b"deep file"
        f.close_stream(sh)
        f.close()

    def test_encrypted_rename(self, tmp_path):
        """Rename operations work under encryption."""
        yak_path = str(tmp_path / "enc.yak")
        password = b"rename-pw"

        f = yak.Yak.create(yak_path, password=password)
        sh = f.create_stream("original.txt")
        f.write(sh, b"some data")
        f.close_stream(sh)
        f.rename_stream("original.txt", "renamed.txt")
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("renamed.txt", yak.OpenMode.READ)
        data = f.read(sh, 100)
        assert data == b"some data"
        f.close_stream(sh)
        f.close()


class TestEncryptionMultiblock:
    """Large writes spanning many blocks with encryption."""

    def test_encrypted_multiblock(self, tmp_path):
        """Large writes spanning many blocks work under encryption."""
        yak_path = str(tmp_path / "enc.yak")
        password = b"multi-pw"

        # Use default 4KB blocks — 100KB data spans ~25 blocks
        data = bytes(i & 0xFF for i in range(100_000))

        f = yak.Yak.create(yak_path, password=password)
        sh = f.create_stream("big.bin")
        f.write(sh, data)
        assert f.stream_length(sh) == len(data)
        f.seek(sh, 0)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)
        f.close()

        # Reopen and verify persistence
        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("big.bin", yak.OpenMode.READ)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)
        f.close()

    def test_encrypted_small_blocks(self, tmp_path):
        """Encryption with small blocks (bss=4, 16 bytes — minimum for AES-XTS)."""
        yak_path = str(tmp_path / "enc_small.yak")
        password = b"small-pw"

        # bss=4 → 16-byte blocks (minimum for XTS), biw=2
        f = yak.Yak.create(
            yak_path, block_index_width=2, block_size_shift=4, password=password
        )
        data = b"sixteen bytes!!" + b"\x00"  # exactly 16 bytes
        sh = f.create_stream("tiny.bin")
        f.write(sh, data)
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("tiny.bin", yak.OpenMode.READ)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)
        f.close()

    @pytest.mark.parametrize("biw,bss", [(2, 6), (4, 6), (4, 12), (2, 12)])
    def test_encrypted_varying_params(self, tmp_path, biw, bss):
        """Encryption works across different biw and bss combinations."""
        yak_path = str(tmp_path / f"enc_{biw}_{bss}.yak")
        password = b"param-pw"
        data = b"X" * 500

        f = yak.Yak.create(
            yak_path,
            block_index_width=biw,
            block_size_shift=bss,
            password=password,
        )
        sh = f.create_stream("data.bin")
        f.write(sh, data)
        f.close_stream(sh)
        f.close()

        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("data.bin", yak.OpenMode.READ)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)

        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"


class TestEncryptionCompression:
    """Encrypted + compressed streams."""

    def test_encrypted_compression(self, tmp_path):
        """Compressed streams work under encryption."""
        yak_path = str(tmp_path / "enc_comp.yak")
        password = b"comp-pw"

        # Highly compressible data
        data = b"ABCDEFGH" * 5000  # 40KB of repetitive data

        f = yak.Yak.create(yak_path, password=password)
        sh = f.create_stream("compressed.bin", compressed=True)
        f.write(sh, data)
        assert f.stream_length(sh) == len(data)
        f.seek(sh, 0)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)
        f.close()

        # Reopen and verify
        f = yak.Yak.open(yak_path, yak.OpenMode.READ, password=password)
        sh = f.open_stream("compressed.bin", yak.OpenMode.READ)
        result = f.read(sh, len(data))
        assert result == data
        f.close_stream(sh)

        issues = f.verify()
        f.close()
        assert issues == [], f"Expected no issues, got: {issues}"
