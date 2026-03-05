from warp_cache import cache


def test_sieve_unvisited_evicted_first():
    """SIEVE: unvisited entries are evicted before visited ones."""
    call_count = 0

    @cache(max_size=3)
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


def test_sieve_second_chance():
    """SIEVE: visited entries get their visited bit cleared (second chance)
    and are only evicted on a subsequent pass if still unvisited."""
    call_count = 0

    @cache(max_size=2)
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


def test_sieve_eviction_respects_capacity():
    """Cache never exceeds max_size."""

    @cache(max_size=5)
    def fn(x):
        return x

    for i in range(100):
        fn(i)
        info = fn.cache_info()
        assert info.current_size <= 5


def test_sieve_hit_sets_visited():
    """A cache hit marks the entry as visited, protecting it from eviction."""
    call_count = 0

    @cache(max_size=3)
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
