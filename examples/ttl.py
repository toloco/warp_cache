# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""TTL (time-to-live) example — cached values expire automatically."""

import logging
import time

from warp_cache import cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


@cache(max_size=64, ttl=2.0)
def get_config(key):
    """Simulate fetching a config value (expensive I/O)."""
    log.info("  [miss] fetching '%s' from source", key)
    return f"value_for_{key}"


if __name__ == "__main__":
    # First call: cache miss
    log.info("Result: %s", get_config("database_url"))

    # Second call: cache hit (no miss log from the function)
    log.info("Result: %s (cached)", get_config("database_url"))

    log.info("Cache info: %s", get_config.cache_info())

    # Wait for TTL to expire
    log.info("\nWaiting 2.5s for TTL expiry...")
    time.sleep(2.5)

    # Third call: cache miss again (entry expired)
    log.info("Result: %s", get_config("database_url"))
    log.info("Cache info: %s", get_config.cache_info())
