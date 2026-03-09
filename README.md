# warp_cache

A thread-safe Python caching decorator built in Rust. Uses SIEVE eviction and GIL-conditional locking - no overhead under the GIL, per-shard RwLock when the GIL is disabled.

## Why this exists

I needed single `@cache()` decorator that works across sync functions, async functions, and threads - with TTL - without juggling separate solutions. Existing options each had gaps:

- **`functools.lru_cache`** - not thread-safe (needs manual `Lock`), no TTL, no async awareness
- **`cachetools`** - has TTL and multiple strategies, but pure Python (slow) and not thread-safe
- **`cachebox` / `moka-py`** - Rust-backed and thread-safe, but weren't designed for GIL-conditional locking or shared memory

warp_cache fills this gap: one decorator, thread-safe by default, async-aware (cache hits return without awaiting), TTL support, and shared memory backend for cross-process use.

With free-threaded Python (3.13+) removing the GIL, thread-safe caching stops being optional. warp_cache handles both cases via conditional compilation (`GilCell` under GIL, `RwLock` under no-GIL).

## Quick start

```python
from warp_cache import cache

@cache()
def expensive(x, y):
    return x + y

expensive(1, 2)  # computes and caches
expensive(1, 2)  # returns cached result
```

If you're already using `functools.lru_cache`, switching is one-line change. Unlike `lru_cache`, this is thread-safe out of the box:

```python
-from functools import lru_cache
+from warp_cache import cache

-@lru_cache(maxsize=128)
+@cache(max_size=128)
 def expensive(x, y):
     return x + y
```

