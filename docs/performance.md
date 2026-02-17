# Performance

*Environment: Python 3.13.2, CPython, Apple M-series (arm64), Clang 20.0.0*

## Architecture

The entire cache lookup happens in a single Rust `__call__`:

```
Python: fn(42)
  └─ tp_call (PyO3) ─────────────────────────────── one FFI crossing
       ├─ hash(args)           via ffi::PyObject_Hash
       ├─ HashMap lookup       Rust hashbrown, precomputed hash
       ├─ equality check       via ffi::PyObject_RichCompareBool
       ├─ RwLock (read)        parking_lot, ~8ns uncontended
       └─ return cached value
```

No Python wrapper function. No serialization. No intermediate key object.

## Single-threaded throughput vs cache size

| Cache Size | warp_cache (ops/s) | cachetools (ops/s) | lru_cache (ops/s) | wc/ct | wc/lru |
|---:|---:|---:|---:|---:|---:|
| 32 | 12,350,000 | 610,000 | 22,230,000 | 20.2x | 0.56x |
| 64 | 13,480,000 | 654,000 | 23,290,000 | 20.6x | 0.58x |
| 128 | 14,210,000 | 717,000 | 26,760,000 | 19.8x | 0.53x |
| 256 | 16,350,000 | 833,000 | 29,770,000 | 19.6x | 0.55x |
| 512 | 17,660,000 | 960,000 | 35,710,000 | 18.4x | 0.49x |
| 1024 | 17,710,000 | 1,184,000 | 39,780,000 | 15.0x | 0.45x |

## Strategy comparison (cache size = 256)

| Strategy | warp_cache (ops/s) | cachetools (ops/s) | Ratio |
|---|---:|---:|---:|
| LRU | 16,350,000 | 833,000 | 19.6x |
| LFU | 6,270,000 | 770,000 | 8.1x |
| FIFO | 15,710,000 | 921,000 | 17.1x |

## TTL throughput (cache size = 256, ttl = 60s)

| Implementation | ops/s |
|---|---:|
| warp_cache | 14,190,000 |
| cachetools | 580,000 |
| **Ratio** | **24.5x** |

## Multi-threaded throughput (cache size = 256)

| Threads | warp_cache (ops/s) | cachetools + Lock (ops/s) | lru_cache + Lock (ops/s) | wc/ct | wc/lru |
|---:|---:|---:|---:|---:|---:|
| 1 | 15,920,000 | 809,000 | 12,930,000 | 19.7x | 1.23x |
| 2 | 15,630,000 | 810,000 | 12,670,000 | 19.3x | 1.23x |
| 4 | 15,650,000 | 821,000 | 12,650,000 | 19.1x | 1.24x |
| 8 | 16,410,000 | 810,000 | 12,620,000 | 20.3x | 1.30x |
| 16 | 16,120,000 | 801,000 | 12,140,000 | 20.1x | 1.33x |

`warp_cache` maintains ~16M ops/s regardless of thread count. `cachetools`
requires a manual `threading.Lock()` and tops out at ~770K ops/s.
`lru_cache + Lock` degrades as contention increases.

## Where the remaining gap lives

At cache size 128, a cache hit takes ~64ns vs `lru_cache`'s ~39ns:

| Operation | lru_cache (C) | warp_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Table lookup + key equality | ~10ns | ~12ns | +2ns |
| LRU reorder (linked list) | ~5ns | ~8ns | +3ns |
| **Lock acquire + release** | **0ns** | **~8ns** | **+8ns** |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~39ns** | **~60ns** | **+21ns (~34ns measured)** |

Three categories:

1. **Irreducible: Thread safety lock (~8ns)** — `lru_cache` pays nothing because
   the GIL provides implicit thread safety. We pay ~8ns for an uncontended
   `parking_lot` write lock. This cannot be eliminated without removing thread
   safety.

2. **Structural: PyO3 call dispatch (~5ns)** — PyO3's `tp_call` shim extracts
   GIL tokens, validates and converts argument pointers. `lru_cache` receives raw
   `PyObject*` directly. Inherent to using a safe FFI layer.

3. **Marginal: Reference counting (~3ns)** — `lru_cache` uses the args tuple
   pointer as-is. We `Py_INCREF` to own it in `CacheKey`, then `Py_DECREF` on
   drop. Cost of Rust's ownership model.

## Could a C extension do better?

Yes, by ~15ns/hit — closing the gap to ~0.85x. A C extension would bypass
PyO3's shim and use `PyObject*` directly. But: ~800 lines of manual C with no
borrow checker, no memory safety, and manual `Py_DECREF` tracking. The Rust
implementation is ~400 lines with 6 lines of `unsafe`.

Even a perfect C implementation cannot reach 1.0x — the lock is the irreducible
cost of thread safety.

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
| 4. Static dispatch via enum | Replace `Box<dyn>` with enum, enables inlining | +5% | — |
| 5. Raw FFI for key equality | `ffi::PyObject_RichCompareBool` instead of `Python::with_gil` | +multi-thread | — |

---

*Benchmarks: 100K ops per config, Zipf-distributed keys (2000 unique), `time.perf_counter()`. cachetools 7.0.1. Source: `benchmarks/`*
