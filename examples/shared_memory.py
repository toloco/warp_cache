# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""Shared memory backend — cache shared across multiple processes."""

import logging
import multiprocessing
import os

from warp_cache import cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


@cache(max_size=256, backend="shared")
def expensive_compute(n):
    """CPU-bound work that benefits from cross-process caching."""
    total = 0
    for i in range(n * 1000):
        total += i * i
    return total


def worker(keys):
    """Each worker process shares the same cache via mmap."""
    pid = os.getpid()
    for k in keys:
        result = expensive_compute(k)
        if k == keys[0]:
            log.info("  Worker %d: compute(%d) = %d", pid, k, result)
    info = expensive_compute.cache_info()
    log.info("  Worker %d: %s", pid, info)


if __name__ == "__main__":
    # Warmup: populate cache in main process
    for i in range(20):
        expensive_compute(i)
    log.info("Main process populated cache: %s\n", expensive_compute.cache_info())

    # Spawn workers — they see the cached values immediately
    keys = list(range(20))
    with multiprocessing.Pool(4) as pool:
        pool.map(worker, [keys[0:5], keys[5:10], keys[10:15], keys[15:20]])

    log.info("\nFinal cache info: %s", expensive_compute.cache_info())