Like `lru_cache`, all arguments must be hashable. See the [usage guide](docs/usage.md#basic-caching) for details.

## How it works

Entire cache lookup happens in single Rust `__call__` - no Python wrapper function, no serialization, no key allocation on hits:

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

On cache hit, the lookup uses `BorrowedArgs` (a raw pointer + precomputed hash) via hashbrown's `Equivalent` trait, so there is no `CacheKey` allocation and no refcount churn. `CacheKey` is only created on miss when the entry needs to be stored.

Two backends:

- **Memory** (default) - sharded `hashbrown::HashMap` with passthrough hasher and `GilCell`/`RwLock` locking. Everything stays in-process.
- **Shared** (`backend="shared"`) - mmap'd shared memory with seqlock. Reads don't take any locks (optimistic seqlock path). Serialization uses a fast-path for primitives (serde), with pickle fallback for complex types.

## Why SIEVE

Both backends use [SIEVE](https://junchengyang.com/publication/nsdi24-SIEVE.pdf) (NSDI'24) for eviction. The main reason: cache hits don't need a write lock.

LRU requires reordering a linked list on every hit - that means write lock (or CAS loop) on every read. SIEVE replaces this with a single store (`visited = 1`), which needs no lock on either backend. On eviction, a "hand" scans the entry list: visited entries get a second chance (bit cleared), unvisited entries get evicted.

Why not the others:
- **LRU** - write lock on every hit, which defeats the point of GilCell
- **ARC** - two lists + ghost entries, much more complex for small gains
- **TinyLFU** - frequency counting overhead, bloom filter maintenance

The hit rate is also better than LRU. Measured with Zipf-distributed keys (1M requests, `benchmarks/bench_sieve.py`):

| Workload | SIEVE | LRU | Miss Reduction |
|---|---:|---:|---:|
| Zipf, 10% cache | 74.5% | 67.5% | +21.6% |
| Scan resistance (70% hot) | 69.9% | 63.5% | +17.6% |
| One-hit wonders (25% unique) | 53.9% | 43.7% | +18.1% |
| Working set shift | 75.5% | 69.7% | +16.6% |

See [eviction quality benchmarks](docs/performance.md#sieve-eviction-quality) for the full breakdown.

## Performance

*Python 3.13.2, Apple M-series (arm64), cache size 256, Zipf-distributed keys (2000 unique), median of 3 rounds. Source: `benchmarks/`*

If you need thread-safe caching, warp_cache is the fastest option available. If you don't need thread safety, `lru_cache` is faster - it's C code in CPython with no FFI boundary to cross.

### Single-threaded

| Library | ops/s | Notes |
|---|---:|---|
| lru_cache | 31.0M | C code, no FFI, no safety overhead |
| warp_cache | 20.4M | Rust via PyO3, thread-safe, SIEVE |
| moka_py | 3.9M | Rust (moka), thread-safe |
| cachebox | 1.5M | Rust, thread-safe |
| cachetools | 826K | Pure Python, not thread-safe |

`lru_cache` wins by about 1.5x single-threaded. The gap comes from PyO3 call dispatch (~5ns) and refcount management (~3ns) - the price you pay for crossing an FFI boundary with a safe wrapper.

### Multi-threaded (GIL-enabled)

| Threads | warp_cache | lru_cache + Lock | cachetools + Lock |
|---:|---:|---:|---:|
| 1 | 20.7M | 12.6M | 767K |
| 4 | 20.8M | 12.5M | 788K |
| 8 | 20.4M | 12.6M | 793K |
| 16 | 19.5M | 11.9M | 795K |

With multiple threads, warp_cache is about 1.6x faster than `lru_cache + Lock`. `lru_cache` itself isn't thread-safe, so real multi-threaded code needs a `threading.Lock()` on every call, and that lock overhead adds up. warp_cache's `GilCell` has no overhead under the GIL because the GIL itself already serializes access.

### Shared memory (cross-process)

| Processes | Total Throughput | Per-Process Avg |
|---:|---:|---:|
| 1 | 5.0M ops/s | 5.0M ops/s |
| 2 | 7.8M ops/s | 3.4M ops/s |
| 4 | 8.1M ops/s | 2.0M ops/s |

The seqlock's optimistic read path means reads don't take locks, which is why multi-process scaling works. No other Python cache library has cross-process shared memory.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="benchmarks/results/comparison_mt_scaling_dark.svg">
  <img src="benchmarks/results/comparison_mt_scaling_light.svg" alt="Multi-thread scaling: GIL vs no-GIL">
</picture>

Full benchmarks and optimization history in [docs/performance.md](docs/performance.md).

## Installation

Prebuilt wheels are available for Linux (x86_64, aarch64), macOS (x86_64, arm64), and Windows (x86_64):

```bash
pip install warp_cache
```

If no wheel is available for your platform, pip will fall back to the source distribution (requires a [Rust toolchain](https://rustup.rs/)).

## Design notes

**Why single FFI crossing?** Early versions had Python wrapper around Rust cache - two FFI crossings per call. Moving `__call__` into Rust saves ~10-15ns by cutting out the wrapper.

**Why `BorrowedArgs`?** On cache hit we only need to *look up* a key, not store it. `BorrowedArgs` holds a raw pointer + precomputed hash and implements hashbrown's `Equivalent` trait, so there is no allocation on the hot path. `CacheKey` (which owns the PyObject) is only created on miss.

**Why `PassthroughHasher`?** Python already computes hashes for all hashable objects. Feeding that through hashbrown's default foldhash would just re-hash something that's already hashed. `PassthroughHasher` passes it through as-is, saves ~1-2ns per lookup.

**Why `GilCell` instead of `RwLock`?** When the GIL is enabled, it already serializes access, so a real `RwLock` would just waste ~8ns per hit doing nothing useful. `GilCell` is an `UnsafeCell` wrapper with the same API as `RwLock` but no actual locking. Under free-threaded Python (`#[cfg(Py_GIL_DISABLED)]`), real per-shard `RwLock` is compiled in instead.

**Why sharded HashMap?** Under free-threaded Python, per-shard `RwLock` lets different threads read from different shards in parallel. Shard count is power-of-2 (selected via `hash & shard_mask`) for bitmask indexing.

**Why seqlock for shared memory?** Cross-process synchronization can't use futexes portably. The seqlock does optimistic reads (just check a sequence counter) with a TTAS spinlock for writes - all in userspace, no kernel calls on the read side.

## Status

- **Core API is stable.** Prebuilt wheels on PyPI, Python 3.9-3.14.
- **Free-threading codepath** (`#[cfg(Py_GIL_DISABLED)]`) is tested but gets less real-world usage than the GIL-enabled path.
- **Shared memory layout** is v3, stable. Existing shared caches survive process restarts.

When warp_cache makes sense:
- You need thread-safe caching (especially with free-threaded Python)
- You need TTL, async support, or cross-process shared memory
- You want better eviction than LRU without configuring anything

When to use something else:
- **Maximum single-threaded speed, no thread safety needed** - use `functools.lru_cache`
- **Stampede prevention or per-entry TTL** - use `cachebox` or `moka-py`
- **Manual cache object API** (dict-like interface) - use `moka-py` or `cachebox`

## Documentation

- **[Usage guide](docs/usage.md)** - SIEVE eviction, async, TTL, shared memory, decorator parameters
- **[Performance](docs/performance.md)** - benchmarks, architecture details, optimization history
- **[Alternatives](docs/alternatives.md)** - comparison with cachebox, moka-py, cachetools, lru_cache
- **[Examples](examples/)** - runnable scripts for every feature (`uv run examples/<name>.py`)
- **[llms.txt](llms.txt)** / **[llms-full.txt](llms-full.txt)** - project info for LLMs and AI agents ([spec](https://llmstxt.org/))

## Contributing

Contributions welcome! See **[CONTRIBUTING.md](CONTRIBUTING.md)** for setup instructions, coding standards, and PR guidelines.

For security issues, please see **[SECURITY.md](SECURITY.md)**.

## License

MIT
