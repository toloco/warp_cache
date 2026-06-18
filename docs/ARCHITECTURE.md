# Architecture

Deep reference for the internals of **warp_cache** — a thread-safe Python caching
decorator backed by a Rust extension (PyO3 + maturin). Uses SIEVE eviction, with TTL
support, async awareness, and a cross-process shared memory backend.

For day-to-day workflow and commands, see [`CLAUDE.md`](../CLAUDE.md) and
[`CONTRIBUTING.md`](../CONTRIBUTING.md).

## Rust core (`src/`)

- **`lib.rs`** — PyO3 module entry, exports `CachedFunction`, `SharedCachedFunction`, info types.
- **`store.rs`** — In-process backend: `CachedFunction` uses sharded `hashbrown::HashMap`
  with passthrough hasher (avoids re-hashing Python's precomputed hash) + GIL-conditional
  locking (`GilCell` under GIL for zero-cost, `parking_lot::RwLock` under free-threaded
  Python). The `__call__` hot path uses `BorrowedArgs` to look up via borrowed pointer
  (no `CacheKey` allocation on hits), with `CacheKey` only materialized on cache miss for
  storage.
- **`serde.rs`** — Fast-path binary serialization for common primitives (None, bool, int,
  float, str, bytes, flat tuples); avoids pickle overhead for the shared backend.
- **`shared_store.rs`** — Cross-process backend: `SharedCachedFunction` holds `ShmCache`
  directly (no Mutex), with cached `max_key_size`/`max_value_size` fields and a pre-built
  `ahash::RandomState`. Serializes via `serde.rs` (with pickle fallback), stores in mmap'd
  shared memory.
- **`entry.rs`** — `SieveEntry` { value, created_at, visited }.
- **`key.rs`** — `CacheKey` wraps `Py<PyAny>` + precomputed hash; uses raw
  `ffi::PyObject_RichCompareBool` for equality. Also provides `BorrowedArgs` (zero-alloc
  borrowed key for hit-path lookups via hashbrown's `Equivalent` trait).
- **`shm/`** — Shared memory infrastructure:
  - `mod.rs` — `ShmCache`: create/open, get/set with serialized bytes. Uses interior
    mutability (`&self` methods): reads are lock-free (seqlock), writes acquire seqlock
    internally. `next_unique_id` is `AtomicU64`.
  - `layout.rs` — Header + SlotHeader structs, memory offsets.
  - `region.rs` — `ShmRegion`: mmap file management (`$TMPDIR/warp_cache/{name}.data` +
    `{name}.lock`).
  - `lock.rs` — `ShmSeqLock`: seqlock (optimistic reads + TTAS spinlock) in shared memory.
  - `hashtable.rs` — Open-addressing with linear probing (power-of-2 capacity, bitmask).
  - `ordering.rs` — SIEVE eviction: intrusive linked list + `sieve_evict()` hand scan.

## Python layer (`warp_cache/`)

- **`_decorator.py`** — `cache()` factory: dispatches to `CachedFunction` (memory) or
  `SharedCachedFunction` (shared). Auto-detects async functions and wraps with
  `AsyncCachedFunction` (cache hit in Rust, only misses `await` the coroutine).
- **`_strategies.py`** — `Backend(IntEnum)`: MEMORY=0, SHARED=1.

## Key design decisions

- **Single FFI crossing**: entire cache lookup happens in Rust `__call__`, no Python
  wrapper overhead.
- **Release profile**: fat LTO + `codegen-units=1` for cross-crate inlining of PyO3 wrappers.
- **SIEVE eviction**: unified across both backends. On hit, sets `visited=1` (single-word
  store). On evict, hand scans for unvisited entry. Lock-free reads on both backends.
- **Thread safety**: GIL-conditional locking — `GilCell` (zero-cost `UnsafeCell` wrapper)
  under GIL-enabled Python, `parking_lot::RwLock` under free-threaded Python
  (`#[cfg(Py_GIL_DISABLED)]`). Shared backend uses seqlock (optimistic reads + TTAS
  spinlock) — no Mutex. Under free-threaded Python, per-shard `RwLock` enables true
  parallel reads across cores.
- **Borrowed key lookup**: hit path uses `BorrowedArgs` (raw pointer + precomputed hash)
  via hashbrown's `Equivalent` trait — no `CacheKey` allocation, no refcount churn on hits.
- **Passthrough hasher**: `PassthroughHasher` feeds Python's precomputed hash directly to
  hashbrown, avoiding foldhash re-hashing (~1–2ns saved per lookup). Shard count is
  power-of-2 for bitmask indexing.

## Critical invariants

- **Hash table capacity must be power-of-2** — bitmask probing uses
  `hash & (capacity - 1)`. Always use `.next_power_of_two()`.
- **`#[repr(C)]` struct field ordering** — place u64 fields before u32 to avoid implicit
  alignment padding; affects `size_of` assertions in `layout.rs`. Any field the lock-free
  read path *writes* (currently only `SlotHeader.visited`) must be an atomic type accessed
  with `Relaxed` — readers touch it without the write lock while writers reuse the slot, so a
  plain field is a data race (issue #37). `AtomicU64` is layout-identical to `u64`, so this
  doesn't change the cross-process layout.
- **Cross-process timestamps must use a system-wide clock (issue #32).** `created_at_nanos`
  is written into shared memory by one process and compared against `now` in another, so
  `shm::current_time_nanos` uses `CLOCK_MONOTONIC` (process-independent on Linux, macOS, and
  the BSDs). Never use `std::time::Instant` for shm timestamps — its epoch is per-process, so
  the two bases are unrelated and TTL silently breaks across processes (the original macOS bug).
- **No second shard guard while one is live (reentrancy, issue #30).** A memory-backend
  lookup runs arbitrary Python `__eq__` (via `PyObject_RichCompareBool`) *while a shard guard
  is held*. That `__eq__` can re-enter the same `CachedFunction` (or, on GIL builds, hand off
  the GIL to another thread that calls in) and take a second, conflicting guard — aliasing
  `&Shard` with `&mut Shard` (UB) on GIL builds, or deadlocking the `RwLock` on free-threaded
  builds. `CachedFunction::try_enter` marks the function active for the duration of each borrow
  region; an entrant that finds it already active **bypasses the cache and recomputes** (and
  must never take a shard guard). The wrapped function is always invoked *outside* this guard,
  so recursive `@cache` functions still cache. GIL builds use a single GIL-serialized flag;
  free-threaded builds use a per-thread set of active function addresses. The shared backend is
  unaffected (key comparison is by serialized bytes, never Python `__eq__`).
- **A raising `__eq__` must propagate, not be swallowed (issue #36).** `PyObject_RichCompareBool`
  returns -1 with a Python exception set when a key's `__eq__` raises. `key.rs::rich_compare_eq`
  reports -1 as "not equal" (so hashbrown stops probing) but leaves the exception set, and every
  memory-backend lookup site (`__call__` read + write-double-check, `get`, `_probe`) calls
  `PyErr::take` after the lookup and returns the error instead of recomputing / returning `Ok`
  with an exception pending (which PyO3 turns into a masking `SystemError`). Any new lookup site
  that can run `__eq__` must do the same check.
