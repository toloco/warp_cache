# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""SIEVE eviction — scan-resistant caching with second chances."""

import logging

from warp_cache import cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


if __name__ == "__main__":
    log.info("SIEVE eviction demo: visited entries get a second chance\n")

    call_count = 0

    @cache(max_size=3)
    def fn(x):
        global call_count
        call_count += 1
        return x * 10

    # Fill the cache: [1, 2, 3] — all unvisited
    fn(1)
    fn(2)
    fn(3)
    log.info("After inserting 1, 2, 3: %s", fn.cache_info())

    # Access 1 and 2 — marks them as visited (protected)
    fn(1)
    fn(2)
    log.info("After accessing 1 and 2 (now visited): hits=%d", fn.cache_info().hits)

    # Insert 4 — triggers eviction. Entry 3 is unvisited, so it's evicted.
    fn(4)
    log.info("After inserting 4: %s", fn.cache_info())

    # Verify: 3 was evicted (miss), 1 and 2 survived (hit)
    call_count = 0
    fn(1)  # hit
    fn(2)  # hit
    fn(3)  # miss — was evicted
    log.info("Accessing 1 (hit), 2 (hit), 3 (miss — evicted): recomputed=%d", call_count)
