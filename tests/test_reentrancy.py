"""Regression tests for issue #30.

A cache lookup holds a shard borrow across hashbrown probing, which runs Python
``__eq__`` (via ``PyObject_RichCompareBool``). If that ``__eq__`` re-enters the
same cached function it used to alias ``&Shard`` with ``&mut Shard`` (UB on GIL
builds) or deadlock the ``RwLock`` (free-threaded builds). The fix makes such
reentrant calls bypass the cache and recompute.
"""

from warp_cache import cache


def test_reentrant_eq_does_not_corrupt_cache():
    """A key whose __eq__ re-enters the same cache must not crash or break the
    capacity invariant (the bug produced current_size=5 with max_size=4)."""
    calls = {"n": 0}

    @cache(max_size=4)
    def f(key):
        calls["n"] += 1
        return calls["n"]

    class Reenter:
        depth = 0

        def __hash__(self):
            # Constant hash forces every key into one shard and forces hashbrown
            # to invoke __eq__ during probing (all keys collide).
            return 0

        def __eq__(self, other):
            # Re-enter the SAME cache while __eq__ runs inside a live borrow.
            if Reenter.depth < 2:
                Reenter.depth += 1
                try:
                    f(Reenter())
                finally:
                    Reenter.depth -= 1
            return self is other

    f(Reenter())  # prime: now the map holds a hash-0 key to collide against
    for _ in range(10):
        f(Reenter())

    info = f.cache_info()
    assert info.current_size <= 4, f"capacity invariant violated: {info.current_size}"


def test_recursive_cached_function_caches():
    """Ordinary recursion re-enters the function OUTSIDE any borrow, so it must
    keep caching subproblems (guards against a fix that over-blocks)."""

    @cache(max_size=128)
    def fib(n):
        if n < 2:
            return n
        return fib(n - 1) + fib(n - 2)

    assert fib(20) == 6765
    info = fib.cache_info()
    assert info.hits > 0
    assert info.current_size > 0


def test_reentrant_call_returns_correct_value():
    """The bypassed reentrant call must still return the correct recomputed value."""

    @cache(max_size=8)
    def f(key):
        return key.tag * 10

    class K:
        def __init__(self, tag, reenter=None):
            self.tag = tag
            self.reenter = reenter
            self.result = None

        def __hash__(self):
            return 0

        def __eq__(self, other):
            if self.reenter is not None and self.result is None:
                self.result = f(self.reenter)  # reentrant (bypassed) call
            return self is other

    f(K(1))  # prime
    probe = K(2, reenter=K(3))
    assert f(probe) == 20
    assert probe.result == 30
