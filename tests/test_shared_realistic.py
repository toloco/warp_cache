"""Realistic shared-cache tests with expensive computation and controlled hit/miss ratios."""

import contextlib
import glob
import hashlib
import multiprocessing
import os
import sys
import tempfile

import pytest

from warp_cache import Strategy, cache
from warp_cache._warp_cache_rs import SharedCachedFunction


def _cleanup_shm():
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, "warp_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


def _expensive_compute(n: int) -> str:
    """~1-3ms CPU-bound computation. Deterministic."""
    data = str(n).encode()
    for _ in range(200):
        data = hashlib.sha256(data).digest()
    return data.hex()


WORKING_SET = 20
MAX_SIZE = 64


class TestSingleProcessRealistic:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_hit_miss_ratio(self):
        @cache(strategy=Strategy.LRU, max_size=MAX_SIZE, backend="shared")
        def fn(n):
            return _expensive_compute(n)

        # Warmup: populate working set (all misses)
        for k in range(WORKING_SET):
            fn(k)

        # Snapshot counters after warmup
        info_before = fn.cache_info()
        hits_before = info_before.hits
        misses_before = info_before.misses

        # Controlled phase: 100 batches of 6 ops each
        # 5 working-set hits + 1 novel miss per batch
        ws_idx = 0
        novel = 1000
        for _ in range(100):
            for _ in range(5):
                fn(ws_idx % WORKING_SET)
                ws_idx += 1
            fn(novel)
            novel += 1

        info_after = fn.cache_info()
        delta_hits = info_after.hits - hits_before
        delta_misses = info_after.misses - misses_before

        assert delta_hits == 500
        assert delta_misses == 100
        assert delta_hits / delta_misses == 5.0

        # Verify correctness: cached values match direct computation
        for k in range(WORKING_SET):
            assert fn(k) == _expensive_compute(k)


# --- Multi-process setup (module-level for fork compatibility) ---

_shared_realistic_fn = SharedCachedFunction(
    _expensive_compute,
    0,
    MAX_SIZE,
    ttl=None,
    max_key_size=512,
    max_value_size=4096,
    shm_name="test_realistic_mp",
)


def _reader_worker(args):
    """Access working-set keys only (all hits after warmup)."""
    start_idx, count = args
    for i in range(count):
        _shared_realistic_fn(start_idx + (i % WORKING_SET))
    info = _shared_realistic_fn.cache_info()
    return info.hits, info.misses


def _writer_worker(args):
    """Access novel keys only (all misses)."""
    start_key, count = args
    for i in range(count):
        _shared_realistic_fn(start_key + i)
    info = _shared_realistic_fn.cache_info()
    return info.hits, info.misses


class TestMultiProcessRealistic:
    def setup_method(self):
        _cleanup_shm()
        _shared_realistic_fn.cache_clear()

    def teardown_method(self):
        _cleanup_shm()

    @pytest.mark.skipif(sys.platform == "win32", reason="No fork on Windows")
    def test_multiprocess_hit_miss_ratio(self):
        ctx = multiprocessing.get_context("fork")

        # Parent warms cache with working set
        for k in range(WORKING_SET):
            _shared_realistic_fn(k)

        info_before = _shared_realistic_fn.cache_info()
        hits_before = info_before.hits
        misses_before = info_before.misses

        # 5 reader processes (100 ops each = 500 hits)
        # 1 writer process (100 novel keys = 100 misses)
        with ctx.Pool(6) as pool:
            pool.map(_reader_worker, [(0, 100) for _ in range(5)])
            pool.map(_writer_worker, [(2000, 100)])

        info_after = _shared_realistic_fn.cache_info()
        delta_hits = info_after.hits - hits_before
        delta_misses = info_after.misses - misses_before

        # Multi-process: counters are approximate due to timing jitter
        ratio = delta_hits / delta_misses if delta_misses > 0 else float("inf")
        assert 4.0 <= ratio <= 6.0, (
            f"Expected hit/miss ratio ~5.0, got {ratio:.2f} "
            f"(hits={delta_hits}, misses={delta_misses})"
        )

        # Verify correctness of cached working-set values
        for k in range(WORKING_SET):
            assert _shared_realistic_fn(k) == _expensive_compute(k)
