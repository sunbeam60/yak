"""Thread safety smoke test: hammer a Yak file from many threads."""

import time
import threading
import pytest
import libyak as yak


NUM_THREADS = 40
NUM_STREAMS = 10
NUM_LIFECYCLE_THREADS = 20


class TestStreamLifecycleStress:
    """Stress-test concurrent stream creation, writing, closing, and deletion.

    Unlike the other thread-safety tests which pre-create streams,
    this hammers the descriptor allocation/deallocation paths and
    concurrent directory-stream modifications.
    """

    @pytest.fixture
    def burn_seconds(self, request):
        return request.config.getoption("--burn-seconds")

    def test_concurrent_create_write_close_delete(self, tmp_path, burn_seconds):
        """Threads concurrently create, write, close, verify, and delete streams.

        All threads share a single "arena" directory, maximising contention
        on the directory stream write-lock (L4 uses open_stream_blocking so
        threads queue up rather than failing).
        Uses small blocks (64 bytes) to force multi-block growth quickly.
        """
        yak_path = str(tmp_path / "lifecycle.yak")
        f = yak.Yak.create(yak_path, block_index_width=4, block_size_shift=6)

        # Single shared directory -- all threads contend on the same
        # directory stream write-lock.
        f.mkdir("arena")

        errors = []
        barrier = threading.Barrier(NUM_LIFECYCLE_THREADS)

        def lifecycle_thread(tid):
            rounds = 0
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds

                while time.monotonic() < deadline:
                    name = f"arena/t{tid}_r{rounds}"
                    content = f"tid={tid},round={rounds}:".encode() + bytes(range(256))

                    # Create and write -- allocates descriptor + blocks
                    h = f.create_stream(name)
                    written = f.write(h, content)
                    assert written == len(content), (
                        f"T{tid} r{rounds}: wrote {written} != {len(content)}"
                    )
                    f.close_stream(h)

                    # Reopen and verify -- descriptor cached correctly
                    h = f.open_stream(name, yak.OpenMode.READ)
                    length = f.stream_length(h)
                    assert length == len(content), (
                        f"T{tid} r{rounds}: length {length} != {len(content)}"
                    )
                    data = f.read(h, length)
                    assert data == content, (
                        f"T{tid} r{rounds}: content mismatch"
                    )
                    f.close_stream(h)

                    # Delete -- frees descriptor slot and blocks for reuse
                    f.delete_stream(name)

                    rounds += 1

            except Exception as e:
                errors.append(f"Thread {tid} (round {rounds}): {e}")

        threads = []
        for i in range(NUM_LIFECYCLE_THREADS):
            t = threading.Thread(target=lifecycle_thread, args=(i,))
            threads.append(t)
            t.start()

        for t in threads:
            t.join(timeout=burn_seconds + 30)

        f.close()
        assert not errors, "Thread errors:\n" + "\n".join(errors)

    def test_concurrent_create_accumulate(self, tmp_path, burn_seconds):
        """Threads create many streams without deleting -- forces descriptor stream growth.

        Each thread creates streams in its own subdirectory so directory
        contention is spread across multiple directory streams while
        descriptor allocation contention is maximised on the shared
        Streams stream.
        """
        yak_path = str(tmp_path / "accumulate.yak")
        f = yak.Yak.create(yak_path, block_index_width=4, block_size_shift=6)

        # Each thread gets its own directory
        for i in range(NUM_LIFECYCLE_THREADS):
            f.mkdir(f"t{i}")

        errors = []
        barrier = threading.Barrier(NUM_LIFECYCLE_THREADS)

        def accumulate_thread(tid):
            rounds = 0
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds

                while time.monotonic() < deadline:
                    name = f"t{tid}/s{rounds}"
                    content = f"{tid}:{rounds}".encode() + b"\xAB" * 80

                    h = f.create_stream(name)
                    f.write(h, content)
                    f.close_stream(h)

                    # Verify immediately
                    h = f.open_stream(name, yak.OpenMode.READ)
                    data = f.read(h, len(content) + 1)
                    assert data == content, (
                        f"T{tid} r{rounds}: verify failed"
                    )
                    f.close_stream(h)

                    rounds += 1

            except Exception as e:
                errors.append(f"Thread {tid} (round {rounds}): {e}")

        threads = []
        for i in range(NUM_LIFECYCLE_THREADS):
            t = threading.Thread(target=accumulate_thread, args=(i,))
            threads.append(t)
            t.start()

        for t in threads:
            t.join(timeout=burn_seconds + 30)

        # Verify all streams survived
        verify_errors = []
        for i in range(NUM_LIFECYCLE_THREADS):
            entries = f.list(f"t{i}")
            for entry in entries:
                h = f.open_stream(f"t{i}/{entry.name}", yak.OpenMode.READ)
                data = f.read(h, 200)
                f.close_stream(h)
                if not data:
                    verify_errors.append(f"t{i}/{entry.name}: empty")

        f.close()
        all_errors = errors + verify_errors
        assert not all_errors, "Thread errors:\n" + "\n".join(all_errors)


