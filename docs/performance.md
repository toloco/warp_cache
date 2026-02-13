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
       ├─ RwLock (write)       parking_lot, ~8ns uncontended
       └─ return cached value
```

No Python wrapper function. No serialization. No intermediate key object.

## Single-threaded throughput vs cache size

| Cache Size | fast_cache (ops/s) | cachetools (ops/s) | lru_cache (ops/s) | fc/ct | fc/lru |
|---:|---:|---:|---:|---:|---:|
| 32 | 14,110,000 | 597,000 | 20,660,000 | 23.6x | 0.68x |
| 64 | 14,750,000 | 652,000 | 23,870,000 | 22.6x | 0.62x |
| 128 | 15,740,000 | 718,000 | 25,520,000 | 21.9x | 0.62x |
| 256 | 17,370,000 | 821,000 | 30,490,000 | 21.2x | 0.57x |
| 512 | 19,460,000 | 945,000 | 36,460,000 | 20.6x | 0.53x |
| 1024 | 20,490,000 | 1,150,000 | 40,110,000 | 17.8x | 0.51x |

## Strategy comparison (cache size = 256)

| Strategy | fast_cache (ops/s) | cachetools (ops/s) | Ratio |
|---|---:|---:|---:|
| LRU | 17,380,000 | 812,000 | 21.4x |
| LFU | 6,660,000 | 750,000 | 8.9x |
| FIFO | 16,700,000 | 898,000 | 18.6x |

## TTL throughput (cache size = 256, ttl = 60s)

| Implementation | ops/s |
|---|---:|
| fast_cache | 15,080,000 |
| cachetools | 566,000 |
| **Ratio** | **26.7x** |

## Multi-threaded throughput (cache size = 256)

| Threads | fast_cache (ops/s) | cachetools + Lock (ops/s) | lru_cache + Lock (ops/s) | fc/ct | fc/lru |
|---:|---:|---:|---:|---:|---:|
| 1 | 17,610,000 | 757,000 | 12,030,000 | 23.3x | 1.46x |
| 2 | 17,540,000 | 772,000 | 12,250,000 | 22.7x | 1.43x |
| 4 | 17,600,000 | 774,000 | 12,130,000 | 22.8x | 1.45x |
| 8 | 17,340,000 | 767,000 | 12,020,000 | 22.6x | 1.44x |
| 16 | 17,450,000 | 770,000 | 11,690,000 | 22.7x | 1.49x |

`fast_cache` maintains ~18M ops/s regardless of thread count. `cachetools`
requires a manual `threading.Lock()` and tops out at ~770K ops/s.
`lru_cache + Lock` degrades as contention increases.

## Where the remaining gap lives

At cache size 128, a cache hit takes ~64ns vs `lru_cache`'s ~39ns:

| Operation | lru_cache (C) | fast_cache (Rust) | Delta |
|---|---:|---:|---:|
| Call dispatch (`tp_call`) | ~5ns | ~10ns | +5ns |
| Hash args (`PyObject_Hash`) | ~15ns | ~15ns | 0 |
| Table lookup + key equality | ~10ns | ~12ns | +2ns |
| LRU reorder (linked list) | ~5ns | ~8ns | +3ns |
| **Lock acquire + release** | **0ns** | **~8ns** | **+8ns** |
| Refcount management | ~2ns | ~5ns | +3ns |
| Return value | ~2ns | ~2ns | 0 |
| **Total** | **~39ns** | **~60ns** | **+21ns** |

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

Under free-threaded Python (no GIL), `fast_cache`'s architecture pays off:

- **fast_cache improves**: `RwLock` enables true parallel reads across cores
- **lru_cache gets worse**: needs a real lock without the GIL's implicit protection
- **Trade-off**: atomic refcounting adds ~2-5ns to single-threaded cost

The benchmark notebook (`benchmarks/bench.ipynb`) automatically creates
temporary uv venvs for each Python version, builds fast_cache via maturin, runs
all benchmarks, and generates cross-version comparison plots inline.

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
