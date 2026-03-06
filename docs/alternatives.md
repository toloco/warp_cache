# Alternatives

The Python caching ecosystem includes several notable libraries. Here's how they compare:

| Feature | warp_cache | cachebox | moka-py | cachetools | lru_cache |
|---|---|---|---|---|---|
| Implementation | Rust (PyO3) | Rust (PyO3) | Rust (moka) | Pure Python | C (CPython) |
| Thread-safe | Yes (builtin) | Yes (builtin) | Yes (builtin) | No (manual Lock) | No |
| Async support | Yes | Yes | Yes | No | No |
| Cross-process (shared mem) | Yes (mmap) | No | No | No | No |
| TTL support | Yes | Yes | Yes (+ TTI) | Yes | No |
| Eviction | SIEVE | LRU/LFU/FIFO/RR | TinyLFU/LRU | LRU/LFU/FIFO/RR | LRU |
| Stampede prevention | No | Yes | Yes (`get_with`) | No | No |
| Per-entry TTL | No | Yes (VTTLCache) | Yes | No | No |
| Manual cache object | No | Yes (dict-like) | Yes (`Moka(...)`) | Yes | No |
| Cache statistics | Yes | Yes (+ memory) | No | Yes | Yes |

**Performance** (same machine, same workload, single-threaded, cache=256):

| Library | ops/s | vs warp_cache |
|---|---:|---:|
| lru_cache | 31.0M | 1.5x faster |
| warp_cache | 20.4M | 1.0x |
| moka_py | 3.9M | 5.3x slower |
| cachebox | 1.5M | 13.6x slower |
| cachetools | 826K | 24.7x slower |

See [full benchmarks](../benchmarks/COMPARISON.md) for multi-thread, TTL, shared memory, and sustained throughput results.

**warp_cache's niche**: the only Rust-backed cache combining shared memory (cross-process mmap), SIEVE eviction (scan-resistant, near-optimal hit rates), and builtin thread safety in a single decorator. If you need stampede prevention or per-entry TTL, look at cachebox or moka-py. If you need a manual cache object API, look at moka-py or cachebox. If you need maximum single-threaded speed, use `lru_cache`.
