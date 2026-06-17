"""Cross-process tests for the shared memory backend.

Uses fork-based multiprocessing so child processes inherit the
decorated functions and share the same mmap files.
"""

import contextlib
import glob
import multiprocessing
import os
import queue
import subprocess
import sys
import tempfile
import textwrap

import pytest

from warp_cache._warp_cache_rs import SharedCachedFunction


def _cleanup_shm():
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, "warp_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


def _shm_dir():
    # Must match src/shm/region.rs::shm_dir: /dev/shm on Linux, $TMPDIR/warp_cache otherwise.
    if sys.platform.startswith("linux"):
        return "/dev/shm"
    return os.path.join(tempfile.gettempdir(), "warp_cache")


def _unlink_shm(name):
    """Remove all files for a shm name so the next open is a cold create."""
    for suffix in (".data", ".lock", ".init"):
        with contextlib.suppress(OSError):
            os.unlink(os.path.join(_shm_dir(), f"{name}{suffix}"))


# Use a fixed shm_name so all processes (even with spawn) share the same cache
_shared_fn = SharedCachedFunction(
    lambda x: x * x,
    16,
    ttl=None,
    max_key_size=512,
    max_value_size=4096,
    shm_name="test_multiproc_shared",
)


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


def _cold_start_worker(shm_name, barrier, q):
    """Construct a fresh shared cache and exercise it. Run concurrently against
    a cold (nonexistent) shm_name to race create_or_open (#33)."""
    try:
        barrier.wait(timeout=15)
        fn = SharedCachedFunction(
            lambda x: x * x,
            32,
            ttl=None,
            max_key_size=512,
            max_value_size=4096,
            shm_name=shm_name,
        )
        ok = all(fn(i) == i * i for i in range(20))
        q.put(bool(ok))
    except BaseException as e:  # noqa: BLE001 - report any failure back to the parent
        q.put(f"ERR:{e!r}")


class TestMultiprocess:
    def setup_method(self):
        _cleanup_shm()
        _shared_fn.cache_clear()

    def teardown_method(self):
        _cleanup_shm()

    @pytest.mark.skipif(sys.platform == "win32", reason="No fork on Windows")
    def test_concurrent_cold_start_no_corruption(self):
        """Many processes cold-starting the SAME fresh cache at once must not
        race in create_or_open (#33). Before the fix a second creator's
        truncate+zero clobbered a region the first had already mapped, causing
        SIGBUS (worker exits with a signal) or torn reads (wrong results)."""
        ctx = multiprocessing.get_context("fork")
        n_procs = 8
        for round_i in range(4):
            shm_name = f"test_cold_start_race_{round_i}"
            _unlink_shm(shm_name)  # ensure a cold create
            barrier = ctx.Barrier(n_procs)
            q = ctx.Queue()
            procs = [
                ctx.Process(target=_cold_start_worker, args=(shm_name, barrier, q))
                for _ in range(n_procs)
            ]
            for p in procs:
                p.start()
            for p in procs:
                p.join(timeout=30)

            exitcodes = [p.exitcode for p in procs]
            results = []
            with contextlib.suppress(queue.Empty):
                while True:
                    results.append(q.get_nowait())

            assert all(c == 0 for c in exitcodes), (
                f"round {round_i}: workers crashed (exitcodes={exitcodes}, results={results})"
            )
            assert len(results) == n_procs and all(r is True for r in results), (
                f"round {round_i}: bad results {results}"
            )
            _unlink_shm(shm_name)

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

    @pytest.mark.skipif(sys.platform == "win32", reason="No shared memory on Windows")
    def test_cross_process_str_key_different_hashseed(self):
        """String keys must be found across processes with different PYTHONHASHSEED.

        Python randomizes hash() for str/bytes per process.  The shared
        backend must hash deterministically (from serialized bytes) so that
        a value written by one process is found by another.
        """
        shm_name = "test_str_key_hashseed"

        # Parent writes a string-keyed entry
        parent_fn = SharedCachedFunction(
            lambda x: f"hello-{x}",
            16,
            ttl=None,
            max_key_size=512,
            max_value_size=4096,
            shm_name=shm_name,
        )
        parent_fn.cache_clear()
        result = parent_fn("world")
        assert result == "hello-world"

        # Child process with a *different* PYTHONHASHSEED reads the same key
        child_script = textwrap.dedent(f"""\
            import sys
            from warp_cache._warp_cache_rs import SharedCachedFunction

            fn = SharedCachedFunction(
                lambda x: f"hello-{{x}}",
                16, ttl=None,
                max_key_size=512, max_value_size=4096,
                shm_name="{shm_name}",
            )
            val = fn.get("world")
            if val is None:
                print("MISS", flush=True)
                sys.exit(1)
            print(f"HIT:{{val}}", flush=True)
        """)

        env = os.environ.copy()
        env["PYTHONHASHSEED"] = "12345"
        proc = subprocess.run(
            [sys.executable, "-c", child_script],
            capture_output=True,
            text=True,
            env=env,
            timeout=10,
        )
        assert proc.returncode == 0, f"Child failed: {proc.stderr}"
        assert proc.stdout.strip() == "HIT:hello-world"
