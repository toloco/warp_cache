import threading
from concurrent.futures import ThreadPoolExecutor

from fast_cache import Strategy, cache


def test_concurrent_access():
    """Multiple threads hitting the same cached function concurrently."""
    call_count = 0
    lock = threading.Lock()

    @cache(strategy=Strategy.LRU, max_size=128)
    def slow_add(a, b):
        nonlocal call_count
        with lock:
            call_count += 1
        return a + b

    def worker(i):
        # Each thread calls with the same args to exercise cache hits
        for _ in range(50):
            assert slow_add(1, 2) == 3
            assert slow_add(i, i) == i * 2

    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = [pool.submit(worker, i) for i in range(8)]
        for f in futures:
            f.result()

    info = slow_add.cache_info()
    # At least some hits should have occurred
    assert info.hits > 0
    # call_count should be much less than 8 * 50 * 2 = 800
    assert call_count < 800


def test_concurrent_different_strategies():
    """Verify thread safety across all strategies."""
    for strategy in Strategy:
        call_count = 0
        lock = threading.Lock()

        @cache(strategy=strategy, max_size=64)
        def fn(x):
            nonlocal call_count
            with lock:
                call_count += 1
            return x * x

        def worker():
            for i in range(100):
                assert fn(i % 20) == (i % 20) ** 2

        threads = [threading.Thread(target=worker) for _ in range(8)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        info = fn.cache_info()
        assert info.hits > 0, f"Expected hits for {strategy.name}"


def test_concurrent_cache_clear():
    """Test that cache_clear during concurrent access doesn't crash."""

    @cache(strategy=Strategy.LRU, max_size=128)
    def fn(x):
        return x

    stop = threading.Event()

    def reader():
        while not stop.is_set():
            fn(1)
            fn(2)

    def clearer():
        for _ in range(50):
            fn.cache_clear()

    threads = [threading.Thread(target=reader) for _ in range(4)]
    threads.append(threading.Thread(target=clearer))
    for t in threads:
        t.start()

    # Let it run briefly
    stop.set()
    for t in threads:
        t.join(timeout=5)
