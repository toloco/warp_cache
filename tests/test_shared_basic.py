"""Basic functionality tests for the shared memory backend."""

import contextlib
import glob
import os
import tempfile

from warp_cache import SharedCacheInfo, Strategy, cache


def _cleanup_shm():
    """Remove any leftover shared memory files."""
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, "warp_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


class TestSharedBasicHitMiss:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_basic_hit_miss(self):
        call_count = 0

        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(x):
            nonlocal call_count
            call_count += 1
            return x * 2

        assert fn(1) == 2
        assert call_count == 1
        assert fn(1) == 2
        assert call_count == 1  # cached

        assert fn(2) == 4
        assert call_count == 2

        info = fn.cache_info()
        assert isinstance(info, SharedCacheInfo)
        assert info.hits == 1
        assert info.misses == 2
        assert info.current_size == 2

    def test_cache_clear(self):
        call_count = 0

        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(x):
            nonlocal call_count
            call_count += 1
            return x + 1

        fn(1)
        fn(2)
        assert fn.cache_info().current_size == 2

        fn.cache_clear()
        info = fn.cache_info()
        assert info.current_size == 0
        assert info.hits == 0
        assert info.misses == 0

        fn(1)
        assert call_count == 3  # re-computed

    def test_none_return_value(self):
        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(x):
            return None

        assert fn(1) is None
        assert fn(1) is None
        assert fn.cache_info().hits == 1

    def test_kwargs(self):
        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(a, b):
            return a + b

        assert fn(a=1, b=2) == 3
        assert fn(b=2, a=1) == 3  # same key regardless of kwarg order
        assert fn.cache_info().hits == 1

    def test_eviction_at_capacity(self):
        @cache(strategy=Strategy.LRU, max_size=4, backend="shared")
        def fn(x):
            return x

        for i in range(4):
            fn(i)
        assert fn.cache_info().current_size == 4

        # This should evict key 0 (LRU)
        fn(99)
        assert fn.cache_info().current_size == 4

        # key 0 was evicted, so calling it again is a miss
        fn(0)
        info = fn.cache_info()
        assert info.misses == 6  # 4 initial + 99 + 0 re-miss

    def test_oversize_skip(self):
        @cache(
            strategy=Strategy.LRU,
            max_size=128,
            backend="shared",
            max_key_size=16,
            max_value_size=16,
        )
        def fn(x):
            return x

        # Small value — cached
        assert fn(1) == 1
        assert fn(1) == 1
        assert fn.cache_info().hits == 1

        # Large key — bypasses cache
        big_key = "x" * 1000
        assert fn(big_key) == big_key
        assert fn(big_key) == big_key  # computed again, not cached
        assert fn.cache_info().oversize_skips > 0

    def test_fast_path_types(self):
        """All fast-path primitive types should cache correctly."""

        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(x):
            return x

        for val in [42, 3.14, "hello", b"world", True, False, None]:
            assert fn(val) == val  # miss
            assert fn(val) == val  # hit
        assert fn.cache_info().hits == 7
        assert fn.cache_info().misses == 7

    def test_fast_path_tuple_keys(self):
        """Tuples of primitives should use fast-path serialization."""

        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(a, b):
            return a + b

        assert fn(1, 2) == 3
        assert fn(1, 2) == 3  # hit
        assert fn.cache_info().hits == 1

    def test_shared_cache_info_repr(self):
        @cache(strategy=Strategy.LRU, max_size=64, backend="shared")
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        r = repr(info)
        assert "SharedCacheInfo" in r
        assert "hits=0" in r
        assert "misses=1" in r


class TestSharedStrategies:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_lru_eviction(self):
        @cache(strategy=Strategy.LRU, max_size=3, backend="shared")
        def fn(x):
            return x

        fn(1)
        fn(2)
        fn(3)
        fn(1)  # touch 1, making 2 the LRU
        fn(4)  # evict 2
        assert fn.cache_info().current_size == 3

        # 2 was evicted
        fn(2)
        assert fn.cache_info().misses == 5  # 1,2,3,4 + re-miss on 2

    def test_fifo_eviction(self):
        @cache(strategy=Strategy.FIFO, max_size=3, backend="shared")
        def fn(x):
            return x

        fn(1)
        fn(2)
        fn(3)
        fn(1)  # touch 1 — FIFO doesn't reorder
        fn(4)  # evict 1 (first inserted)
        assert fn.cache_info().current_size == 3

        # 1 was evicted
        fn(1)
        assert fn.cache_info().misses == 5

    def test_mru_eviction(self):
        @cache(strategy=Strategy.MRU, max_size=3, backend="shared")
        def fn(x):
            return x

        fn(1)
        fn(2)
        fn(3)
        fn(2)  # touch 2, making it most recently used
        fn(4)  # evict 2 (MRU)
        assert fn.cache_info().current_size == 3

        # 2 was evicted
        fn(2)
        assert fn.cache_info().misses == 5

    def test_lfu_eviction(self):
        @cache(strategy=Strategy.LFU, max_size=3, backend="shared")
        def fn(x):
            return x

        fn(1)
        fn(2)
        fn(3)
        fn(1)
        fn(1)  # freq(1) = 2
        fn(2)  # freq(2) = 1
        # freq(3) = 0 — least frequent
        fn(4)  # evict 3 (lowest frequency)
        assert fn.cache_info().current_size == 3

        # 3 was evicted
        fn(3)
        assert fn.cache_info().misses == 5


class TestSharedTTL:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_ttl_expiry(self):
        import time

        @cache(strategy=Strategy.LRU, max_size=128, ttl=0.1, backend="shared")
        def fn(x):
            return x * 2

        assert fn(1) == 2
        assert fn.cache_info().misses == 1

        time.sleep(0.15)

        # Should be expired
        assert fn(1) == 2
        assert fn.cache_info().misses == 2

    def test_ttl_not_expired(self):
        @cache(strategy=Strategy.LRU, max_size=128, ttl=10.0, backend="shared")
        def fn(x):
            return x * 2

        assert fn(1) == 2
        assert fn(1) == 2
        assert fn.cache_info().hits == 1


class TestSharedMemoryBackend:
    """Test backend='memory' vs backend='shared' routing."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_default_is_memory(self):
        from warp_cache._warp_cache_rs import CacheInfo

        @cache(strategy=Strategy.LRU, max_size=128)
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        assert isinstance(info, CacheInfo)

    def test_shared_returns_shared_info(self):
        @cache(strategy=Strategy.LRU, max_size=128, backend="shared")
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        assert isinstance(info, SharedCacheInfo)
        assert hasattr(info, "oversize_skips")
