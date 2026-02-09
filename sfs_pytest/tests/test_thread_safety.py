"""Thread safety smoke test: hammer an SFS file from many threads."""

import time
import threading
import pytest
import sfs


NUM_THREADS = 40
NUM_STREAMS = 10


class TestThreadSafety:
    @pytest.fixture
    def burn_seconds(self, request):
        return request.config.getoption("--burn-seconds")

    @pytest.fixture
    def sfs_path(self, tmp_path):
        """Create and populate an SFS file with known data."""
        path = str(tmp_path / "test.sfs")
        f = sfs.Sfs.create(path)

        # Create some directories
        f.mkdir("docs")
        f.mkdir("docs/images")
        f.mkdir("data")

        # Create streams with predictable content
        for i in range(NUM_STREAMS):
            content = f"stream-{i}:".encode() + bytes(range(256)) * (i + 1)
            h = f.create_stream(f"data/file{i}.bin")
            f.write(h, content)
            f.close_stream(h)

        # A stream in root
        h = f.create_stream("readme")
        f.write(h, b"hello from root")
        f.close_stream(h)

        # A stream in a nested dir
        h = f.create_stream("docs/images/logo")
        f.write(h, b"PNG-FAKE-DATA" * 100)
        f.close_stream(h)

        f.close()
        return path

    def test_concurrent_reads(self, sfs_path, burn_seconds):
        """One Sfs instance shared across 40 threads, each reading all streams in a loop."""
        f = sfs.Sfs.open(sfs_path)
        errors = []
        barrier = threading.Barrier(NUM_THREADS)

        def reader_thread(thread_id):
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds
                rounds = 0

                while time.monotonic() < deadline:
                    # List root
                    root_entries = f.list()
                    names = {e.name for e in root_entries}
                    assert "docs" in names, f"T{thread_id}: missing 'docs' in root"
                    assert "data" in names, f"T{thread_id}: missing 'data' in root"
                    assert "readme" in names, f"T{thread_id}: missing 'readme' in root"

                    # List nested dir
                    img_entries = f.list("docs/images")
                    assert len(img_entries) == 1, f"T{thread_id}: expected 1 entry in docs/images"
                    assert img_entries[0].name == "logo"

                    # Read each data stream and verify content
                    for i in range(NUM_STREAMS):
                        expected = f"stream-{i}:".encode() + bytes(range(256)) * (i + 1)
                        h = f.open_stream(f"data/file{i}.bin", sfs.OpenMode.READ)
                        length = f.stream_length(h)
                        assert length == len(expected), (
                            f"T{thread_id}: file{i}.bin length {length} != {len(expected)}"
                        )
                        data = f.read(h, length)
                        assert data == expected, (
                            f"T{thread_id}: file{i}.bin content mismatch"
                        )
                        f.close_stream(h)

                    # Read root stream
                    h = f.open_stream("readme", sfs.OpenMode.READ)
                    data = f.read(h, 100)
                    assert data == b"hello from root", f"T{thread_id}: readme mismatch"
                    f.close_stream(h)

                    # Read nested stream
                    h = f.open_stream("docs/images/logo", sfs.OpenMode.READ)
                    data = f.read(h, 1300)
                    assert data == b"PNG-FAKE-DATA" * 100, f"T{thread_id}: logo mismatch"
                    f.close_stream(h)

                    rounds += 1
                    time.sleep(0.01)

            except Exception as e:
                errors.append(f"Thread {thread_id} (round {rounds}): {e}")

        threads = []
        for i in range(NUM_THREADS):
            t = threading.Thread(target=reader_thread, args=(i,))
            threads.append(t)
            t.start()

        for t in threads:
            t.join(timeout=burn_seconds + 30)

        f.close()
        assert not errors, "Thread errors:\n" + "\n".join(errors)

    def test_shared_instance_concurrent_writes(self, sfs_path, burn_seconds):
        """One Sfs instance shared across 40 threads, each writing its own stream in a loop."""
        f = sfs.Sfs.open(sfs_path)

        # Create all streams upfront (directory writes are serialized by design)
        for i in range(NUM_THREADS):
            h = f.create_stream(f"data/thread_{i}")
            f.close_stream(h)

        errors = []
        barrier = threading.Barrier(NUM_THREADS)

        def writer_thread(thread_id):
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds
                rounds = 0

                while time.monotonic() < deadline:
                    # Write with round-specific content so we catch stale reads
                    content = f"written-by-{thread_id}-round-{rounds}:".encode() + bytes(range(256)) * 4

                    h = f.open_stream(f"data/thread_{thread_id}", sfs.OpenMode.WRITE)
                    f.truncate(h, 0)
                    f.seek(h, 0)
                    f.write(h, content)
                    f.close_stream(h)

                    # Read it back from the same shared instance
                    h = f.open_stream(f"data/thread_{thread_id}", sfs.OpenMode.READ)
                    length = f.stream_length(h)
                    assert length == len(content), (
                        f"T{thread_id} round {rounds}: length {length} != {len(content)}"
                    )
                    data = f.read(h, length)
                    assert data == content, f"T{thread_id} round {rounds}: content mismatch"
                    f.close_stream(h)

                    rounds += 1
                    time.sleep(0.01)

            except Exception as e:
                errors.append(f"Thread {thread_id} (round {rounds}): {e}")

        threads = []
        for i in range(NUM_THREADS):
            t = threading.Thread(target=writer_thread, args=(i,))
            threads.append(t)
            t.start()

        for t in threads:
            t.join(timeout=burn_seconds + 30)

        f.close()
        assert not errors, "Thread errors:\n" + "\n".join(errors)
