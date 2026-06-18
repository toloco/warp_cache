"""Regression tests for #43 and #44.

#43: sync decorated functions used to return the raw Rust object with no
metadata — `__name__`/`__doc__`/`__wrapped__` missing and `inspect.signature`
collapsing to `(*args, **kwargs)`, breaking logging/doc tools/FastAPI/click.

#44: an async decorated function was not detected as a coroutine function, so
frameworks that branch on `iscoroutinefunction` (FastAPI/Starlette, anyio,
pytest-asyncio) treated it as sync and dropped the returned coroutine.
"""

import asyncio
import contextlib
import glob
import inspect
import os
import sys
import tempfile

import pytest

from warp_cache import cache


def _cleanup_shm():
    shm_dir = os.path.join(tempfile.gettempdir(), "warp_cache")
    if os.path.isdir(shm_dir):
        for f in glob.glob(os.path.join(shm_dir, "*")):
            with contextlib.suppress(OSError):
                os.unlink(f)


@pytest.mark.parametrize(
    "backend",
    [
        "memory",
        pytest.param(
            "shared",
            marks=pytest.mark.skipif(
                sys.platform == "win32", reason="shared memory is Unix-only"
            ),
        ),
    ],
)
def test_sync_preserves_introspection(backend):
    """#43: name/qualname/module/doc/__wrapped__ and a resolvable signature."""
    _cleanup_shm()
    try:

        @cache(max_size=128, backend=backend)
        def add(a, b=2):
            """add docstring"""
            return a + b

        assert add.__name__ == "add"
        assert add.__qualname__.endswith("add")
        assert add.__module__ == __name__
        assert add.__doc__ == "add docstring"
        assert add.__wrapped__ is not None
        assert str(inspect.signature(add)) == "(a, b=2)"

        # Wrapping must not break caching.
        assert add(1) == 3
        assert add(1) == 3
        assert add.cache_info().hits == 1
    finally:
        _cleanup_shm()


def test_async_preserves_introspection():
    """#43 (async path): name/doc/__wrapped__/signature stay intact."""

    @cache(max_size=128)
    async def fetch(a, b=2):
        """fetch docstring"""
        return a + b

    assert fetch.__name__ == "fetch"
    assert fetch.__doc__ == "fetch docstring"
    assert fetch.__wrapped__ is not None
    assert str(inspect.signature(fetch)) == "(a, b=2)"


def test_async_is_detected_as_coroutine_function():
    """#44: iscoroutinefunction must report True so frameworks await it."""

    @cache(max_size=128)
    async def fetch(x):
        return x

    if hasattr(inspect, "markcoroutinefunction"):  # Python 3.12+
        assert inspect.iscoroutinefunction(fetch)
    else:  # 3.10 / 3.11: asyncio's sentinel check (not deprecated there)
        assert asyncio.iscoroutinefunction(fetch)

    # And it still returns an awaitable yielding the right value.
    assert asyncio.run(fetch(7)) == 7
