# Usage Guide

## Basic caching

```python
from warp_cache import cache

@cache()
def expensive(x, y):
    # ... slow computation ...
    return x + y

expensive(1, 2)  # computes and caches
expensive(1, 2)  # returns cached result
```

**Arguments must be hashable.** Like `functools.lru_cache`, `warp_cache` uses
`hash()` to build cache keys. Passing unhashable types raises `TypeError`:

```python
@cache()
def process(data):
    return sum(data)

process((1, 2, 3))    # ok - tuples are hashable
process("hello")       # ok - strings are hashable
process([1, 2, 3])    # TypeError - lists are not hashable
process({"a": 1})     # TypeError - dicts are not hashable
```

If you need to cache a function that takes unhashable arguments, convert them
to hashable equivalents before passing (e.g. `tuple(my_list)`,
`tuple(sorted(my_dict.items()))`).

## Eviction

warp_cache uses **SIEVE** eviction - an algorithm that gets better hit rates than LRU with O(1) cost per access. There is no strategy parameter; SIEVE is used for both backends.

SIEVE works by maintaining a `visited` bit on each cache entry:

- **On cache hit**: the entry's `visited` bit is set to 1 (protecting it from eviction)
- **On eviction**: a rotating "hand" scans the cache. Entries with `visited=1` get a second chance (bit cleared to 0, hand advances). The first entry found with `visited=0` is evicted.

Frequently-accessed entries stay protected, while entries that were cached but never re-accessed get evicted first. Similar to LRU but handles sequential scans better and costs less per hit (no list reordering).

## Async functions

Async functions are detected at decoration time - no special syntax needed.
Cache lookups still go through the Rust path; only cache misses `await` the
wrapped coroutine.

```python
import asyncio
from warp_cache import cache

@cache(max_size=256)
async def fetch_user(user_id: int) -> dict:
    # ... slow I/O ...
    return {"id": user_id}

async def main():
    user = await fetch_user(42)   # miss - awaits the coroutine
    user = await fetch_user(42)   # hit - returns cached result instantly

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
from warp_cache import cache, Backend

@cache(max_size=256, backend=Backend.MEMORY)   # enum
@cache(max_size=256, backend="memory")          # equivalent string
```

| Backend | Value | Storage | Use case |
|---------|-------|---------|----------|
| `Backend.MEMORY` | `0` | In-process (default) | Single-process applications |
| `Backend.SHARED` | `1` | Memory-mapped file | Cross-process sharing via mmap |

### Memory backend (default)

The memory backend keeps cached data in the process heap. Keys are stored as Python objects directly (no serialization), and lookups go through a single Rust `__call__` - hash, lookup, equality check, and return all happen in one FFI crossing with no copying.

Thread safety uses a sharded `hashbrown::HashMap` with GIL-conditional locking - under GIL-enabled Python, `GilCell` has no overhead; under free-threaded Python, per-shard `parking_lot::RwLock` allows parallel reads. Write lock is only taken on cache misses for SIEVE eviction.

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
  - **Data file** - contains a header, a hash table (open-addressing with linear probing), and a fixed-size slab arena for entries
  - **Lock file** - holds a seqlock (sequence counter + spinlock) for cross-process synchronization. Reads are optimistic (no lock taken); only writes acquire the spinlock
- File location: `/dev/shm/` on Linux, `$TMPDIR/warp_cache/` on macOS
- The file name is derived deterministically from the function's `__module__` and `__qualname__`, so the same function in different processes maps to the same cache automatically
- If an existing cache file has different parameters (capacity, key/value sizes, version), it is recreated

**Serialization overhead:**

Keys and values are serialized using a fast-path binary format for common primitives (None, bool, int, float, str, bytes, flat tuples) with pickle fallback for complex types. This adds per-operation cost compared to the memory backend, which stores Python objects directly. Expect roughly 2x lower throughput - the gap is unavoidable cross-process overhead: serialization, deterministic hashing, seqlock, and mmap copy. No Mutex is used; reads don't take any locks. The shared backend makes sense when the cached computation is expensive enough (network I/O, ML inference, heavy math) that serialization cost doesn't matter.

