# Design: reentrancy soundness for the in-process backend (issue #30)

**Issue:** [#30](https://github.com/toloco/warp_cache/issues/30) — *[critical] GilCell/UnsafeCell
aliasing is unsound under reentrancy.*
**Status:** approved design, pre-implementation.
**Scope decision:** fix **both** build configs (GIL-enabled + free-threaded).
**Behavior decision:** on detected reentrancy, **bypass + recompute** (no panic / no raise).

---

## Problem

On the in-process backend (`src/store.rs`), a cache lookup holds a per-shard guard across
hashbrown probing. Probing invokes Python `__eq__` via `PyObject_RichCompareBool`. If a cache
key's `__eq__` calls back into the **same** `CachedFunction`, the reentrant call takes a
conflicting guard on the same shard:

- **GIL-enabled builds (default)** use `GilCell<Shard>` (an `UnsafeCell` wrapper). The reentrant
  write produces `&mut Shard` aliasing the outer live `&Shard` → **aliasing UB**, and the reentrant
  `insert` can reallocate/rehash the hashbrown table the outer `get` is still probing →
  **use-after-free**. It also corrupts the capacity invariant (`current_size > max_size`).
- **Free-threaded builds (3.13t+)** use `parking_lot::RwLock<Shard>`. The same reentry (read-then-write
  on one thread) **deadlocks**, because `parking_lot` locks are not reentrant.

Both are reachable purely from the safe Python API with ordinary objects that define a custom
`__eq__`/`__hash__`.

### Root cause (evidence)

| Fact | Location |
|---|---|
| GIL build: `GilCell` is an `UnsafeCell` wrapper; read/write guards `Deref`/`DerefMut` to it | `src/store.rs:78-140` |
| Free-threaded build: `parking_lot::RwLock<Shard>` | `src/store.rs:68-69` |
| `__hash__` is computed **before** any borrow (not a reentrancy vector) | `src/store.rs:264-295` |
| Reentrancy surface is **only** `__eq__` via `PyObject_RichCompareBool`, called *inside* a live borrow | `src/key.rs:88-101` (`BorrowedArgs::equivalent`), `src/key.rs:42-56` (`CacheKey::eq`) |
| `__call__` fast path: read guard held across `map.get` | `src/store.rs:356-377` |
| `__call__` slow path: write guard held across `map.get`/`evict_one`/`insert` | `src/store.rs:395-431` |
| `get` / `_probe`: read guard held across `map.get` | `src/store.rs:449`, `src/store.rs:482` |
| `set`: write guard held across `map.get`/`evict_one`/`insert` | `src/store.rs:515-540` |
| `cache_clear` / `cache_info`: take guards but run **no** Python code (can't *trigger* reentry, but can be *called from* a reentrant `__eq__`) | `src/store.rs:545-567` |
| The wrapped function (`fn_obj`) is called with **no** guard held | `src/store.rs:380` |

### Not affected

The shared backend (`SharedCachedFunction` / `src/shm/`) compares keys by serialized bytes and
never runs Python `__eq__` during a lookup, so it has no reentrancy surface. No change there.

---

## Goals / non-goals

**Goals**
- Eliminate the aliasing UB / use-after-free on GIL builds.
- Eliminate the same-thread reentrant deadlock on free-threaded builds.
- Preserve correct behavior and **negligible** hot-path cost for normal (non-reentrant) usage,
  including the canonical recursive-cache pattern.
- Cover every method that takes a shard guard.

**Non-goals**
- Rewriting hashbrown probing to compare keys outside the guard (large, racy rewrite).
- Changing the shared backend.
- Supporting reentrant `__eq__` as a *caching* feature (the reentrant call is computed but not cached).

---

## Approach: scoped reentrancy guard + bypass-recompute

A reentrancy guard detects when the current thread is already executing inside a **borrow region**
of the **same** `CachedFunction`, and makes the reentrant call **bypass the cache**.

### The decisive detail: scope the guard to borrow regions, not the whole method

The wrapped function call (`fn_obj`, `store.rs:380`) sits *between* the fast-path read borrow and
the slow-path write borrow, with **no guard held**. Therefore:

- A guard that wraps the **whole method** would flag the recursive call in
  `@cache def fib(n): return fib(n-1) + fib(n-2)` as reentrant and break the canonical
  recursive-cache use case (recursion re-enters *outside* any borrow).
- A guard scoped to **only the borrow regions** lets recursion proceed normally (the outer call has
  already exited its borrow region before `fn_obj` runs), while still catching pathological `__eq__`
  reentry, which happens *inside* a borrow region.

### The rule

> A call that is about to enter a borrow region, while this thread **already holds** a borrow region
> on the **same** `CachedFunction`, is reentrant → **bypass** (do not take any guard).

Cross-function reentry (`f`'s `__eq__` calls `g`) and cross-shard reentry are *not* bypassed — they
touch different memory and are sound. (Per-function granularity may bypass same-function/different-shard
reentry; that is pathological and bypass remains correct, just conservative.)

### Per-method bypass semantics

| Method | Normal | On reentry (bypass) |
|---|---|---|
| `__call__` | lookup → compute on miss → cache | compute via `fn_obj`, return **uncached**; count as a miss |
| `get` | lookup | return `None` (miss) |
| `_probe` | lookup | return `(false, None)` |
| `set` | store | skip the store (no-op), return `Ok(())` |
| `cache_clear` | clear all shards | no-op |
| `cache_info` | sum shard sizes | return counters with `current_size = 0` (shards not read) |

`cache_clear`/`cache_info` are guarded defensively: they cannot *trigger* reentry, but they *can be
called from* another method's `__eq__`, where their shard guards would alias/deadlock.

### Per-build mechanism

The reentrancy flag must be thread-scoped. To keep the hot path cheap and consistent with the
codebase's existing "zero-cost under the GIL" philosophy:

- **GIL build (`#[cfg(not(Py_GIL_DISABLED))]`):** a non-atomic per-`CachedFunction` flag, sound under
  the same "the GIL serializes all `#[pymethods]` access" `unsafe impl Sync` rationale already used by
  `GilCell`. The same thread observes its own set flag → reentry detected. ~1 ns per borrow region.
- **Free-threaded build (`#[cfg(Py_GIL_DISABLED)]`):** a `thread_local` set of active
  `CachedFunction` pointers. (A shared atomic flag would false-positive under genuine parallel load
  and tank the hit rate.) `parking_lot::RwLock` stays for cross-thread correctness; the guard stops
  same-thread reentry *before* the lock is taken, so it can never deadlock.

Both expose the same internal API — roughly `try_enter(&self) -> Option<EnterGuard>`:
- `Some(guard)`: this thread now owns the borrow region; the guard clears the flag on drop (RAII).
- `None`: reentrant → the caller bypasses.

### Control-flow shape (per borrowing method)

```text
compute hash / shard_idx        // no borrow, reentry here is harmless
match self.try_enter() {
    Some(_enter) => {            // RAII clears on drop
        let guard = shard.read()/.write();
        ... existing logic ...   // map.get/insert may run __eq__ -> reentry sees flag set -> bypass
    }                            // _enter + shard guard dropped here
    None => { ... bypass ... }   // never touch a shard guard
}
// fn_obj is called OUTSIDE any try_enter scope -> recursion works
```

`__call__` calls `try_enter` twice: once around the fast-path read region and once around the
slow-path write region, with the `fn_obj` call in between (unguarded). Reaching the slow path implies
this call was *not* reentrant (a reentrant call bypasses at the fast path), so the second `try_enter`
always succeeds.

---

## Soundness argument

- **No aliasing (GIL):** the only path to a conflicting guard was reentry; the guard makes a reentrant
  call return *before* taking any shard guard, so a write guard never coexists with another guard on
  the same shard. Read+read reentry (the only sound combination) is also bypassed — conservative but safe.
- **No deadlock (free-threaded):** a reentrant lock acquisition never happens; the guard bypasses first.
- **The flag itself is race-free:** the flag is set/cleared by complete statements with no live
  reference held across the `__eq__` call; reentrant access only *reads* the flag. Cross-thread access
  is serialized by the GIL (GIL build) or isolated per thread (free-threaded).
- **Recursion preserved:** recursive calls re-enter via `fn_obj`, outside every `try_enter` scope, so
  the flag is clear and they cache normally.

---

## Test plan (TDD)

New `tests/test_reentrancy.py`:

1. **Reproducer:** a key class with constant `__hash__` (force bucket collisions so `__eq__` actually
   runs) whose `__eq__` calls back into the same `@cache`d function. Assert: no crash, the call
   returns the correct value, and `cache_info().current_size <= max_size` (the invariant the issue
   shows violated). *Pre-fix:* UB (may crash/corrupt). *Post-fix:* passes.
2. **Recursion regression:** `@cache` recursive `fib`/`factorial` returns correct results and
   `current_size` reflects cached subproblems — proves same-function recursion still caches.
3. **Cross-function reentry:** `f`'s key `__eq__` calls a *different* cached `g`; `g` caches normally.
4. **Cross-shard reentry:** reentrant call lands on a different shard; still cached/normal.
5. **Reentry across all entry points:** `get`, `_probe`, `set` reentry returns miss/no-op safely.
6. **TTL + reentry:** guard fires correctly with a TTL configured.
7. Run the **full existing suite** unchanged (`make test`) to confirm no normal-path regression.

Rust: `cargo test` stays green.

---

## Verification gates (this is a "risky change" per CLAUDE.md)

Touches `src/store.rs` + locking, so beyond `make all`:

- `make test-matrix -j` — Python 3.9–3.14 **including free-threaded** (exercises the
  `Py_GIL_DISABLED` path).
- `make bench` — confirm the hot-path (hit) regression is acceptable. Target: negligible
  (single non-atomic flag op on GIL builds). If a benchmark shows a material regression, report it and
  revisit the mechanism before opening the PR.

---

## Open decisions (minor; defaults chosen)

1. **`cache_info` on reentry** returns `current_size = 0` (can't read shards safely). Acceptable —
   calling `cache_info` from inside a key's `__eq__` is pathological. *(Default: as stated.)*
2. **Miss accounting for bypassed `__call__`:** counted as a miss (it did compute, not serve from
   cache). *(Default: count as miss.)*
3. **Docs:** add a short "reentrant keys are not cached" note to user docs referencing #30.
   *(Default: include, since `README`/`docs` updates are required when behavior is user-visible.)*
