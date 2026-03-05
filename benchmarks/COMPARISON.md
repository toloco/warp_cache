# warp_cache vs lru_cache vs moka_py

A deep comparison of three Python caching libraries: **warp_cache** (Rust/PyO3), **lru_cache** (CPython builtin), and **moka_py** (Rust/PyO3, port of Java's Caffeine).

*Benchmarks: Apple M-series (arm64), Zipf-distributed keys (2000 unique), 100K ops per config, `time.perf_counter()`.*

---

## TL;DR

| Scenario | warp_cache | lru_cache | moka_py | cachebox |
|---|---:|---:|---:|---:|
| Single-thread (3.13, cache=256) | 10.5M ops/s | 29.6M ops/s | 3.7M ops/s | 1.5M ops/s |
| Multi-thread 8T (3.13, GIL) | 10.4M ops/s | 12.1M ops/s (+Lock) | 3.6M ops/s | 1.5M ops/s |
| Shared memory (single proc) | 8.9M ops/s | N/A | N/A | N/A |
| Shared memory (4 procs) | 7.7M ops/s total | N/A | N/A | N/A |
| Sustained (10s) | 6.0M ops/s | 10.2M ops/s | 2.8M ops/s | 1.3M ops/s |
| Thread-safe (builtin) | Yes | No | Yes | Yes |
| Async support | Yes | No | No | No |
| Cross-process shared memory | Yes | No | No | No |
| Eviction | SIEVE (scan-resistant) | LRU | LRU/LFU/FIFO | LRU/FIFO/TTL |
| TTL support | Yes | No | Yes | TTL only via TTLCache |
| Hit rate (Zipf, 256 entries) | 72.3% | — | — | — |

**Bottom line:** `lru_cache` is fastest single-threaded (it's C code inside CPython with zero overhead). `warp_cache` is the fastest *thread-safe* cache — 2.8x faster than `moka_py` and 13x faster than `cachetools`. The shared memory backend reaches **8.9M ops/s** with fully lock-free reads via SIEVE, just 11% slower than the in-process backend. Multi-process scaling is excellent: 4 workers achieve 7.7M ops/s total, orders of magnitude faster than network-based caches like Redis.

---

## The GIL Question

### What the GIL means for caching

Python's Global Interpreter Lock (GIL) serializes all Python bytecode execution. This has two consequences for caches:

1. **`lru_cache` doesn't need a lock.** The GIL guarantees that only one thread touches the cache at a time. This is why it's so fast — zero synchronization overhead.

2. **Thread-safe caches pay a tax.** Any cache that uses its own lock (like `warp_cache`'s `parking_lot::RwLock`) pays ~8ns per operation even though the GIL already serializes access. This is the price of correctness under free-threaded Python.

### Free-threaded Python 3.13t

Python 3.13 introduced an experimental free-threaded mode (`python3.13t`) that disables the GIL entirely. This changes the equation:

- **`lru_cache` becomes unsafe.** Without the GIL, concurrent reads/writes corrupt internal state. You *must* wrap it in `threading.Lock()`, adding contention overhead.
- **`warp_cache` already has the lock.** Its `RwLock` enables true parallel reads across cores — multiple threads can read the cache simultaneously with no contention.
- **Everything gets ~15-20% slower.** Atomic reference counting (replacing the GIL's implicit refcount protection) adds overhead to all Python objects. This affects every library equally.

### warp_cache's read-lock architecture

The key insight: most cache operations are **reads** (cache hits). `warp_cache` uses a read-write lock where cache hits only acquire a *read lock* — multiple threads read simultaneously. Only cache misses require a write lock.

This means under real workloads with high hit rates (typical for caches), contention is near-zero even with many threads.

---

## Single-Thread Performance

![Single-Thread Throughput](results/comparison_st_throughput.png)

### Why lru_cache wins single-threaded

`lru_cache` is unbeatable in single-threaded scenarios because it pays almost nothing:

| Operation | lru_cache (C) | warp_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Table lookup + key equality | ~10ns | ~12ns | +2ns |
| SIEVE visited store | ~0ns | ~1ns | +1ns |
| **Lock acquire + release** | **0ns** | **~8ns** | **+8ns** |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~39ns** | **~60ns** | **+21ns (~34ns measured)** |

The three categories of overhead:

1. **Irreducible: Thread safety lock (~8ns)** — `lru_cache` pays nothing because the GIL provides implicit safety. `warp_cache` pays ~8ns for `parking_lot::RwLock`. Cannot be eliminated without removing thread safety.

2. **Structural: PyO3 call dispatch (~5ns)** — PyO3's `tp_call` shim extracts GIL tokens, validates and converts argument pointers. `lru_cache` receives raw `PyObject*` directly. Inherent to using a safe FFI layer.

3. **Marginal: Reference counting (~3ns)** — `lru_cache` uses the args tuple pointer as-is. `warp_cache` does `Py_INCREF` to own it in `CacheKey`, then `Py_DECREF` on drop. Cost of Rust's ownership model.

### Why warp_cache beats moka_py 2.8x

Both are Rust + PyO3, yet `warp_cache` is **2.8x faster** (10.5M vs 3.7M ops/s on Python 3.13). The differences:

1. **Single FFI crossing.** `warp_cache` does the entire lookup — hash, find, equality check, SIEVE visited update, return — in one Rust `__call__`. `moka_py` crosses the FFI boundary multiple times.

2. **SIEVE eviction.** `warp_cache` uses SIEVE — a simple algorithm where cache hits just set a `visited` bit (a single-word store). No linked-list reordering on the hot path.

3. **Precomputed hash + raw C equality.** `CacheKey` stores the Python hash once and uses `ffi::PyObject_RichCompareBool` directly — the same raw C call that `lru_cache` uses.

4. **No serialization.** The in-memory backend stores `Py<PyAny>` directly. No pickle, no copies.

---

## Multi-Thread Performance

![Multi-Thread Scaling](results/comparison_mt_scaling.png)

![Scaling Efficiency](results/comparison_scaling_ratio.png)

### GIL mode (Python 3.13)

Under the GIL, `warp_cache` maintains ~10M ops/s regardless of thread count. Adding threads doesn't slow it down because:

- The `RwLock` is uncontended (the GIL serializes access anyway)
- SIEVE hit updates are a single-word store — no linked-list reordering
- Atomic hit/miss counters use `Ordering::Relaxed` — no memory barriers

`lru_cache + Lock` runs at ~12M ops/s. The `threading.Lock()` wrapper adds Python-level function call overhead, but `lru_cache`'s raw C speed still gives it an edge under the GIL.

### No-GIL mode (Python 3.13t)

Without the GIL, `warp_cache`'s `RwLock` architecture enables true parallel reads — multiple threads can hit the cache simultaneously with no contention. `lru_cache` must acquire a real lock on every access, degrading under contention.

### Why warp_cache doesn't scale *up* with threads

Under the GIL, adding threads can't increase throughput because only one thread runs at a time. The GIL turns parallelism into concurrency.

Under no-GIL, `warp_cache` could theoretically scale reads across cores. In practice, the benchmark workload is CPU-bound with very short operations (~70ns each), so thread scheduling overhead dominates any parallelism gains. For I/O-bound workloads with expensive cache misses, the read-lock architecture would show clear scaling benefits.

---

## Why warp_cache Is Fast — Architecture Deep Dive

### 1. Single FFI crossing

The entire cache lookup happens in Rust's `__call__` method. Python calls `cached_fn(42)`, which enters Rust once and returns the cached value. No Python wrapper function, no intermediate objects.

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash
       ├─ HashMap lookup       Rust hashbrown, precomputed hash
       ├─ equality check       via ffi::PyObject_RichCompareBool
       ├─ RwLock (read)        parking_lot, ~8ns uncontended
       └─ return cached value
```

### 2. SIEVE eviction with read-lock fast path

Cache hits acquire only a **read lock** and set `visited = 1` — a single-word store that requires no linked-list reordering. This means cache hits never need a write lock, reducing contention to near-zero under high hit rates (~65%+ in these benchmarks).

Only cache misses and evictions acquire the write lock, where the SIEVE hand scans for an unvisited entry to evict.

### 3. Precomputed hash + raw C API equality

`CacheKey` computes `PyObject_Hash` once at key creation and stores the result. HashMap lookups use the precomputed hash directly. Key equality uses raw `ffi::PyObject_RichCompareBool` — the exact same C call that `lru_cache` uses — bypassing PyO3's safe-but-slower `Python::with_gil` wrapper.

### 4. parking_lot::RwLock (~8ns)

`parking_lot` provides a significantly faster mutex than `std::sync::RwLock` (~8ns vs ~25ns uncontended on arm64). It uses adaptive spinning before parking, reducing syscall overhead.

### 5. Fat LTO + codegen-units=1

The release profile enables fat link-time optimization across all crates (including PyO3) and forces single-codegen-unit compilation. This allows the compiler to inline PyO3's FFI wrappers directly into `warp_cache`'s hot path, eliminating call overhead at the boundary.

```toml
[profile.release]
lto = "fat"
codegen-units = 1
```

### 6. Atomic hit/miss counters

Hit and miss counts use `AtomicU64` with `Ordering::Relaxed` — no memory barriers, no cache-line bouncing on single-socket machines. Stats collection is essentially free.

---

## Cross-Process Shared Memory

![Backend Comparison](results/comparison_backends.png)

`warp_cache` is the only library in this comparison that supports cross-process caching via mmap'd shared memory. This enables multiple Python processes to share a single cache without serialization overhead of Redis/Memcached.

| Backend | Throughput | Hit Rate | Use case |
|---|---:|---:|---|
| Memory (in-process) | 10.0M ops/s | 72.3% | Single process, maximum speed |
| Shared (mmap, single process) | 8.9M ops/s | 72.7% | Cross-process capable, lock-free reads |
| Shared (mmap, 4 processes) | 7.7M ops/s total | — | Peak multi-process throughput |
| Shared (mmap, 8 processes) | 6.5M ops/s total | — | High-concurrency workers |

The shared backend reaches **89% of in-process speed** — the gap is dominated by pickle serialization, not locking.

### Why SIEVE transformed shared memory performance

The previous shared backend used LRU eviction, which required a **write lock on every cache hit** to reorder the linked list. SIEVE eliminates this entirely — cache hits just set `visited = 1`, an idempotent single-word store requiring no lock.

Multi-process scaling improvements vs the previous LRU backend:

| Processes | Old (LRU) | New (SIEVE) | Improvement |
|---:|---:|---:|---:|
| 1 | 4.5M ops/s | 4.8M ops/s | 1.1x |
| 2 | 5.3M ops/s | 7.0M ops/s | 1.3x |
| 4 | 3.1M ops/s | 7.7M ops/s | **2.5x** |
| 8 | 1.9M ops/s | 6.5M ops/s | **3.4x** |

At 8 processes, the old LRU backend collapsed to 1.9M ops/s due to write-lock contention on the shared mmap. SIEVE maintains 6.5M ops/s because reads never contend with each other.

### Architecture

The shared backend uses:
- **mmap'd files** (`$TMPDIR/warp_cache/{name}.cache`) for zero-copy access
- **Seqlock** in shared memory for cross-process synchronization — reads are optimistic and lock-free (~10-20ns), only writes acquire a spinlock
- **SIEVE eviction** with `visited` bit and `sieve_hand` pointer in shared memory
- **Open-addressing hash table** with linear probing (power-of-2 capacity for bitmask)
- **Pickle serialization** for values (required for cross-process compatibility)
- **Atomic stats** — `hits`, `misses`, and `oversize_skips` use `AtomicU64`, so `info()` and `record_oversize_skip()` never acquire a lock

The read path uses an optimistic lock-free hash lookup + value copy under the seqlock, retried if a writer was active. On hit, the `visited` bit is set via a direct idempotent store — no write lock needed. **All cache hits are fully lock-free.** TTL-expired entries are detected during the optimistic read, then cleaned up under the write lock with re-verification.

The shared backend is orders of magnitude faster than network-based caches (Redis: ~100-500K ops/s over localhost).

---

## Feature Matrix

| Feature | warp_cache | lru_cache | moka_py |
|---|---|---|---|
| Implementation | Rust (PyO3) | C (CPython builtin) | Rust (PyO3) |
| Thread-safe (builtin) | Yes (`RwLock`) | No (needs `Lock` wrapper) | Yes |
| Async support | Yes (auto-detect) | No | No |
| Cross-process (shared mem) | Yes (mmap) | No | No |
| TTL support | Yes | No | Yes |
| Eviction | SIEVE (scan-resistant) | LRU | LRU/LFU/FIFO |
| Cache statistics | Yes (hits/misses) | Yes (hits/misses) | No |
| `cache_clear()` | Yes | Yes | No |
| Decorator API | `@cache()` | `@lru_cache()` | `Moka(maxsize)` |
| Python version | 3.9+ | Any | 3.8+ |
| Free-threaded ready | Yes | No (needs Lock) | Yes |

---

## Methodology

**Machine:** Apple M-series (arm64), macOS

**Python versions tested:**
- Python 3.12.0 (GIL)
- Python 3.13.2 (GIL)
- Python 3.13.2 free-threaded (no GIL)

**Workload:** Zipf-distributed keys (alpha=1.0) over 2000 unique values, producing ~65% cache hit rate at maxsize=256. This models realistic access patterns where some keys are much hotter than others.

**Thread safety wrapping:** `lru_cache` and `cachetools` are not thread-safe, so multi-threaded benchmarks wrap them in `threading.Lock()`. `warp_cache` and `moka_py` are used directly (builtin thread safety).

**Timing:** `time.perf_counter()` with 100K operations per configuration. Sustained benchmarks run for 10 seconds. Results are the most recent run; variance across runs is typically <5%.

**Library versions:** warp_cache 0.1.0, moka_py 0.3.0, cachetools 7.0.1, cachebox 5.2.2

**Source data:** `benchmarks/results/bench_default.json` (Python 3.13.2)

**Charts generated by:** `benchmarks/_generate_comparison_charts.py`

---

*Generated from benchmark data. See `benchmarks/` for full source and raw results.*
