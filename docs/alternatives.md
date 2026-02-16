# Alternatives

The Python caching ecosystem includes several notable libraries. Here's how they compare:

| Feature | warp_cache | cachebox | moka-py | cachetools | lru_cache |
|---|---|---|---|---|---|
| Implementation | Rust (PyO3) | Rust (PyO3) | Rust (moka) | Pure Python | C (CPython) |
| Thread-safe | Yes (builtin) | Yes (builtin) | Yes (builtin) | No (manual Lock) | No |
| Async support | Yes | Yes | Yes | No | No |
| Cross-process (shared mem) | Yes (mmap) | No | No | No | No |
| TTL support | Yes | Yes | Yes (+ TTI) | Yes | No |
| LRU | Yes | Yes | Yes | Yes | Yes |
| LFU | Yes | Yes | TinyLFU | Yes | No |
| FIFO | Yes | Yes | No | Yes | No |
| MRU | Yes | No | No | No | No |
| Custom key function | No | No | No | Yes | No |
| Stampede prevention | No | Yes | Yes | No | No |
| Per-entry TTL | No | Yes (VTTLCache) | Yes | No | No |

**Performance ballpark** (not directly comparable — different benchmarking setups):

- [cachebox](https://github.com/awolverp/cachebox): ~3.7M ops/s LRU insert (from cachebox-benchmark, Python 3.13)
- [moka-py](https://github.com/deliro/moka-py): ~8.9M ops/s get (from moka-py README)
- warp_cache: 14-20M ops/s get (our benchmarks, see [performance](performance.md))

These numbers come from different machines, different workloads, and different measurement methodologies — treat them as order-of-magnitude indicators, not head-to-head results.

**warp_cache's niche**: the only Rust-backed cache combining shared memory (cross-process mmap), all four eviction strategies (LRU/MRU/FIFO/LFU), and builtin thread safety in a single decorator. If you need stampede prevention or per-entry TTL, look at cachebox or moka-py. If you need a custom key function, cachetools is the way to go.
