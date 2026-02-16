"""Stress tests that push the cache harder than the basic suite."""

import random
import threading
import time
from concurrent.futures import ThreadPoolExecutor

from warp_cache import Strategy, cache

# ---------------------------------------------------------------------------
# 1. High-volume insert/get — 100k ops per strategy, verify correctness
# ---------------------------------------------------------------------------


def test_high_volume():
    for strategy in Strategy:

        @cache(strategy=strategy, max_size=1024)
        def fn(x):
            return x * 3 + 1

        for i in range(100_000):
            key = i % 2000  # 2000 unique keys, many repeats
            assert fn(key) == key * 3 + 1

        info = fn.cache_info()
        assert info.hits + info.misses == 100_000
        assert info.current_size <= 1024


# ---------------------------------------------------------------------------
# 2. Eviction churn — tiny cache with many unique keys
# ---------------------------------------------------------------------------


def test_eviction_churn():
    for strategy in Strategy:

        @cache(strategy=strategy, max_size=10)
        def fn(x):
            return x

        for i in range(10_000):
            assert fn(i) == i
            info = fn.cache_info()
            assert info.current_size <= 10


# ---------------------------------------------------------------------------
# 3. Heavy contention — 16 threads, 10k ops each, shared cache
# ---------------------------------------------------------------------------


def test_heavy_contention():
    call_count = 0
    lock = threading.Lock()

    @cache(strategy=Strategy.LRU, max_size=64)
    def fn(x):
        nonlocal call_count
        with lock:
            call_count += 1
        return x * x

    n_threads = 16
    ops_per_thread = 10_000

    def worker():
        rng = random.Random(threading.get_ident())
        for _ in range(ops_per_thread):
            key = rng.randint(0, 127)
            assert fn(key) == key * key

    with ThreadPoolExecutor(max_workers=n_threads) as pool:
        futures = [pool.submit(worker) for _ in range(n_threads)]
        for f in futures:
            f.result()  # propagate exceptions

    info = fn.cache_info()
    total_lookups = n_threads * ops_per_thread
    assert info.hits + info.misses == total_lookups
    assert info.current_size <= 64


# ---------------------------------------------------------------------------
# 4. TTL under load — short TTL with rapid inserts/reads
# ---------------------------------------------------------------------------


def test_ttl_under_load():
    call_count = 0

    @cache(strategy=Strategy.LRU, max_size=256, ttl=0.05)
    def fn(x):
        nonlocal call_count
        call_count += 1
        return (x, time.monotonic())

    # Phase 1: fill the cache
    for i in range(200):
        val = fn(i)
        assert val[0] == i

    first_pass_calls = call_count

    # Phase 2: wait for TTL to expire
    time.sleep(0.1)

    # Phase 3: all entries should have expired — function called again
    for i in range(200):
        val = fn(i)
        assert val[0] == i

    # Every key should have been recomputed
    assert call_count >= first_pass_calls + 200


# ---------------------------------------------------------------------------
# 5. Mixed workload — random hits, misses, clears, info from many threads
# ---------------------------------------------------------------------------


def test_mixed_workload():
    @cache(strategy=Strategy.LFU, max_size=128)
    def fn(x):
        return x + 1

    errors = []

    def worker():
        rng = random.Random(threading.get_ident())
        try:
            for _ in range(5_000):
                action = rng.random()
                if action < 0.6:
                    # Cache lookup (most common)
                    key = rng.randint(0, 255)
                    assert fn(key) == key + 1
                elif action < 0.85:
                    # Cache info
                    info = fn.cache_info()
                    assert info.max_size == 128
                    assert info.current_size <= 128
                else:
                    # Cache clear
                    fn.cache_clear()
        except Exception as exc:
            errors.append(exc)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert not errors, f"Worker threads raised: {errors}"
