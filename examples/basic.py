# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""Basic caching example — memoize an expensive function."""

import logging

from warp_cache import cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


@cache(max_size=256)
def fibonacci(n):
    if n < 2:
        return n
    return fibonacci(n - 1) + fibonacci(n - 2)


if __name__ == "__main__":
    # Without caching this would be exponentially slow
    result = fibonacci(80)
    log.info("fibonacci(80) = %s", result)

    info = fibonacci.cache_info()
    log.info("Cache info: %s", info)

    # Clear and recompute
    fibonacci.cache_clear()
    log.info("Cleared cache, recomputing fibonacci(10) = %s", fibonacci(10))
    log.info("After clear + recompute: %s", fibonacci.cache_info())
