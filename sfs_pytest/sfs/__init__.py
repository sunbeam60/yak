"""Python wrapper around the Stream File System C ABI."""

from enum import IntEnum

from .lib import _lib, hello

import ctypes


class SfsError(Exception):
    """Raised when an SFS operation fails."""
    pass


class EntryType(IntEnum):
    STREAM = 0
    DIRECTORY = 1


class DirEntry:
    """An entry returned when listing a directory."""

    def __init__(self, name: str, entry_type: EntryType):
        self.name = name
        self.entry_type = entry_type

    def __repr__(self) -> str:
        return f"DirEntry({self.name!r}, {self.entry_type.name})"


class OpenMode(IntEnum):
    READ = 0
    WRITE = 1


def _check_error():
    """Raise SfsError with the last error message from the C library."""
    msg_ptr = _lib.sfs_last_error_message()
    if msg_ptr:
        raise SfsError(msg_ptr.decode("utf-8"))
    raise SfsError("unknown error")


class Sfs:
    """Represents an open SFS file."""

    def __init__(self, handle):
        self._handle = handle

    @staticmethod
    def create(path: str) -> "Sfs":
        """Create a new SFS file at the given path."""
        handle = _lib.sfs_create(path.encode("utf-8"))
        if handle is None:
            _check_error()
        return Sfs(handle)

    @staticmethod
    def open(path: str) -> "Sfs":
        """Open an existing SFS file at the given path."""
        handle = _lib.sfs_open(path.encode("utf-8"))
        if handle is None:
            _check_error()
        return Sfs(handle)

    def close(self) -> None:
        """Close the SFS file."""
        if self._handle is not None:
            _lib.sfs_close(self._handle)
            self._handle = None

    def mkdir(self, path: str) -> None:
        """Create a directory inside the SFS file."""
        result = _lib.sfs_mkdir(self._handle, path.encode("utf-8"))
        if result != 0:
            _check_error()

    def rmdir(self, path: str) -> None:
        """Remove an empty directory from the SFS file."""
        result = _lib.sfs_rmdir(self._handle, path.encode("utf-8"))
        if result != 0:
            _check_error()

    def list(self, path: str = "") -> list[DirEntry]:
        """List the contents of a directory. Use '' for root."""
        list_handle = _lib.sfs_list(self._handle, path.encode("utf-8"))
        if list_handle is None:
            _check_error()
        try:
            count = _lib.sfs_list_count(list_handle)
            entries = []
            for i in range(count):
                name_ptr = _lib.sfs_list_entry_name(list_handle, i)
                name = name_ptr.decode("utf-8")
                entry_type = EntryType(_lib.sfs_list_entry_type(list_handle, i))
                entries.append(DirEntry(name, entry_type))
            return entries
        finally:
            _lib.sfs_list_free(list_handle)

    def rename_dir(self, old_path: str, new_path: str) -> None:
        """Rename/move a directory."""
        result = _lib.sfs_rename_dir(
            self._handle,
            old_path.encode("utf-8"),
            new_path.encode("utf-8"),
        )
        if result != 0:
            _check_error()

    # --- Stream lifecycle ---

    def create_stream(self, path: str) -> int:
        """Create a new stream and open it for writing. Returns a handle."""
        handle = _lib.sfs_create_stream(self._handle, path.encode("utf-8"))
        if handle < 0:
            _check_error()
        return handle

    def open_stream(self, path: str, mode: OpenMode) -> int:
        """Open an existing stream. Returns a handle."""
        handle = _lib.sfs_open_stream(
            self._handle, path.encode("utf-8"), int(mode)
        )
        if handle < 0:
            _check_error()
        return handle

    def close_stream(self, handle: int) -> None:
        """Close a stream handle."""
        result = _lib.sfs_close_stream(self._handle, handle)
        if result != 0:
            _check_error()

    def delete_stream(self, path: str) -> None:
        """Delete a stream by path. Must not be currently open."""
        result = _lib.sfs_delete_stream(self._handle, path.encode("utf-8"))
        if result != 0:
            _check_error()

    def rename_stream(self, old_path: str, new_path: str) -> None:
        """Rename/move a stream by path."""
        result = _lib.sfs_rename_stream(
            self._handle,
            old_path.encode("utf-8"),
            new_path.encode("utf-8"),
        )
        if result != 0:
            _check_error()

    # --- Stream I/O ---

    def read(self, handle: int, length: int) -> bytes:
        """Read up to `length` bytes from the stream."""
        buf = ctypes.create_string_buffer(length)
        result = _lib.sfs_read(self._handle, handle, buf, length)
        if result < 0:
            _check_error()
        return buf.raw[:result]

    def write(self, handle: int, data: bytes) -> int:
        """Write data to the stream. Returns bytes written."""
        result = _lib.sfs_write(self._handle, handle, data, len(data))
        if result < 0:
            _check_error()
        return result

    def seek(self, handle: int, pos: int) -> None:
        """Set the head position."""
        result = _lib.sfs_seek(self._handle, handle, pos)
        if result != 0:
            _check_error()

    def tell(self, handle: int) -> int:
        """Get the current head position."""
        result = _lib.sfs_tell(self._handle, handle)
        if result < 0:
            _check_error()
        return result

    def stream_length(self, handle: int) -> int:
        """Get the total length of the stream."""
        result = _lib.sfs_stream_length(self._handle, handle)
        if result < 0:
            _check_error()
        return result

    def truncate(self, handle: int, new_len: int) -> None:
        """Truncate the stream to the given length."""
        result = _lib.sfs_truncate(self._handle, handle, new_len)
        if result != 0:
            _check_error()
