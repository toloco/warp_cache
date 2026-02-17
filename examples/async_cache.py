# /// script
# requires-python = ">=3.10"
# dependencies = ["warp_cache"]
# ///
"""Async caching — warp_cache auto-detects async functions."""

import asyncio
import logging

from warp_cache import cache

logging.basicConfig(level=logging.INFO, format="%(message)s")
log = logging.getLogger(__name__)


@cache(max_size=64, ttl=10.0)
async def fetch_user(user_id):
    """Simulate an async API call."""
    log.info("  [miss] fetching user %s", user_id)
    await asyncio.sleep(0.1)  # simulate network latency
    return {"id": user_id, "name": f"User {user_id}"}


async def main():
    # First call: cache miss
    user = await fetch_user(42)
    log.info("Got: %s", user)

    # Second call: instant cache hit (no network round-trip)
    user = await fetch_user(42)
    log.info("Got: %s (cached)", user)

    # Concurrent fetches for different keys — all miss
    users = await asyncio.gather(
        fetch_user(1),
        fetch_user(2),
        fetch_user(3),
    )
    log.info("Fetched %d users concurrently", len(users))

    # Re-fetch the same users — all hit from cache
    log.info("\nRe-fetching same users (all cached)...")
    users = await asyncio.gather(
        fetch_user(1),
        fetch_user(2),
        fetch_user(3),
        fetch_user(42),
    )
    log.info("Fetched %d users from cache", len(users))

    log.info("\nCache info: %s", fetch_user.cache_info())


if __name__ == "__main__":
    asyncio.run(main())
