# Usage Guide

## Basic caching

```python
from fast_cache import cache

@cache()
def expensive(x, y):
    # ... slow computation ...
    return x + y

expensive(1, 2)  # computes and caches
expensive(1, 2)  # returns cached result
```

**Arguments must be hashable.** Like `functools.lru_cache`, `fast_cache` uses
`hash()` to build cache keys. Passing unhashable types raises `TypeError`:

```python
@cache()
def process(data):
    return sum(data)

process((1, 2, 3))    # ok — tuples are hashable
process("hello")       # ok — strings are hashable
process([1, 2, 3])    # TypeError — lists are not hashable
process({"a": 1})     # TypeError — dicts are not hashable
```

If you need to cache a function that takes unhashable arguments, convert them
to hashable equivalents before passing (e.g. `tuple(my_list)`,
`tuple(sorted(my_dict.items()))`).

## Eviction strategies

The `Strategy` enum controls how entries are evicted when the cache is full:

```python
from fast_cache import cache, Strategy

@cache(strategy=Strategy.LRU, max_size=256)
def fetch(url):
    ...

@cache(strategy=Strategy.LFU, max_size=1000)
def lookup(key):
    ...
```

| Strategy | Value | Evicts | Best for |
|----------|-------|--------|----------|
| `Strategy.LRU` | `0` | Least recently used (default) | General-purpose caching |
| `Strategy.MRU` | `1` | Most recently used | Scans where old items are re-accessed |
| `Strategy.FIFO` | `2` | Oldest insertion | Simple age-based rotation |
| `Strategy.LFU` | `3` | Least frequently used | Skewed access patterns with hot keys |

`Strategy` is an `IntEnum`, so you can also pass the integer value directly (e.g. `strategy=0` for LRU).

## Async functions

Async functions are detected automatically at decoration time — no special
syntax needed. Cache lookups still go through the fast Rust path; only cache
misses `await` the wrapped coroutine.

```python
import asyncio
from fast_cache import cache

@cache(max_size=256)
async def fetch_user(user_id: int) -> dict:
    # ... slow I/O ...
    return {"id": user_id}

async def main():
    user = await fetch_user(42)   # miss — awaits the coroutine
    user = await fetch_user(42)   # hit — returns cached result instantly

asyncio.run(main())
```

## TTL (time-to-live)

```python
@cache(max_size=128, ttl=60.0)  # entries expire after 60 seconds
def get_config(name):
    ...
```

## Backends

The `Backend` enum selects where cached data is stored. `Backend` is an `IntEnum`, but the decorator also accepts the strings `"memory"` and `"shared"` for convenience.

```python
from fast_cache import cache, Backend

@cache(max_size=256, backend=Backend.MEMORY)   # enum
@cache(max_size=256, backend="memory")          # equivalent string
```

| Backend | Value | Storage | Use case |
|---------|-------|---------|----------|
| `Backend.MEMORY` | `0` | In-process (default) | Single-process applications |
| `Backend.SHARED` | `1` | Memory-mapped file | Cross-process sharing via mmap |

### Memory backend (default)

The memory backend keeps all cached data in the process's own heap. Keys are stored as live Python objects (no serialization), and lookups go through a single Rust `__call__` — hash, lookup, equality check, and return all happen in one FFI crossing with no copying.

Thread safety is provided by a `parking_lot::RwLock` (~8ns uncontended). This is the fastest backend, reaching **14-20M ops/s** single-threaded.

```python
@cache(max_size=256)  # backend="memory" is the default
def compute(x):
    return x ** 2
```

Use this backend when all callers live in the same process (web server threads, thread pools, async tasks, etc.).

### Shared backend (cross-process)

The shared backend stores cached data in memory-mapped files, making entries visible across multiple processes. This is useful for multi-process deployments (e.g. Gunicorn workers, Celery tasks) where you want to avoid recomputing the same expensive results in each process.

```python
@cache(max_size=1024, backend="shared")
def get_embedding(text: str) -> list[float]:
    # computed once, shared across all worker processes
    ...
```

**How it works:**

- Two mmap files are created per decorated function:
  - **Data file** — contains a header, a hash table (open-addressing with linear probing), and a fixed-size slab arena for entries
  - **Lock file** — holds a POSIX `pthread_rwlock` with `PTHREAD_PROCESS_SHARED`, so multiple processes can coordinate reads and writes
- File location: `/dev/shm/` on Linux, `$TMPDIR/fast_cache/` on macOS
- The file name is derived deterministically from the function's `__module__` and `__qualname__`, so the same function in different processes maps to the same cache automatically
- If an existing cache file has different parameters (capacity, strategy, key/value sizes), it is recreated

**Serialization overhead:**

Both keys and values are serialized with `pickle.dumps` on write and `pickle.loads` on read. This adds significant per-operation cost compared to the memory backend, which stores live Python objects directly. Expect roughly **10-100x** lower throughput depending on the size and complexity of your keys and values. The shared backend is designed for cases where the cached computation is expensive enough (network I/O, ML inference, heavy math) that the serialization cost is negligible in comparison.

**Size limits:**

Each entry has a fixed slot size determined at creation time. Keys and values that exceed the configured limits are silently skipped (the function is called but the result is not cached). You can monitor skips via `cache_info().oversize_skips`.

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_key_size` | `512` bytes | Maximum pickle size of the key (args tuple) |
| `max_value_size` | `4096` bytes | Maximum pickle size of the return value |

```python
# Large values: increase max_value_size
@cache(max_size=256, backend="shared", max_value_size=65536)
def get_large_result(query: str) -> dict:
    ...
```

### Platform support

| Platform | `backend="memory"` | `backend="shared"` |
|----------|--------------------|--------------------|
| Linux (x86_64, aarch64) | Yes | Yes (`/dev/shm/`) |
| macOS (x86_64, arm64) | Yes | Yes (`$TMPDIR/fast_cache/`) |
| Windows (x86_64) | Yes | No |

The shared backend relies on POSIX APIs (`mmap`, `pthread_rwlock` with `PTHREAD_PROCESS_SHARED`) that are not available on Windows. Using `backend="shared"` on Windows raises a `RuntimeError` at decoration time. The memory backend works on all platforms.

## Inspecting and clearing the cache

```python
@cache(max_size=100)
def compute(n):
    return n ** 2

compute(5)
compute(5)

info = compute.cache_info()
print(info)  # CacheInfo(hits=1, misses=1, max_size=100, current_size=1)

compute.cache_clear()  # removes all entries, resets counters
```

## Thread safety

The cache is safe to use from multiple threads with no additional locking:

```python
from concurrent.futures import ThreadPoolExecutor
from fast_cache import cache

@cache(max_size=256)
def work(x):
    return x * x

with ThreadPoolExecutor(max_workers=8) as pool:
    results = list(pool.map(work, range(100)))
```

## Decorator parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `strategy` | `Strategy` | `Strategy.LRU` | Eviction strategy |
| `max_size` | `int` | `128` | Maximum number of cached entries |
| `ttl` | `float \| None` | `None` | Time-to-live in seconds (`None` = no expiry) |
| `backend` | `str \| int \| Backend` | `Backend.MEMORY` | `"memory"` for in-process, `"shared"` for cross-process |
| `max_key_size` | `int` | `512` | Max serialized key bytes (shared backend only) |
| `max_value_size` | `int` | `4096` | Max serialized value bytes (shared backend only) |
