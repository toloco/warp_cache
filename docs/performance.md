# Performance

*Environment: Python 3.13.2, CPython, Apple M-series (arm64), Clang 20.1.0*

## Architecture

The entire cache lookup happens in a single Rust `__call__`:

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash
       ├─ shard select         hash % n_shards
       ├─ RwLock::read()       per-shard read lock (~8ns)
       ├─ HashMap lookup       hashbrown
       ├─ equality check       via ffi::PyObject_RichCompareBool
       ├─ SIEVE visited=1      AtomicBool store, lock-free
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
| 32 | 13.0M | 19.6M | 594K | 1.1M | 3.1M | 21.9x | 0.66x |
| 64 | 15.0M | 22.3M | 628K | 1.2M | 3.3M | 23.9x | 0.67x |
| 128 | 16.6M | 25.7M | 711K | 1.3M | 3.4M | 23.3x | 0.65x |
| 256 | 18.1M | 32.1M | 814K | 1.5M | 3.7M | 22.2x | 0.56x |
| 512 | 18.6M | 34.5M | 948K | 1.8M | 4.1M | 19.6x | 0.54x |
| 1024 | 19.9M | 39.5M | 1.1M | 2.4M | 4.4M | 17.6x | 0.50x |

## TTL throughput (cache size = 256)

| TTL | warp_cache | cachetools | moka_py | wc/ct |
|---|---:|---:|---:|---:|
| 1ms | 6.7M | 584K | 2.5M | 11.5x |
| 10ms | 6.9M | 528K | 2.7M | 13.1x |
| 100ms | 6.9M | 529K | 2.7M | 13.0x |
| 1s | 7.0M | 526K | 2.6M | 13.3x |
| None | 6.9M | 532K | 2.7M | 13.0x |

TTL adds minimal overhead — the expiry timestamp is checked during the normal read path, with no background eviction thread.

## Multi-threaded throughput (cache size = 256)

| Threads | warp_cache | lru_cache + Lock | cachetools + Lock | cachebox | moka_py | wc/lru |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 18.3M | 11.9M | 778K | 1.5M | 3.6M | 1.54x |
| 2 | 17.3M | 12.2M | 793K | 1.5M | 3.5M | 1.42x |
| 4 | 18.0M | 12.6M | 803K | 1.5M | 3.6M | 1.43x |
| 8 | 17.9M | 12.3M | 774K | 1.5M | 3.6M | 1.46x |
| 16 | 17.2M | 11.6M | 785K | 1.5M | 3.6M | 1.48x |
| 32 | 16.8M | 11.6M | 779K | 1.4M | 3.6M | 1.45x |

`warp_cache` maintains ~17-18M ops/s regardless of thread count — stable scaling with no contention, and **1.4-1.5x faster** than `lru_cache + Lock` even under the GIL. The sharded `RwLock` architecture means cache hits only acquire a cheap per-shard read lock (~8ns), while `lru_cache + Lock` must acquire a global `threading.Lock()` on every access. Under free-threaded Python (no GIL), `warp_cache`'s per-shard locking enables true parallel reads across shards while `lru_cache` must acquire a real lock.

## Shared memory backend

The shared memory backend uses SIEVE with **fully lock-free reads** — no Mutex, no RwLock. `ShmCache` uses interior mutability: reads go through the seqlock's optimistic path (lock-free), and the `visited` bit is set via a direct idempotent store. Writes acquire the seqlock's TTAS spinlock internally. Size limit checks use cached struct fields, avoiding shared memory header reads on the hot path.

### Memory vs shared throughput

| Backend | Throughput | Hit Rate | Notes |
|---|---:|---:|---|
| Memory (in-process) | 17.2M ops/s | 71.2% | Sharded hashbrown HashMap + RwLock + SIEVE |
| Shared (mmap, single process) | 9.2M ops/s | 72.3% | Seqlock + lock-free reads, no Mutex |

The shared backend reaches **54% of in-process speed**. The gap is dominated by serialization (serde fast-path for primitives, pickle fallback), ahash of key bytes, seqlock overhead, and mmap copy — all irreducible cross-process costs.

### Multi-process scaling

| Processes | Total Throughput | Per-Process Avg |
|---:|---:|---:|
| 1 | 4.8M ops/s | 4.8M ops/s |
| 2 | 7.2M ops/s | 3.6M ops/s |
| 4 | 7.5M ops/s | 1.9M ops/s |
| 8 | 6.6M ops/s | 0.8M ops/s |

Lock-free reads enable excellent multi-process scaling. Total throughput peaks at 4 processes (7.5M ops/s) — 1.6x the single-process rate. Even with 8 processes contending on the same mmap'd file, throughput stays at 6.6M ops/s.

For comparison, the previous LRU-based shared backend achieved only 3.1M ops/s at 4 processes and 1.9M ops/s at 8 processes — SIEVE delivers **2.5x and 3.4x improvements** respectively, thanks to eliminating write locks on the read path.

## SIEVE eviction quality

Beyond throughput, SIEVE delivers **measurably better hit rates** than LRU across all workload patterns. These benchmarks compare `warp_cache` (SIEVE) against `functools.lru_cache` (LRU) using 1M requests with Zipf-distributed keys.

Run them yourself: `make bench-sieve` or `python benchmarks/bench_sieve.py --quick`.

### Hit ratio vs cache size

With Zipf alpha=1.0 over 10K unique keys, SIEVE consistently outperforms LRU at every cache size — the advantage peaks at **21.6% miss reduction** at 10% cache ratio:

