# Reentrancy Soundness Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the in-process cache backend sound when a key's `__eq__`/`__hash__` re-enters the
same cache (issue #30): eliminate aliasing UB on GIL builds and the reentrant deadlock on
free-threaded builds, without breaking recursive `@cache` functions or regressing the hot path.

**Architecture:** A reentrancy guard (`try_enter`) marks a `CachedFunction` "active" for the duration
of each shard **borrow region** only (never across the wrapped-function call, so recursion still
caches). A call that finds the function already active **bypasses** the cache and recomputes. On GIL
builds the guard is a single non-atomic flag serialized by the GIL (also blocks the rarer
cross-thread "GIL handed off mid-`__eq__`" variant); on free-threaded builds it is a per-thread set
of active function addresses (the `parking_lot::RwLock` still handles cross-thread access).

**Tech Stack:** Rust + PyO3 (`src/store.rs`, `src/key.rs`), maturin build, pytest, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-06-16-reentrancy-soundness-design.md`

---

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `src/store.rs` | Reentrancy guard types + `try_enter` + a flag field on `CachedFunction`; guard each borrow region in `__call__`/`get`/`_probe`/`set`/`cache_clear`/`cache_info` | Modify |
| `src/key.rs` | Update stale SAFETY comments that claim the GIL alone makes `__eq__` calls safe | Modify |
| `tests/test_reentrancy.py` | New regression tests (reproducer, recursion, bypass semantics, all entry points) | Create |
| `docs/ARCHITECTURE.md` | Document the reentrancy invariant | Modify |
| `README.md`, `llms.txt`, `llms-full.txt` | One-line caveat: reentrant keys are computed but not cached | Modify |

**Key conventions (from `make`/CLAUDE.md):**
- Fast rebuild after a Rust edit: `make build-debug`
- Run one test file without rebuilding Rust: `uv run pytest tests/test_reentrancy.py -v`
- Full local check: `make all` (fmt + lint + test). Lint is `-D warnings` for clippy.
- This is a **risky change** (touches `store.rs` + locking): also run `make test-matrix -j` and `make bench`.

---

## Task 1: Failing regression tests (reproducer + recursion + bypass value)

**Files:**
- Create: `tests/test_reentrancy.py`

- [ ] **Step 1: Write the failing tests**

```python
# tests/test_reentrancy.py
"""Regression tests for issue #30.

A cache lookup holds a shard borrow across hashbrown probing, which runs Python
``__eq__`` (via ``PyObject_RichCompareBool``). If that ``__eq__`` re-enters the
same cached function it used to alias ``&Shard`` with ``&mut Shard`` (UB on GIL
builds) or deadlock the ``RwLock`` (free-threaded builds). The fix makes such
reentrant calls bypass the cache and recompute.
"""

from warp_cache import cache


def test_reentrant_eq_does_not_corrupt_cache():
    """A key whose __eq__ re-enters the same cache must not crash or break the
    capacity invariant (the bug produced current_size=5 with max_size=4)."""
    calls = {"n": 0}

    @cache(max_size=4)
    def f(key):
        calls["n"] += 1
        return calls["n"]

    class Reenter:
        depth = 0

        def __hash__(self):
            # Constant hash forces every key into one shard and forces hashbrown
            # to invoke __eq__ during probing (all keys collide).
            return 0

        def __eq__(self, other):
            # Re-enter the SAME cache while __eq__ runs inside a live borrow.
            if Reenter.depth < 2:
                Reenter.depth += 1
                try:
                    f(Reenter())
                finally:
                    Reenter.depth -= 1
            return self is other

    f(Reenter())  # prime: now the map holds a hash-0 key to collide against
    for _ in range(10):
        f(Reenter())

    info = f.cache_info()
    assert info.current_size <= 4, f"capacity invariant violated: {info.current_size}"


def test_recursive_cached_function_caches():
    """Ordinary recursion re-enters the function OUTSIDE any borrow, so it must
    keep caching subproblems (guards against a fix that over-blocks)."""

    @cache(max_size=128)
    def fib(n):
        if n < 2:
            return n
        return fib(n - 1) + fib(n - 2)

    assert fib(20) == 6765
    info = fib.cache_info()
    assert info.hits > 0
    assert info.current_size > 0


def test_reentrant_call_returns_correct_value():
    """The bypassed reentrant call must still return the correct recomputed value."""

    @cache(max_size=8)
    def f(key):
        return key.tag * 10

    class K:
        def __init__(self, tag, reenter=None):
            self.tag = tag
            self.reenter = reenter
            self.result = None

        def __hash__(self):
            return 0

        def __eq__(self, other):
            if self.reenter is not None and self.result is None:
                self.result = f(self.reenter)  # reentrant (bypassed) call
            return self is other

    f(K(1))  # prime
    probe = K(2, reenter=K(3))
    assert f(probe) == 20
    assert probe.result == 30
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py -v`
Expected: `test_reentrant_eq_does_not_corrupt_cache` and `test_reentrant_call_returns_correct_value`
**FAIL** — they crash (segfault / abort), corrupt the cache (`current_size > 4`), or on a
free-threaded build hang. `test_recursive_cached_function_caches` passes (recursion is not the bug;
it is here to lock in that the fix keeps it working).

> Note: the failure mode is undefined behavior, so it may manifest as a hard crash that aborts
> pytest rather than a clean assertion failure. That is expected and is itself the "fail before".

- [ ] **Step 3: Commit the tests**

```bash
git add tests/test_reentrancy.py
git commit -m "$(cat <<'EOF'
test: add reentrancy regression tests for #30

Reproducer (custom __eq__ re-entering the same cache), a recursion guard, and a
bypass-value check. Reproducer fails (UB/corruption/deadlock) before the fix.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Reentrancy-guard infrastructure + guard `__call__`

**Files:**
- Modify: `src/store.rs` (add guard types after the `GilCell` block ~line 140; add field to
  `CachedFunction` ~line 172; init in `new` ~line 333; add `try_enter`; rewrite `__call__` ~line 344)

- [ ] **Step 1: Add the guard infrastructure**

Insert this block in `src/store.rs` immediately **after** the `GilCell`/`GilWriteGuard` definitions
(after line 140, before `struct Shard {`):

```rust
// ---------------------------------------------------------------------------
// Reentrancy guard (issue #30).
//
// A cache lookup holds a shard guard across hashbrown probing, which invokes
// Python `__eq__` via `PyObject_RichCompareBool`. That Python code can (a)
// re-enter the SAME CachedFunction on this thread, or (b) yield the GIL to
// another thread that calls in — either way taking a second, conflicting shard
// guard. On GIL builds that aliases `&Shard` with `&mut Shard` (UB + possible
// use-after-free if the table reallocates); on free-threaded builds the
// same-thread reentrant lock deadlocks.
//
// `try_enter` marks the function active for the duration of ONE borrow region.
// Any entrant that finds it already active bypasses the cache. The wrapped
// function is always called OUTSIDE this guard, so ordinary recursion
// (e.g. `@cache fib`) is unaffected.
// ---------------------------------------------------------------------------

// GIL builds: one non-atomic flag, serialized by the GIL exactly like GilCell.
// While the flag is set, any other entrant (reentrant, OR a thread the GIL was
// handed to mid-`__eq__`) bypasses, so two guards never coexist on one shard.
#[cfg(not(Py_GIL_DISABLED))]
struct ReentryCell(UnsafeCell<bool>);

#[cfg(not(Py_GIL_DISABLED))]
unsafe impl Sync for ReentryCell {}

#[cfg(not(Py_GIL_DISABLED))]
impl ReentryCell {
    fn new() -> Self {
        ReentryCell(UnsafeCell::new(false))
    }
}

#[cfg(not(Py_GIL_DISABLED))]
struct EnterGuard<'a>(&'a UnsafeCell<bool>);

#[cfg(not(Py_GIL_DISABLED))]
impl Drop for EnterGuard<'_> {
    #[inline(always)]
    fn drop(&mut self) {
        // SAFETY: GIL held; access serialized.
        unsafe { *self.0.get() = false }
    }
}

// Free-threaded builds: a per-thread set of active CachedFunction addresses.
// Cross-thread access is handled by the real parking_lot::RwLock; this guard
// only prevents same-thread reentry (which would deadlock that RwLock).
#[cfg(Py_GIL_DISABLED)]
thread_local! {
    static ACTIVE: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(Py_GIL_DISABLED)]
struct EnterGuard {
    id: usize,
}

#[cfg(Py_GIL_DISABLED)]
impl Drop for EnterGuard {
    #[inline]
    fn drop(&mut self) {
        ACTIVE.with(|a| {
            let mut v = a.borrow_mut();
            if let Some(pos) = v.iter().rposition(|&x| x == self.id) {
                v.swap_remove(pos);
            }
        });
    }
}
```

- [ ] **Step 2: Add the flag field to `CachedFunction`**

In the `CachedFunction` struct (currently `src/store.rs:171-180`), add a final field:

```rust
#[pyclass(frozen)]
pub struct CachedFunction {
    fn_obj: Py<PyAny>,
    shards: Box<[ShardLock]>,
    shard_mask: usize,
    ttl: Option<Duration>,
    max_size: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    #[cfg(not(Py_GIL_DISABLED))]
    reentry: ReentryCell,
}
```

- [ ] **Step 3: Initialize the field in `new`**

In `new` (currently `src/store.rs:333-341`), add the field to the struct literal:

```rust
        CachedFunction {
            fn_obj,
            shards: shards.into_boxed_slice(),
            shard_mask: n_shards - 1,
            ttl: ttl.map(Duration::from_secs_f64),
            max_size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            #[cfg(not(Py_GIL_DISABLED))]
            reentry: ReentryCell::new(),
        }
```

- [ ] **Step 4: Add the `try_enter` method**

Add these two cfg-gated methods inside the **non-`#[pymethods]`** `impl CachedFunction` block (the one
that contains `evict_one`/`make_key`/`hash_args`, currently ending at `src/store.rs:296`), just before
its closing `}`:

```rust
    /// Mark this function active for one borrow region. Returns `None` if it is
    /// already active (reentrant call / GIL handed off mid-`__eq__`), in which
    /// case the caller must bypass the cache. The returned guard clears the
    /// marker on drop. See issue #30.
    #[cfg(not(Py_GIL_DISABLED))]
    #[inline(always)]
    fn try_enter(&self) -> Option<EnterGuard<'_>> {
        let p = self.reentry.0.get();
        // SAFETY: GIL held; access serialized. No reference is held across any
        // Python call — we read then write a bool by value.
        unsafe {
            if *p {
                None
            } else {
                *p = true;
                Some(EnterGuard(&self.reentry.0))
            }
        }
    }

    #[cfg(Py_GIL_DISABLED)]
    #[inline]
    fn try_enter(&self) -> Option<EnterGuard> {
        let id = self as *const Self as usize;
        ACTIVE.with(|a| {
            let mut v = a.borrow_mut();
            if v.contains(&id) {
                None
            } else {
                v.push(id);
                Some(EnterGuard { id })
            }
        })
    }
```

- [ ] **Step 5: Rewrite `__call__` to guard both borrow regions**

Replace the entire `__call__` method (currently `src/store.rs:344-435`) with:

```rust
    #[pyo3(signature = (*args, **kwargs))]
    fn __call__<'py>(
        &self,
        py: Python<'py>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        // Step 1: compute hash + pointer without creating a CacheKey
        let (hash, key_ptr, _key_owner) = Self::hash_args(py, &args, &kwargs)?;
        let borrowed = BorrowedArgs { hash, ptr: key_ptr };
        let shard_idx = hash as usize & self.shard_mask;

        // FAST PATH: read lock on one shard, guarded against reentrancy.
        // `entered` is false when this call is reentrant (a key's __eq__/__hash__
        // ran inside a live borrow of this same cache, or the GIL was handed off
        // mid-comparison). Such calls bypass the cache and recompute. See #30.
        let entered = match self.try_enter() {
            Some(_enter) => {
                let shard = self.shards[shard_idx].read();
                if let Some(entry) = shard.map.get(&borrowed) {
                    if let Some(ttl) = self.ttl {
                        if entry.created_at.elapsed() <= ttl {
                            entry.visited.store(true, Ordering::Relaxed);
                            let val = entry.value.clone_ref(py);
                            drop(shard);
                            self.hits.fetch_add(1, Ordering::Relaxed);
                            return Ok(val);
                        }
                        // Expired — fall through to miss path
                    } else {
                        entry.visited.store(true, Ordering::Relaxed);
                        let val = entry.value.clone_ref(py);
                        drop(shard);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        return Ok(val);
                    }
                }
                true
            }
            None => false,
        };

        // Cache miss (or reentrant bypass): call the wrapped function (no lock held)
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?.unbind();

        // Only populate the cache for non-reentrant misses.
        if entered {
            // NOW create a CacheKey since we need to store it.
            let cache_key = match _key_owner {
                Some(obj) => CacheKey::with_hash(hash, obj),
                None => {
                    // No-kwargs path: incref the args tuple for storage
                    let obj: Py<PyAny> = unsafe {
                        ffi::Py_IncRef(key_ptr);
                        Bound::from_owned_ptr(py, key_ptr).unbind()
                    };
                    CacheKey::with_hash(hash, obj)
                }
            };

            // SLOW PATH: write lock, double-check, evict if needed, insert.
            // Reaching here implies this call was not reentrant (a reentrant
            // call returns from the `None` arm above), so try_enter succeeds;
            // the `if let` is a defensive guard.
            if let Some(_enter) = self.try_enter() {
                let mut shard = self.shards[shard_idx].write();

                // Double-check: another thread may have inserted while we were computing
                let needs_insert = match shard.map.get(&cache_key) {
                    Some(entry) => {
                        if let Some(ttl) = self.ttl {
                            entry.created_at.elapsed() > ttl
                        } else {
                            false
                        }
                    }
                    None => true,
                };

                if needs_insert {
                    // Remove expired entry from map if present (order cleaned lazily)
                    shard.map.remove(&cache_key);

                    // Evict if at capacity
                    while shard.map.len() >= shard.capacity {
                        Self::evict_one(&mut shard);
                        if shard.order.is_empty() {
                            break;
                        }
                    }

                    let entry = SieveEntry {
                        value: result.clone_ref(py),
                        created_at: Instant::now(),
                        visited: AtomicBool::new(false),
                    };
                    shard.map.insert(cache_key.clone(), entry);
                    shard.order.push_back(cache_key);
                }
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }
```

- [ ] **Step 6: Build and run the reentrancy + full suite**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py -v`
Expected: all three tests **PASS** (no crash; `current_size <= 4`; recursion still caches; reentrant
value correct).

Run: `make test-only`
Expected: full existing pytest suite still **PASSES** (no normal-path regression).

Run: `cargo test`
Expected: Rust unit tests **PASS**.

- [ ] **Step 7: Commit**

```bash
git add src/store.rs
git commit -m "$(cat <<'EOF'
fix: guard __call__ against reentrant __eq__ aliasing/deadlock (#30)

Add a borrow-region-scoped reentrancy guard (try_enter). A reentrant call (or a
thread handed the GIL mid-__eq__) bypasses the cache and recomputes instead of
taking a second, conflicting shard guard. The wrapped function is called outside
the guard, so recursive @cache functions keep caching.

GIL builds use a non-atomic GIL-serialized flag; free-threaded builds use a
per-thread active set (parking_lot::RwLock still handles cross-thread access).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Guard `get`, `_probe`, and `set`

**Files:**
- Modify: `src/store.rs` (`get` ~line 439, `_probe` ~line 472, `set` ~line 505)
- Modify: `tests/test_reentrancy.py` (append entry-point test)

- [ ] **Step 1: Write the failing test (append to `tests/test_reentrancy.py`)**

```python
def test_reentry_via_get_set_probe_is_safe():
    """A key __eq__ that re-enters through get/set/_probe during a lookup must
    bypass safely (no crash / no deadlock)."""

    @cache(max_size=8)
    def f(key):
        return 1

    class K:
        def __init__(self, action):
            self.action = action

        def __hash__(self):
            return 0

        def __eq__(self, other):
            self.action()  # re-enter through another entry point during probe
            return self is other

    f(K(lambda: None))  # prime with a hash-0 key so later probes collide
    f(K(lambda: f.get(K(lambda: None))))
    f(K(lambda: f.set(99, K(lambda: None))))
    f(K(lambda: f._probe(K(lambda: None))))
    # Reaching here without crashing or hanging is the assertion.
    assert f.cache_info().current_size <= 8
```

- [ ] **Step 2: Run to verify it fails**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py::test_reentry_via_get_set_probe_is_safe -v`
Expected: **FAIL** (UB/crash on GIL builds; would deadlock on free-threaded).

- [ ] **Step 3: Rewrite `get`**

Replace `get` (currently `src/store.rs:438-468`) with:

```rust
    /// Cache lookup only. Returns the cached value or None on miss.
    #[pyo3(signature = (*args, **kwargs))]
    fn get<'py>(
        &self,
        py: Python<'py>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<Option<Py<PyAny>>> {
        let (hash, key_ptr, _key_owner) = Self::hash_args(py, &args, &kwargs)?;
        let borrowed = BorrowedArgs { hash, ptr: key_ptr };
        let shard_idx = hash as usize & self.shard_mask;

        // Reentrant calls bypass (report miss) rather than take a second guard.
        if let Some(_enter) = self.try_enter() {
            let shard = self.shards[shard_idx].read();
            if let Some(entry) = shard.map.get(&borrowed) {
                if let Some(ttl) = self.ttl {
                    if entry.created_at.elapsed() > ttl {
                        drop(shard);
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        return Ok(None);
                    }
                }
                entry.visited.store(true, Ordering::Relaxed);
                let val = entry.value.clone_ref(py);
                drop(shard);
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Some(val));
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }
```

- [ ] **Step 4: Rewrite `_probe`**

Replace `_probe` (currently `src/store.rs:471-501`) with:

```rust
    /// Cache lookup returning (hit, value) to distinguish cached None from miss.
    #[pyo3(signature = (*args, **kwargs))]
    fn _probe<'py>(
        &self,
        py: Python<'py>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<(bool, Py<PyAny>)> {
        let (hash, key_ptr, _key_owner) = Self::hash_args(py, &args, &kwargs)?;
        let borrowed = BorrowedArgs { hash, ptr: key_ptr };
        let shard_idx = hash as usize & self.shard_mask;

        // Reentrant calls bypass (report miss).
        if let Some(_enter) = self.try_enter() {
            let shard = self.shards[shard_idx].read();
            if let Some(entry) = shard.map.get(&borrowed) {
                if let Some(ttl) = self.ttl {
                    if entry.created_at.elapsed() > ttl {
                        drop(shard);
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        return Ok((false, py.None()));
                    }
                }
                entry.visited.store(true, Ordering::Relaxed);
                let val = entry.value.clone_ref(py);
                drop(shard);
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok((true, val));
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);
        Ok((false, py.None()))
    }
```

- [ ] **Step 5: Rewrite `set`**

Replace `set` (currently `src/store.rs:503-543`) with:

```rust
    /// Store a value in the cache for the given arguments.
    #[pyo3(signature = (value, *args, **kwargs))]
    fn set<'py>(
        &self,
        py: Python<'py>,
        value: Py<PyAny>,
        args: Bound<'py, PyTuple>,
        kwargs: Option<Bound<'py, PyDict>>,
    ) -> PyResult<()> {
        let cache_key = Self::make_key(py, &args, &kwargs)?;
        let shard_idx = cache_key.shard_index(self.shard_mask);

        // Reentrant calls skip the store rather than take a second guard.
        if let Some(_enter) = self.try_enter() {
            let mut shard = self.shards[shard_idx].write();

            if shard.map.get(&cache_key).is_none() {
                // New key: evict if needed, then insert
                while shard.map.len() >= shard.capacity {
                    Self::evict_one(&mut shard);
                    if shard.order.is_empty() {
                        break;
                    }
                }
                let entry = SieveEntry {
                    value: value.clone_ref(py),
                    created_at: Instant::now(),
                    visited: AtomicBool::new(false),
                };
                shard.map.insert(cache_key.clone(), entry);
                shard.order.push_back(cache_key);
            } else {
                // Existing key: update value in place
                let entry = SieveEntry {
                    value: value.clone_ref(py),
                    created_at: Instant::now(),
                    visited: AtomicBool::new(false),
                };
                shard.map.insert(cache_key, entry);
            }
        }

        Ok(())
    }
```

- [ ] **Step 6: Build and test**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py -v`
Expected: all reentrancy tests **PASS**.

Run: `make test-only`
Expected: full suite **PASSES** (async backend uses `_probe`/`set`/`get`; confirm `test_async.py` green).

- [ ] **Step 7: Commit**

```bash
git add src/store.rs tests/test_reentrancy.py
git commit -m "$(cat <<'EOF'
fix: guard get/_probe/set against reentrancy (#30)

Reentrant lookups report a miss; reentrant set skips the store. Same
borrow-region guard as __call__.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Guard `cache_clear` and `cache_info`

These run no Python code, so they cannot *trigger* reentry — but they *can be called from* a key's
`__eq__`, where their shard guards would alias/deadlock. Guard defensively.

**Files:**
- Modify: `src/store.rs` (`cache_info` ~line 545, `cache_clear` ~line 558)
- Modify: `tests/test_reentrancy.py`

- [ ] **Step 1: Write the failing test (append)**

```python
def test_reentry_via_clear_and_info_is_safe():
    """cache_clear()/cache_info() called from inside a key __eq__ must bypass
    safely instead of aliasing the outer borrow."""

    @cache(max_size=8)
    def f(key):
        return 1

    class K:
        def __init__(self, action):
            self.action = action

        def __hash__(self):
            return 0

        def __eq__(self, other):
            self.action()
            return self is other

    f(K(lambda: None))  # prime
    f(K(lambda: f.cache_clear()))
    info_during = []
    f(K(lambda: info_during.append(f.cache_info())))
    # No crash/deadlock == pass. Reentrant cache_info reports current_size 0.
    assert info_during and info_during[0].current_size == 0
```

- [ ] **Step 2: Run to verify it fails**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py::test_reentry_via_clear_and_info_is_safe -v`
Expected: **FAIL** (UB/crash; deadlock on free-threaded).

- [ ] **Step 3: Rewrite `cache_info`**

Replace `cache_info` (currently `src/store.rs:545-556`) with:

```rust
    fn cache_info(&self) -> CacheInfo {
        // If reentrant, we cannot safely read the shards; report counters only.
        let _enter = match self.try_enter() {
            Some(g) => g,
            None => {
                return CacheInfo {
                    hits: self.hits.load(Ordering::Relaxed),
                    misses: self.misses.load(Ordering::Relaxed),
                    max_size: self.max_size,
                    current_size: 0,
                };
            }
        };

        let mut current_size = 0;
        for shard in self.shards.iter() {
            current_size += shard.read().map.len();
        }
        CacheInfo {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            max_size: self.max_size,
            current_size,
        }
    }
```

- [ ] **Step 4: Rewrite `cache_clear`**

Replace `cache_clear` (currently `src/store.rs:558-567`) with:

```rust
    fn cache_clear(&self) {
        // If reentrant, skip rather than alias the outer borrow.
        let _enter = match self.try_enter() {
            Some(g) => g,
            None => return,
        };

        for shard in self.shards.iter() {
            let mut s = shard.write();
            s.map.clear();
            s.order.clear();
            s.hand = 0;
        }
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
```

- [ ] **Step 5: Build and test**

Run: `make build-debug && uv run pytest tests/test_reentrancy.py -v`
Expected: all reentrancy tests **PASS**.

Run: `make test-only`
Expected: full suite **PASSES**.

- [ ] **Step 6: Commit**

```bash
git add src/store.rs tests/test_reentrancy.py
git commit -m "$(cat <<'EOF'
fix: guard cache_clear/cache_info against reentrant invocation (#30)

These run no Python code but can be called from a key __eq__; guard them so
they bypass instead of aliasing the outer borrow. Reentrant cache_info reports
current_size 0.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Update SAFETY comments and docs

**Files:**
- Modify: `src/key.rs` (SAFETY comments at lines 48-49 and 94-95)
- Modify: `docs/ARCHITECTURE.md`
- Modify: `README.md`, `llms.txt`, `llms-full.txt`

- [ ] **Step 1: Update the SAFETY comment in `CacheKey::eq` (`src/key.rs:48-49`)**

Replace the two comment lines:

```rust
        // SAFETY: PartialEq is only called from HashMap lookups inside
        // #[pymethods] (__call__, cache_clear), so the GIL is always held.
        // This is the same direct C API call that lru_cache uses.
```

with:

```rust
        // SAFETY: PartialEq is only called from HashMap lookups inside
        // #[pymethods], so the GIL is always held. The arbitrary Python __eq__
        // this can run may re-enter the cache or hand off the GIL; CachedFunction
        // serializes that via its reentrancy guard (try_enter, see issue #30), so
        // no aliasing/reentrant shard guard is taken during this comparison.
        // This is the same direct C API call that lru_cache uses.
```

- [ ] **Step 2: Update the SAFETY comment in `BorrowedArgs::equivalent` (`src/key.rs:94-96`)**

Replace:

```rust
        // SAFETY: Called only inside #[pymethods] where the GIL is held.
        // `self.ptr` points to a live Python object (the args tuple on the
        // call stack) and `key.key_obj` is an owned reference in the map.
```

with:

```rust
        // SAFETY: Called only inside #[pymethods] where the GIL is held.
        // `self.ptr` points to a live Python object (the args tuple on the
        // call stack) and `key.key_obj` is an owned reference in the map. The
        // arbitrary Python __eq__ this runs may re-enter; CachedFunction's
        // reentrancy guard prevents a second, aliasing shard guard (issue #30).
```

- [ ] **Step 3: Document the invariant in `docs/ARCHITECTURE.md`**

Add a bullet to the critical-invariants / thread-safety section (find the existing thread-safety
discussion; if there is an invariants list, append there):

```markdown
- **Reentrancy guard (issue #30).** A cache lookup runs Python `__eq__` (via
  `PyObject_RichCompareBool`) while a shard guard is live. `CachedFunction::try_enter` marks the
  function active for the duration of each borrow region; a call that finds it already active
  (a key `__eq__`/`__hash__` re-entering the same cache, or — on GIL builds — another thread handed
  the GIL mid-comparison) **bypasses the cache and recomputes**. The wrapped function is always
  invoked outside this guard, so recursive `@cache` functions still cache. GIL builds use a single
  GIL-serialized flag; free-threaded builds use a per-thread active set.
```

- [ ] **Step 4: Add a user-facing caveat to `README.md`, `llms.txt`, `llms-full.txt`**

Add one line near the caveats/limitations of each (match the existing wording/heading style):

```markdown
- Cache keys whose `__eq__`/`__hash__` call back into the *same* cached function are computed but
  not cached for that reentrant call (this keeps the cache memory-safe under reentrancy; see #30).
```

- [ ] **Step 5: Verify docs build/lint isn't broken and nothing else regressed**

Run: `make build-debug && make test-only`
Expected: full suite **PASSES** (comment/doc changes are non-functional).

- [ ] **Step 6: Commit**

```bash
git add src/key.rs docs/ARCHITECTURE.md README.md llms.txt llms-full.txt
git commit -m "$(cat <<'EOF'
docs: document reentrancy guard and update SAFETY comments (#30)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Full quality gate + risky-change matrix & benchmarks

**Files:** none (verification only; commit any fmt fixes)

- [ ] **Step 1: Run the full gate**

Run: `make all`
Expected: fmt clean, lint clean (`ruff`, `ty`, `cargo clippy -- -D warnings`), all tests pass.
If `make fmt` changes files, commit them: `git commit -am "style: cargo/ruff fmt"`.

- [ ] **Step 2: Run the Python version matrix (includes free-threaded 3.13t → exercises the
  `Py_GIL_DISABLED` path)**

Run: `make test-matrix -j`
Expected: 3.9–3.14 (incl. 3.13t) **PASS**. The free-threaded run is the only place the
`#[cfg(Py_GIL_DISABLED)]` guard code is compiled and exercised; confirm `tests/test_reentrancy.py`
passes there (proves the deadlock is gone).

> If 3.13t is not installed locally, install it (`uv python install 3.13t`) or note that CI covers
> it; do not skip silently.

- [ ] **Step 3: Run benchmarks and confirm no material hot-path regression**

Run: `make bench`
Expected: single-thread hit throughput within noise of `master` (the guard adds one GIL-serialized
flag op per borrow region). Record the before/after hit-path numbers for the PR body. If a material
regression appears, stop and revisit the mechanism before opening the PR.

- [ ] **Step 4: Commit any formatting/benchmark-artifact changes if needed**

```bash
git status   # commit only intended changes (e.g. fmt); do NOT commit local bench JSON unless desired
```

---

## Task 7: Open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin HEAD
```

- [ ] **Step 2: Create the PR**

```bash
gh pr create --base master --title "fix: reentrancy soundness for in-process cache (#30)" --body "$(cat <<'EOF'
Closes #30

## What & why
A cache lookup holds a shard guard across hashbrown probing, which runs Python `__eq__`
(`PyObject_RichCompareBool`). A key whose `__eq__`/`__hash__` re-enters the same cache took a second,
conflicting shard guard: on GIL builds this aliased `&Shard` with `&mut Shard` (UB + possible
use-after-free via table realloc, plus `current_size > max_size` corruption); on free-threaded builds
it deadlocked the `RwLock`. (The same can happen across threads on GIL builds when the GIL is handed
off mid-`__eq__`.)

## Fix
A borrow-region-scoped reentrancy guard (`try_enter`). A reentrant entrant **bypasses the cache and
recomputes** instead of taking a conflicting guard. The wrapped function is called outside the guard,
so recursive `@cache` functions keep caching. GIL builds use a single GIL-serialized flag;
free-threaded builds use a per-thread active set (the `parking_lot::RwLock` still handles cross-thread
access). The shared backend is unaffected (no Python `__eq__` during lookup).

## Behavioral impact
- Reentrant `__call__` is computed but not cached (counted as a miss).
- Reentrant `get`/`_probe` report a miss; reentrant `set` is a no-op; reentrant `cache_info` reports
  `current_size = 0`. All previously UB/deadlock.

## Tests / gates
- New `tests/test_reentrancy.py`: reproducer (no crash + capacity invariant holds), recursion still
  caches, bypass returns correct value, all entry points safe.
- Gates run: `make all`; `make test-matrix -j` (3.9–3.14 incl. free-threaded 3.13t — exercises the
  `Py_GIL_DISABLED` path); `make bench` (hot-path within noise — numbers below).

<!-- paste before/after bench hit-path numbers here -->

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Stop — request human review.** Do not merge.

---

## Self-review notes (author)

- **Spec coverage:** both build configs (Tasks 2 mechanism + Task 6 matrix); bypass+recompute (Tasks
  2–4 per-method table); guard scoped to borrow regions / recursion preserved (Task 1 recursion test +
  Task 2 `__call__` shape); all six methods guarded (Tasks 2–4); shared backend untouched (no task);
  docs (Task 5); risky-change gates (Task 6).
- **Naming consistency:** `try_enter`, `EnterGuard`, `ReentryCell`, `reentry`, `ACTIVE`, `entered`
  used identically across all tasks.
- **No placeholders:** every code/step is concrete. The only intentional fill-in is the benchmark
  numbers pasted into the PR body in Task 7.
- **Known limitation:** the reproducer triggers UB, so "fail before" on GIL builds may be a crash
  rather than a clean assertion failure (documented in Task 1). The free-threaded deadlock and the
  capacity-invariant assertion are the deterministic fail-before signals.
```
