"""Basic functionality tests for the shared memory backend."""

import contextlib
import glob
import os
import tempfile

from warp_cache import SharedCacheInfo, cache


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

        @cache(max_size=128, backend="shared")
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

        @cache(max_size=128, backend="shared")
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
        @cache(max_size=128, backend="shared")
        def fn(x):
            return None

        assert fn(1) is None
        assert fn(1) is None
        assert fn.cache_info().hits == 1

    def test_kwargs(self):
        @cache(max_size=128, backend="shared")
        def fn(a, b):
            return a + b

        assert fn(a=1, b=2) == 3
        assert fn(b=2, a=1) == 3  # same key regardless of kwarg order
        assert fn.cache_info().hits == 1

    def test_eviction_at_capacity(self):
        @cache(max_size=4, backend="shared")
        def fn(x):
            return x

        for i in range(4):
            fn(i)
        assert fn.cache_info().current_size == 4

        # This should evict an unvisited entry (SIEVE)
        fn(99)
        assert fn.cache_info().current_size == 4

    def test_oversize_skip(self):
        @cache(
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

        @cache(max_size=128, backend="shared")
        def fn(x):
            return x

        for val in [42, 3.14, "hello", b"world", True, False, None]:
            assert fn(val) == val  # miss
            assert fn(val) == val  # hit
        assert fn.cache_info().hits == 7
        assert fn.cache_info().misses == 7

    def test_fast_path_tuple_keys(self):
        """Tuples of primitives should use fast-path serialization."""

        @cache(max_size=128, backend="shared")
        def fn(a, b):
            return a + b

        assert fn(1, 2) == 3
        assert fn(1, 2) == 3  # hit
        assert fn.cache_info().hits == 1

    def test_shared_cache_info_repr(self):
        @cache(max_size=64, backend="shared")
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        r = repr(info)
        assert "SharedCacheInfo" in r
        assert "hits=0" in r
        assert "misses=1" in r


class TestSharedSieve:
    """Test SIEVE eviction behavior in the shared memory backend."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_unvisited_evicted_first(self):
        """SIEVE: unvisited entries are evicted before visited ones."""
        call_count = 0

        @cache(max_size=3, backend="shared")
        def fn(x):
            nonlocal call_count
            call_count += 1
            return x

        fn(1)  # miss, inserted (unvisited)
        fn(2)  # miss, inserted (unvisited)
        fn(3)  # miss, inserted (unvisited)
        assert call_count == 3

        # Access 2 and 3 — marks them as visited
        fn(2)  # hit → visited=true
        fn(3)  # hit → visited=true
        assert call_count == 3

        # Insert 4 — must evict. 1 is unvisited, should be evicted
        fn(4)  # miss, evicts 1
        assert call_count == 4

        # Verify: 1 was evicted (miss), 2 and 3 survive (hit)
        call_count = 0
        fn(2)  # hit
        assert call_count == 0
        fn(3)  # hit
        assert call_count == 0
        fn(1)  # miss — was evicted
        assert call_count == 1

    def test_second_chance(self):
        """SIEVE: visited entries get their visited bit cleared (second chance)
        and are only evicted on a subsequent pass if still unvisited."""
        call_count = 0

        @cache(max_size=2, backend="shared")
        def fn(x):
            nonlocal call_count
            call_count += 1
            return x

        fn(1)  # miss
        fn(2)  # miss
        assert call_count == 2

        # Visit both entries
        fn(1)  # hit → visited=true
        fn(2)  # hit → visited=true

        # Insert 3 — all entries visited, so the hand scans and clears visited bits,
        # then evicts the first entry it finds unvisited on the second pass
        fn(3)  # miss, evicts one of {1, 2}
        assert call_count == 3

        info = fn.cache_info()
        assert info.current_size == 2

    def test_eviction_respects_capacity(self):
        """Cache never exceeds max_size."""

        @cache(max_size=5, backend="shared")
        def fn(x):
            return x

        for i in range(100):
            fn(i)
            info = fn.cache_info()
            assert info.current_size <= 5

    def test_hit_sets_visited(self):
        """A cache hit marks the entry as visited, protecting it from eviction."""
        call_count = 0

        @cache(max_size=3, backend="shared")
        def fn(x):
            nonlocal call_count
            call_count += 1
            return x

        fn(1)  # miss
        fn(2)  # miss
        fn(3)  # miss
        # All entries are unvisited

        # Visit entry 1
        fn(1)  # hit → visited=true

        # Insert 4 — evicts an unvisited entry (2 or 3), not 1
        fn(4)  # miss
        assert call_count == 4

        # Entry 1 should still be cached
        call_count = 0
        fn(1)  # hit
        assert call_count == 0


class TestSharedTTL:
    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_ttl_expiry(self):
        import time

        @cache(max_size=128, ttl=0.1, backend="shared")
        def fn(x):
            return x * 2

        assert fn(1) == 2
        assert fn.cache_info().misses == 1

        time.sleep(0.15)

        # Should be expired
        assert fn(1) == 2
        assert fn.cache_info().misses == 2

    def test_ttl_not_expired(self):
        @cache(max_size=128, ttl=10.0, backend="shared")
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

        @cache(max_size=128)
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        assert isinstance(info, CacheInfo)

    def test_shared_returns_shared_info(self):
        @cache(max_size=128, backend="shared")
        def fn(x):
            return x

        fn(1)
        info = fn.cache_info()
        assert isinstance(info, SharedCacheInfo)
        assert hasattr(info, "oversize_skips")
