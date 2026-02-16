# warp_cache

A thread-safe Python caching decorator backed by a Rust extension. Through a
series of optimizations — eliminating serialization, moving the call wrapper
into Rust, applying link-time optimization, and using direct C API calls — we
achieve **0.55-0.66x** of `lru_cache`'s single-threaded throughput while
providing native thread safety that delivers **1.3-1.4x** higher throughput
under concurrent load — and **18-24x** faster than pure-Python `cachetools`.

## Features

- **Drop-in replacement for `functools.lru_cache`** — same decorator pattern and hashable-argument requirement, with added thread safety, TTL, eviction strategies, and async support
- **Thread-safe** out of the box (`parking_lot::RwLock` in Rust)
- **Async support**: works with `async def` functions — zero overhead on sync path
- **Shared memory backend**: cross-process caching via mmap
- **Multiple eviction strategies**: LRU, MRU, FIFO, LFU
- **TTL support**: optional time-to-live expiration
- **Single FFI crossing**: entire cache lookup happens in Rust, no Python wrapper overhead
- **12-18M ops/s** single-threaded, **16M ops/s** under concurrent load, **18-24x** faster than `cachetools`

## Installation

Prebuilt wheels are available for Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64):

```bash
pip install -i https://test.pypi.org/simple/ warp_cache
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
| Single-threaded | 12-18M ops/s | 0.6-1.2M ops/s | 21-40M ops/s |
| Multi-threaded (8T) | 16M ops/s | 770K ops/s (with Lock) | 12M ops/s (with Lock) |
| Thread-safe | Yes (RwLock) | No (manual Lock) | No |
| Async support | Yes | No | No |
| Cross-process (shared) | ~7.8M ops/s (mmap) | No | No |
| TTL support | Yes | Yes | No |
| Eviction strategies | LRU, MRU, FIFO, LFU | LRU, LFU, FIFO, RR | LRU only |
| Implementation | Rust (PyO3) | Pure Python | C (CPython) |

Under concurrent load, `warp_cache` delivers **1.3-1.4x** higher throughput than `lru_cache + Lock` and **18-24x** higher than `cachetools`. See [full benchmarks](docs/performance.md) for details.

## Documentation

- **[Usage guide](docs/usage.md)** — eviction strategies, async, TTL, shared memory, decorator parameters
- **[Performance](docs/performance.md)** — benchmarks, architecture deep-dive, optimization journey
- **[Alternatives](docs/alternatives.md)** — comparison with cachebox, moka-py, cachetools, lru_cache
- **[Development](docs/development.md)** — building from source, running tests

## License

MIT