class TestThreadSafety:
    @pytest.fixture
    def burn_seconds(self, request):
        return request.config.getoption("--burn-seconds")

    @pytest.fixture
    def yak_path(self, tmp_path):
        """Create and populate a Yak file with known data."""
        path = str(tmp_path / "test.yak")
        f = yak.Yak.create(path)

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

    def test_concurrent_reads(self, yak_path, burn_seconds):
        """One Yak instance shared across 40 threads, each reading all streams in a loop."""
        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)
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
                        h = f.open_stream(f"data/file{i}.bin", yak.OpenMode.READ)
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
                    h = f.open_stream("readme", yak.OpenMode.READ)
                    data = f.read(h, 100)
                    assert data == b"hello from root", f"T{thread_id}: readme mismatch"
                    f.close_stream(h)

                    # Read nested stream
                    h = f.open_stream("docs/images/logo", yak.OpenMode.READ)
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

    def test_concurrent_read_write_mixed(self, tmp_path, burn_seconds):
        """Reader and writer threads operate concurrently on independent streams.

        10 reader threads repeatedly open/read/verify pre-populated streams.
        10 writer threads create, write, verify, and delete new streams.
        """
        NUM_READERS = 10
        NUM_WRITERS = 10
        TOTAL_MIXED_THREADS = NUM_READERS + NUM_WRITERS

        yak_path = str(tmp_path / "mixed.yak")
        f = yak.Yak.create(yak_path)

        # Setup: pre-populate streams for readers
        read_streams = []
        for i in range(NUM_READERS):
            name = f"read_stream_{i}"
            content = f"read-{i}:".encode() + bytes(range(256)) * (i + 1)
            h = f.create_stream(name)
            f.write(h, content)
            f.close_stream(h)
            read_streams.append((name, content))

        # Setup: per-writer directories
        for i in range(NUM_WRITERS):
            f.mkdir(f"writer_{i}")

        errors = []
        barrier = threading.Barrier(TOTAL_MIXED_THREADS)

        def reader_thread(tid):
            rounds = 0
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds

                while time.monotonic() < deadline:
                    for name, expected in read_streams:
                        h = f.open_stream(name, yak.OpenMode.READ)
                        length = f.stream_length(h)
                        assert length == len(expected), (
                            f"Reader T{tid} r{rounds}: {name} length {length} != {len(expected)}"
                        )
                        data = f.read(h, length)
                        assert data == expected, (
                            f"Reader T{tid} r{rounds}: {name} content mismatch"
                        )
                        f.close_stream(h)
                    rounds += 1

            except Exception as e:
                errors.append(f"Reader {tid} (round {rounds}): {e}")

        def writer_thread(tid):
            rounds = 0
            try:
                barrier.wait(timeout=5)
                deadline = time.monotonic() + burn_seconds

                while time.monotonic() < deadline:
                    name = f"writer_{tid}/s{rounds}"
                    content = f"w{tid}r{rounds}:".encode() + b"\xCD" * 200

                    h = f.create_stream(name)
                    f.write(h, content)
                    f.close_stream(h)

                    # Verify
                    h = f.open_stream(name, yak.OpenMode.READ)
                    data = f.read(h, len(content) + 1)
                    assert data == content, (
                        f"Writer T{tid} r{rounds}: verify failed"
                    )
                    f.close_stream(h)

                    f.delete_stream(name)
                    rounds += 1

            except Exception as e:
                errors.append(f"Writer {tid} (round {rounds}): {e}")

        threads = []
        for i in range(NUM_READERS):
            t = threading.Thread(target=reader_thread, args=(i,))
            threads.append(t)
        for i in range(NUM_WRITERS):
            t = threading.Thread(target=writer_thread, args=(i,))
            threads.append(t)

        t0 = time.perf_counter()
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=burn_seconds + 30)
        elapsed_ms = (time.perf_counter() - t0) * 1000

        import sys
        print(f"  mixed read+write: {elapsed_ms:.0f} ms", file=sys.stderr)

        # Post-mortem: verify original read streams are intact
        for name, expected in read_streams:
            h = f.open_stream(name, yak.OpenMode.READ)
            data = f.read(h, len(expected))
            assert data == expected, f"Post-mortem: {name} content corrupted"
            f.close_stream(h)

        f.close()
        assert not errors, "Thread errors:\n" + "\n".join(errors)

    def test_shared_instance_concurrent_writes(self, yak_path, burn_seconds):
        """One Yak instance shared across 40 threads, each writing its own stream in a loop."""
        f = yak.Yak.open(yak_path, yak.OpenMode.WRITE)

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

                    h = f.open_stream(f"data/thread_{thread_id}", yak.OpenMode.WRITE)
                    f.truncate(h, 0)
                    f.seek(h, 0)
                    f.write(h, content)
                    f.close_stream(h)

                    # Read it back from the same shared instance
                    h = f.open_stream(f"data/thread_{thread_id}", yak.OpenMode.READ)
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
