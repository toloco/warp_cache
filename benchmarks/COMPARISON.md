# warp_cache vs lru_cache vs moka_py vs cachebox

A head-to-head comparison of four Python caching libraries, all benchmarked on the same machine, same workload, same measurement methodology.

*Environment: Python 3.13.2, Apple M-series (arm64), Zipf-distributed keys (2000 unique), 100K ops per config, median of 3 rounds, `time.perf_counter()`.*

---

## TL;DR

| | warp_cache | lru_cache | moka_py | cachebox |
|---|---:|---:|---:|---:|
| **Single-thread (cache=256)** | **20.4M ops/s** | **31.0M ops/s** | 3.9M ops/s | 1.5M ops/s |
| **Multi-thread 8T** | **20.4M ops/s** | 12.6M ops/s (+Lock) | 3.7M ops/s | 1.5M ops/s |
| **Sustained (10s)** | **8.6M ops/s** | **10.5M ops/s** | 2.8M ops/s | 1.3M ops/s |
| Shared memory | 9.7M ops/s | N/A | N/A | N/A |
| Implementation | Rust (PyO3) | C (CPython) | Rust (PyO3, moka) | Rust (PyO3) |
| Thread-safe (builtin) | Yes | No | Yes | Yes |
| Eviction | SIEVE | LRU | TinyLFU / LRU | LRU / LFU / FIFO / RR |
| TTL support | Yes | No | Yes (+ TTI) | Yes (TTLCache, VTTLCache) |

`lru_cache` is fastest single-threaded - it's C code inside CPython with no lock overhead. Among thread-safe caches, `warp_cache` is fastest at 20.4M ops/s (5.3x faster than `moka_py`, 14x faster than `cachebox`). With multiple threads, `warp_cache` is about 1.6x faster than `lru_cache + Lock`. All three Rust libraries have builtin thread safety, but performance varies a lot. Only `warp_cache` has cross-process shared memory.

---

## The libraries

