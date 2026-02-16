from warp_cache import Strategy, cache


def test_basic_hit_miss():
    call_count = 0

    @cache(strategy=Strategy.LRU, max_size=128)
    def add(a, b):
        nonlocal call_count
        call_count += 1
        return a + b

    assert add(1, 2) == 3
    assert call_count == 1
    assert add(1, 2) == 3  # hit
    assert call_count == 1
    assert add(2, 3) == 5  # miss
    assert call_count == 2

    info = add.cache_info()
    assert info.hits == 1
    assert info.misses == 2
    assert info.current_size == 2


def test_cache_clear():
    @cache(max_size=128)
    def square(x):
        return x * x

    assert square(3) == 9
    assert square(3) == 9
    info = square.cache_info()
    assert info.hits == 1

    square.cache_clear()
    info = square.cache_info()
    assert info.hits == 0
    assert info.misses == 0
    assert info.current_size == 0

    assert square(3) == 9
    info = square.cache_info()
    assert info.misses == 1


def test_none_return_value():
    """Verify that functions returning None are cached correctly."""
    call_count = 0

    @cache(max_size=128)
    def returns_none(x):
        nonlocal call_count
        call_count += 1
        return None

    result = returns_none(1)
    assert result is None
    assert call_count == 1

    result = returns_none(1)
    assert result is None
    assert call_count == 1  # should not call again


def test_kwargs():
    call_count = 0

    @cache(max_size=128)
    def greet(name, greeting="hello"):
        nonlocal call_count
        call_count += 1
        return f"{greeting} {name}"

    assert greet("alice", greeting="hi") == "hi alice"
    assert greet("alice", greeting="hi") == "hi alice"
    assert call_count == 1

    assert greet("alice", greeting="hey") == "hey alice"
    assert call_count == 2


def test_eviction_at_capacity():
    @cache(strategy=Strategy.LRU, max_size=3)
    def identity(x):
        return x

    identity(1)
    identity(2)
    identity(3)
    info = identity.cache_info()
    assert info.current_size == 3

    # Adding a 4th should evict the oldest (1)
    identity(4)
    info = identity.cache_info()
    assert info.current_size == 3

    # 1 should be a miss now
    identity(1)
    assert identity.cache_info().misses == 5  # 1,2,3,4 were misses, then 1 again
