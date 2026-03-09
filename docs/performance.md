# Performance

*Environment: Python 3.13.2, CPython, Apple M-series (arm64), Clang 20.1.0*

## Architecture

The entire cache lookup happens in a single Rust `__call__`:

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash (raw FFI, no PyO3 wrapper)
       ├─ shard select         hash & shard_mask (power-of-2 bitmask)
       ├─ GilCell::read()      zero-cost under GIL (UnsafeCell)
       ├─ HashMap lookup       hashbrown + passthrough hasher (no re-hash)
       ├─ equality check       via ffi::PyObject_RichCompareBool (borrowed pointer)
       ├─ SIEVE visited=1      AtomicBool store, lock-free
       └─ return cached value
```

No Python wrapper function, no serialization, no key allocation on hits. The lookup uses `BorrowedArgs` (a raw pointer + precomputed hash) via hashbrown's `Equivalent` trait. A `CacheKey` is only created on cache miss when the entry needs to be stored.

## SIEVE eviction

Both the in-process and shared memory backends use **SIEVE** - an eviction algorithm that gets better hit rates than LRU with O(1) cost per operation.

How it works:
- **On hit**: set `visited = 1` - one store, no linked-list reordering, no lock needed.
- **On evict**: a "hand" scans the entry list. Visited entries get a second chance (bit cleared to 0); unvisited entries are evicted.

This is simpler than LRU (no list reordering on every hit) and gets better hit rates on skewed workloads. The key thing: cache hits never need a write lock.

## Single-threaded throughput vs cache size

| Cache Size | warp_cache | lru_cache | cachetools | cachebox | moka_py | wc/ct | wc/lru |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 32 | 16.5M | 21.6M | 611K | 1.1M | 3.2M | 27.0x | 0.76x |
| 64 | 17.3M | 23.0M | 650K | 1.2M | 3.4M | 26.6x | 0.75x |
| 128 | 19.1M | 26.4M | 710K | 1.3M | 3.6M | 26.9x | 0.72x |
| 256 | 20.4M | 31.0M | 826K | 1.5M | 3.9M | 24.7x | 0.66x |
| 512 | 21.8M | 36.0M | 946K | 1.8M | 4.2M | 23.0x | 0.61x |
| 1024 | 22.9M | 40.1M | 1.2M | 2.4M | 4.6M | 19.1x | 0.57x |

## TTL throughput (cache size = 256)

| TTL | warp_cache | cachetools | moka_py | wc/ct |
|---|---:|---:|---:|---:|
| 1ms | 7.3M | 604K | 2.6M | 12.1x |
| 10ms | 7.3M | 527K | 2.7M | 13.9x |
| 100ms | 7.4M | 534K | 2.7M | 13.8x |
| 1s | 7.2M | 523K | 2.7M | 13.8x |
| None | 7.3M | 523K | 2.7M | 13.9x |

TTL adds minimal overhead - the expiry timestamp is checked during the normal read path, with no background eviction thread.

## Multi-threaded throughput (cache size = 256)

| Threads | warp_cache | lru_cache + Lock | cachetools + Lock | cachebox | moka_py | wc/lru |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 20.7M | 12.6M | 767K | 1.5M | 3.7M | 1.64x |
| 2 | 20.7M | 12.3M | 800K | 1.6M | 3.8M | 1.68x |
| 4 | 20.8M | 12.5M | 788K | 1.5M | 3.7M | 1.66x |
| 8 | 20.4M | 12.6M | 793K | 1.5M | 3.7M | 1.62x |
| 16 | 19.5M | 11.9M | 795K | 1.5M | 3.7M | 1.64x |
| 32 | 17.8M | 11.5M | 784K | 1.4M | 3.8M | 1.55x |

`warp_cache` stays at ~18-21M ops/s regardless of thread count, about 1.6-1.7x faster than `lru_cache + Lock` under the GIL. The reason: `GilCell` has no locking overhead (the GIL itself serializes access), while `lru_cache + Lock` pays for a `threading.Lock()` on every call. Under free-threaded Python (no GIL), `warp_cache` switches to per-shard `RwLock` so different threads can read different shards in parallel.

## Shared memory backend

The shared memory backend uses SIEVE with lock-free reads - no Mutex, no RwLock. `ShmCache` uses interior mutability: reads go through the seqlock's optimistic path, and the `visited` bit is set via a direct store. Writes acquire the seqlock's TTAS spinlock internally. Size limit checks use cached struct fields so we don't read the shared memory header on every call.

### Memory vs shared throughput

| Backend | Throughput | Hit Rate | Notes |
|---|---:|---:|---|
| Memory (in-process) | 20.0M ops/s | 71.2% | Sharded hashbrown HashMap + GilCell + passthrough hasher + SIEVE |
| Shared (mmap, single process) | 9.7M ops/s | 73.0% | Seqlock + lock-free reads, no Mutex |

The shared backend reaches about 49% of in-process speed. The gap is serialization (serde fast-path for primitives, pickle fallback), ahash of key bytes, seqlock overhead, and mmap copy - unavoidable costs when you work across processes.

### Multi-process scaling

| Processes | Total Throughput | Per-Process Avg |
|---:|---:|---:|
| 1 | 5.0M ops/s | 5.0M ops/s |
| 2 | 7.8M ops/s | 3.4M ops/s |
| 4 | 8.1M ops/s | 2.0M ops/s |
| 8 | 6.5M ops/s | 1.0M ops/s |

Because reads don't take locks, multi-process scaling works well. Total throughput peaks at 4 processes (8.1M ops/s) - 1.6x the single-process rate. Even with 8 processes on the same mmap'd file, throughput stays at 6.5M ops/s.

For comparison, the previous LRU-based shared backend did 3.1M ops/s at 4 processes and 1.9M ops/s at 8 processes. SIEVE is 2.5x and 3.4x faster respectively, because reads no longer need a write lock.

## SIEVE eviction quality

SIEVE also gets better hit rates than LRU across all workload patterns we tested. These benchmarks compare `warp_cache` (SIEVE) against `functools.lru_cache` (LRU) using 1M requests with Zipf-distributed keys.

Run them yourself: `make bench-sieve` or `python benchmarks/bench_sieve.py --quick`.

### Hit ratio vs cache size

With Zipf alpha=1.0 over 10K unique keys, SIEVE beats LRU at every cache size. The biggest difference is at 10% cache ratio (21.6% fewer misses):

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

Three phases: Zipf over keys 0-999, then keys 1000-1999 (completely new set), then back to 0-999. Cache size = 200. Both algorithms adapt, but SIEVE maintains a consistent advantage:

| Phase | SIEVE | LRU |
|---|---:|---:|
| Phase 1 (keys 0-999) | 75.5% | 69.7% |
| Phase 2 (keys 1000-1999) | 75.6% | 69.9% |
| Phase 3 (return to 0-999) | 75.5% | 69.6% |

*Benchmarks: `benchmarks/bench_sieve.py`, 1M ops, Zipf-distributed keys, seed=42.*

## Where the remaining gap lives

### Memory backend vs lru_cache

At cache size 256, a cache hit takes ~49ns vs `lru_cache`'s ~32ns:

| Operation | lru_cache (C) | warp_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Shard select + lock | ~0ns | ~0ns | 0 |
| Table lookup + key equality | ~5ns | ~5ns | 0 |
| SIEVE visited store | ~0ns | ~1ns | +1ns |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~34ns** | **~51ns** | **+17ns** |

Most of the gap is PyO3 call dispatch (~5ns) - PyO3's `tp_call` shim extracts GIL tokens, validates and converts argument pointers. `lru_cache` receives raw `PyObject*` directly. This is just the cost of using a safe FFI layer.

Three optimizations that made the biggest difference:

1. **GilCell** - Under GIL-enabled Python, `GilCell` (an `UnsafeCell` wrapper) replaces `parking_lot::RwLock`, saving ~8ns per hit. Under free-threaded Python (`#[cfg(Py_GIL_DISABLED)]`), real `RwLock` is used instead.

