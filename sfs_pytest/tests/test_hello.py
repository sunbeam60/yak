"""Tests for the SFS hello world function via the C ABI."""

import sfs


def test_hello():
    """The hello function should return the expected greeting."""
    result = sfs.hello()
    assert result == "Hello from Stream File System!"
