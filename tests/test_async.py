import asyncio
import sys

import pytest

from warp_cache import Strategy, cache
from warp_cache._decorator import AsyncCachedFunction

# ── Basic hit/miss ────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_basic_hit_miss():
    call_count = 0

    @cache(strategy=Strategy.LRU, max_size=128)
    async def add(a, b):
        nonlocal call_count
        call_count += 1
        return a + b

    assert isinstance(add, AsyncCachedFunction)
    assert await add(1, 2) == 3
    assert call_count == 1
    assert await add(1, 2) == 3  # hit
    assert call_count == 1
    assert await add(2, 3) == 5  # miss
    assert call_count == 2

    info = add.cache_info()
    assert info.hits == 1
    assert info.misses == 2
    assert info.current_size == 2


# ── cache_clear ───────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_cache_clear():
    @cache(max_size=128)
    async def square(x):
        return x * x

    assert await square(3) == 9
    assert await square(3) == 9
    info = square.cache_info()
    assert info.hits == 1

    square.cache_clear()
    info = square.cache_info()
    assert info.hits == 0
    assert info.misses == 0
    assert info.current_size == 0


# ── TTL ───────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_ttl():
    call_count = 0

    @cache(max_size=128, ttl=0.1)
    async def identity(x):
        nonlocal call_count
        call_count += 1
        return x

    assert await identity(1) == 1
    assert call_count == 1
    assert await identity(1) == 1  # hit
    assert call_count == 1

    await asyncio.sleep(0.15)

    assert await identity(1) == 1  # expired, re-compute
    assert call_count == 2


# ── kwargs ────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_kwargs():
    @cache(max_size=128)
    async def greet(name, greeting="hello"):
        return f"{greeting} {name}"

    assert await greet("world") == "hello world"
    assert await greet("world", greeting="hi") == "hi world"
    assert await greet("world") == "hello world"  # hit

    info = greet.cache_info()
    assert info.hits == 1
    assert info.misses == 2


# ── Concurrent coroutines ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_concurrent():
    call_count = 0

    @cache(max_size=128)
    async def slow_fn(x):
        nonlocal call_count
        call_count += 1
        await asyncio.sleep(0.01)
        return x * 2

    results = await asyncio.gather(
        slow_fn(1),
        slow_fn(2),
        slow_fn(3),
        slow_fn(1),
        slow_fn(2),
        slow_fn(3),
    )
    assert results == [2, 4, 6, 2, 4, 6]
    # At least 3 unique calls, possibly more due to concurrent misses
    assert call_count >= 3


# ── Strategies ────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_strategies():
    for strat in [Strategy.LRU, Strategy.MRU, Strategy.FIFO, Strategy.LFU]:

        @cache(strategy=strat, max_size=2)
        async def fn(x):
            return x

        assert await fn(1) == 1
        assert await fn(2) == 2
        assert await fn(3) == 3  # triggers eviction
        info = fn.cache_info()
        assert info.current_size == 2


# ── Shared backend ───────────────────────────────────────────────────────


@pytest.mark.skipif(sys.platform == "win32", reason="shared memory is Unix-only")
@pytest.mark.asyncio
async def test_async_shared_backend():
    call_count = 0

    @cache(max_size=128, backend="shared")
    async def add(a, b):
        nonlocal call_count
        call_count += 1
        return a + b

    assert isinstance(add, AsyncCachedFunction)
    add.cache_clear()  # clear stale shared memory from prior runs

    assert await add(1, 2) == 3
    assert call_count == 1
    assert await add(1, 2) == 3  # hit
    assert call_count == 1


# ── Repr / attributes ────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_async_repr_and_attrs():
    @cache(max_size=128)
    async def my_func(x):
        """My docstring."""
        return x

    assert "my_func" in repr(my_func)
    assert my_func.__name__ == "my_func"
    assert my_func.__doc__ == "My docstring."
    assert my_func.__wrapped__ is not None


# ── Sync functions still work ─────────────────────────────────────────────


def test_sync_still_works():
    """Ensure adding async support didn't break sync functions."""

    @cache(max_size=128)
    def add(a, b):
        return a + b

    assert not isinstance(add, AsyncCachedFunction)
    assert add(1, 2) == 3
    assert add(1, 2) == 3
    info = add.cache_info()
    assert info.hits == 1
