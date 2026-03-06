use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hashbrown::HashMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use pyo3::{ffi, Bound, PyErr};

use crate::entry::SieveEntry;
use crate::key::{BorrowedArgs, CacheKey};

const MAX_SHARDS: usize = 16;
const MIN_SHARD_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// Passthrough hasher: CacheKey already carries a well-distributed Python hash.
// Re-hashing it through foldhash (hashbrown's default) wastes ~1-2ns per
// lookup. This hasher stores the hash verbatim.
// ---------------------------------------------------------------------------
struct PassthroughHasher(u64);

impl Hasher for PassthroughHasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline(always)]
    fn write(&mut self, bytes: &[u8]) {
        // Fallback: interpret first 8 bytes as u64.
        debug_assert!(bytes.len() >= 8, "PassthroughHasher expects >= 8 bytes");
        self.0 = u64::from_ne_bytes(bytes[..8].try_into().unwrap());
    }
    #[inline(always)]
    fn write_isize(&mut self, i: isize) {
        self.0 = i as u64;
    }
    #[inline(always)]
    fn write_i64(&mut self, i: i64) {
        self.0 = i as u64;
    }
    #[inline(always)]
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }
}

#[derive(Clone, Default)]
struct PassthroughBuildHasher;

impl BuildHasher for PassthroughBuildHasher {
    type Hasher = PassthroughHasher;
    #[inline(always)]
    fn build_hasher(&self) -> PassthroughHasher {
        PassthroughHasher(0)
    }
}

// ---------------------------------------------------------------------------
// GIL-conditional lock: under GIL-enabled Python the GIL already serialises
// all #[pymethods] access, so the per-shard RwLock is pure overhead (~8ns).
// Under free-threaded Python (3.13t+) we need real locking.
// ---------------------------------------------------------------------------

#[cfg(Py_GIL_DISABLED)]
type ShardLock = parking_lot::RwLock<Shard>;

#[cfg(not(Py_GIL_DISABLED))]
type ShardLock = GilCell<Shard>;

/// Zero-cost lock substitute for GIL-enabled builds.
///
/// SAFETY: All access is through `#[pymethods]` which hold the GIL.
/// We never call `py.allow_threads()` while a guard is live.
#[cfg(not(Py_GIL_DISABLED))]
struct GilCell<T>(UnsafeCell<T>);

#[cfg(not(Py_GIL_DISABLED))]
unsafe impl<T: Send> Send for GilCell<T> {}
#[cfg(not(Py_GIL_DISABLED))]
unsafe impl<T: Send> Sync for GilCell<T> {}

#[cfg(not(Py_GIL_DISABLED))]
impl<T> GilCell<T> {
    fn new(val: T) -> Self {
        GilCell(UnsafeCell::new(val))
    }

    #[inline(always)]
    fn read(&self) -> GilReadGuard<'_, T> {
        GilReadGuard(&self.0)
    }

    #[inline(always)]
    fn write(&self) -> GilWriteGuard<'_, T> {
        GilWriteGuard(&self.0)
    }
}

#[cfg(not(Py_GIL_DISABLED))]
struct GilReadGuard<'a, T>(&'a UnsafeCell<T>);

#[cfg(not(Py_GIL_DISABLED))]
impl<T> Drop for GilReadGuard<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {}
}

#[cfg(not(Py_GIL_DISABLED))]
impl<T> Deref for GilReadGuard<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        // SAFETY: GIL is held; no concurrent writers.
        unsafe { &*self.0.get() }
    }
}

#[cfg(not(Py_GIL_DISABLED))]
struct GilWriteGuard<'a, T>(&'a UnsafeCell<T>);

#[cfg(not(Py_GIL_DISABLED))]
impl<T> Deref for GilWriteGuard<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        unsafe { &*self.0.get() }
    }
}

#[cfg(not(Py_GIL_DISABLED))]
impl<T> DerefMut for GilWriteGuard<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.0.get() }
    }
}

struct Shard {
    map: HashMap<CacheKey, SieveEntry, PassthroughBuildHasher>,
    order: VecDeque<CacheKey>,
    hand: usize,
    capacity: usize,
}

#[pyclass(frozen)]
pub struct CacheInfo {
    #[pyo3(get)]
    pub hits: u64,
    #[pyo3(get)]
    pub misses: u64,
    #[pyo3(get)]
    pub max_size: usize,
    #[pyo3(get)]
    pub current_size: usize,
}

#[pymethods]
impl CacheInfo {
    fn __repr__(&self) -> String {
        format!(
            "CacheInfo(hits={}, misses={}, max_size={}, current_size={})",
            self.hits, self.misses, self.max_size, self.current_size
        )
    }
}

