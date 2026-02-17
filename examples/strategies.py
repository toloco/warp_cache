# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""Eviction strategies — LRU, MRU, FIFO, LFU."""

import logging

from warp_cache import Strategy, cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


def demo_strategy(name, strategy):
    @cache(strategy=strategy, max_size=3)
    def fn(x):
        return x * 10

    # Fill the cache: [1, 2, 3]
    fn(1)
    fn(2)
    fn(3)

    # Access 1 and 2 again (affects LRU/LFU ordering)
    fn(1)
    fn(2)

    # Insert 4 — triggers eviction
    fn(4)

    info = fn.cache_info()
    log.info("%4s: hits=%d, misses=%d, size=%d", name, info.hits, info.misses, info.current_size)


if __name__ == "__main__":
    log.info("Each strategy evicts a different entry when the cache is full:\n")
    demo_strategy("LRU", Strategy.LRU)    # Evicts least recently used (3)
    demo_strategy("MRU", Strategy.MRU)    # Evicts most recently used (2)
    demo_strategy("FIFO", Strategy.FIFO)  # Evicts first inserted (1)
    demo_strategy("LFU", Strategy.LFU)    # Evicts least frequently used (3)
