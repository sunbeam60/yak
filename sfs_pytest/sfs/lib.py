"""Low-level ctypes bindings to the stream_fs_c shared library."""

import ctypes
import platform
from pathlib import Path


def _find_library() -> Path:
    """Locate the compiled stream_fs_c shared library."""
    repo_root = Path(__file__).resolve().parents[2]

    # Determine the shared library file name based on the platform
    system = platform.system()
    if system == "Windows":
        lib_name = "stream_fs_c.dll"
    elif system == "Darwin":
        lib_name = "libstream_fs_c.dylib"
    else:
        lib_name = "libstream_fs_c.so"

    # Check both debug and release build directories
    for profile in ("debug", "release"):
        candidate = repo_root / "target" / profile / lib_name
        if candidate.exists():
            return candidate

    raise FileNotFoundError(
        f"Could not find {lib_name}. "
        "Build the project first with: cargo build"
    )


def _load_library():
    """Load the shared library and set up function signatures."""
    path = _find_library()
    lib = ctypes.CDLL(str(path))

    # --- Hello (legacy) ---
    lib.sfs_hello.restype = ctypes.c_void_p
    lib.sfs_hello.argtypes = []

    lib.sfs_free_string.restype = None
    lib.sfs_free_string.argtypes = [ctypes.c_void_p]

    # --- Error handling ---
    lib.sfs_last_error_message.restype = ctypes.c_char_p
    lib.sfs_last_error_message.argtypes = []

    # --- SFS file lifecycle ---
    lib.sfs_create.restype = ctypes.c_void_p
    lib.sfs_create.argtypes = [ctypes.c_char_p, ctypes.c_uint8, ctypes.c_uint8]

    lib.sfs_open.restype = ctypes.c_void_p
    lib.sfs_open.argtypes = [ctypes.c_char_p, ctypes.c_int]

    lib.sfs_close.restype = ctypes.c_int
    lib.sfs_close.argtypes = [ctypes.c_void_p]

    # --- Directory operations ---
    lib.sfs_mkdir.restype = ctypes.c_int
    lib.sfs_mkdir.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    lib.sfs_rmdir.restype = ctypes.c_int
    lib.sfs_rmdir.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    lib.sfs_rename_dir.restype = ctypes.c_int
    lib.sfs_rename_dir.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]

    # --- Listing ---
    lib.sfs_list.restype = ctypes.c_void_p
    lib.sfs_list.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    lib.sfs_list_count.restype = ctypes.c_int
    lib.sfs_list_count.argtypes = [ctypes.c_void_p]

    lib.sfs_list_entry_name.restype = ctypes.c_char_p
    lib.sfs_list_entry_name.argtypes = [ctypes.c_void_p, ctypes.c_int]

    lib.sfs_list_entry_type.restype = ctypes.c_int
    lib.sfs_list_entry_type.argtypes = [ctypes.c_void_p, ctypes.c_int]

    lib.sfs_list_free.restype = None
    lib.sfs_list_free.argtypes = [ctypes.c_void_p]

    # --- Stream lifecycle ---
    lib.sfs_create_stream.restype = ctypes.c_int64
    lib.sfs_create_stream.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    lib.sfs_open_stream.restype = ctypes.c_int64
    lib.sfs_open_stream.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]

    lib.sfs_close_stream.restype = ctypes.c_int
    lib.sfs_close_stream.argtypes = [ctypes.c_void_p, ctypes.c_int64]

    lib.sfs_delete_stream.restype = ctypes.c_int
    lib.sfs_delete_stream.argtypes = [ctypes.c_void_p, ctypes.c_char_p]

    lib.sfs_rename_stream.restype = ctypes.c_int
    lib.sfs_rename_stream.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p]

    # --- Stream I/O ---
    lib.sfs_read.restype = ctypes.c_int64
    lib.sfs_read.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_void_p, ctypes.c_uint64]

    lib.sfs_write.restype = ctypes.c_int64
    lib.sfs_write.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_char_p, ctypes.c_uint64]

    lib.sfs_seek.restype = ctypes.c_int
    lib.sfs_seek.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_uint64]

    lib.sfs_tell.restype = ctypes.c_int64
    lib.sfs_tell.argtypes = [ctypes.c_void_p, ctypes.c_int64]

    lib.sfs_stream_length.restype = ctypes.c_int64
    lib.sfs_stream_length.argtypes = [ctypes.c_void_p, ctypes.c_int64]

    lib.sfs_truncate.restype = ctypes.c_int
    lib.sfs_truncate.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_uint64]

    lib.sfs_reserve.restype = ctypes.c_int
    lib.sfs_reserve.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_uint64]

    lib.sfs_stream_reserved.restype = ctypes.c_int64
    lib.sfs_stream_reserved.argtypes = [ctypes.c_void_p, ctypes.c_int64]

    # --- Verify ---
    lib.sfs_verify.restype = ctypes.c_void_p
    lib.sfs_verify.argtypes = [ctypes.c_void_p]

    lib.sfs_verify_count.restype = ctypes.c_int
    lib.sfs_verify_count.argtypes = [ctypes.c_void_p]

    lib.sfs_verify_issue.restype = ctypes.c_char_p
    lib.sfs_verify_issue.argtypes = [ctypes.c_void_p, ctypes.c_int]

    lib.sfs_verify_free.restype = None
    lib.sfs_verify_free.argtypes = [ctypes.c_void_p]

    return lib


_lib = _load_library()


def hello() -> str:
    """Return a greeting from the Stream File System library."""
    ptr = _lib.sfs_hello()
    try:
        greeting = ctypes.cast(ptr, ctypes.c_char_p).value.decode("utf-8")
    finally:
        _lib.sfs_free_string(ptr)
    return greeting
