use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::entry::SieveEntry;
use crate::key::CacheKey;

struct SieveState {
    order: VecDeque<CacheKey>,
    hand: usize,
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
    map: papaya::HashMap<CacheKey, SieveEntry>,
    sieve: Mutex<SieveState>,
    ttl: Option<Duration>,
    capacity: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CachedFunction {
    /// Evict one entry using the SIEVE algorithm.
    /// Must be called with `self.sieve` locked. The `sieve` parameter is the
    /// locked guard, and `pinned` is a papaya pin obtained by the caller.
    fn evict_one(
        sieve: &mut SieveState,
        map: &papaya::HashMap<CacheKey, SieveEntry>,
    ) {
        let initial_len = sieve.order.len();
        if initial_len == 0 {
            return;
        }

        let pinned = map.pin();
        let mut scanned = 0;
        while scanned <= initial_len {
            if sieve.order.is_empty() {
                break;
            }
            if sieve.hand >= sieve.order.len() {
                sieve.hand = 0;
            }

            let key = sieve.order[sieve.hand].clone();

            match pinned.get(&key) {
                Some(entry) => {
                    if entry.visited.load(Ordering::Relaxed) {
                        // Second chance: clear visited bit, advance hand
                        entry.visited.store(false, Ordering::Relaxed);
                        sieve.hand += 1;
                        scanned += 1;
                    } else {
                        // Evict this entry
                        pinned.remove(&key);
                        sieve.order.remove(sieve.hand);
                        if sieve.hand >= sieve.order.len() && !sieve.order.is_empty() {
                            sieve.hand = 0;
                        }
                        return;
                    }
                }
                None => {
                    // Stale entry (TTL-removed or otherwise gone from map)
                    sieve.order.remove(sieve.hand);
                    if sieve.hand >= sieve.order.len() && !sieve.order.is_empty() {
                        sieve.hand = 0;
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
}

#[pymethods]
impl CachedFunction {
    #[new]
    #[pyo3(signature = (fn_obj, max_size, ttl=None))]
    fn new(fn_obj: Py<PyAny>, max_size: usize, ttl: Option<f64>) -> Self {
        CachedFunction {
            fn_obj,
            map: papaya::HashMap::with_capacity(max_size),
            sieve: Mutex::new(SieveState {
                order: VecDeque::with_capacity(max_size),
                hand: 0,
            }),
            ttl: ttl.map(Duration::from_secs_f64),
            capacity: max_size,
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
        // Build the cache key
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
        let cache_key = CacheKey::new(py, key_obj)?;

        // FAST PATH: lock-free lookup via papaya
        {
            let pinned = self.map.pin();
            if let Some(entry) = pinned.get(&cache_key) {
                if let Some(ttl) = self.ttl {
                    if entry.created_at.elapsed() > ttl {
                        // Expired — remove lock-free and fall through to miss
                        pinned.remove(&cache_key);
                    } else {
                        entry.visited.store(true, Ordering::Relaxed);
                        let val = entry.value.clone_ref(py);
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        return Ok(val);
                    }
                } else {
                    entry.visited.store(true, Ordering::Relaxed);
                    let val = entry.value.clone_ref(py);
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(val);
                }
            }
        }

        // Cache miss: call the wrapped function (no lock held)
        let result = self.fn_obj.bind(py).call(args, kwargs.as_ref())?.unbind();

        // SLOW PATH: lock sieve, double-check, evict if needed, insert
        {
            let pinned = self.map.pin();
            let mut sieve = self.sieve.lock();

            // Double-check: another thread may have inserted while we were computing
            let needs_insert = match pinned.get(&cache_key) {
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
                pinned.remove(&cache_key);

                // Evict if at capacity
                while pinned.len() >= self.capacity {
                    Self::evict_one(&mut sieve, &self.map);
                    if sieve.order.is_empty() {
                        break;
                    }
                }

                let entry = SieveEntry {
                    value: result.clone_ref(py),
                    created_at: Instant::now(),
                    visited: std::sync::atomic::AtomicBool::new(false),
                };
                pinned.insert(cache_key.clone(), entry);
                sieve.order.push_back(cache_key);
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
        let cache_key = Self::make_key(py, &args, &kwargs)?;

        let pinned = self.map.pin();
        if let Some(entry) = pinned.get(&cache_key) {
            if let Some(ttl) = self.ttl {
                if entry.created_at.elapsed() > ttl {
                    // Expired — remove lock-free
                    pinned.remove(&cache_key);
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    return Ok(None);
                }
            }
            entry.visited.store(true, Ordering::Relaxed);
            let val = entry.value.clone_ref(py);
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(val));
        }

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
        let pinned = self.map.pin();
        let mut sieve = self.sieve.lock();

        // Evict if at capacity (and key is not already present)
        if pinned.get(&cache_key).is_none() {
            while pinned.len() >= self.capacity {
                Self::evict_one(&mut sieve, &self.map);
                if sieve.order.is_empty() {
                    break;
                }
            }
            let entry = SieveEntry {
                value: value.clone_ref(py),
                created_at: Instant::now(),
                visited: std::sync::atomic::AtomicBool::new(false),
            };
            pinned.insert(cache_key.clone(), entry);
            sieve.order.push_back(cache_key);
        } else {
            // Key exists — update in place
            let entry = SieveEntry {
                value: value.clone_ref(py),
                created_at: Instant::now(),
                visited: std::sync::atomic::AtomicBool::new(false),
            };
            pinned.insert(cache_key, entry);
        }

        Ok(())
    }

    fn cache_info(&self) -> CacheInfo {
        let pinned = self.map.pin();
        CacheInfo {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            max_size: self.capacity,
            current_size: pinned.len(),
        }
    }

    fn cache_clear(&self) {
        let mut sieve = self.sieve.lock();
        sieve.order.clear();
        sieve.hand = 0;
        let pinned = self.map.pin();
        pinned.clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
    }
}
