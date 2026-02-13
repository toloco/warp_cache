from fast_cache import Strategy, cache


def test_lru_eviction_order():
    """LRU: least recently used evicted first."""
    call_count = 0

    @cache(strategy=Strategy.LRU, max_size=3)
    def fn(x):
        nonlocal call_count
        call_count += 1
        return x

    fn(1)  # miss. Cache order (LRU→MRU): [1]
    fn(2)  # miss. [1, 2]
    fn(3)  # miss. [1, 2, 3]
    fn(1)  # hit, promotes 1. [2, 3, 1]
    assert call_count == 3

    fn(4)  # miss, evicts 2 (LRU). [3, 1, 4]
    assert call_count == 4

    # Verify: 2 was evicted (miss), 1 and 3 are still present (hit)
    call_count = 0
    fn(1)  # hit
    assert call_count == 0
    fn(3)  # hit
    assert call_count == 0
    fn(2)  # miss — was evicted
    assert call_count == 1


def test_fifo_eviction_order():
    """FIFO: first inserted evicted first, access doesn't change order."""
    call_count = 0

    @cache(strategy=Strategy.FIFO, max_size=3)
    def fn(x):
        nonlocal call_count
        call_count += 1
        return x

    fn(1)  # miss. Insertion order: [1]
    fn(2)  # miss. [1, 2]
    fn(3)  # miss. [1, 2, 3]
    fn(1)  # hit (FIFO doesn't reorder). Still [1, 2, 3]
    assert call_count == 3

    fn(4)  # miss, evicts 1 (oldest). [2, 3, 4]
    assert call_count == 4

    # Verify: 1 was evicted (miss), 2 and 3 are still present (hit)
    call_count = 0
    fn(2)  # hit
    assert call_count == 0
    fn(3)  # hit
    assert call_count == 0
    fn(1)  # miss — was evicted
    assert call_count == 1


def test_mru_eviction_order():
    """MRU: most recently used evicted first."""
    call_count = 0

    @cache(strategy=Strategy.MRU, max_size=3)
    def fn(x):
        nonlocal call_count
        call_count += 1
        return x

    fn(1)  # miss. [1]
    fn(2)  # miss. [1, 2]
    fn(3)  # miss. [1, 2, 3]
    fn(2)  # hit, 2 becomes most recent. [1, 3, 2]
    assert call_count == 3

    fn(4)  # miss, evicts 2 (MRU). [1, 3, 4]
    assert call_count == 4

    # Verify: 2 was evicted (miss), 1 and 3 are still present (hit)
    call_count = 0
    fn(1)  # hit
    assert call_count == 0
    fn(3)  # hit
    assert call_count == 0
    fn(2)  # miss — was evicted
    assert call_count == 1


def test_lfu_eviction_order():
    """LFU: least frequently used evicted first."""
    call_count = 0

    @cache(strategy=Strategy.LFU, max_size=3)
    def fn(x):
        nonlocal call_count
        call_count += 1
        return x

    fn(1)  # miss, freq(1)=0
    fn(2)  # miss, freq(2)=0
    fn(3)  # miss, freq(3)=0
    fn(1)  # hit, freq(1)=1
    fn(1)  # hit, freq(1)=2
    fn(2)  # hit, freq(2)=1
    # freqs: 1→2, 2→1, 3→0
    assert call_count == 3

    fn(4)  # miss, evicts 3 (lowest freq=0)
    assert call_count == 4

    # Verify: 3 was evicted (miss), 1 and 2 are still present (hit)
    call_count = 0
    fn(1)  # hit
    assert call_count == 0
    fn(2)  # hit
    assert call_count == 0
    fn(3)  # miss — was evicted
    assert call_count == 1
