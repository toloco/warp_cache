from __future__ import annotations

import asyncio
import sys
import warnings
from collections.abc import Callable
from typing import Any, TypeVar

if sys.version_info >= (3, 10):
    from typing import ParamSpec, Protocol, runtime_checkable
else:
    from typing_extensions import ParamSpec, Protocol, runtime_checkable

from warp_cache._strategies import Backend
from warp_cache._warp_cache_rs import (
    CachedFunction,
    CacheInfo,
    SharedCachedFunction,
    SharedCacheInfo,
)

F = TypeVar("F", bound=Callable[..., Any])
P = ParamSpec("P")
R = TypeVar("R")


@runtime_checkable
class BaseCacheInfo(Protocol):
    """Common interface for cache info objects from both backends."""

    @property
    def hits(self) -> int: ...
    @property
    def misses(self) -> int: ...
    @property
    def max_size(self) -> int: ...
    @property
    def current_size(self) -> int: ...


class CachedCallable(Protocol[P, R]):
    """Protocol for a cached function — preserves the original call signature."""

    def __call__(self, *args: P.args, **kwargs: P.kwargs) -> R: ...
    def cache_info(self) -> BaseCacheInfo: ...
    def cache_clear(self) -> None: ...


class AsyncCachedFunction:
    """Async wrapper around a Rust CachedFunction or SharedCachedFunction.

    Uses the Rust get/set methods for cache lookup/store so that the
    async function is only awaited on cache miss.

    Implements single-flight coalescing: when multiple coroutines miss the
    cache for the same key concurrently, only one computes the result and
    the rest wait for it.
    """

    def __init__(
        self, fn: Callable[..., Any], inner: CachedFunction | SharedCachedFunction
    ) -> None:
        self._fn = fn
        self._inner = inner
        self._inflight: dict[Any, asyncio.Event] = {}
        self.__wrapped__ = fn
        self.__name__ = getattr(fn, "__name__", repr(fn))
        self.__qualname__ = getattr(fn, "__qualname__", self.__name__)
        self.__module__ = getattr(fn, "__module__", __name__)
        self.__doc__ = getattr(fn, "__doc__", None)

    @staticmethod
    def _make_inflight_key(
        args: tuple[Any, ...], kwargs: dict[str, Any] | None
    ) -> Any:
        if kwargs:
            return (args, tuple(sorted(kwargs.items())))
        return args

    async def __call__(self, *args: Any, **kwargs: Any) -> Any:
        hit, cached = self._inner._probe(*args, **kwargs)
        if hit:
            return cached

        key = self._make_inflight_key(args, kwargs or None)

        while True:
            event = self._inflight.get(key)
            if event is not None:
                await event.wait()
                hit, cached = self._inner._probe(*args, **kwargs)
                if hit:
                    return cached
                # Leader failed — loop back to check for a new leader
                continue

            # We're the first: register our intent
            event = asyncio.Event()
            self._inflight[key] = event
            try:
                result = await self._fn(*args, **kwargs)
                self._inner.set(result, *args, **kwargs)
                return result
            finally:
                event.set()
                self._inflight.pop(key, None)

    def cache_info(self) -> CacheInfo | SharedCacheInfo:
        return self._inner.cache_info()

    def cache_clear(self) -> None:
        return self._inner.cache_clear()

    def __repr__(self) -> str:
        return f"<AsyncCachedFunction {self.__qualname__}>"


_BACKEND_STR_MAP = {"memory": Backend.MEMORY, "shared": Backend.SHARED}


def _resolve_backend(backend: str | int | Backend) -> Backend:
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
    max_size: int = 128,
    ttl: float | None = None,
    backend: str | int | Backend = Backend.MEMORY,
    max_key_size: int | None = None,
    max_value_size: int | None = None,
) -> Callable[[Callable[P, R]], CachedCallable[P, R]]:
    """Caching decorator backed by a Rust store.

    Supports both sync and async functions. The async detection happens
    once at decoration time — zero overhead on the sync path.

    Uses SIEVE eviction — a simple, scan-resistant algorithm that provides
    near-optimal hit rates with O(1) overhead per access.

    Args:
        max_size: Maximum number of cached entries.
        ttl: Time-to-live in seconds (None = no expiry).
        backend: Backend.MEMORY (default) for in-process cache,
                 Backend.SHARED for cross-process shared memory cache.
                 Also accepts the strings "memory" and "shared".
        max_key_size: Max serialized key size in bytes (shared backend only).
        max_value_size: Max serialized value size in bytes (shared backend only).
    """
    resolved_backend = _resolve_backend(backend)

    def decorator(fn: Callable[P, R]) -> CachedCallable[P, R]:
        if resolved_backend == Backend.SHARED:
            inner = SharedCachedFunction(
                fn,
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
            inner = CachedFunction(fn, max_size, ttl=ttl)

        if asyncio.iscoroutinefunction(fn):
            return AsyncCachedFunction(fn, inner)  # type: ignore[return-value]

        return inner

    return decorator
