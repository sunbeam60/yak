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

    # Use c_void_p so we get the raw pointer back (c_char_p auto-converts
    # to bytes and loses the pointer needed for freeing).
    lib.sfs_hello.restype = ctypes.c_void_p
    lib.sfs_hello.argtypes = []

    lib.sfs_free_string.restype = None
    lib.sfs_free_string.argtypes = [ctypes.c_void_p]

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
