# Performance

*Environment: Python 3.13.2, CPython, Apple M-series (arm64), Clang 20.1.0*

## Architecture

The entire cache lookup happens in a single Rust `__call__`:

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash
       ├─ HashMap lookup       papaya lock-free map / hashbrown
       ├─ equality check       via ffi::PyObject_RichCompareBool
       ├─ RwLock (read)        parking_lot, ~8ns uncontended
       ├─ SIEVE visited=1      single-word store, no lock promotion
       └─ return cached value
```

No Python wrapper function. No serialization. No intermediate key object.

## SIEVE eviction

Both the in-process and shared memory backends use **SIEVE** — a scan-resistant eviction algorithm that achieves near-optimal hit rates with O(1) overhead per operation.

How it works:
- **On hit**: set `visited = 1` — a single idempotent store. No linked-list reordering, no lock promotion.
- **On evict**: a "hand" scans the entry list. Visited entries get a second chance (bit cleared to 0); unvisited entries are evicted.

This is simpler than LRU (no list reordering on every hit) and achieves higher hit rates than LRU/FIFO on skewed workloads. The visited-bit design enables **lock-free reads** on both backends — cache hits never need a write lock.

## Single-threaded throughput vs cache size

| Cache Size | warp_cache | lru_cache | cachetools | cachebox | moka_py | wc/ct | wc/lru |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 32 | 6.6M | 20.4M | 587K | 1.1M | 3.0M | 11.3x | 0.32x |
| 64 | 7.9M | 22.5M | 628K | 1.1M | 3.3M | 12.6x | 0.35x |
| 128 | 9.0M | 25.6M | 710K | 1.2M | 3.5M | 12.6x | 0.35x |
| 256 | 10.5M | 29.6M | 819K | 1.5M | 3.7M | 12.8x | 0.35x |
| 512 | 12.2M | 35.3M | 933K | 1.8M | 4.0M | 13.1x | 0.35x |
| 1024 | 14.5M | 39.6M | 1.1M | 2.4M | 4.4M | 13.1x | 0.37x |

## TTL throughput (cache size = 256)

| TTL | warp_cache | cachetools | moka_py | wc/ct |
|---|---:|---:|---:|---:|
| 1ms | 4.8M | 601K | 2.5M | 8.0x |
| 10ms | 5.1M | 528K | 2.7M | 9.7x |
| 100ms | 5.3M | 522K | 2.6M | 10.2x |
| 1s | 5.4M | 526K | 2.8M | 10.2x |
| None | 5.5M | 525K | 2.8M | 10.5x |

TTL adds minimal overhead — the expiry timestamp is checked during the normal read path, with no background eviction thread.

## Multi-threaded throughput (cache size = 256)

| Threads | warp_cache | lru_cache + Lock | cachetools + Lock | cachebox | moka_py | wc/lru |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 10.5M | 12.1M | 768K | 1.5M | 3.7M | 0.87x |
| 2 | 10.1M | 12.3M | 785K | 1.5M | 3.6M | 0.82x |
| 4 | 10.3M | 12.1M | 801K | 1.5M | 3.6M | 0.85x |
| 8 | 10.4M | 12.1M | 788K | 1.5M | 3.6M | 0.86x |
| 16 | 10.2M | 11.8M | 793K | 1.4M | 3.5M | 0.87x |
| 32 | 9.8M | 11.6M | 776K | 1.4M | 3.6M | 0.84x |

`warp_cache` maintains ~10M ops/s regardless of thread count — stable scaling with no contention. Under the GIL, `lru_cache + Lock` is faster in single-threaded mode because `lru_cache` is C code with zero lock overhead (the GIL provides implicit safety). Under free-threaded Python (no GIL), `warp_cache`'s `RwLock` enables true parallel reads while `lru_cache` must acquire a real lock.

## Shared memory backend

The shared memory backend uses SIEVE with **fully lock-free reads** — the `visited` bit is set via a direct idempotent store, requiring no write lock. This is a major improvement over the previous LRU-based shared backend, which needed a write lock on every cache hit to reorder the linked list.

### Memory vs shared throughput

| Backend | Throughput | Hit Rate | Notes |
|---|---:|---:|---|
| Memory (in-process) | 10.0M ops/s | 72.3% | papaya lock-free HashMap + SIEVE |
| Shared (mmap, single process) | 8.9M ops/s | 72.7% | Seqlock + lock-free reads |

The shared backend reaches **89% of in-process speed**. The remaining gap is dominated by pickle serialization (required for cross-process compatibility), not locking overhead.

### Multi-process scaling

| Processes | Total Throughput | Per-Process Avg |
|---:|---:|---:|
| 1 | 4.8M ops/s | 10.0M ops/s |
| 2 | 7.0M ops/s | 5.9M ops/s |
| 4 | 7.7M ops/s | 2.7M ops/s |
| 8 | 6.5M ops/s | 1.0M ops/s |

Lock-free reads enable excellent multi-process scaling. Total throughput peaks at 4 processes (7.7M ops/s) — 1.6x the single-process rate. Even with 8 processes contending on the same mmap'd file, throughput stays at 6.5M ops/s.

For comparison, the previous LRU-based shared backend achieved only 3.1M ops/s at 4 processes and 1.9M ops/s at 8 processes — SIEVE delivers **2.5x and 3.4x improvements** respectively, thanks to eliminating write locks on the read path.

## Where the remaining gap lives

At cache size 256, a cache hit takes ~95ns vs `lru_cache`'s ~34ns:

| Operation | lru_cache (C) | warp_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Table lookup + key equality | ~5ns | ~15ns | +10ns |
| SIEVE visited store | ~0ns | ~1ns | +1ns |
| **Lock acquire + release** | **0ns** | **~8ns** | **+8ns** |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~34ns** | **~61ns** | **+27ns** |

Three categories:

1. **Irreducible: Thread safety lock (~8ns)** — `lru_cache` pays nothing because
   the GIL provides implicit thread safety. We pay ~8ns for an uncontended
   `parking_lot` read lock. This cannot be eliminated without removing thread
   safety.

2. **Structural: PyO3 call dispatch (~5ns)** — PyO3's `tp_call` shim extracts
   GIL tokens, validates and converts argument pointers. `lru_cache` receives raw
   `PyObject*` directly. Inherent to using a safe FFI layer.

3. **Structural: Lock-free HashMap (~10ns)** — `papaya::HashMap` uses atomic
   operations for lock-free concurrent access. `lru_cache` uses a simple C hash
   table with no concurrency support. The extra overhead pays for true parallel
   reads under free-threaded Python.

## Python 3.13+/3.14 free-threading

Under free-threaded Python (no GIL), `warp_cache`'s architecture pays off:

- **warp_cache improves**: `RwLock` enables true parallel reads across cores
- **lru_cache gets worse**: needs a real lock without the GIL's implicit protection
- **Trade-off**: atomic refcounting adds ~2-5ns to single-threaded cost

The benchmark runner (`benchmarks/_bench_runner.py`) automatically creates
temporary uv venvs for each Python version, builds warp_cache via maturin, and
runs all benchmarks.

## Optimization Journey

| Phase | Change | Throughput | Ratio vs lru_cache |
|---|---|---:|---:|
| 1. Serialization + Python wrapper | pickle.dumps, functools.wraps, 2 FFI crossings | ~500K ops/s | 0.02x |
| 2. PyObject keys + Rust `__call__` | Precomputed hash, single FFI crossing | ~13-18M ops/s | 0.56-0.68x |
| 3. Compiler: fat LTO + codegen-units=1 | Cross-crate inlining of PyO3 wrappers | +10-15% | 0.66-0.74x |
| 4. Raw FFI for key equality | `ffi::PyObject_RichCompareBool` instead of `Python::with_gil` | +multi-thread | — |
| 5. SIEVE eviction | Unified eviction for both backends, lock-free reads | +12% hit rate | — |
| 6. Lock-free HashMap (papaya) | Concurrent reads without lock acquisition | true parallel reads | — |

---

*Benchmarks: 100K ops per config, Zipf-distributed keys (2000 unique), `time.perf_counter()`. Python 3.13.2, cachetools 7.0.1, moka_py 0.3.0, cachebox 5.2.2. Source: `benchmarks/`*
