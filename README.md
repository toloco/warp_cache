# warp_cache

A thread-safe Python caching decorator backed by a Rust extension. Uses
**SIEVE eviction** for scan-resistant, near-optimal hit rates with zero-cost
locking under the GIL. The entire cache lookup happens in a single Rust `__call__` — no Python
wrapper overhead. **16-23M ops/s** single-threaded, **25x** faster than
`cachetools`, with a cross-process shared memory backend reaching **9.7M ops/s**.

## Features

- **Drop-in replacement for `functools.lru_cache`** — same decorator pattern and hashable-argument requirement, with added thread safety, TTL, and async support
- **[SIEVE eviction](https://junchengyang.com/publication/nsdi24-SIEVE.pdf)** — a simple, scan-resistant algorithm with near-optimal hit rates and O(1) overhead per access
- **Thread-safe** out of the box (zero-cost `GilCell` under GIL, sharded `RwLock` under free-threaded Python)
- **Async support**: works with `async def` functions — zero overhead on sync path
- **Shared memory backend**: cross-process caching via mmap with fully lock-free reads
- **TTL support**: optional time-to-live expiration
- **Single FFI crossing**: entire cache lookup happens in Rust, no Python wrapper overhead
- **16-23M ops/s** single-threaded, **20M+ ops/s** under concurrent load, **25x** faster than `cachetools`

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
| Single-threaded (cache=256) | 20.4M ops/s | 826K ops/s | 31.0M ops/s |
| Multi-threaded (8T) | 20.4M ops/s | 793K ops/s (with Lock) | 12.6M ops/s (with Lock) |
| Shared memory (single proc) | 9.7M ops/s (mmap) | No | No |
| Shared memory (4 procs) | 8.1M ops/s total | No | No |
| Thread-safe | Yes (GilCell / sharded RwLock) | No (manual Lock) | No |
| Async support | Yes | No | No |
| TTL support | Yes | Yes | No |
| Eviction | SIEVE (scan-resistant) | LRU, LFU, FIFO, RR | LRU only |
| Implementation | Rust (PyO3) | Pure Python | C (CPython) |

`warp_cache` is the fastest *thread-safe* cache — **25x** faster than `cachetools` and **5.3x** faster than `moka_py`. Under multi-threaded load, it's **1.6x faster** than `lru_cache + Lock`. See [full benchmarks](docs/performance.md) for details.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="benchmarks/results/comparison_mt_scaling_dark.svg">
  <img src="benchmarks/results/comparison_mt_scaling_light.svg" alt="Multi-thread scaling: GIL vs no-GIL">
</picture>

## Eviction quality: SIEVE vs LRU

Beyond throughput, SIEVE delivers **up to 21.6% miss reduction** vs LRU. From the [NSDI'24 paper](https://junchengyang.com/publication/nsdi24-SIEVE.pdf), key findings reproduced in `benchmarks/bench_sieve.py` (1M requests, Zipf-distributed keys):

| Workload | SIEVE | LRU | Miss Reduction |
|---|---:|---:|---:|
| Zipf, 10% cache | 74.5% | 67.5% | +21.6% |
| Scan resistance (70% hot) | 69.9% | 63.5% | +17.6% |
| One-hit wonders (25% unique) | 53.9% | 43.7% | +18.1% |
| Working set shift | 75.5% | 69.7% | +16.6% |

SIEVE's visited-bit design protects hot entries from sequential scans and filters out one-hit wonders that would pollute LRU. See [eviction quality benchmarks](docs/performance.md#sieve-eviction-quality) for the full breakdown.

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
