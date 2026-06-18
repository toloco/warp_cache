"""Basic functionality tests for the shared memory backend."""

import contextlib
import glob
import os
import stat
import sys
import tempfile

import pytest

from warp_cache import SharedCacheInfo, cache


def _cleanup_shm():
    """Remove any leftover shared memory files."""
    tmpdir = tempfile.gettempdir()
    shm_dir = os.path.join(tmpdir, f"warp_cache-{os.getuid()}")
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


class TestSharedTTLConfigMismatch:
    """Regression for #42: opening an existing shm region with a different TTL
    used to silently reuse the creator's region (and its TTL stored in the
    header), ignoring the second caller's requested TTL. A TTL mismatch must now
    recreate the region, just like a capacity/key/value-size mismatch."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_ttl_mismatch_recreates_region(self):
        import time

        from warp_cache._warp_cache_rs import SharedCachedFunction

        shm_name = "test_ttl_mismatch_42"

        # First constructor fixes the region's TTL to 0.1s.
        SharedCachedFunction(
            lambda x: x, 16, ttl=0.1, max_key_size=512, max_value_size=4096, shm_name=shm_name
        )

        # Second constructor requests ttl=None on the same region. After the fix
        # the TTL mismatch recreates the region with no TTL; before the fix it
        # silently reused the first region and honored ttl=0.1.
        fn_b = SharedCachedFunction(
            lambda x: x, 16, ttl=None, max_key_size=512, max_value_size=4096, shm_name=shm_name
        )

        assert fn_b(1) == 1  # miss -> store
        time.sleep(0.3)  # well past the first constructor's 0.1s TTL
        assert fn_b(1) == 1  # ttl=None -> still a hit; ttl=0.1 (bug) -> expired miss
        assert fn_b.cache_info().hits == 1, (
            "second constructor's ttl=None was ignored — entry expired per the "
            "first constructor's ttl=0.1 (region was reused, not recreated)"
        )


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


class TestSharedMaxSizeZero:
    """Regression for #31: max_size=0 on the shared backend used to build a
    zero-slot slab and read+write out of bounds on the first insert."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    def test_max_size_zero_raises(self):
        with pytest.raises(ValueError, match="max_size must be >= 1"):

            @cache(max_size=0, backend="shared")
            def fn(x):
                return x

    def test_max_size_one_is_usable(self):
        @cache(max_size=1, backend="shared")
        def fn(x):
            return x * 2

        assert fn(3) == 6
        assert fn(3) == 6


class TestSharedMaxSizeTooLarge:
    """Regression for #41: max_size whose 2x hash table overflows u32 used to
    panic across FFI (debug) or silently build a 1-bucket table that drops every
    insert after the first (release). Both must now be a clean ValueError."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    @pytest.mark.parametrize(
        "max_size",
        [
            2**31,  # capacity*2 overflows u32 (the headline trigger)
            2**32,  # also exercises the usize->u32 truncation path (would wrap to 0)
            2**30 + 1,  # just past the largest table that fits in u32
        ],
    )
    def test_oversized_max_size_raises(self, max_size):
        with pytest.raises(ValueError, match="max_size must be <="):

            @cache(max_size=max_size, backend="shared")
            def fn(x):
                return x


class TestSharedFilePermissions:
    """Regression for #39: the mmap cache files hold serialized (and possibly
    pickled) return values, so they must be owner-only (0o600) inside a per-user
    0o700 directory — not world-readable in a shared dir like /dev/shm, where
    another local user could read them or pre-create a crafted file."""

    def setup_method(self):
        _cleanup_shm()

    def teardown_method(self):
        _cleanup_shm()

    @pytest.mark.skipif(sys.platform == "win32", reason="shared memory is Unix-only")
    def test_shm_dir_and_files_are_owner_only(self):
        # Mirror src/shm/region.rs::shm_dir.
        base = "/dev/shm" if sys.platform.startswith("linux") else tempfile.gettempdir()
        shm_dir = os.path.join(base, f"warp_cache-{os.getuid()}")

        @cache(max_size=16, backend="shared")
        def fn(x):
            return x * 2

        assert fn(1) == 2  # creates the region + files

        # The per-user directory must be private (owner rwx only).
        dmode = stat.S_IMODE(os.stat(shm_dir).st_mode)
        assert dmode == 0o700, f"{shm_dir} is {oct(dmode)}, expected 0o700"

        # Every cache file must be owner read/write only — no group/other bits.
        files = glob.glob(os.path.join(shm_dir, "*"))
        assert files, "no shm files were created"
        for f in files:
            fmode = stat.S_IMODE(os.stat(f).st_mode)
            assert fmode & 0o077 == 0, f"{f} is group/other-accessible: {oct(fmode)}"
