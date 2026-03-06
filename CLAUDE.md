# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**warp_cache** — a thread-safe Python caching decorator backed by a Rust extension (PyO3 + maturin). Uses SIEVE eviction, with TTL support, async awareness, and a cross-process shared memory backend.

## Build & Test Commands

```bash
make setup              # Create venv + install dev deps (uv sync --dev)
make build              # Build Rust extension (release, via maturin)
make build-debug        # Build Rust extension (debug, faster compile)
make test               # Build + run all tests
make test-only          # Run tests without rebuilding
make fmt                # Format Python (ruff) + Rust (cargo fmt)
make lint               # Lint Python (ruff) + Rust (cargo clippy)
make all                # Format, lint, test
```

**Run a single test:**
```bash
uv run pytest tests/test_basic.py::test_cache_hit -v
```

**Test across Python versions:**
```bash
make test-matrix -j     # Parallel across 3.10-3.14
make test PYTHON=3.13   # Specific version
```

## Architecture

### Rust core (`src/`)

- **`lib.rs`** — PyO3 module entry, exports `CachedFunction`, `SharedCachedFunction`, info types
- **`store.rs`** — In-process backend: `CachedFunction` uses a sharded `hashbrown::HashMap` + `parking_lot::RwLock` per shard (read lock for cache hits, write lock for misses/eviction). The `__call__` method does the entire cache lookup in Rust (hash → shard select → read lock → lookup → equality check → SIEVE visited update → return) in a single FFI crossing
- **`serde.rs`** — Fast-path binary serialization for common primitives (None, bool, int, float, str, bytes, flat tuples); avoids pickle overhead for the shared backend
- **`shared_store.rs`** — Cross-process backend: `SharedCachedFunction` holds `ShmCache` directly (no Mutex), with cached `max_key_size`/`max_value_size` fields and a pre-built `ahash::RandomState`. Serializes via serde.rs (with pickle fallback), stores in mmap'd shared memory
- **`entry.rs`** — `CacheEntry` { value, created_at, visited }
- **`key.rs`** — `CacheKey` wraps `Py<PyAny>` + precomputed hash; uses raw `ffi::PyObject_RichCompareBool` for equality (safe because called inside `#[pymethods]` where GIL is held)
- **`shm/`** — Shared memory infrastructure:
  - `mod.rs` — `ShmCache`: create/open, get/set with serialized bytes. Uses interior mutability (`&self` methods): reads are lock-free (seqlock), writes acquire seqlock internally. `next_unique_id` is `AtomicU64`
  - `layout.rs` — Header + SlotHeader structs, memory offsets
  - `region.rs` — `ShmRegion`: mmap file management (`$TMPDIR/warp_cache/{name}.cache`)
  - `lock.rs` — `ShmSeqLock`: seqlock (optimistic reads + TTAS spinlock) in shared memory
  - `hashtable.rs` — Open-addressing with linear probing (power-of-2 capacity, bitmask)
  - `ordering.rs` — SIEVE eviction: intrusive linked list + `sieve_evict()` hand scan

### Python layer (`warp_cache/`)

- **`_decorator.py`** — `cache()` factory: dispatches to `CachedFunction` (memory) or `SharedCachedFunction` (shared). Auto-detects async functions and wraps with `AsyncCachedFunction` (cache hit in Rust, only misses `await` the coroutine)
- **`_strategies.py`** — `Backend(IntEnum)`: MEMORY=0, SHARED=1

### Key design decisions

- **Single FFI crossing**: entire cache lookup happens in Rust `__call__`, no Python wrapper overhead
- **Release profile**: fat LTO + `codegen-units=1` for cross-crate inlining of PyO3 wrappers
- **SIEVE eviction**: unified across both backends. On hit, sets `visited=1` (single-word store). On evict, hand scans for unvisited entry. Lock-free reads on both backends
- **Thread safety**: sharded `hashbrown::HashMap` + `parking_lot::RwLock` per shard (read lock for hits, write lock for misses) for in-process backend; seqlock (optimistic reads + TTAS spinlock) for shared backend — no Mutex, `ShmCache` uses `&self` methods with interior mutability. Cache hits only acquire a cheap per-shard read lock (memory) or are fully lock-free (shared). Enables true parallel reads across shards under free-threaded Python (3.13t+)

## Critical Invariants

- **Hash table capacity must be power-of-2** — bitmask probing uses `hash & (capacity - 1)`. Always use `.next_power_of_two()`
- **`#[repr(C)]` struct field ordering** — place u64 fields before u32 to avoid implicit alignment padding; affects `size_of` assertions in layout.rs

## Linting

- Python: ruff (rules: E, F, W, I, UP, B, SIM; line-length=100; target py310)
- Rust: `cargo clippy -- -D warnings`