**Size limits:**

Each entry has a fixed slot size determined at creation time. Keys and values that exceed the configured limits are silently skipped (the function is called but the result is not cached). You can monitor skips via `cache_info().oversize_skips`.

| Parameter | Default | Description |
|-----------|---------|-------------|
| `max_key_size` | `512` bytes | Maximum serialized size of the key (args tuple) |
| `max_value_size` | `4096` bytes | Maximum serialized size of the return value |

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
| macOS (x86_64, arm64) | Yes | Yes (`$TMPDIR/warp_cache/`) |
| Windows (x86_64) | Yes | No |

The shared backend relies on POSIX `mmap` which is not available on Windows. The seqlock uses portable atomics rather than platform-specific threading primitives. Using `backend="shared"` on Windows raises a `RuntimeError` at decoration time. The memory backend works on all platforms.

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
from warp_cache import cache

@cache(max_size=256)
def work(x):
    return x * x

with ThreadPoolExecutor(max_workers=8) as pool:
    results = list(pool.map(work, range(100)))
```

## Key serialization behavior

### How cache keys are formed

Both backends build the cache key from `*args` and `**kwargs`:

- **No kwargs (common path):** The `args` tuple is used directly as the key.
- **With kwargs:** Keywords are sorted by name to ensure deterministic ordering,
  then combined with args as `(args, tuple(sorted(kwargs.items())))`. This means
  `fn(a=1, b=2)` and `fn(b=2, a=1)` always hit the same cache entry.

Arguments must be **hashable** (memory backend) or **serializable** (shared backend).

### Memory backend

Keys are stored as Python objects on the heap — no serialization. Lookups use
Python's built-in `hash()` and `==` via the C API. This is fast but means the
cache is inherently single-process (Python object pointers are not meaningful
across processes).

### Shared backend

Keys and values are serialized to bytes before storage. The serialization uses a
**fast-path binary format** for common primitive types, falling back to **pickle**
for everything else:

| Type | Format | Size |
|------|--------|------|
| `None` | Tag byte | 1 byte |
| `bool` | Tag byte | 1 byte |
| `int` (fits i64) | Tag + little-endian i64 | 9 bytes |
| `float` | Tag + IEEE 754 f64 | 9 bytes |
| `str` | Tag + 4-byte length + UTF-8 | 5 + len bytes |
| `bytes` | Tag + 4-byte length + data | 5 + len bytes |
| Flat tuple of above | Tag + count + elements | varies |
| Everything else | Pickle (protocol 5) | varies |

The fast-path avoids pickle overhead entirely for the most common argument types.
Large integers (outside i64 range), nested structures, dicts, sets, and custom
objects fall back to pickle automatically.

### Cross-process determinism

The shared backend must ensure that the same function arguments produce the same
cache key in every process. Python's `hash()` is randomized per-process
(`PYTHONHASHSEED`), so the shared backend does **not** use it. Instead:

1. Keys are serialized to a deterministic byte sequence (the binary format above)
2. The bytes are hashed with **ahash** using fixed seeds (same seeds in every process)
3. Lookups verify matches using byte-level comparison (`memcmp`), not Python equality

This makes the shared backend completely immune to `PYTHONHASHSEED` — different
processes with different hash seeds will always agree on cache entries.

## Decorator parameters

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `max_size` | `int` | `128` | Maximum number of cached entries |
| `ttl` | `float \| None` | `None` | Time-to-live in seconds (`None` = no expiry) |
| `backend` | `str \| int \| Backend` | `Backend.MEMORY` | `"memory"` for in-process, `"shared"` for cross-process |
| `max_key_size` | `int` | `512` | Max serialized key bytes (shared backend only) |
| `max_value_size` | `int` | `4096` | Max serialized value bytes (shared backend only) |