2. **Borrowed key lookup** - The hit path uses `BorrowedArgs` (raw pointer + precomputed hash) via hashbrown's `Equivalent` trait. No `CacheKey` allocated, no `args.clone()`. `CacheKey` is only created on cache miss.

3. **Passthrough hasher** - `PassthroughHasher` feeds Python's precomputed hash directly to hashbrown, skipping foldhash re-hashing (~1-2ns saved).

On cache hit, we only acquire a GilCell read (no overhead under GIL) or a per-shard read lock (under free-threading). The SIEVE visited bit uses `AtomicBool::store(Relaxed)` which needs no lock upgrade. Write lock is only taken on cache misses for SIEVE eviction.

### Shared backend vs memory backend

The shared backend hit path takes ~103ns vs the memory backend's ~49ns. The ~54ns difference is unavoidable cross-process overhead:

| Operation | Cost | Notes |
|---|---:|---|
| Key serialization (serde fast-path) | ~10ns | Unavoidable for cross-process |
| ahash of key bytes | ~4ns | Deterministic hash (Python's is randomized per-process) |
| Seqlock (read_begin + validate) | ~15ns | Optimistic lock-free read |
| HT lookup in mmap | ~10ns | Slightly slower than hashbrown in heap |
| Value `.to_vec()` copy | ~8ns | Must copy from mmap before seqlock validate |
| Value deserialization (serde) | ~8ns | Unavoidable for cross-process |
| **Total delta** | **~55ns** | |

No Mutex or RwLock on the shared backend's read path - `ShmCache` uses interior mutability, the seqlock handles all the synchronization.

## Python 3.13+/3.14 free-threading

Under free-threaded Python (no GIL):

- **warp_cache**: `#[cfg(Py_GIL_DISABLED)]` switches from `GilCell` to real per-shard `RwLock`, so threads can read different shards in parallel
- **lru_cache**: needs a real lock without the GIL's implicit protection, gets slower
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
| 4. Raw FFI for key equality | `ffi::PyObject_RichCompareBool` instead of `Python::with_gil` | +multi-thread | - |
| 5. SIEVE eviction | Unified eviction for both backends, lock-free reads | +12% hit rate | - |
| 6. Sharded RwLock (hashbrown) | Per-shard read locks, parallel reads across shards | ~18M ops/s | 0.56x |
| 7. Shared backend: remove Mutex | `ShmCache` interior mutability, cached hash state + size limits | cleaner arch | - |
| 8. Passthrough hasher + borrowed keys + GilCell | No re-hash, no alloc on hit, no lock under GIL | ~20M ops/s | 0.66x |

---

*Benchmarks: 100K ops per config, median of 3 rounds, Zipf-distributed keys (2000 unique), `time.perf_counter()`. Python 3.13.2, cachetools 7.0.1, moka_py 0.3.0, cachebox 5.2.2. Source: `benchmarks/`*