#[pyclass(frozen)]
pub struct CachedFunction {
    fn_obj: Py<PyAny>,
    shards: Box<[ShardLock]>,
    shard_mask: usize,
    ttl: Option<Duration>,
    max_size: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CachedFunction {
    /// Evict one entry using the SIEVE algorithm.
    /// Called with the shard write-locked (caller passes `&mut Shard`).
    fn evict_one(shard: &mut Shard) {
        let initial_len = shard.order.len();
        if initial_len == 0 {
            return;
        }

        let mut scanned = 0;
        while scanned <= initial_len {
            if shard.order.is_empty() {
                break;
            }
            if shard.hand >= shard.order.len() {
                shard.hand = 0;
            }

            let key = shard.order[shard.hand].clone();

            // Read visited status (ends immutable borrow before mutations)
            let status = shard
                .map
                .get(&key)
                .map(|e| e.visited.load(Ordering::Relaxed));

            match status {
                Some(true) => {
                    // Second chance: clear visited bit, advance hand
                    if let Some(entry) = shard.map.get(&key) {
                        entry.visited.store(false, Ordering::Relaxed);
                    }
                    shard.hand += 1;
                    scanned += 1;
                }
                Some(false) => {
                    // Evict this entry
                    shard.map.remove(&key);
                    shard.order.remove(shard.hand);
                    if shard.hand >= shard.order.len() && !shard.order.is_empty() {
                        shard.hand = 0;
                    }
                    return;
                }
                None => {
                    // Stale entry (TTL-removed or otherwise gone from map)
                    shard.order.remove(shard.hand);
                    if shard.hand >= shard.order.len() && !shard.order.is_empty() {
                        shard.hand = 0;
                    }
                    // Don't increment scanned — order shifted, retry at same position
                }
            }
        }
    }

    #[inline(always)]
    fn make_key<'py>(
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: &Option<Bound<'py, PyDict>>,
    ) -> PyResult<CacheKey> {
        let key_obj: Py<PyAny> = match kwargs {
            Some(ref kw) if !kw.is_empty() => {
                let builtins = py.import("builtins")?;
                let items = kw.call_method0("items")?;
                let sorted_items = builtins.call_method1("sorted", (items,))?;
                let kw_tup = builtins.getattr("tuple")?.call1((sorted_items,))?;
                let combined = PyTuple::new(py, [args.as_any().clone(), kw_tup])?;
                combined.unbind().into()
            }
            _ => args.clone().unbind().into(),
        };
        CacheKey::new(py, key_obj)
    }

    /// Compute hash + key pointer for the common no-kwargs fast path, or fall
    /// back to building a composite key object when kwargs are present.
    /// Returns `(hash, key_ptr)` where key_ptr is the raw PyObject* to compare.
    /// On the kwargs path, also returns an owned Py<PyAny> to keep the
    /// composite key alive; on the no-kwargs path this is None.
    #[inline(always)]
    fn hash_args<'py>(
        py: Python<'py>,
        args: &Bound<'py, PyTuple>,
        kwargs: &Option<Bound<'py, PyDict>>,
    ) -> PyResult<(isize, *mut ffi::PyObject, Option<Py<PyAny>>)> {
        match kwargs {
            Some(ref kw) if !kw.is_empty() => {
                // Rare path: build composite key, hash it
                let builtins = py.import("builtins")?;
                let items = kw.call_method0("items")?;
                let sorted_items = builtins.call_method1("sorted", (items,))?;
                let kw_tup = builtins.getattr("tuple")?.call1((sorted_items,))?;
                let combined = PyTuple::new(py, [args.as_any().clone(), kw_tup])?;
                let key_obj: Py<PyAny> = combined.unbind().into();
                let ptr = key_obj.as_ptr();
                let hash = unsafe { ffi::PyObject_Hash(ptr) };
                if hash == -1 {
                    return Err(PyErr::fetch(py));
                }
                Ok((hash, ptr, Some(key_obj)))
            }
            _ => {
                // Fast path: hash the args tuple directly via raw FFI
                let ptr = args.as_ptr();
                let hash = unsafe { ffi::PyObject_Hash(ptr) };
                if hash == -1 {
                    return Err(PyErr::fetch(py));
                }
                Ok((hash, ptr, None))
            }
        }
    }
}

#[pymethods]
impl CachedFunction {
    #[new]
    #[pyo3(signature = (fn_obj, max_size, ttl=None))]
    fn new(fn_obj: Py<PyAny>, max_size: usize, ttl: Option<f64>) -> Self {
        let n_shards = (max_size / MIN_SHARD_SIZE)
            .clamp(1, MAX_SHARDS)
            .next_power_of_two()
            .min(MAX_SHARDS);
        let per_shard = max_size.div_ceil(n_shards);

        #[cfg(Py_GIL_DISABLED)]
        let shards: Vec<ShardLock> = (0..n_shards)
            .map(|_| {
                parking_lot::RwLock::new(Shard {
                    map: HashMap::with_capacity_and_hasher(per_shard, PassthroughBuildHasher),
                    order: VecDeque::with_capacity(per_shard),
                    hand: 0,
                    capacity: per_shard,
                })
            })
            .collect();

        #[cfg(not(Py_GIL_DISABLED))]
        let shards: Vec<ShardLock> = (0..n_shards)
            .map(|_| {
                GilCell::new(Shard {
                    map: HashMap::with_capacity_and_hasher(per_shard, PassthroughBuildHasher),
                    order: VecDeque::with_capacity(per_shard),
                    hand: 0,
                    capacity: per_shard,
                })
            })
            .collect();

        CachedFunction {
            fn_obj,
            shards: shards.into_boxed_slice(),
            shard_mask: n_shards - 1,
            ttl: ttl.map(Duration::from_secs_f64),
            max_size,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

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

        // FAST PATH: read lock on one shard, lookup via BorrowedArgs
        {
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
        }

        // Cache miss: call the wrapped function (no lock held)
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?.unbind();

        // SLOW PATH: write lock, double-check, evict if needed, insert.
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

        {
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

        self.misses.fetch_add(1, Ordering::Relaxed);
        Ok(result)
    }

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

        drop(shard);
        self.misses.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

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

        Ok(())
    }

    fn cache_info(&self) -> CacheInfo {
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

    fn cache_clear(&self) {
        for shard in self.shards.iter() {
            let mut s = shard.write();
            s.map.clear();
            s.order.clear();
            s.hand = 0;
        }
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}
