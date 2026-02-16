import time

from warp_cache import cache


def test_ttl_expiry():
    call_count = 0

    @cache(max_size=128, ttl=0.1)
    def compute(x):
        nonlocal call_count
        call_count += 1
        return x * 2

    assert compute(5) == 10
    assert call_count == 1
    assert compute(5) == 10  # hit
    assert call_count == 1

    time.sleep(0.15)

    assert compute(5) == 10  # expired, recomputed
    assert call_count == 2


def test_ttl_not_expired():
    call_count = 0

    @cache(max_size=128, ttl=1.0)
    def compute(x):
        nonlocal call_count
        call_count += 1
        return x + 1

    assert compute(3) == 4
    assert compute(3) == 4
    assert call_count == 1  # still cached


def test_no_ttl():
    """Without TTL, entries never expire."""
    call_count = 0

    @cache(max_size=128)
    def compute(x):
        nonlocal call_count
        call_count += 1
        return x

    compute(1)
    time.sleep(0.05)
    compute(1)
    assert call_count == 1