| Library | What it is | PyPI |
|---|---|---|
| **[warp_cache](https://github.com/toloco/warp_cache)** | Rust/PyO3 caching decorator with SIEVE eviction and shared memory backend | `pip install warp_cache` |
| **[lru_cache](https://docs.python.org/3/library/functools.html#functools.lru_cache)** | CPython builtin LRU cache decorator, implemented in C. No dependencies, not thread-safe | (builtin) |
| **[moka-py](https://github.com/deliro/moka-py)** | Rust port of Java's Caffeine cache with TinyLFU admission. Offers both decorator and manual cache object APIs | `pip install moka-py` |
| **[cachebox](https://github.com/awolverp/cachebox)** | Rust/PyO3 with 7 cache types (LRU, LFU, FIFO, RR, TTL, VTTL, plain). Dictionary-like API with decorator support | `pip install cachebox` |

---

## Feature matrix

| Feature | warp_cache | lru_cache | moka_py | cachebox |
|---|:---:|:---:|:---:|:---:|
| Implementation | Rust (PyO3) | C (CPython) | Rust (PyO3) | Rust (PyO3) |
| Thread-safe (builtin) | Yes (lock-free reads) | No | Yes | Yes |
| Async support | Yes (auto-detect) | No | Yes (`@cached`) | Yes (`@cached`) |
| Cross-process shared memory | Yes (mmap) | No | No | No |
| TTL support | Yes | No | Yes | Yes |
| TTI (time-to-idle) | No | No | Yes | No |
| Per-entry TTL | No | No | Yes | Yes (VTTLCache) |
| Eviction strategies | SIEVE | LRU | TinyLFU, LRU | LRU, LFU, FIFO, RR |
| Stampede prevention | No | No | Yes (`get_with`) | Yes |
| Eviction listener | No | No | Yes | No |
| Cache statistics | Yes (hits/misses) | Yes (hits/misses) | No | Yes (hits/misses + memory) |
| `cache_clear()` | Yes | Yes | Yes | Yes |
| Manual cache object | No (decorator only) | No (decorator only) | Yes (`Moka(...)`) | Yes (dict-like API) |
| Copy-on-return | No | No | No | Yes (configurable) |
| Decorator API | `@cache()` | `@lru_cache()` | `@cached()` | `@cached(Cache())` |
| Free-threaded Python ready | Yes | No (needs Lock) | Yes | Yes |
| Python versions | 3.10+ | Any | 3.9+ | 3.9+ |

---

## Single-thread performance

Cache hit throughput across different cache sizes, Zipf-distributed keys:

| Cache Size | warp_cache | lru_cache | moka_py | cachebox |
|---:|---:|---:|---:|---:|
| 32 | 16.5M | 21.6M | 3.2M | 1.1M |
| 64 | 17.3M | 23.0M | 3.4M | 1.2M |
| 128 | 19.1M | 26.4M | 3.6M | 1.3M |
| 256 | 20.4M | 31.0M | 3.9M | 1.5M |
| 512 | 21.8M | 36.0M | 4.2M | 1.8M |
| 1024 | 22.9M | 40.1M | 4.6M | 2.4M |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/comparison_st_throughput_dark.svg">
  <img src="results/comparison_st_throughput_light.svg" alt="Single-Thread Throughput">
</picture>

### Why is lru_cache fastest?

`lru_cache` is C code inside CPython. It has no thread-safety overhead (the GIL provides implicit safety), no PyO3 dispatch cost, and no reference counting overhead. An extension module can't match that under the GIL.

### Why is warp_cache 5.3x faster than moka_py?

Both are Rust + PyO3, but `warp_cache` is 5.3x faster. The differences:

1. **Single FFI crossing.** `warp_cache` does the entire lookup - hash, find, equality check, SIEVE visited update, return - in one Rust `__call__`. `moka_py` crosses the FFI boundary multiple times.

2. **SIEVE eviction.** Cache hits just set a `visited` bit (a single-word store). No linked-list reordering, no frequency counter updates on the hot path.

3. **Precomputed hash + raw C equality.** `CacheKey` stores the Python hash once and uses `ffi::PyObject_RichCompareBool` directly - the same raw C call that `lru_cache` uses.

4. **No serialization.** The in-memory backend stores `Py<PyAny>` directly. No copies.

### Why is cachebox slower than moka_py?

Despite both being Rust + PyO3, cachebox's `@cached` decorator adds more Python-level overhead. The LRU linked-list reordering on every hit is also more expensive than moka_py's deferred frequency tracking. cachebox's default `copy_level=1` (copy dict/list/set return values) adds additional overhead that the benchmarks measure.

---

## Multi-thread performance

All thread-safe libraries used directly. `lru_cache` wrapped in `threading.Lock()`.

| Threads | warp_cache | lru_cache + Lock | moka_py | cachebox |
|---:|---:|---:|---:|---:|
| 1 | 20.7M | 12.6M | 3.7M | 1.5M |
| 2 | 20.7M | 12.3M | 3.8M | 1.6M |
| 4 | 20.8M | 12.5M | 3.7M | 1.5M |
| 8 | 20.4M | 12.6M | 3.7M | 1.5M |
| 16 | 19.5M | 11.9M | 3.7M | 1.5M |
| 32 | 17.8M | 11.5M | 3.8M | 1.4M |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/comparison_mt_scaling_dark.svg">
  <img src="results/comparison_mt_scaling_light.svg" alt="Multi-Thread Scaling">
</picture>

Under the GIL, `warp_cache` is about 1.6-1.7x faster than `lru_cache + Lock` across all thread counts. All burst benchmarks report the median of 3 rounds. `GilCell` has no locking overhead (the GIL itself serializes access), while `lru_cache + Lock` pays for a `threading.Lock()` on every call.

Under free-threaded Python (no GIL), `warp_cache` switches to per-shard `RwLock` via `#[cfg(Py_GIL_DISABLED)]`, so threads can read different shards in parallel. `lru_cache` still needs a real lock on every access.

---

## Sustained throughput

10-second sustained benchmark (cache size = 256, Zipf-distributed keys):

| Library | ops/s | vs warp_cache |
|---|---:|---:|
| lru_cache | 10.5M | 1.2x faster |
| **warp_cache** | **8.6M** | **1.0x** |
| moka_py | 2.8M | 3.1x slower |
| cachebox | 1.3M | 6.6x slower |

Sustained throughput is lower than burst because it includes GC pauses, CPU frequency scaling, and cache-line effects over time. Relative ordering stays the same.

---

## TTL throughput

Cache size = 256, various TTL values (10-second sustained per configuration):

| TTL | warp_cache | moka_py | ratio |
|---|---:|---:|---:|
| 1ms | 7.3M | 2.6M | 2.8x |
| 10ms | 7.3M | 2.7M | 2.7x |
| 100ms | 7.4M | 2.7M | 2.7x |
| 1s | 7.2M | 2.7M | 2.7x |
| None | 7.3M | 2.7M | 2.7x |

TTL adds minimal overhead to `warp_cache` - the expiry timestamp is checked inline during the read path. `cachebox` is excluded from TTL benchmarks because its `TTLCache` uses FIFO eviction (not LRU-comparable). `lru_cache` does not support TTL.

---

## Async throughput

Cache hit throughput for `async def` cached functions (cache size = 256, Zipf-distributed keys):

| Mode | warp_cache | moka_py | ratio |
|---|---:|---:|---:|
| Sync | 19.9M | 3.8M | 5.2x |
| Async | 5.8M | 3.2M | 1.8x |

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="results/comparison_async_dark.svg">
  <img src="results/comparison_async_light.svg" alt="Sync vs Async Throughput">
</picture>

Async cache hits are slower than sync because every call creates and resolves a Python coroutine object, even though the actual cache lookup is synchronous Rust code. `warp_cache` async hits are still 1.8x faster than `moka_py` async. The overhead comes from CPython's coroutine machinery, not the cache itself - `AsyncCachedFunction` calls Rust `get()` synchronously and only `await`s on cache miss.

---

## Cross-process shared memory

`warp_cache` is the only library in this comparison that supports cross-process caching via mmap'd shared memory.

| Backend | Throughput | Hit Rate |
|---|---:|---:|
| Memory (in-process) | 20.0M ops/s | 71.2% |
| Shared (mmap, single process) | 9.7M ops/s | 73.0% |
| Shared (mmap, 4 processes) | 8.1M ops/s total | - |
| Shared (mmap, 8 processes) | 6.5M ops/s total | - |

The shared backend gets about 49% of in-process speed with no Mutex on the read path. The gap is unavoidable cross-process overhead: serialization, deterministic hashing, seqlock, and mmap copy.

For reference, Redis over localhost does ~100-500K ops/s. Shared memory avoids the network round-trip entirely and needs no external services.

---

## Architecture deep dive

### Why warp_cache is fast

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash (raw FFI)
       ├─ shard select         hash & shard_mask (power-of-2 bitmask)
       ├─ GilCell::read()      zero-cost under GIL (UnsafeCell)
       ├─ HashMap lookup       hashbrown + passthrough hasher (no re-hash)
       ├─ equality check       via ffi::PyObject_RichCompareBool (borrowed ptr)
       ├─ visited.store(true)  AtomicBool, lock-free
       └─ return cached value
```

1. **Single FFI crossing** - the entire lookup happens in Rust's `__call__`. No Python wrapper, no intermediate objects.
2. **No allocation on hits** - lookups use `BorrowedArgs` (raw pointer + precomputed hash) via hashbrown's `Equivalent` trait. `CacheKey` is only created on cache miss.
3. **GilCell** - under GIL-enabled Python, `GilCell` skips real locking (~0ns vs ~8ns for `RwLock`). Under free-threaded Python, per-shard `RwLock` is used instead.
4. **Passthrough hasher** - Python already computes hashes, so we feed them directly to hashbrown instead of re-hashing.
5. **Fat LTO + codegen-units=1** - LTO inlines PyO3's FFI wrappers into the hot path.

### How moka_py works

`moka_py` wraps Rust's `moka` crate (inspired by Java's Caffeine). It uses W-TinyLFU - a window + main cache with frequency sketches for admission filtering. Good hit rates, but more bookkeeping per access. The Python `@cached` decorator crosses the FFI boundary for both key hashing and value retrieval.

### How cachebox works

`cachebox` implements 7 different cache types in Rust using Google's SwissTable (`hashbrown`). The `@cached` decorator wraps a cache object instance. It defaults to copying dict/list/set return values (`copy_level=1`) to prevent mutation of cached data - a safety feature that adds overhead. Its thread safety uses internal locks.

### How lru_cache works

`lru_cache` is C code compiled into CPython. It uses the GIL for implicit thread safety (no lock overhead). The cache is a doubly-linked list over a C hash table - very simple, very fast. Under free-threaded Python, it needs an external `threading.Lock()`.

---

## When to use each

| Use case | Recommendation |
|---|---|
| Single-threaded, maximum speed | **lru_cache** - C code in CPython, nothing is faster |
| Thread-safe, high throughput | **warp_cache** - fastest thread-safe option (5.3x over moka_py) |
| Cross-process (Gunicorn, Celery) | **warp_cache** - only option with shared memory |
| Per-entry TTL with stampede prevention | **cachebox** (VTTLCache) or **moka_py** (`get_with`) |
| Time-to-idle (TTI) expiration | **moka_py** - only option with TTI |
| Manual cache object API (no decorator) | **moka_py** (`Moka(...)`) or **cachebox** (dict-like) |
| Async with concurrent dedup | **moka_py** (`wait_concurrent=True`) |
| Free-threaded Python (no GIL) | **warp_cache**, **moka_py**, or **cachebox** - all three are ready |

---

## Methodology

**Machine:** Apple M-series (arm64), macOS

**Python:** 3.13.2 (CPython, GIL enabled)

**Workload:** Zipf-distributed keys (alpha=1.0) over 2000 unique values, producing ~72% cache hit rate at maxsize=256. This models realistic access patterns where some keys are much hotter than others.

**Thread safety wrapping:** `lru_cache` is not thread-safe, so multi-threaded benchmarks wrap it in `threading.Lock()`. `warp_cache`, `moka_py`, and `cachebox` are used directly (builtin thread safety).

**Timing:** `time.perf_counter()` with 100K operations per burst configuration, median of 3 rounds. Sustained benchmarks run for 10 seconds (single run - the 10s integration already averages out variance).

**Library versions:** warp_cache 0.1.0, moka_py 0.3.0, cachebox 5.2.2

**Source data:** `benchmarks/results/bench_default.json`

**Benchmark runner:** `benchmarks/_bench_runner.py`

---

*All benchmarks run on the same machine, same workload, same measurement methodology. See `benchmarks/` for full source and raw results.*
