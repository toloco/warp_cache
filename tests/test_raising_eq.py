"""Regression tests for issue #36.

When a cache key's ``__eq__`` raises during a lookup, ``PyObject_RichCompareBool``
returns -1 with a Python exception set. The old code compared the result ``== 1``,
mapping -1 to "not equal" and silently dropping the exception. The user's real
error was lost and PyO3 later surfaced a confusing
``SystemError: ... returned a result with an exception set`` (and, in collision
cases, a spurious recompute). The fix fetches the pending exception after the
lookup and propagates it.
"""

import pytest

from warp_cache import cache


class RaisingEq:
    """Constant hash so all instances collide into one slot — this forces
    hashbrown to invoke ``__eq__`` during probing — and ``__eq__`` always raises."""

    def __hash__(self):
        return 0

    def __eq__(self, other):
        raise RuntimeError("boom from __eq__")


def test_raising_eq_propagates_on_call():
    @cache(max_size=128)
    def f(key):
        return 1

    f(RaisingEq())  # prime: empty bucket, no comparison yet
    # Second call collides (same hash) -> __eq__ runs and raises. The original
    # RuntimeError must propagate, not a masked SystemError.
    with pytest.raises(RuntimeError, match="boom from __eq__"):
        f(RaisingEq())


def test_raising_eq_propagates_on_get():
    @cache(max_size=128)
    def f(key):
        return 1

    f(RaisingEq())  # prime
    with pytest.raises(RuntimeError, match="boom from __eq__"):
        f.get(RaisingEq())


def test_raising_eq_propagates_on_probe():
    @cache(max_size=128)
    def f(key):
        return 1

    f(RaisingEq())  # prime
    with pytest.raises(RuntimeError, match="boom from __eq__"):
        f._probe(RaisingEq())


def test_cache_usable_after_raising_eq():
    """A raising __eq__ must not leave a dangling exception that poisons the
    next, unrelated call."""

    @cache(max_size=128)
    def f(key):
        return 1

    f(RaisingEq())
    with pytest.raises(RuntimeError):
        f(RaisingEq())

    # Different key type, no collision, no raising — must work cleanly.
    assert f("ok") == 1
    assert f("ok") == 1  # cached hit, no lingering error


def test_non_raising_collision_still_caches():
    """Guard against over-eager error detection: keys that collide by hash but
    compare cleanly (return False) must still cache independently."""

    class CleanCollide:
        def __init__(self, tag):
            self.tag = tag

        def __hash__(self):
            return 0  # force collisions

        def __eq__(self, other):
            return isinstance(other, CleanCollide) and self.tag == other.tag

    calls = {"n": 0}

    @cache(max_size=8)
    def f(key):
        calls["n"] += 1
        return key.tag

    a, b = CleanCollide(1), CleanCollide(2)
    assert f(a) == 1
    assert f(b) == 2  # collides with a but != a -> distinct entry
    assert f(a) == 1  # hit
    assert f(b) == 2  # hit
    assert calls["n"] == 2