| Cache % | SIEVE | LRU | Miss Reduction |
|---:|---:|---:|---:|
| 0.1% | 26.2% | 13.1% | +15.1% |
| 1% | 49.6% | 39.0% | +17.4% |
| 5% | 67.2% | 58.6% | +20.7% |
| 10% | 74.5% | 67.5% | **+21.6%** |
| 25% | 84.0% | 79.9% | +20.6% |
| 50% | 91.3% | 89.7% | +15.8% |

### Scan resistance

This is SIEVE's key advantage. Hot working set (100 keys, Zipf) mixed with sequential scans (10K unique keys, each accessed once). Cache size = 200 (fits the entire hot set). SIEVE protects hot items via the visited bit; LRU pushes them out during scans:

| Hot Fraction | SIEVE | LRU | Miss Reduction |
|---:|---:|---:|---:|
| 100% | 99.99% | 99.99% | +0.0% |
| 90% | 90.0% | 88.9% | +10.0% |
| 80% | 80.0% | 76.0% | +16.6% |
| 70% | 69.9% | 63.5% | **+17.6%** |
| 50% | 50.0% | 40.9% | +15.3% |
| 30% | 30.0% | 21.1% | +11.3% |

At 70% hot fraction, SIEVE retains almost all hot items (69.9% hit rate ≈ hot fraction) while LRU drops to 63.5% as scans push hot entries out of the cache.

### One-hit-wonder filtering

Mix of Zipf-distributed reused keys with unique one-time keys (one-hit wonders). SIEVE inserts with `visited=0` and evicts on the first hand scan; LRU gives every entry a full tenure through the cache:

| OHW Ratio | SIEVE | LRU | Miss Reduction |
|---:|---:|---:|---:|
| 0% | 72.4% | 65.0% | +21.2% |
| 25% | 53.9% | 43.7% | +18.1% |
| 50% | 35.6% | 25.8% | +13.2% |
| 75% | 17.2% | 10.6% | +7.4% |

### Working set shift

Three phases: Zipf over keys 0–999, then keys 1000–1999 (completely new set), then back to 0–999. Cache size = 200. Both algorithms adapt, but SIEVE maintains a consistent advantage:

| Phase | SIEVE | LRU |
|---|---:|---:|
| Phase 1 (keys 0–999) | 75.5% | 69.7% |
| Phase 2 (keys 1000–1999) | 75.6% | 69.9% |
| Phase 3 (return to 0–999) | 75.5% | 69.6% |

*Benchmarks: `benchmarks/bench_sieve.py`, 1M ops, Zipf-distributed keys, seed=42.*

## Where the remaining gap lives

### Memory backend vs lru_cache

At cache size 256, a cache hit takes ~55ns vs `lru_cache`'s ~31ns:

| Operation | lru_cache (C) | warp_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Shard select + RwLock::read | ~0ns | ~8ns | +8ns |
| Table lookup + key equality | ~5ns | ~5ns | 0 |
| SIEVE visited store | ~0ns | ~1ns | +1ns |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~31ns** | **~55ns** | **+24ns** |

Two categories:

1. **Structural: PyO3 call dispatch (~5ns)** — PyO3's `tp_call` shim extracts
   GIL tokens, validates and converts argument pointers. `lru_cache` receives raw
   `PyObject*` directly. Inherent to using a safe FFI layer.

2. **Structural: per-shard RwLock (~8ns)** — `parking_lot::RwLock::read()` is
   cheap (~8ns uncontended) and enables true parallel reads across shards.
   `lru_cache` uses a simple C hash table with no concurrency support.

Note: cache hits acquire only a **per-shard read lock** — the SIEVE visited bit
uses `AtomicBool::store(Relaxed)` which requires no lock upgrade. The write lock
is only acquired on cache misses for SIEVE eviction.

### Shared backend vs memory backend

The shared backend hit path takes ~109ns vs the memory backend's ~55ns. The ~54ns delta is irreducible cross-process overhead:

| Operation | Cost | Notes |
|---|---:|---|
| Key serialization (serde fast-path) | ~10ns | Unavoidable for cross-process |
| ahash of key bytes | ~4ns | Deterministic hash (Python's is randomized per-process) |
| Seqlock (read_begin + validate) | ~15ns | Optimistic lock-free read |
| HT lookup in mmap | ~10ns | Slightly slower than hashbrown in heap |
| Value `.to_vec()` copy | ~8ns | Must copy from mmap before seqlock validate |
| Value deserialization (serde) | ~8ns | Unavoidable for cross-process |
| **Total delta** | **~55ns** | |

No Mutex or RwLock on the shared backend's read path — `ShmCache` uses interior mutability with the seqlock providing all necessary synchronization.

## Python 3.13+/3.14 free-threading

Under free-threaded Python (no GIL), `warp_cache`'s architecture pays off:

- **warp_cache improves**: sharded `RwLock` enables true parallel reads across cores
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
| 6. Sharded RwLock (hashbrown) | Per-shard read locks, true parallel reads across shards | ~18M ops/s | 0.56x |
| 7. Shared backend: remove Mutex | `ShmCache` interior mutability, cached hash state + size limits | cleaner arch | — |

---

*Benchmarks: 100K ops per config, Zipf-distributed keys (2000 unique), `time.perf_counter()`. Python 3.13.2, cachetools 7.0.1, moka_py 0.3.0, cachebox 5.2.2. Source: `benchmarks/`*
