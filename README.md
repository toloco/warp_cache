# warp_cache

A thread-safe Python caching decorator backed by a Rust extension. Uses
**SIEVE eviction** for scan-resistant, near-optimal hit rates with lock-free
reads. The entire cache lookup happens in a single Rust `__call__` — no Python
wrapper overhead. **7-15M ops/s** single-threaded, **13x** faster than
`cachetools`, with a cross-process shared memory backend reaching **8.9M ops/s**.

## Features

- **Drop-in replacement for `functools.lru_cache`** — same decorator pattern and hashable-argument requirement, with added thread safety, TTL, and async support
- **SIEVE eviction** — a simple, scan-resistant algorithm with near-optimal hit rates and O(1) overhead per access
- **Thread-safe** out of the box (`parking_lot::RwLock` in Rust)
- **Async support**: works with `async def` functions — zero overhead on sync path
- **Shared memory backend**: cross-process caching via mmap with fully lock-free reads
- **TTL support**: optional time-to-live expiration
- **Single FFI crossing**: entire cache lookup happens in Rust, no Python wrapper overhead
- **7-15M ops/s** single-threaded, **10M ops/s** under concurrent load, **13x** faster than `cachetools`

## Installation

Prebuilt wheels are available for Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64):

```bash
pip install warp_cache
```

If no wheel is available for your platform, pip will fall back to the source distribution (requires a [Rust toolchain](https://rustup.rs/)).

## Quick example

```python
from warp_cache import cache

@cache()
def expensive(x, y):
    return x + y

expensive(1, 2)  # computes and caches
expensive(1, 2)  # returns cached result
```

If you're already using `functools.lru_cache`, switching is a one-line change:

```python
-from functools import lru_cache
+from warp_cache import cache

-@lru_cache(maxsize=128)
+@cache(max_size=128)
 def expensive(x, y):
     return x + y
```

Like `lru_cache`, all arguments must be hashable. See the [usage guide](docs/usage.md#basic-caching) for details.

## Performance at a glance

| Metric | warp_cache | cachetools | lru_cache |
|---|---|---|---|
| Single-threaded (cache=256) | 10.5M ops/s | 819K ops/s | 29.6M ops/s |
| Multi-threaded (8T) | 10.4M ops/s | 788K ops/s (with Lock) | 12.1M ops/s (with Lock) |
| Shared memory (single proc) | 8.9M ops/s (mmap) | No | No |
| Shared memory (4 procs) | 7.7M ops/s total | No | No |
| Thread-safe | Yes (RwLock) | No (manual Lock) | No |
| Async support | Yes | No | No |
| TTL support | Yes | Yes | No |
| Eviction | SIEVE (scan-resistant) | LRU, LFU, FIFO, RR | LRU only |
| Implementation | Rust (PyO3) | Pure Python | C (CPython) |

`warp_cache` is the fastest *thread-safe* cache — **13x** faster than `cachetools` and **2.8x** faster than `moka_py`. The shared memory backend reaches 89% of in-process speed with fully lock-free reads. See [full benchmarks](docs/performance.md) for details.

## Documentation

- **[Usage guide](docs/usage.md)** — SIEVE eviction, async, TTL, shared memory, decorator parameters
- **[Performance](docs/performance.md)** — benchmarks, architecture deep-dive, optimization journey
- **[Alternatives](docs/alternatives.md)** — comparison with cachebox, moka-py, cachetools, lru_cache
- **[Examples](examples/)** — runnable scripts for every feature (`uv run examples/<name>.py`)
- **[llms.txt](llms.txt)** / **[llms-full.txt](llms-full.txt)** — project info for LLMs and AI agents ([spec](https://llmstxt.org/))

## Contributing

Contributions are welcome! See **[CONTRIBUTING.md](CONTRIBUTING.md)** for setup instructions, coding standards, and PR guidelines.

For security issues, please see **[SECURITY.md](SECURITY.md)**.

## License

MIT
