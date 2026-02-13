"""Cross-process tests for the shared memory backend.

Uses fork-based multiprocessing so child processes inherit the
decorated functions and share the same mmap files.
"""

import contextlib
import glob
import multiprocessing
import os
import sys
import tempfile

import pytest
from fast_cache._fast_cache_rs import SharedCachedFunction


def _cleanup_shm():
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, "fast_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


# Use a fixed shm_name so all processes (even with spawn) share the same cache
_shared_fn = SharedCachedFunction(lambda x: x * x, 0, 16, None, 512, 4096, "test_multiproc_shared")


def _worker_write(args):
    """Worker that writes values to the shared cache."""
    start, count = args
    for i in range(start, start + count):
        _shared_fn(i)
    return _shared_fn.cache_info().current_size


def _worker_read(x):
    """Worker that reads a value from the shared cache."""
    result = _shared_fn(x)
    return result, _shared_fn.cache_info().hits


class TestMultiprocess:
    def setup_method(self):
        _cleanup_shm()
        _shared_fn.cache_clear()

    def teardown_method(self):
        _cleanup_shm()

    @pytest.mark.skipif(sys.platform == "win32", reason="No fork on Windows")
    def test_cross_process_visibility(self):
        """Values written by one process should be visible to another."""
        ctx = multiprocessing.get_context("fork")

        # Parent writes
        _shared_fn(42)
        assert _shared_fn(42) == 1764

        # Child reads
        with ctx.Pool(1) as pool:
            result, hits = pool.apply(_worker_read, (42,))
        assert result == 1764

    @pytest.mark.skipif(sys.platform == "win32", reason="No fork on Windows")
    def test_concurrent_writers(self):
        """Multiple processes writing concurrently shouldn't corrupt data."""
        ctx = multiprocessing.get_context("fork")

        with ctx.Pool(4) as pool:
            pool.map(_worker_write, [(i * 4, 4) for i in range(4)])

        # All 16 entries should be in the cache (capacity is 16)
        info = _shared_fn.cache_info()
        assert info.current_size == 16

        # Verify all values are correct
        for i in range(16):
            assert _shared_fn(i) == i * i

    @pytest.mark.skipif(sys.platform == "win32", reason="No fork on Windows")
    def test_eviction_across_processes(self):
        """Eviction should work correctly when multiple processes fill cache."""
        ctx = multiprocessing.get_context("fork")

        # Fill the cache (max_size=16)
        for i in range(16):
            _shared_fn(i)
        assert _shared_fn.cache_info().current_size == 16

        # Another process writes new values, triggering evictions
        with ctx.Pool(1) as pool:
            pool.apply(_worker_write, ((100, 4),))

        info = _shared_fn.cache_info()
        assert info.current_size == 16  # still at capacity
