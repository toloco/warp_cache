use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hashbrown::HashMap;
use parking_lot::RwLock;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use crate::entry::SieveEntry;
use crate::key::CacheKey;

const MAX_SHARDS: usize = 16;
const MIN_SHARD_SIZE: usize = 8;

struct Shard {
    map: HashMap<CacheKey, SieveEntry>,
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
    shards: Box<[RwLock<Shard>]>,
    n_shards: usize,
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
}

#[pymethods]
impl CachedFunction {
    #[new]
    #[pyo3(signature = (fn_obj, max_size, ttl=None))]
    fn new(fn_obj: Py<PyAny>, max_size: usize, ttl: Option<f64>) -> Self {
        let n_shards = (max_size / MIN_SHARD_SIZE).clamp(1, MAX_SHARDS);
        let per_shard = max_size.div_ceil(n_shards);
        let shards: Vec<RwLock<Shard>> = (0..n_shards)
            .map(|_| {
                RwLock::new(Shard {
                    map: HashMap::with_capacity(per_shard),
                    order: VecDeque::with_capacity(per_shard),
                    hand: 0,
                    capacity: per_shard,
                })
            })
            .collect();
        CachedFunction {
            fn_obj,
            shards: shards.into_boxed_slice(),
            n_shards,
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
        let shard_idx = cache_key.shard_index(self.n_shards);

        // FAST PATH: read lock on one shard
        {
            let shard = self.shards[shard_idx].read();
            if let Some(entry) = shard.map.get(&cache_key) {
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

        // SLOW PATH: write lock, double-check, evict if needed, insert
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
        let cache_key = Self::make_key(py, &args, &kwargs)?;
        let shard_idx = cache_key.shard_index(self.n_shards);

        let shard = self.shards[shard_idx].read();
        if let Some(entry) = shard.map.get(&cache_key) {
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
        let shard_idx = cache_key.shard_index(self.n_shards);

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
