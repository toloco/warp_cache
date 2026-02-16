import asyncio
import warnings

from warp_cache._strategies import Backend, Strategy
from warp_cache._warp_cache_rs import CachedFunction, SharedCachedFunction


class AsyncCachedFunction:
    """Async wrapper around a Rust CachedFunction or SharedCachedFunction.

    Uses the Rust get/set methods for cache lookup/store so that the
    async function is only awaited on cache miss.
    """

    def __init__(self, fn, inner):
        self._fn = fn
        self._inner = inner
        self.__wrapped__ = fn
        self.__name__ = getattr(fn, "__name__", repr(fn))
        self.__qualname__ = getattr(fn, "__qualname__", self.__name__)
        self.__module__ = getattr(fn, "__module__", None)
        self.__doc__ = getattr(fn, "__doc__", None)

    async def __call__(self, *args, **kwargs):
        cached = self._inner.get(*args, **kwargs)
        if cached is not None:
            return cached
        result = await self._fn(*args, **kwargs)
        self._inner.set(result, *args, **kwargs)
        return result

    def cache_info(self):
        return self._inner.cache_info()

    def cache_clear(self):
        return self._inner.cache_clear()

    def __repr__(self):
        return f"<AsyncCachedFunction {self.__qualname__}>"


_BACKEND_STR_MAP = {"memory": Backend.MEMORY, "shared": Backend.SHARED}


def _resolve_backend(backend):
    """Accept Backend enum, int, or string and return a Backend member."""
    if isinstance(backend, Backend):
        return backend
    if isinstance(backend, int):
        return Backend(backend)
    if isinstance(backend, str):
        try:
            return _BACKEND_STR_MAP[backend]
        except KeyError:
            raise ValueError(f"Unknown backend: {backend!r}. Use 'memory' or 'shared'.") from None
    raise TypeError(f"backend must be a Backend, int, or str, got {type(backend).__name__}")


def cache(
    strategy: Strategy = Strategy.LRU,
    max_size: int = 128,
    ttl: float | None = None,
    backend: "str | int | Backend" = Backend.MEMORY,
    max_key_size: int | None = None,
    max_value_size: int | None = None,
):
    """Caching decorator backed by a Rust store.

    Supports both sync and async functions. The async detection happens
    once at decoration time — zero overhead on the sync path.

    Args:
        strategy: Eviction strategy (LRU, MRU, FIFO, LFU).
        max_size: Maximum number of cached entries.
        ttl: Time-to-live in seconds (None = no expiry).
        backend: Backend.MEMORY (default) for in-process cache,
                 Backend.SHARED for cross-process shared memory cache.
                 Also accepts the strings "memory" and "shared".
        max_key_size: Max serialized key size in bytes (shared backend only).
        max_value_size: Max serialized value size in bytes (shared backend only).
    """
    resolved_backend = _resolve_backend(backend)

    def decorator(fn):
        if resolved_backend == Backend.SHARED:
            inner = SharedCachedFunction(
                fn,
                int(strategy),
                max_size,
                ttl=ttl,
                max_key_size=max_key_size if max_key_size is not None else 512,
                max_value_size=max_value_size if max_value_size is not None else 4096,
            )
        else:
            if max_key_size is not None:
                warnings.warn(
                    "max_key_size has no effect with the memory backend",
                    stacklevel=2,
                )
            if max_value_size is not None:
                warnings.warn(
                    "max_value_size has no effect with the memory backend",
                    stacklevel=2,
                )
            inner = CachedFunction(fn, int(strategy), max_size, ttl=ttl)

        if asyncio.iscoroutinefunction(fn):
            return AsyncCachedFunction(fn, inner)

        return inner

    return decorator
